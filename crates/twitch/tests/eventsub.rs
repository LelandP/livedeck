//! The EventSub connection against scripted sockets and a wiremock Helix: welcome then subscribe,
//! notifications, a silent socket, a `session_reconnect` handover and a revocation.

use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use std::time::Duration;

use htpc_twitch_api::{
    Credentials, EventSub, EventSubConfig, EventSubEvent, RaidWatch, Refresher, Socket,
    SocketConnector, SocketFuture, TokenSource,
};
use serde_json::{json, Value};
use tokio::sync::mpsc;
use wiremock::matchers::{method, path, query_param};
use wiremock::{Mock, MockServer, Request, ResponseTemplate};

type Frames = mpsc::Sender<Result<String, String>>;

/// Hands every connection to the test: the URL asked for, and the sender that feeds the socket.
struct Scripted(mpsc::UnboundedSender<(String, Frames)>);

impl SocketConnector for Scripted {
    fn connect<'a>(&'a self, url: &'a str) -> SocketFuture<'a> {
        Box::pin(async move {
            let (tx, rx) = mpsc::channel(16);
            self.0
                .send((url.to_owned(), tx))
                .map_err(|_| "test over".to_owned())?;
            Ok(Socket::new(rx))
        })
    }
}

/// Fixed credentials. Refreshing works once and bumps the generation.
#[derive(Default)]
struct Token {
    generation: AtomicU64,
}

impl Refresher for Token {
    fn generation(&self) -> u64 {
        self.generation.load(Ordering::SeqCst)
    }

    async fn refresh(&self, seen: u64) -> bool {
        self.generation
            .compare_exchange(seen, seen + 1, Ordering::SeqCst, Ordering::SeqCst)
            .is_ok()
    }
}

impl TokenSource for Token {
    fn credentials(&self) -> Option<Credentials> {
        Some(Credentials {
            client_id: "client".into(),
            access_token: format!("token{}", self.generation()),
        })
    }
}

struct Harness {
    server: MockServer,
    sub: EventSub,
    connects: mpsc::UnboundedReceiver<(String, Frames)>,
    events: mpsc::UnboundedReceiver<EventSubEvent>,
}

const WS: &str = "wss://eventsub.test/ws";

async fn start() -> Harness {
    let server = MockServer::start().await;
    let created = AtomicU64::new(0);
    Mock::given(method("POST"))
        .and(path("/eventsub/subscriptions"))
        .respond_with(move |_: &Request| {
            let n = created.fetch_add(1, Ordering::SeqCst) + 1;
            ResponseTemplate::new(202).set_body_json(json!({
                "data": [{ "id": format!("sub-{n}"), "status": "enabled", "type": "channel.raid" }],
                "total": 1, "total_cost": 1, "max_total_cost": 10
            }))
        })
        .mount(&server)
        .await;
    Mock::given(method("DELETE"))
        .and(path("/eventsub/subscriptions"))
        .respond_with(ResponseTemplate::new(204))
        .mount(&server)
        .await;
    let config = EventSubConfig {
        ws_url: WS.into(),
        helix_url: server.uri(),
        keepalive_grace: Duration::from_millis(300),
        welcome_timeout: Duration::from_secs(2),
        backoff_initial: Duration::from_millis(10),
        backoff_max: Duration::from_millis(10),
        stable_after: Duration::from_secs(60),
    };
    let (conn_tx, connects) = mpsc::unbounded_channel();
    let (ev_tx, events) = mpsc::unbounded_channel();
    let sub = EventSub::with_config(
        config,
        Arc::new(Scripted(conn_tx)),
        Arc::new(Token::default()),
        ev_tx,
    )
    .unwrap();
    Harness {
        server,
        sub,
        connects,
        events,
    }
}

impl Harness {
    async fn next_connect(&mut self) -> (String, Frames) {
        tokio::time::timeout(Duration::from_secs(3), self.connects.recv())
            .await
            .expect("no connection made")
            .expect("connector gone")
    }

    async fn next_event(&mut self) -> EventSubEvent {
        tokio::time::timeout(Duration::from_secs(3), self.events.recv())
            .await
            .expect("no event")
            .expect("events closed")
    }

    /// The subscribe and delete requests so far, as ("POST", broadcaster, session) or
    /// ("DELETE", id, "").
    async fn helix_calls(&self) -> Vec<(String, String, String)> {
        let requests = self.server.received_requests().await.unwrap_or_default();
        requests
            .iter()
            .map(|r| {
                if r.method.as_str() == "DELETE" {
                    let id = r
                        .url
                        .query_pairs()
                        .find(|(k, _)| k == "id")
                        .map(|(_, v)| v.into_owned())
                        .unwrap_or_default();
                    ("DELETE".into(), id, String::new())
                } else {
                    let body: Value = serde_json::from_slice(&r.body).unwrap();
                    (
                        "POST".into(),
                        body["condition"]["from_broadcaster_user_id"]
                            .as_str()
                            .unwrap()
                            .into(),
                        body["transport"]["session_id"].as_str().unwrap().into(),
                    )
                }
            })
            .collect()
    }

    /// Waits until Helix has seen `n` requests.
    async fn wait_for_calls(&self, n: usize) -> Vec<(String, String, String)> {
        for _ in 0..300 {
            let calls = self.helix_calls().await;
            if calls.len() >= n {
                return calls;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
        panic!("Helix saw {:?}, wanted {n} calls", self.helix_calls().await);
    }
}

fn welcome(session: &str, keepalive: u64) -> Result<String, String> {
    Ok(json!({
        "metadata": { "message_id": format!("w-{session}"), "message_type": "session_welcome",
                      "message_timestamp": "2026-11-06T20:00:00Z" },
        "payload": { "session": { "id": session, "status": "connected",
            "keepalive_timeout_seconds": keepalive, "reconnect_url": null } }
    })
    .to_string())
}

fn keepalive(n: u32) -> Result<String, String> {
    Ok(json!({
        "metadata": { "message_id": format!("k{n}"), "message_type": "session_keepalive" },
        "payload": {}
    })
    .to_string())
}

fn reconnect(session: &str, url: &str) -> Result<String, String> {
    Ok(json!({
        "metadata": { "message_id": "r1", "message_type": "session_reconnect" },
        "payload": { "session": { "id": session, "status": "reconnecting",
            "keepalive_timeout_seconds": null, "reconnect_url": url } }
    })
    .to_string())
}

fn raid(message_id: &str, from: &str, to: &str, viewers: u64) -> Result<String, String> {
    Ok(json!({
        "metadata": { "message_id": message_id, "message_type": "notification",
                      "subscription_type": "channel.raid", "subscription_version": "1" },
        "payload": {
            "subscription": { "id": "sub-1", "status": "enabled", "type": "channel.raid",
                              "version": "1", "cost": 0 },
            "event": {
                "from_broadcaster_user_id": "100", "from_broadcaster_user_login": from,
                "from_broadcaster_user_name": from,
                "to_broadcaster_user_id": "200", "to_broadcaster_user_login": to,
                "to_broadcaster_user_name": to.to_uppercase(), "viewers": viewers
            }
        }
    })
    .to_string())
}

fn revocation(sub_id: &str) -> Result<String, String> {
    Ok(json!({
        "metadata": { "message_id": "rev1", "message_type": "revocation",
                      "subscription_type": "channel.raid" },
        "payload": { "subscription": { "id": sub_id, "status": "authorization_revoked",
            "type": "channel.raid", "version": "1" } }
    })
    .to_string())
}

fn post(broadcaster: &str, session: &str) -> (String, String, String) {
    ("POST".into(), broadcaster.into(), session.into())
}

fn delete(id: &str) -> (String, String, String) {
    ("DELETE".into(), id.into(), String::new())
}

#[tokio::test]
async fn nothing_connects_until_a_channel_is_watched() {
    let mut h = start().await;
    tokio::time::sleep(Duration::from_millis(100)).await;
    assert!(h.connects.try_recv().is_err(), "no socket while idle");
    h.sub.watch_raids("100");
    let (url, _frames) = h.next_connect().await;
    assert_eq!(url, WS);
}

#[tokio::test]
async fn the_welcome_leads_to_a_subscription_and_raids_come_through() {
    let mut h = start().await;
    h.sub.watch_raids("100");
    let (_, frames) = h.next_connect().await;
    frames.send(welcome("s1", 10)).await.unwrap();
    assert_eq!(h.wait_for_calls(1).await, [post("100", "s1")]);
    let first = &h.server.received_requests().await.unwrap()[0];
    assert_eq!(first.headers["client-id"], "client");
    assert_eq!(first.headers["authorization"], "Bearer token0");

    frames.send(keepalive(1)).await.unwrap();
    frames
        .send(raid("n1", "ender", "next_up", 1234))
        .await
        .unwrap();
    let EventSubEvent::Raid(r) = h.next_event().await;
    assert_eq!(
        (r.from_login.as_str(), r.to_login.as_str()),
        ("ender", "next_up")
    );
    assert_eq!((r.to_name.as_str(), r.viewers), ("NEXT_UP", 1234));

    // Twitch may send a notification twice: the repeat is dropped.
    frames
        .send(raid("n1", "ender", "next_up", 1234))
        .await
        .unwrap();
    frames.send(raid("n2", "ender", "other", 5)).await.unwrap();
    let EventSubEvent::Raid(r) = h.next_event().await;
    assert_eq!(r.to_login, "other");
}

#[tokio::test]
async fn changing_channels_swaps_the_subscription_and_clear_removes_it() {
    let mut h = start().await;
    h.sub.watch_raids("100");
    let (_, frames) = h.next_connect().await;
    frames.send(welcome("s1", 10)).await.unwrap();
    h.wait_for_calls(1).await;

    h.sub.watch_raids("100");
    h.sub.watch_raids("300");
    let calls = h.wait_for_calls(3).await;
    assert_eq!(
        calls,
        [post("100", "s1"), delete("sub-1"), post("300", "s1")],
        "the same channel again changes nothing"
    );
    h.sub.clear();
    let calls = h.wait_for_calls(4).await;
    assert_eq!(calls[3], delete("sub-2"));
    // Same socket throughout.
    tokio::time::sleep(Duration::from_millis(50)).await;
    assert!(h.connects.try_recv().is_err());
    drop(frames);
}

#[tokio::test]
async fn a_silent_socket_is_replaced_and_the_subscription_made_again() {
    let mut h = start().await;
    h.sub.watch_raids("100");
    let (_, frames) = h.next_connect().await;
    // A keepalive of 0 s: silence for more than the 300 ms grace means the socket is dead.
    frames.send(welcome("s1", 0)).await.unwrap();
    h.wait_for_calls(1).await;
    let (url, again) = h.next_connect().await;
    assert_eq!(url, WS, "a fresh connection, not a handover");
    again.send(welcome("s2", 10)).await.unwrap();
    let calls = h.wait_for_calls(2).await;
    assert_eq!(calls, [post("100", "s1"), post("100", "s2")]);
    drop(frames);
}

#[tokio::test]
async fn a_closed_socket_reconnects() {
    let mut h = start().await;
    h.sub.watch_raids("100");
    let (_, frames) = h.next_connect().await;
    frames.send(welcome("s1", 10)).await.unwrap();
    h.wait_for_calls(1).await;
    frames.send(Err("closed by Twitch".into())).await.unwrap();
    let (_, again) = h.next_connect().await;
    again.send(welcome("s2", 10)).await.unwrap();
    assert_eq!(h.wait_for_calls(2).await[1], post("100", "s2"));
}

#[tokio::test]
async fn session_reconnect_hands_over_without_subscribing_again() {
    let mut h = start().await;
    h.sub.watch_raids("100");
    let (_, old) = h.next_connect().await;
    old.send(welcome("s1", 10)).await.unwrap();
    h.wait_for_calls(1).await;

    old.send(reconnect("s1", "wss://other.test/ws"))
        .await
        .unwrap();
    let (url, new) = h.next_connect().await;
    assert_eq!(url, "wss://other.test/ws");
    // Until the new socket says welcome, the old one still delivers.
    old.send(raid("n1", "ender", "via_old", 1)).await.unwrap();
    tokio::time::sleep(Duration::from_millis(50)).await;
    new.send(welcome("s1", 10)).await.unwrap();
    let EventSubEvent::Raid(r) = h.next_event().await;
    assert_eq!(r.to_login, "via_old");

    new.send(raid("n2", "ender", "via_new", 2)).await.unwrap();
    let EventSubEvent::Raid(r) = h.next_event().await;
    assert_eq!(r.to_login, "via_new");
    tokio::time::sleep(Duration::from_millis(50)).await;
    assert_eq!(h.helix_calls().await.len(), 1, "subscriptions move over");
}

#[tokio::test]
async fn a_revoked_subscription_is_made_again_when_asked() {
    let mut h = start().await;
    h.sub.watch_raids("100");
    let (_, frames) = h.next_connect().await;
    frames.send(welcome("s1", 10)).await.unwrap();
    h.wait_for_calls(1).await;
    frames.send(revocation("sub-1")).await.unwrap();
    tokio::time::sleep(Duration::from_millis(50)).await;
    // Asking for the same channel again subscribes again: the old one is gone, nothing to delete.
    h.sub.watch_raids("100");
    let calls = h.wait_for_calls(2).await;
    assert_eq!(calls, [post("100", "s1"), post("100", "s1")]);
}

#[tokio::test]
async fn a_401_refreshes_the_token_and_tries_again() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/eventsub/subscriptions"))
        .and(wiremock::matchers::header("authorization", "Bearer token0"))
        .respond_with(
            ResponseTemplate::new(401).set_body_json(
                json!({ "error": "Unauthorized", "message": "Invalid OAuth token" }),
            ),
        )
        .mount(&server)
        .await;
    Mock::given(method("POST"))
        .and(path("/eventsub/subscriptions"))
        .and(wiremock::matchers::header("authorization", "Bearer token1"))
        .respond_with(ResponseTemplate::new(202).set_body_json(json!({ "data": [{ "id": "ok" }] })))
        .mount(&server)
        .await;
    Mock::given(method("DELETE"))
        .and(path("/eventsub/subscriptions"))
        .and(query_param("id", "ok"))
        .respond_with(ResponseTemplate::new(204))
        .expect(1)
        .mount(&server)
        .await;
    let config = EventSubConfig {
        ws_url: WS.into(),
        helix_url: server.uri(),
        ..EventSubConfig::default()
    };
    let (conn_tx, mut connects) = mpsc::unbounded_channel();
    let (ev_tx, _events) = mpsc::unbounded_channel();
    let sub = EventSub::with_config(
        config,
        Arc::new(Scripted(conn_tx)),
        Arc::new(Token::default()),
        ev_tx,
    )
    .unwrap();
    sub.watch_raids("100");
    let (_, frames) = connects.recv().await.unwrap();
    frames.send(welcome("s1", 10)).await.unwrap();
    // The subscription only exists if the retry worked: clearing deletes it by its id.
    tokio::time::sleep(Duration::from_millis(200)).await;
    sub.clear();
    tokio::time::sleep(Duration::from_millis(200)).await;
    server.verify().await;
}
