use std::sync::{Arc, Mutex};
use std::time::Duration;

use htpc_core::{ChatSpan, ChatStatus, Moderation};
use htpc_kick_api::{fake_live, Error, FakeKick};
use tokio::sync::mpsc;

use super::*;
use crate::kick::socket::FrameFuture;
use crate::session::Backoff;

/// Hands out links whose server side the test holds, and records the addresses asked for.
#[derive(Clone, Default)]
struct Scripted {
    servers: Arc<Mutex<Vec<Server>>>,
    urls: Arc<Mutex<Vec<String>>>,
}

/// The server end of one link.
struct Server {
    to_client: mpsc::Sender<Result<String, String>>,
    from_client: mpsc::UnboundedReceiver<String>,
}

impl Scripted {
    fn server(&self, i: usize) -> Server {
        loop {
            let mut servers = self.servers.lock().unwrap();
            if servers.len() > i {
                return servers.remove(i);
            }
            drop(servers);
            std::thread::yield_now();
        }
    }
}

impl FrameConnector for Scripted {
    fn connect<'a>(&'a self, url: &'a str) -> FrameFuture<'a> {
        self.urls.lock().unwrap().push(url.to_owned());
        let (to_client, incoming) = mpsc::channel(64);
        let (outgoing, from_client) = mpsc::unbounded_channel();
        self.servers.lock().unwrap().push(Server {
            to_client,
            from_client,
        });
        Box::pin(async move { Ok(FrameLink { incoming, outgoing }) })
    }
}

fn config() -> SessionConfig {
    SessionConfig {
        backoff: Backoff {
            initial: Duration::from_secs(1),
            max: Duration::from_secs(8),
        },
        keepalive: Duration::from_secs(45),
        idle_timeout: Duration::from_secs(120),
        flush_every: Duration::from_millis(150),
        stable_after: Duration::from_secs(30),
    }
}

fn kick() -> Arc<FakeKick> {
    Arc::new(FakeKick::new().with_pages(vec![vec![fake_live("xqc", 10)]]))
}

fn start(api: Arc<FakeKick>, connector: &Scripted) -> mpsc::UnboundedReceiver<ChatEvent> {
    let (tx, rx) = mpsc::unbounded_channel();
    let ctx = Ctx {
        api,
        connector: Arc::new(connector.clone()),
        slug: "xqc".into(),
        config: config(),
    };
    tokio::spawn(run(ctx, tx));
    rx
}

/// Lets the session task run until it waits on something.
async fn settle() {
    for _ in 0..20 {
        tokio::task::yield_now().await;
    }
}

fn recorded_frames() -> Vec<String> {
    let path = format!(
        "{}/../kick/tests/fixtures/pusher_chat.jsonl",
        env!("CARGO_MANIFEST_DIR")
    );
    std::fs::read_to_string(&path)
        .unwrap_or_else(|e| panic!("{path}: {e}"))
        .lines()
        .map(str::to_owned)
        .collect()
}

#[tokio::test(start_paused = true)]
async fn subscribes_to_the_chatroom_and_goes_live() {
    let connector = Scripted::default();
    let mut rx = start(kick(), &connector);
    settle().await;
    assert_eq!(
        rx.recv().await,
        Some(ChatEvent::Status(ChatStatus::Connecting))
    );
    let mut server = connector.server(0);
    let subscribe = server.from_client.recv().await.unwrap();
    // `fake_live` gives "xqc" the chatroom id 1010.
    assert!(
        subscribe.contains(r#""channel":"chatrooms.1010.v2""#),
        "{subscribe}"
    );
    assert!(connector.urls.lock().unwrap()[0].starts_with("wss://ws-us2.pusher.com/app/"));
    server
        .to_client
        .send(Ok(r#"{"event":"pusher_internal:subscription_succeeded","data":"{}","channel":"chatrooms.1010.v2"}"#.into()))
        .await
        .unwrap();
    settle().await;
    assert_eq!(rx.recv().await, Some(ChatEvent::Status(ChatStatus::Live)));
}

#[tokio::test(start_paused = true)]
async fn recorded_lines_arrive_in_batches_with_emotes_and_badges() {
    let connector = Scripted::default();
    let mut rx = start(kick(), &connector);
    settle().await;
    let server = connector.server(0);
    let frames = recorded_frames();
    for frame in &frames {
        server.to_client.send(Ok(frame.clone())).await.unwrap();
    }
    settle().await;
    tokio::time::sleep(Duration::from_millis(200)).await;
    settle().await;

    let mut lines = Vec::new();
    while let Ok(event) = rx.try_recv() {
        if let ChatEvent::Messages(batch) = event {
            lines.extend(batch);
        }
    }
    let expected = frames
        .iter()
        .filter(|f| f.contains("ChatMessageEvent"))
        .count();
    assert_eq!(lines.len(), expected);
    let first = &lines[0];
    assert_eq!(first.user, "awa_xo");
    assert_eq!(first.login, "awa-xo");
    assert_eq!(first.text(), "you get 100k and the uzi");
    assert!(first.color.is_some());
    assert!(first.badges[0].starts_with("https://ext.cdn.kick.com/chat/badges/"));
    assert!(lines.iter().any(|l| l
        .spans
        .iter()
        .any(|s| matches!(s, ChatSpan::Emote { url, .. } if url.starts_with("https://files.kick.com/emotes/")))));
}

#[tokio::test(start_paused = true)]
async fn moderation_comes_after_the_lines_before_it() {
    let connector = Scripted::default();
    let mut rx = start(kick(), &connector);
    settle().await;
    let server = connector.server(0);
    let line = recorded_frames()
        .into_iter()
        .find(|f| f.contains("ChatMessageEvent"))
        .unwrap();
    server.to_client.send(Ok(line)).await.unwrap();
    server
        .to_client
        .send(Ok(
            r#"{"event":"App\\Events\\ChatroomClearEvent","data":"{}","channel":"x"}"#.into(),
        ))
        .await
        .unwrap();
    settle().await;
    let events: Vec<_> = std::iter::from_fn(|| rx.try_recv().ok()).collect();
    let messages = events
        .iter()
        .position(|e| matches!(e, ChatEvent::Messages(_)));
    let cleared = events
        .iter()
        .position(|e| *e == ChatEvent::Moderated(Moderation::ClearAll));
    assert!(
        messages.is_some() && cleared.is_some() && messages < cleared,
        "{events:?}"
    );
}

#[tokio::test(start_paused = true)]
async fn pings_are_answered() {
    let connector = Scripted::default();
    let _rx = start(kick(), &connector);
    settle().await;
    let mut server = connector.server(0);
    let _subscribe = server.from_client.recv().await.unwrap();
    server
        .to_client
        .send(Ok(r#"{"event":"pusher:ping","data":{}}"#.into()))
        .await
        .unwrap();
    settle().await;
    assert_eq!(
        server.from_client.recv().await.unwrap(),
        r#"{"event":"pusher:pong","data":{}}"#
    );
}

#[tokio::test(start_paused = true)]
async fn a_dropped_connection_reconnects_after_a_pause() {
    let connector = Scripted::default();
    let mut rx = start(kick(), &connector);
    settle().await;
    let server = connector.server(0);
    server
        .to_client
        .send(Err("network gone".into()))
        .await
        .unwrap();
    settle().await;
    let events: Vec<_> = std::iter::from_fn(|| rx.try_recv().ok()).collect();
    assert!(events.contains(&ChatEvent::Status(ChatStatus::Reconnecting)));
    assert_eq!(
        connector.urls.lock().unwrap().len(),
        1,
        "waits before trying again"
    );
    tokio::time::sleep(Duration::from_millis(1_100)).await;
    settle().await;
    assert_eq!(connector.urls.lock().unwrap().len(), 2);
}

#[tokio::test(start_paused = true)]
async fn a_refused_app_key_is_asked_for_again() {
    let api = kick();
    let connector = Scripted::default();
    let _rx = start(Arc::clone(&api), &connector);
    settle().await;
    let asked_before = api.calls();
    let server = connector.server(0);
    server
        .to_client
        .send(Ok(r#"{"event":"pusher:error","data":{"code":4001,"message":"App key x not in this cluster"}}"#.into()))
        .await
        .unwrap();
    settle().await;
    tokio::time::sleep(Duration::from_millis(1_100)).await;
    settle().await;
    assert_eq!(connector.urls.lock().unwrap().len(), 2);
    // The channel is not looked up again, the chat app is.
    assert_eq!(api.calls(), asked_before + 1);
}

#[tokio::test(start_paused = true)]
async fn an_unknown_channel_says_so_and_stops() {
    let connector = Scripted::default();
    let (tx, mut rx) = mpsc::unbounded_channel();
    let ctx = Ctx {
        api: Arc::new(FakeKick::new()),
        connector: Arc::new(connector.clone()),
        slug: "nobody".into(),
        config: config(),
    };
    run(ctx, tx).await;
    assert_eq!(
        rx.recv().await,
        Some(ChatEvent::Status(ChatStatus::Connecting))
    );
    assert!(
        matches!(rx.recv().await, Some(ChatEvent::Messages(m)) if m[0].text().contains("not available"))
    );
    assert!(connector.urls.lock().unwrap().is_empty());
}

#[tokio::test(start_paused = true)]
async fn a_failed_lookup_is_retried() {
    let connector = Scripted::default();
    let (tx, mut rx) = mpsc::unbounded_channel();
    let ctx = Ctx {
        api: Arc::new(FakeKick::new().failing(Error::Network("down".into()))),
        connector: Arc::new(connector.clone()),
        slug: "xqc".into(),
        config: config(),
    };
    let task = tokio::spawn(run(ctx, tx));
    settle().await;
    assert_eq!(
        rx.recv().await,
        Some(ChatEvent::Status(ChatStatus::Connecting))
    );
    assert_eq!(
        rx.recv().await,
        Some(ChatEvent::Status(ChatStatus::Reconnecting))
    );
    assert!(connector.urls.lock().unwrap().is_empty());
    task.abort();
}
