//! Kick client behind the mockable [`KickApi`] trait.
//!
//! Kick's official API wants a token for every call, and a token needs the app's client secret,
//! which an open-source app cannot keep. Signed out (the only way LiveDeck uses Kick), this crate
//! talks to the website's own unofficial API, as the Twitch crate does with GQL. It can break
//! without notice.
//!
//! - [`KickWebClient`]: the real client.
//! - [`FakeKick`]: canned data, for tests and `--mock`.
//!
//! This crate does not depend on `htpc-core`. The app maps its types to domain types.

mod error;
mod fake;
mod model;
mod web;

use std::future::Future;

pub use error::{Error, Result};
pub use fake::{live as fake_live, FakeKick};
pub use model::{ChannelInfo, LiveStream, LivestreamPage, PusherApp, SearchHit};
pub use web::{
    fallback_pusher_app, KickWebClient, DEFAULT_REALTIME_URL, DEFAULT_SITE_URL, PAGE_SIZE,
};

/// The Pusher application the Kick website reads chat from, as of 2026-11-06. Kick hands out the
/// current key at `POST web.kick.com/api/v1/realtime/channels/<id>/chat/connection`; this one is
/// the fallback when that call fails.
pub const PUSHER_KEY: &str = "34bf7a0ff419a2a775b9";
pub const PUSHER_CLUSTER: &str = "us2";

/// The Pusher WebSocket address for `key` in `cluster`.
pub fn pusher_url(key: &str, cluster: &str) -> String {
    format!(
        "wss://ws-{cluster}.pusher.com/app/{key}?protocol=7&client=js&version=8.4.0&flash=false"
    )
}

/// The parts of Kick the app uses. Every method is one request.
pub trait KickApi: Send + Sync + 'static {
    /// The live streams with the most viewers, page `page` (from 1).
    fn top_livestreams(&self, page: u32) -> impl Future<Output = Result<LivestreamPage>> + Send;

    /// The channel with this slug, with its stream while it is live. `Ok(None)` if there is no
    /// such channel.
    fn channel(&self, slug: &str) -> impl Future<Output = Result<Option<ChannelInfo>>> + Send;

    /// Channels whose name matches `query`, best match first. Viewer counts are not included:
    /// look a live one up with [`KickApi::channel`].
    fn search_channels(&self, query: &str) -> impl Future<Output = Result<Vec<SearchHit>>> + Send;

    /// The Pusher app the chat of channel `channel_id` is served from.
    fn chat_app(&self, channel_id: u64) -> impl Future<Output = Result<PusherApp>> + Send;
}
