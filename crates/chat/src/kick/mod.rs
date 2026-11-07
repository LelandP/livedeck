//! Reading Kick chat: anonymous, over Kick's Pusher app on a WebSocket.
//!
//! `KickChat` looks the channel up (its chatroom id), asks Kick which Pusher app serves its chat,
//! subscribes to `chatrooms.<id>.v2`, and reports like `IrcChat`: status, batched lines and
//! moderation. It reconnects with the same growing pause.

mod map;
mod session;
mod socket;

use std::sync::Arc;

use htpc_kick_api::KickApi;
use tokio::sync::mpsc::UnboundedSender;

use crate::client::{ChatClient, ChatEvent, ChatSession};
use crate::session::SessionConfig;

pub use map::{emote_url, read_frame, spans, Frame};
pub use socket::{FrameConnector, FrameFuture, FrameLink, WsFrames};

/// Kick chat for any channel slug.
pub struct KickChat<K> {
    api: Arc<K>,
    connector: Arc<dyn FrameConnector>,
    config: SessionConfig,
}

impl<K: KickApi> KickChat<K> {
    pub fn new(api: Arc<K>) -> Self {
        Self {
            api,
            connector: Arc::new(WsFrames),
            config: SessionConfig::default(),
        }
    }

    /// Replaces how sockets are opened (tests).
    pub fn with_connector(mut self, connector: Arc<dyn FrameConnector>) -> Self {
        self.connector = connector;
        self
    }

    pub fn with_config(mut self, config: SessionConfig) -> Self {
        self.config = config;
        self
    }
}

impl<K: KickApi> ChatClient for KickChat<K> {
    fn join(&self, channel: &str, events: UnboundedSender<ChatEvent>) -> ChatSession {
        let ctx = session::Ctx {
            api: Arc::clone(&self.api),
            connector: Arc::clone(&self.connector),
            slug: channel.to_ascii_lowercase(),
            config: self.config,
        };
        ChatSession::new(tokio::spawn(session::run(ctx, events)))
    }
}
