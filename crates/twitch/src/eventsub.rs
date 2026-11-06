//! EventSub over a WebSocket: Twitch pushes events to us instead of us polling for them.
//!
//! One [`EventSub`] is one connection to `wss://eventsub.wss.twitch.tv/ws`, run by a tokio task.
//! The protocol:
//! - `session_welcome` gives a session id. Subscriptions are made over Helix
//!   (`POST /eventsub/subscriptions` with that id) and must come within 10 s, or Twitch closes the
//!   socket.
//! - `session_keepalive` comes when nothing else did for the announced keepalive time. Silence for
//!   longer than that (plus a grace) means the socket is dead: reconnect and subscribe again.
//! - `session_reconnect` gives a new URL. We connect there and wait for its welcome before dropping
//!   the old socket. The subscriptions move over on their own.
//! - `notification` carries an event, `revocation` says a subscription is gone.
//!
//! The app holds at most one subscription: `channel.raid` from the channel on screen. It costs 1
//! of the 10 a WebSocket may have, and needs no scope, only a user token. The task connects only
//! while something is wanted, so an unused socket is never kept open for Twitch to close.

use std::collections::VecDeque;
use std::future::Future;
use std::pin::Pin;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use futures_util::StreamExt;
use reqwest::{Client, RequestBuilder, StatusCode};
use serde::Deserialize;
use serde_json::{json, Value};
use tokio::sync::mpsc;
use tokio::task::JoinHandle;
use tokio::time::{sleep_until, timeout, Instant};

use crate::helix::DEFAULT_BASE_URL;
use crate::http::{build_client, error_message, map_status};
use crate::{Credentials, Error, Refresher, Result};

/// Twitch's EventSub WebSocket.
pub const DEFAULT_EVENTSUB_URL: &str = "wss://eventsub.wss.twitch.tv/ws";

/// The subscription type for a channel raiding another one.
pub const RAID: &str = "channel.raid";

/// Where the EventSub client gets its credentials, and how it renews them when Helix answers 401.
/// The app's session implements it.
pub trait TokenSource: Refresher {
    /// The current client ID and user token, or `None` while signed out.
    fn credentials(&self) -> Option<Credentials>;
}

/// What the EventSub connection reports.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum EventSubEvent {
    /// The channel we watch for raids raided another one.
    Raid(Raid),
}

/// A `channel.raid` notification.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub struct Raid {
    #[serde(rename = "from_broadcaster_user_id")]
    pub from_id: String,
    #[serde(rename = "from_broadcaster_user_login")]
    pub from_login: String,
    #[serde(rename = "from_broadcaster_user_name", default)]
    pub from_name: String,
    #[serde(rename = "to_broadcaster_user_id")]
    pub to_id: String,
    #[serde(rename = "to_broadcaster_user_login")]
    pub to_login: String,
    #[serde(rename = "to_broadcaster_user_name", default)]
    pub to_name: String,
    #[serde(default)]
    pub viewers: u64,
}

/// What the app asks of the raid watch. [`EventSub`] is the real one, [`crate::FakeEventSub`]
/// records the calls for tests.
pub trait RaidWatch: Send + Sync + 'static {
    /// Watches `broadcaster_id` for raids, replacing the channel watched so far.
    fn watch_raids(&self, broadcaster_id: &str);
    /// Watches nothing. The connection may stay open for the next channel.
    fn clear(&self);
}

/// Timing and addresses of an EventSub connection. Tests point them at fakes and shorten them.
#[derive(Debug, Clone)]
pub struct EventSubConfig {
    pub ws_url: String,
    /// Helix, for the subscribe and delete requests.
    pub helix_url: String,
    /// Added to the keepalive time Twitch announces before silence counts as a dead socket.
    pub keepalive_grace: Duration,
    /// How long a new socket may take to say welcome.
    pub welcome_timeout: Duration,
    /// Pause before reconnecting: `backoff_initial`, doubling up to `backoff_max`.
    pub backoff_initial: Duration,
    pub backoff_max: Duration,
    /// A connection this old when it drops starts the backoff over.
    pub stable_after: Duration,
}

impl Default for EventSubConfig {
    fn default() -> Self {
        Self {
            ws_url: DEFAULT_EVENTSUB_URL.to_owned(),
            helix_url: DEFAULT_BASE_URL.to_owned(),
            keepalive_grace: Duration::from_secs(5),
            welcome_timeout: Duration::from_secs(10),
            backoff_initial: Duration::from_secs(1),
            backoff_max: Duration::from_secs(60),
            stable_after: Duration::from_secs(60),
        }
    }
}

impl EventSubConfig {
    fn backoff(&self, attempt: u32) -> Duration {
        let doublings = attempt.saturating_sub(1).min(16);
        self.backoff_initial
            .saturating_mul(1 << doublings)
            .min(self.backoff_max)
    }
}

/// An open socket, as the text frames it receives. `Err` is fatal: the socket is gone. Dropping
/// it closes the socket.
pub struct Socket {
    pub frames: mpsc::Receiver<std::result::Result<String, String>>,
    reader: Option<JoinHandle<()>>,
}

impl Socket {
    /// A socket fed by whoever holds the sender (tests).
    pub fn new(frames: mpsc::Receiver<std::result::Result<String, String>>) -> Self {
        Self {
            frames,
            reader: None,
        }
    }
}

impl Drop for Socket {
    fn drop(&mut self) {
        if let Some(reader) = self.reader.take() {
            reader.abort();
        }
    }
}

pub type SocketFuture<'a> =
    Pin<Box<dyn Future<Output = std::result::Result<Socket, String>> + Send + 'a>>;

/// Opens sockets. The real one is [`WsConnector`]; tests script their own.
pub trait SocketConnector: Send + Sync + 'static {
    fn connect<'a>(&'a self, url: &'a str) -> SocketFuture<'a>;
}

/// Frames queued from the socket before the connection task reads them.
const FRAME_BUFFER: usize = 64;

/// A real WebSocket (TLS with the webpki roots, as chat uses).
#[derive(Debug, Clone, Copy, Default)]
pub struct WsConnector;

impl SocketConnector for WsConnector {
    fn connect<'a>(&'a self, url: &'a str) -> SocketFuture<'a> {
        use async_tungstenite::tungstenite::Message;
        Box::pin(async move {
            let (mut ws, _) = async_tungstenite::tokio::connect_async(url)
                .await
                .map_err(|e| format!("could not connect to EventSub: {e}"))?;
            let (tx, rx) = mpsc::channel(FRAME_BUFFER);
            // Reading also answers Twitch's pings (tungstenite queues the pong and flushes it on
            // the next read).
            let reader = tokio::spawn(async move {
                while let Some(item) = ws.next().await {
                    let item = match item {
                        Ok(Message::Text(text)) => Ok(text.as_str().to_owned()),
                        Ok(Message::Close(frame)) => Err(format!("closed by Twitch: {frame:?}")),
                        Ok(_) => continue,
                        Err(e) => Err(e.to_string()),
                    };
                    let fatal = item.is_err();
                    if tx.send(item).await.is_err() || fatal {
                        break;
                    }
                }
            });
            Ok(Socket {
                frames: rx,
                reader: Some(reader),
            })
        })
    }
}

enum Ctl {
    Watch(String),
    Clear,
}

/// The EventSub connection. Dropping it ends the task and closes the socket (the subscription dies
/// with the session on Twitch's side).
pub struct EventSub {
    ctl: mpsc::UnboundedSender<Ctl>,
    task: JoinHandle<()>,
}

impl EventSub {
    /// Twitch's servers. Nothing connects until [`RaidWatch::watch_raids`]. Must be called inside
    /// a tokio runtime.
    pub fn start<T: TokenSource>(
        auth: Arc<T>,
        events: mpsc::UnboundedSender<EventSubEvent>,
    ) -> Result<Self> {
        Self::with_config(
            EventSubConfig::default(),
            Arc::new(WsConnector),
            auth,
            events,
        )
    }

    pub fn with_config<T: TokenSource>(
        config: EventSubConfig,
        connector: Arc<dyn SocketConnector>,
        auth: Arc<T>,
        events: mpsc::UnboundedSender<EventSubEvent>,
    ) -> Result<Self> {
        let config = EventSubConfig {
            helix_url: config.helix_url.trim_end_matches('/').to_owned(),
            ..config
        };
        let http = build_client(&config.helix_url)?;
        let (ctl_tx, ctl_rx) = mpsc::unbounded_channel();
        let runner = Runner {
            cfg: config,
            connector,
            auth,
            http,
            events,
            ctl: ctl_rx,
            want: None,
            sub: None,
            seen: VecDeque::new(),
        };
        Ok(Self {
            ctl: ctl_tx,
            task: tokio::spawn(runner.run()),
        })
    }
}

impl RaidWatch for EventSub {
    fn watch_raids(&self, broadcaster_id: &str) {
        let _ = self.ctl.send(Ctl::Watch(broadcaster_id.to_owned()));
    }

    fn clear(&self) {
        let _ = self.ctl.send(Ctl::Clear);
    }
}

impl Drop for EventSub {
    fn drop(&mut self) {
        self.task.abort();
    }
}

/// Raid watch calls a [`FakeEventSub`] received.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum FakeEventSubCall {
    Watch(String),
    Clear,
}

/// Records what it was asked. Events are sent by the test itself.
#[derive(Debug, Clone, Default)]
pub struct FakeEventSub {
    calls: Arc<Mutex<Vec<FakeEventSubCall>>>,
}

impl FakeEventSub {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn calls(&self) -> Vec<FakeEventSubCall> {
        self.calls.lock().unwrap_or_else(|e| e.into_inner()).clone()
    }

    fn push(&self, call: FakeEventSubCall) {
        self.calls
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .push(call);
    }
}

impl RaidWatch for FakeEventSub {
    fn watch_raids(&self, broadcaster_id: &str) {
        self.push(FakeEventSubCall::Watch(broadcaster_id.to_owned()));
    }

    fn clear(&self) {
        self.push(FakeEventSubCall::Clear);
    }
}

// ---- the wire format ---------------------------------------------------------------------------

#[derive(Deserialize)]
struct Envelope {
    metadata: Metadata,
    #[serde(default)]
    payload: Value,
}

#[derive(Deserialize)]
struct Metadata {
    #[serde(default)]
    message_id: String,
    message_type: String,
}

#[derive(Deserialize)]
struct SessionPayload {
    session: SessionInfo,
}

#[derive(Deserialize)]
struct SessionInfo {
    id: String,
    #[serde(default)]
    keepalive_timeout_seconds: Option<u64>,
    #[serde(default)]
    reconnect_url: Option<String>,
}

#[derive(Deserialize)]
struct SubscriptionPayload {
    subscription: SubscriptionInfo,
    #[serde(default)]
    event: Value,
}

#[derive(Deserialize)]
struct SubscriptionInfo {
    #[serde(default)]
    id: String,
    #[serde(rename = "type")]
    kind: String,
    #[serde(default)]
    status: String,
}

#[derive(Deserialize)]
struct Created {
    data: Vec<CreatedSub>,
}

#[derive(Deserialize)]
struct CreatedSub {
    id: String,
}

/// One message from the socket, as far as we care.
enum Incoming {
    Welcome(SessionInfo),
    Keepalive,
    Reconnect(String),
    Notification(SubscriptionPayload),
    Revocation(SubscriptionInfo),
    /// A message type we do not know (Twitch may add some).
    Other,
}

fn parse(text: &str) -> std::result::Result<(String, Incoming), String> {
    let env: Envelope = serde_json::from_str(text).map_err(|e| e.to_string())?;
    let payload = env.payload;
    let session = |p: Value| {
        serde_json::from_value::<SessionPayload>(p)
            .map(|s| s.session)
            .map_err(|e| e.to_string())
    };
    let incoming = match env.metadata.message_type.as_str() {
        "session_welcome" => Incoming::Welcome(session(payload)?),
        "session_keepalive" => Incoming::Keepalive,
        "session_reconnect" => {
            let url = session(payload)?
                .reconnect_url
                .ok_or("a reconnect without a URL")?;
            Incoming::Reconnect(url)
        }
        "notification" => {
            Incoming::Notification(serde_json::from_value(payload).map_err(|e| e.to_string())?)
        }
        "revocation" => Incoming::Revocation(
            serde_json::from_value::<SubscriptionPayload>(payload)
                .map_err(|e| e.to_string())?
                .subscription,
        ),
        _ => Incoming::Other,
    };
    Ok((env.metadata.message_id, incoming))
}

// ---- the connection task -----------------------------------------------------------------------

/// Message ids remembered to drop repeats (Twitch may send a notification twice).
const SEEN_IDS: usize = 32;

/// The subscription we hold.
struct Sub {
    id: String,
    broadcaster: String,
}

/// A socket that said welcome.
struct Conn {
    socket: Socket,
    session_id: String,
    keepalive: Duration,
    opened: Instant,
}

enum Ended {
    /// The socket is gone (closed, silent, failed reconnect).
    Dropped { stable: bool },
    /// Nobody listens or controls any more.
    Gone,
}

struct Runner<T> {
    cfg: EventSubConfig,
    connector: Arc<dyn SocketConnector>,
    auth: Arc<T>,
    http: Client,
    events: mpsc::UnboundedSender<EventSubEvent>,
    ctl: mpsc::UnboundedReceiver<Ctl>,
    /// The channel to watch for raids.
    want: Option<String>,
    /// The subscription in place on the current session.
    sub: Option<Sub>,
    seen: VecDeque<String>,
}

impl<T: TokenSource> Runner<T> {
    async fn run(mut self) {
        let mut attempt: u32 = 0;
        loop {
            // Idle, with no socket, until a channel is wanted.
            while self.want.is_none() {
                match self.ctl.recv().await {
                    Some(ctl) => self.note(ctl),
                    None => return,
                }
            }
            let url = self.cfg.ws_url.clone();
            match self.open(&url).await {
                Ok(conn) => match self.drive(conn).await {
                    Ended::Gone => return,
                    Ended::Dropped { stable } => {
                        if stable {
                            attempt = 0;
                        }
                        tracing::info!("EventSub connection lost");
                    }
                },
                Err(err) => tracing::warn!(%err, "EventSub connection failed"),
            }
            // A subscription lives and dies with its session.
            self.sub = None;
            attempt += 1;
            let until = Instant::now() + self.cfg.backoff(attempt);
            tracing::debug!(attempt, "reconnecting EventSub");
            loop {
                tokio::select! {
                    () = sleep_until(until) => break,
                    ctl = self.ctl.recv() => match ctl {
                        Some(ctl) => self.note(ctl),
                        None => return,
                    },
                }
            }
        }
    }

    /// Records what is wanted. Acting on it waits for a session.
    fn note(&mut self, ctl: Ctl) {
        self.want = match ctl {
            Ctl::Watch(id) => Some(id),
            Ctl::Clear => None,
        };
    }

    /// Connects to `url` and waits for the welcome.
    async fn open(&self, url: &str) -> std::result::Result<Conn, String> {
        let mut socket = self.connector.connect(url).await?;
        let welcome = async {
            loop {
                let text = match socket.frames.recv().await {
                    Some(Ok(text)) => text,
                    Some(Err(err)) => return Err(err),
                    None => return Err("closed before the welcome".to_owned()),
                };
                match parse(&text) {
                    Ok((_, Incoming::Welcome(info))) => return Ok(info),
                    Ok(_) => {}
                    Err(err) => tracing::debug!(%err, "unreadable EventSub message"),
                }
            }
        };
        let info = timeout(self.cfg.welcome_timeout, welcome)
            .await
            .map_err(|_| "no welcome from EventSub".to_owned())??;
        tracing::debug!(session = %info.id, "EventSub session open");
        Ok(Conn {
            socket,
            session_id: info.id,
            keepalive: Duration::from_secs(info.keepalive_timeout_seconds.unwrap_or(10)),
            opened: Instant::now(),
        })
    }

    /// Talks over one session until it ends.
    async fn drive(&mut self, mut conn: Conn) -> Ended {
        self.sync(&conn.session_id).await;
        let mut last_heard = Instant::now();
        loop {
            let silent_at = last_heard + conn.keepalive + self.cfg.keepalive_grace;
            tokio::select! {
                frame = conn.socket.frames.recv() => {
                    let text = match frame {
                        Some(Ok(text)) => text,
                        Some(Err(err)) => {
                            tracing::debug!(%err, "EventSub socket error");
                            return self.dropped(&conn);
                        }
                        None => return self.dropped(&conn),
                    };
                    last_heard = Instant::now();
                    match parse(&text) {
                        Ok((_, Incoming::Reconnect(url))) => {
                            // The old socket keeps working until the new one says welcome.
                            match self.open(&url).await {
                                Ok(new) => {
                                    let mut old = std::mem::replace(&mut conn, new);
                                    // Whatever the old one still had is still news.
                                    while let Ok(Ok(text)) = old.socket.frames.try_recv() {
                                        if !self.handle(&text) {
                                            return Ended::Gone;
                                        }
                                    }
                                    tracing::debug!("EventSub moved to a new socket");
                                    last_heard = Instant::now();
                                }
                                Err(err) => {
                                    tracing::warn!(%err, "EventSub reconnect failed");
                                    return self.dropped(&conn);
                                }
                            }
                        }
                        Ok(_) => {
                            if !self.handle(&text) {
                                return Ended::Gone;
                            }
                        }
                        Err(err) => tracing::debug!(%err, "unreadable EventSub message"),
                    }
                }
                ctl = self.ctl.recv() => match ctl {
                    Some(ctl) => {
                        self.note(ctl);
                        self.sync(&conn.session_id).await;
                    }
                    None => return Ended::Gone,
                },
                () = sleep_until(silent_at) => {
                    tracing::warn!("EventSub went silent, reconnecting");
                    return self.dropped(&conn);
                }
            }
        }
    }

    fn dropped(&self, conn: &Conn) -> Ended {
        Ended::Dropped {
            stable: conn.opened.elapsed() >= self.cfg.stable_after,
        }
    }

    /// Acts on a notification or revocation. False once nobody listens.
    fn handle(&mut self, text: &str) -> bool {
        let Ok((id, incoming)) = parse(text) else {
            return true;
        };
        match incoming {
            Incoming::Notification(n) => {
                if !id.is_empty() {
                    if self.seen.contains(&id) {
                        tracing::debug!(%id, "EventSub repeated a notification");
                        return true;
                    }
                    self.seen.push_back(id);
                    if self.seen.len() > SEEN_IDS {
                        self.seen.pop_front();
                    }
                }
                if n.subscription.kind != RAID {
                    return true;
                }
                match serde_json::from_value::<Raid>(n.event) {
                    Ok(raid) => {
                        tracing::info!(from = %raid.from_login, to = %raid.to_login, "raid");
                        return self.events.send(EventSubEvent::Raid(raid)).is_ok();
                    }
                    Err(err) => tracing::warn!(%err, "unreadable raid notification"),
                }
            }
            Incoming::Revocation(sub) => {
                tracing::warn!(kind = %sub.kind, status = %sub.status, "EventSub subscription revoked");
                if self.sub.as_ref().is_some_and(|s| s.id == sub.id) {
                    self.sub = None;
                }
            }
            Incoming::Welcome(_)
            | Incoming::Keepalive
            | Incoming::Reconnect(_)
            | Incoming::Other => {}
        }
        true
    }

    /// Makes the subscriptions on `session_id` match what is wanted.
    async fn sync(&mut self, session_id: &str) {
        if let (Some(want), Some(sub)) = (&self.want, &self.sub) {
            if *want == sub.broadcaster {
                return;
            }
        }
        if let Some(old) = self.sub.take() {
            if let Err(err) = self.delete(&old.id).await {
                // It ends with the session anyway.
                tracing::debug!(%err, "could not delete the old EventSub subscription");
            }
        }
        let Some(want) = self.want.clone() else {
            return;
        };
        match self.subscribe(session_id, &want).await {
            Ok(id) => {
                tracing::debug!(broadcaster = %want, "watching for raids");
                self.sub = Some(Sub {
                    id,
                    broadcaster: want,
                });
            }
            Err(err) => tracing::warn!(%err, broadcaster = %want, "could not watch for raids"),
        }
    }

    async fn subscribe(&self, session_id: &str, broadcaster: &str) -> Result<String> {
        let url = format!("{}/eventsub/subscriptions", self.cfg.helix_url);
        let body = json!({
            "type": RAID,
            "version": "1",
            "condition": { "from_broadcaster_user_id": broadcaster },
            "transport": { "method": "websocket", "session_id": session_id },
        });
        let resp = self.send(|| self.http.post(&url).json(&body)).await?;
        let bytes = resp.bytes().await.map_err(Error::Network)?;
        let created: Created =
            serde_json::from_slice(&bytes).map_err(|e| Error::Decode(e.to_string()))?;
        created
            .data
            .into_iter()
            .next()
            .map(|s| s.id)
            .ok_or_else(|| Error::Decode("no subscription in the answer".into()))
    }

    async fn delete(&self, id: &str) -> Result<()> {
        let url = format!("{}/eventsub/subscriptions", self.cfg.helix_url);
        self.send(|| self.http.delete(&url).query(&[("id", id)]))
            .await
            .map(drop)
    }

    /// Sends a Helix request with the current credentials, and once more after a refresh if it
    /// was refused with 401.
    async fn send(&self, request: impl Fn() -> RequestBuilder) -> Result<reqwest::Response> {
        let seen = self.auth.generation();
        match self.send_once(&request).await {
            Err(Error::Unauthorized(why)) => {
                if self.auth.refresh(seen).await {
                    self.send_once(&request).await
                } else {
                    Err(Error::Unauthorized(why))
                }
            }
            other => other,
        }
    }

    async fn send_once(&self, request: &impl Fn() -> RequestBuilder) -> Result<reqwest::Response> {
        let creds = self.auth.credentials().ok_or(Error::NotSignedIn)?;
        let resp = request()
            .header("Client-Id", &creds.client_id)
            .bearer_auth(&creds.access_token)
            .send()
            .await
            .map_err(Error::Network)?;
        let status = resp.status();
        if status.is_success() || status == StatusCode::NO_CONTENT {
            return Ok(resp);
        }
        let message = error_message(resp).await;
        Err(map_status(status, message))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_raid_notification_parses() {
        let text = r#"{
            "metadata": { "message_id": "m1", "message_type": "notification",
                          "subscription_type": "channel.raid" },
            "payload": {
                "subscription": { "id": "s1", "type": "channel.raid", "status": "enabled" },
                "event": {
                    "from_broadcaster_user_id": "1", "from_broadcaster_user_login": "ender",
                    "from_broadcaster_user_name": "Ender",
                    "to_broadcaster_user_id": "2", "to_broadcaster_user_login": "next_up",
                    "to_broadcaster_user_name": "Next_Up", "viewers": 1234
                }
            }
        }"#;
        let Ok((id, Incoming::Notification(n))) = parse(text) else {
            panic!("not a notification");
        };
        assert_eq!(id, "m1");
        let raid: Raid = serde_json::from_value(n.event).unwrap();
        assert_eq!(raid.from_login, "ender");
        assert_eq!(raid.to_login, "next_up");
        assert_eq!(raid.to_name, "Next_Up");
        assert_eq!(raid.viewers, 1234);
    }

    #[test]
    fn session_messages_parse() {
        let welcome = r#"{"metadata":{"message_id":"a","message_type":"session_welcome"},
            "payload":{"session":{"id":"sess","status":"connected","keepalive_timeout_seconds":10,
            "reconnect_url":null}}}"#;
        assert!(
            matches!(parse(welcome), Ok((_, Incoming::Welcome(s))) if s.id == "sess"
            && s.keepalive_timeout_seconds == Some(10))
        );
        let reconnect = r#"{"metadata":{"message_id":"b","message_type":"session_reconnect"},
            "payload":{"session":{"id":"sess","status":"reconnecting",
            "reconnect_url":"wss://other/ws"}}}"#;
        assert!(
            matches!(parse(reconnect), Ok((_, Incoming::Reconnect(u))) if u == "wss://other/ws")
        );
        let keepalive =
            r#"{"metadata":{"message_id":"c","message_type":"session_keepalive"},"payload":{}}"#;
        assert!(matches!(parse(keepalive), Ok((_, Incoming::Keepalive))));
        let unknown = r#"{"metadata":{"message_id":"d","message_type":"something_new"}}"#;
        assert!(matches!(parse(unknown), Ok((_, Incoming::Other))));
        assert!(parse("not json").is_err());
    }

    #[test]
    fn the_backoff_doubles_up_to_its_cap() {
        let cfg = EventSubConfig {
            backoff_initial: Duration::from_secs(1),
            backoff_max: Duration::from_secs(5),
            ..EventSubConfig::default()
        };
        let waits: Vec<u64> = (1..=5).map(|a| cfg.backoff(a).as_secs()).collect();
        assert_eq!(waits, [1, 2, 4, 5, 5]);
    }

    #[test]
    fn the_fake_records_its_calls() {
        let fake = FakeEventSub::new();
        fake.watch_raids("1");
        fake.clear();
        assert_eq!(
            fake.calls(),
            [FakeEventSubCall::Watch("1".into()), FakeEventSubCall::Clear]
        );
    }
}
