use std::collections::VecDeque;
use std::sync::Mutex;

use htpc_core::Moderation;
use tokio::sync::mpsc::{self, UnboundedReceiver};
use tokio::task::JoinHandle;

use super::*;
use crate::emotes::TwitchCdn;

const JOIN: &str = ":justinfan1!justinfan1@justinfan1.tmi.twitch.tv JOIN #chan";

fn privmsg(id: &str, text: &str) -> String {
    format!(
        "@badge-info=;badges=;color=#FF0000;display-name=Alice;emotes=;id={id};room-id=123;tmi-sent-ts=1;user-id=2 :alice!alice@alice.tmi.twitch.tv PRIVMSG #chan :{text}"
    )
}

/// Hands out prepared links in order, then refuses. Remembers when each attempt was made.
#[derive(Default)]
struct Script {
    links: Mutex<VecDeque<Link>>,
    attempts: Mutex<Vec<Instant>>,
}

impl Script {
    fn with(links: Vec<Link>) -> Arc<Self> {
        Arc::new(Self {
            links: Mutex::new(links.into()),
            attempts: Mutex::default(),
        })
    }

    fn attempts(&self) -> Vec<Instant> {
        self.attempts.lock().unwrap().clone()
    }

    fn gaps(&self) -> Vec<Duration> {
        let a = self.attempts();
        a.windows(2).map(|w| w[1] - w[0]).collect()
    }
}

impl Connector for Script {
    fn connect(&self) -> crate::link::ConnectFuture<'_> {
        Box::pin(async move {
            self.attempts.lock().unwrap().push(Instant::now());
            self.links
                .lock()
                .unwrap()
                .pop_front()
                .ok_or_else(|| "server unreachable".to_owned())
        })
    }
}

/// The test's end of a link.
struct Wire {
    to_session: mpsc::Sender<Result<IRCMessage, String>>,
    from_session: UnboundedReceiver<IRCMessage>,
}

impl Wire {
    fn say(&self, raw: &str) {
        let message = IRCMessage::parse(raw).expect("test line parses");
        self.to_session.try_send(Ok(message)).expect("room in link");
    }

    /// What the session has sent so far, as raw IRC without tags.
    fn sent(&mut self) -> Vec<String> {
        let mut out = Vec::new();
        while let Ok(m) = self.from_session.try_recv() {
            out.push(format!("{} {}", m.command, m.params.join(" ")));
        }
        out
    }
}

fn wire() -> (Link, Wire) {
    let (to_session, incoming) = mpsc::channel(64);
    let (outgoing, from_session) = mpsc::unbounded_channel();
    (
        Link { incoming, outgoing },
        Wire {
            to_session,
            from_session,
        },
    )
}

fn ctx(
    connector: Arc<dyn Connector>,
    loader: Option<Arc<HttpBadges>>,
    config: SessionConfig,
) -> Ctx {
    let badges = Arc::new(BadgeBook::default());
    Ctx {
        connector,
        channel: "chan".into(),
        mapper: Mapper::new("chan", Arc::new(TwitchCdn), Arc::clone(&badges)),
        badges,
        loader,
        config,
    }
}

fn start(connector: Arc<dyn Connector>) -> (UnboundedReceiver<ChatEvent>, JoinHandle<()>) {
    let (tx, rx) = mpsc::unbounded_channel();
    let task = tokio::spawn(run(ctx(connector, None, SessionConfig::default()), tx));
    (rx, task)
}

async fn next(rx: &mut UnboundedReceiver<ChatEvent>) -> ChatEvent {
    rx.recv().await.expect("session ended")
}

fn lines(event: ChatEvent) -> Vec<ChatMessage> {
    match event {
        ChatEvent::Messages(lines) => lines,
        other => panic!("expected lines, got {other:?}"),
    }
}

#[test]
fn the_pause_doubles_up_to_the_cap() {
    let b = Backoff {
        initial: Duration::from_secs(1),
        max: Duration::from_secs(30),
    };
    let secs: Vec<u64> = (1..=8).map(|n| b.delay(n).as_secs()).collect();
    assert_eq!(secs, [1, 2, 4, 8, 16, 30, 30, 30]);
    // Absurd attempt numbers do not overflow.
    assert_eq!(b.delay(u32::MAX), Duration::from_secs(30));
    assert_eq!(b.delay(0), Duration::from_secs(1));
}

#[tokio::test(start_paused = true)]
async fn it_says_hello_anonymously_and_goes_live_on_the_join() {
    let (link, mut wire) = wire();
    let (mut rx, _task) = start(Script::with(vec![link]));
    assert_eq!(
        next(&mut rx).await,
        ChatEvent::Status(ChatStatus::Connecting)
    );

    wire.say(JOIN);
    assert_eq!(next(&mut rx).await, ChatEvent::Status(ChatStatus::Live));

    let sent = wire.sent();
    assert_eq!(sent[0], "CAP REQ twitch.tv/tags twitch.tv/commands");
    assert!(sent[1].starts_with("NICK justinfan"), "{sent:?}");
    assert_eq!(sent[2], "JOIN #chan");
    assert_eq!(sent.len(), 3);
}

#[tokio::test(start_paused = true)]
async fn lines_arriving_together_are_one_batch() {
    let (link, wire) = wire();
    let (mut rx, _task) = start(Script::with(vec![link]));
    wire.say(&privmsg("m1", "one"));
    wire.say(&privmsg("m2", "two"));
    wire.say(&privmsg("m3", "three"));

    assert_eq!(
        next(&mut rx).await,
        ChatEvent::Status(ChatStatus::Connecting)
    );
    assert_eq!(next(&mut rx).await, ChatEvent::Status(ChatStatus::Live));
    let batch = lines(next(&mut rx).await);
    let texts: Vec<_> = batch.iter().map(ChatMessage::text).collect();
    assert_eq!(texts, ["one", "two", "three"]);
    assert_eq!(batch[0].id, "m1");
    assert_eq!(batch[0].user, "Alice");
}

#[tokio::test(start_paused = true)]
async fn a_ban_is_sent_after_the_lines_it_removes() {
    let (link, wire) = wire();
    let (mut rx, _task) = start(Script::with(vec![link]));
    wire.say(&privmsg("m1", "spam"));
    wire.say("@room-id=123;target-user-id=2;tmi-sent-ts=1 :tmi.twitch.tv CLEARCHAT #chan :alice");

    next(&mut rx).await; // Connecting
    next(&mut rx).await; // Live
    assert_eq!(lines(next(&mut rx).await).len(), 1);
    assert_eq!(
        next(&mut rx).await,
        ChatEvent::Moderated(Moderation::ClearUser("alice".into()))
    );
}

#[tokio::test(start_paused = true)]
async fn the_server_ping_is_answered() {
    let (link, mut wire) = wire();
    let (mut rx, _task) = start(Script::with(vec![link]));
    next(&mut rx).await;
    wire.sent(); // the hello
    wire.say("PING :tmi.twitch.tv");
    wire.say(JOIN);
    next(&mut rx).await; // Live: the ping was handled before it
    assert_eq!(wire.sent(), ["PONG tmi.twitch.tv"]);
}

#[tokio::test(start_paused = true)]
async fn a_dropped_connection_is_reported_and_retried_after_a_pause() {
    let (first, wire1) = wire();
    let (second, wire2) = wire();
    let script = Script::with(vec![first, second]);
    let (mut rx, _task) = start(Arc::clone(&script) as Arc<dyn Connector>);

    wire1.say(JOIN);
    assert_eq!(
        next(&mut rx).await,
        ChatEvent::Status(ChatStatus::Connecting)
    );
    assert_eq!(next(&mut rx).await, ChatEvent::Status(ChatStatus::Live));
    drop(wire1);
    assert_eq!(
        next(&mut rx).await,
        ChatEvent::Status(ChatStatus::Reconnecting)
    );

    wire2.say(JOIN);
    assert_eq!(next(&mut rx).await, ChatEvent::Status(ChatStatus::Live));
    assert_eq!(script.gaps(), [Duration::from_secs(1)]);
}

#[tokio::test(start_paused = true)]
async fn a_connection_that_keeps_failing_is_retried_less_and_less_often() {
    // Each link goes live for a moment and drops: that is not stable, so the pause keeps growing.
    let mut links = Vec::new();
    let mut wires = Vec::new();
    for _ in 0..4 {
        let (link, wire) = wire();
        wire.say(JOIN);
        links.push(link);
        wires.push(wire);
    }
    let script = Script::with(links);
    let (_rx, _task) = start(Arc::clone(&script) as Arc<dyn Connector>);
    // Let the first connection start, then cut each one right after it opened.
    for wire in wires {
        tokio::time::sleep(Duration::from_millis(10)).await;
        drop(wire);
    }
    tokio::time::sleep(Duration::from_secs(120)).await;

    let gaps = script.gaps();
    let secs: Vec<u64> = gaps.iter().take(3).map(Duration::as_secs).collect();
    assert_eq!(secs, [1, 2, 4], "{gaps:?}");
}

#[tokio::test(start_paused = true)]
async fn failing_to_connect_backs_off_to_the_cap_and_says_reconnecting_once() {
    let script = Script::with(Vec::new());
    let (mut rx, _task) = start(Arc::clone(&script) as Arc<dyn Connector>);
    assert_eq!(
        next(&mut rx).await,
        ChatEvent::Status(ChatStatus::Connecting)
    );
    assert_eq!(
        next(&mut rx).await,
        ChatEvent::Status(ChatStatus::Reconnecting)
    );

    tokio::time::sleep(Duration::from_secs(200)).await;
    let secs: Vec<u64> = script.gaps().iter().map(Duration::as_secs).collect();
    assert_eq!(&secs[..7], [1, 2, 4, 8, 16, 30, 30]);
    assert!(rx.try_recv().is_err(), "the status is not repeated");
}

#[tokio::test(start_paused = true)]
async fn a_stable_connection_starts_the_backoff_over() {
    let (first, wire1) = wire();
    let (second, wire2) = wire();
    // The first retry has failed once before the second link works.
    let script = Script::with(vec![first]);
    let (mut rx, _task) = start(Arc::clone(&script) as Arc<dyn Connector>);
    wire1.say(JOIN);
    next(&mut rx).await;
    next(&mut rx).await; // Live
                         // Chat is quiet but the server answers our pings: the connection is fine for a long time.
    for _ in 0..8 {
        tokio::time::sleep(Duration::from_secs(10)).await;
        wire1.say("PONG :tmi.twitch.tv");
    }
    let dropped_at = Instant::now();
    drop(wire1);
    next(&mut rx).await; // Reconnecting
    script.links.lock().unwrap().push_back(second);
    wire2.say(JOIN);
    next(&mut rx).await; // Live
                         // The pause after a long, healthy connection is the first step again.
    assert_eq!(script.attempts()[1] - dropped_at, Duration::from_secs(1));
}

#[tokio::test(start_paused = true)]
async fn the_server_can_ask_for_a_reconnect() {
    let (first, wire1) = wire();
    let (second, wire2) = wire();
    let script = Script::with(vec![first, second]);
    let (mut rx, _task) = start(script);
    wire1.say(JOIN);
    next(&mut rx).await;
    next(&mut rx).await;
    wire1.say(":tmi.twitch.tv RECONNECT");
    assert_eq!(
        next(&mut rx).await,
        ChatEvent::Status(ChatStatus::Reconnecting)
    );
    wire2.say(JOIN);
    assert_eq!(next(&mut rx).await, ChatEvent::Status(ChatStatus::Live));
}

#[tokio::test(start_paused = true)]
async fn a_silent_connection_is_pinged_and_then_given_up() {
    let (first, mut wire1) = wire();
    let (second, wire2) = wire();
    let script = Script::with(vec![first, second]);
    let (mut rx, _task) = start(script);
    wire1.say(JOIN);
    next(&mut rx).await;
    next(&mut rx).await; // Live
    wire1.sent(); // the hello

    // Nothing more is heard (a Wi-Fi drop, or waking from sleep): we ping, then give up.
    let started = Instant::now();
    assert_eq!(
        next(&mut rx).await,
        ChatEvent::Status(ChatStatus::Reconnecting)
    );
    assert!(started.elapsed() >= SessionConfig::default().idle_timeout);
    assert!(
        wire1.sent().iter().any(|m| m.starts_with("PING")),
        "a ping went out before the silence was called"
    );

    wire2.say(JOIN);
    assert_eq!(next(&mut rx).await, ChatEvent::Status(ChatStatus::Live));
}

#[tokio::test(start_paused = true)]
async fn the_session_ends_when_nobody_listens() {
    let (link, wire) = wire();
    let (rx, task) = start(Script::with(vec![link]));
    drop(rx);
    wire.say(&privmsg("m1", "hello?"));
    let ended = tokio::time::timeout(Duration::from_secs(60), task).await;
    assert!(ended.is_ok(), "the task finished");
}

#[tokio::test(start_paused = true)]
async fn a_bad_channel_name_gives_a_notice_instead_of_connecting() {
    let script = Script::with(Vec::new());
    let (tx, mut rx) = mpsc::unbounded_channel();
    let mut c = ctx(
        Arc::clone(&script) as Arc<dyn Connector>,
        None,
        SessionConfig::default(),
    );
    c.channel = "not a login!".into();
    run(c, tx).await;
    let batch = lines(next(&mut rx).await);
    assert_eq!(batch[0].kind, htpc_core::ChatKind::Notice);
    assert!(script.attempts().is_empty());
}

#[tokio::test]
async fn badges_come_from_graphql_and_reach_the_first_lines() {
    use wiremock::matchers::{body_string_contains, method};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(body_string_contains("broadcastBadges"))
        .respond_with(ResponseTemplate::new(200).set_body_string(
            r#"{"data":{"user":{"broadcastBadges":[{"setID":"subscriber","version":"6","imageURL":"https://cdn/sub6.png"}]}}}"#,
        ))
        .mount(&server)
        .await;
    Mock::given(method("POST"))
        .and(body_string_contains("query { badges"))
        .respond_with(ResponseTemplate::new(200).set_body_string(
            r#"{"data":{"badges":[{"setID":"moderator","version":"1","imageURL":"https://cdn/mod.png"}]}}"#,
        ))
        .mount(&server)
        .await;

    let (link, wire) = wire();
    // The line is already waiting when the session starts: its badges must still be known.
    wire.say(
        "@badge-info=subscriber/8;badges=moderator/1,subscriber/6,vip/1;color=;display-name=Alice;emotes=;id=m1;room-id=123;tmi-sent-ts=1;user-id=2 :alice!alice@alice.tmi.twitch.tv PRIVMSG #chan :hi",
    );
    let config = SessionConfig {
        flush_every: Duration::from_millis(10),
        ..SessionConfig::default()
    };
    let loader = Arc::new(HttpBadges::with_endpoint(server.uri(), "web-id").unwrap());
    let (tx, mut rx) = mpsc::unbounded_channel();
    let task = tokio::spawn(run(ctx(Script::with(vec![link]), Some(loader), config), tx));

    let mut got = None;
    while let Some(event) = rx.recv().await {
        if let ChatEvent::Messages(batch) = event {
            got = Some(batch);
            break;
        }
    }
    task.abort();
    let batch = got.expect("a line arrived");
    // Global and channel badges, in the order sent. The vip badge has no image and is left out.
    assert_eq!(
        batch[0].badges,
        ["https://cdn/mod.png", "https://cdn/sub6.png"]
    );
}

#[tokio::test]
async fn slow_or_failing_badges_do_not_stop_the_chat() {
    use wiremock::matchers::method;
    use wiremock::{Mock, MockServer, ResponseTemplate};

    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .respond_with(ResponseTemplate::new(500))
        .mount(&server)
        .await;
    let (link, wire) = wire();
    wire.say(&privmsg("m1", "hello"));
    let loader = Arc::new(HttpBadges::with_endpoint(server.uri(), "web-id").unwrap());
    let config = SessionConfig {
        flush_every: Duration::from_millis(10),
        ..SessionConfig::default()
    };
    let (tx, mut rx) = mpsc::unbounded_channel();
    let task = tokio::spawn(run(ctx(Script::with(vec![link]), Some(loader), config), tx));
    let mut got = None;
    while let Some(event) = rx.recv().await {
        if let ChatEvent::Messages(batch) = event {
            got = Some(batch);
            break;
        }
    }
    task.abort();
    assert_eq!(got.expect("lines still arrive")[0].text(), "hello");
}
