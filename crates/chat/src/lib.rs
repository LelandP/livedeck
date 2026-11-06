//! Reading Twitch chat. (Writing goes through Helix, in `htpc-twitch-api`.)
//!
//! `IrcChat` joins a channel anonymously over IRC on a WebSocket, maps what the server sends to
//! the core's chat types, and reports status, batched lines and moderation through [`ChatEvent`]s.
//! It reconnects with a growing pause and says so. Everything is behind [`ChatClient`], and the
//! socket behind [`Connector`], so the rest of the app and the tests use fakes.

mod badges;
mod client;
mod emotes;
mod fake;
mod link;
mod map;
mod session;

pub use badges::{parse_channel, parse_global, BadgeBook, BadgeSet, HttpBadges};
pub use client::{ChatClient, ChatEvent, ChatSession, IrcChat};
pub use emotes::{EmoteProvider, TwitchCdn};
pub use fake::FakeChat;
pub use link::{ConnectFuture, Connector, Link, WsConnector};
pub use map::{build_spans, Mapped, Mapper};
pub use session::{Backoff, SessionConfig};
