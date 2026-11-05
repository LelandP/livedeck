//! The chat client interface the app talks to.

use std::sync::Arc;

use htpc_core::{ChatMessage, ChatStatus, Moderation};
use tokio::sync::mpsc::UnboundedSender;
use tokio::task::JoinHandle;

use crate::badges::{BadgeBook, HttpBadges};
use crate::emotes::{EmoteProvider, TwitchCdn};
use crate::link::{Connector, WsConnector};
use crate::map::Mapper;
use crate::session::{self, SessionConfig};

/// What a chat connection reports.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ChatEvent {
    Status(ChatStatus),
    /// New lines, oldest first. Several arrive together when chat is busy.
    Messages(Vec<ChatMessage>),
    Moderated(Moderation),
}

/// Joined channel. The connection lives as long as this does: dropping it leaves the channel.
#[derive(Debug)]
pub struct ChatSession {
    task: Option<JoinHandle<()>>,
}

impl ChatSession {
    pub fn new(task: JoinHandle<()>) -> Self {
        Self { task: Some(task) }
    }
}

impl Drop for ChatSession {
    fn drop(&mut self) {
        if let Some(task) = self.task.take() {
            task.abort();
        }
    }
}

/// Anything that can deliver the chat of a channel.
pub trait ChatClient: Send + Sync + 'static {
    /// Starts receiving the chat of `channel` and reports into `events` until the returned session
    /// is dropped. The client reconnects on its own. Must be called inside a tokio runtime.
    fn join(&self, channel: &str, events: UnboundedSender<ChatEvent>) -> ChatSession;
}

/// Anonymous, read-only chat over IRC on a WebSocket.
pub struct IrcChat {
    connector: Arc<dyn Connector>,
    emotes: Arc<dyn EmoteProvider>,
    badge_source: Option<Arc<HttpBadges>>,
    config: SessionConfig,
}

impl IrcChat {
    /// Twitch's servers, Twitch's emote CDN and Twitch's badge listing.
    pub fn new() -> Self {
        let badge_source = match HttpBadges::new() {
            Ok(b) => Some(Arc::new(b)),
            Err(err) => {
                tracing::warn!(%err, "badges are off");
                None
            }
        };
        Self {
            connector: Arc::new(WsConnector),
            emotes: Arc::new(TwitchCdn),
            badge_source,
            config: SessionConfig::default(),
        }
    }

    /// Replaces how connections are made (tests, other servers).
    pub fn with_connector(mut self, connector: Arc<dyn Connector>) -> Self {
        self.connector = connector;
        self
    }

    /// Replaces where emote images come from, e.g. to add third-party sets.
    pub fn with_emotes(mut self, emotes: Arc<dyn EmoteProvider>) -> Self {
        self.emotes = emotes;
        self
    }

    /// Replaces where badge listings are downloaded from. `None` shows no badges.
    pub fn with_badge_source(mut self, source: Option<HttpBadges>) -> Self {
        self.badge_source = source.map(Arc::new);
        self
    }

    pub fn with_config(mut self, config: SessionConfig) -> Self {
        self.config = config;
        self
    }
}

impl Default for IrcChat {
    fn default() -> Self {
        Self::new()
    }
}

impl ChatClient for IrcChat {
    fn join(&self, channel: &str, events: UnboundedSender<ChatEvent>) -> ChatSession {
        let badges = Arc::new(BadgeBook::default());
        let mapper = Mapper::new(channel, Arc::clone(&self.emotes), Arc::clone(&badges));
        let ctx = session::Ctx {
            connector: Arc::clone(&self.connector),
            channel: channel.to_ascii_lowercase(),
            mapper,
            badges,
            loader: self.badge_source.clone(),
            config: self.config,
        };
        ChatSession::new(tokio::spawn(session::run(ctx, events)))
    }
}
