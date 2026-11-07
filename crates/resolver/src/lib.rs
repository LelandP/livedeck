//! Resolves a channel to an HLS playlist URL behind the [`StreamResolver`] trait.
//!
//! - [`StreamlinkResolver`]: runs `streamlink --stream-url` through an injectable
//!   [`ProcessRunner`], with a timeout and error mapping.
//! - [`FakeResolver`] and [`FakeRunner`]: canned answers for tests and `--mock`.
//!
//! This crate does not depend on `htpc-core`. The app maps its types.

mod fake;
mod runner;
mod streamlink;

use std::future::Future;

pub use fake::{FakeResolver, FakeRunner};
pub use runner::{ProcessOutput, ProcessRunner, RunError, TokioRunner};
pub use streamlink::{map_output, StreamlinkResolver, DEFAULT_TIMEOUT};

/// The URL of an HLS playlist that mpv can open.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PlaylistUrl(pub String);

/// Why a channel could not be resolved.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum ResolveError {
    /// The `streamlink` program is not installed (or not on `PATH`).
    #[error("Streamlink is not installed")]
    NotInstalled,
    /// The channel exists but is not live, or has no playable stream. Streamlink answers the same
    /// for a channel that does not exist.
    #[error("the channel is offline")]
    Offline,
    /// Streamlink cannot handle the channel name.
    #[error("channel not found")]
    NotFound,
    /// Streamlink did not answer in time.
    #[error("Streamlink took too long to answer")]
    TimedOut,
    /// Anything else: Streamlink failed. Carries its last error line.
    #[error("could not resolve the stream: {0}")]
    Failed(String),
}

/// Where a channel lives, which decides its page address and what a valid name is.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Site {
    #[default]
    Twitch,
    Kick,
}

/// Looks up the playlist of a live channel.
pub trait StreamResolver: Send + Sync + 'static {
    /// `channel` is the login (Twitch) or slug (Kick) on `site`. `quality` is a Streamlink stream
    /// name (`best`, `720p`, `worst`, ...).
    fn resolve(
        &self,
        site: Site,
        channel: &str,
        quality: &str,
    ) -> impl Future<Output = Result<PlaylistUrl, ResolveError>> + Send;
}
