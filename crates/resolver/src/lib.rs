//! Resolves a [`Target`] (a live channel, a Twitch video or a Twitch clip) to a URL mpv can play,
//! behind the [`StreamResolver`] trait.
//!
//! The trait takes a [`Target`] rather than having a method per kind: every kind runs the same
//! Streamlink call (a page URL and a quality list), so one method keeps one path through the app's
//! resolve queue, and a fake overrides one thing.
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

/// The URL that mpv can open: an HLS playlist for a channel or a video, an MP4 file for a clip.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PlaylistUrl(pub String);

/// Why a target could not be resolved.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum ResolveError {
    /// The `streamlink` program is not installed (or not on `PATH`).
    #[error("Streamlink is not installed")]
    NotInstalled,
    /// Nothing to play: the channel exists but is not live, or the video or clip has no playable
    /// stream. Streamlink answers the same for a channel or video that does not exist.
    #[error("the channel is offline")]
    Offline,
    /// The target is not a valid channel name, video id or clip slug, or Streamlink cannot handle
    /// it. An invalid one never reaches the program.
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
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub enum Site {
    #[default]
    Twitch,
    Kick,
}

/// What to resolve. Each kind is checked before anything reaches the command line (see
/// [`Target::page_url`]), so a bad name, id or slug fails as [`ResolveError::NotFound`].
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum Target {
    /// A channel's live stream. `login` is the login (Twitch) or slug (Kick) on `site`.
    Channel { site: Site, login: String },
    /// A Twitch video (a past broadcast or highlight) by its id: digits only, at most 12.
    TwitchVideo(String),
    /// A Twitch clip by its slug: letters, digits, `_` and `-`, at most 100. It resolves to an MP4
    /// file instead of a playlist.
    TwitchClip(String),
}

impl Target {
    /// The live stream of `login` on `site`.
    pub fn channel(site: Site, login: impl Into<String>) -> Self {
        Self::Channel {
            site,
            login: login.into(),
        }
    }

    /// The name, id or slug, for logs and the fakes.
    pub fn name(&self) -> &str {
        match self {
            Self::Channel { login, .. } => login,
            Self::TwitchVideo(id) => id,
            Self::TwitchClip(slug) => slug,
        }
    }

    /// The page Streamlink picks its plugin by, or `None` if the name, id or slug is not valid.
    /// Checking here keeps anything unexpected out of the URL that is handed to the program.
    pub fn page_url(&self) -> Option<String> {
        match self {
            Self::Channel { site, login } => valid_channel(*site, login).then(|| match site {
                Site::Twitch => format!("https://twitch.tv/{login}"),
                Site::Kick => format!("https://kick.com/{login}"),
            }),
            Self::TwitchVideo(id) => {
                valid_video_id(id).then(|| format!("https://www.twitch.tv/videos/{id}"))
            }
            Self::TwitchClip(slug) => {
                valid_clip_slug(slug).then(|| format!("https://clips.twitch.tv/{slug}"))
            }
        }
    }
}

/// Twitch channel logins are letters, digits and underscores, Kick slugs also take `-`, which may
/// not lead (it would read as an option).
fn valid_channel(site: Site, channel: &str) -> bool {
    let (max, extra) = match site {
        Site::Twitch => (25, '_'),
        Site::Kick => (50, '-'),
    };
    !channel.is_empty()
        && channel.len() <= max
        && !channel.starts_with('-')
        && channel
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '_' || c == extra)
}

/// Twitch video ids are digits only. Today's are 10 digits; 12 leaves room to grow.
fn valid_video_id(id: &str) -> bool {
    (1..=12).contains(&id.len()) && id.bytes().all(|b| b.is_ascii_digit())
}

/// Twitch clip slugs are letters, digits, `_` and `-` (`AmazingSlugName-AbC123_xyz`). A leading
/// `-` is refused, as for Kick slugs.
fn valid_clip_slug(slug: &str) -> bool {
    (1..=100).contains(&slug.len())
        && !slug.starts_with('-')
        && slug
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'_' || b == b'-')
}

/// Looks up what mpv should open for a live channel, a video or a clip.
pub trait StreamResolver: Send + Sync + 'static {
    /// `quality` is a Streamlink stream name (`best`, `720p`, `worst`, ...) or a comma-separated
    /// fallback list of them. Videos and clips use the same names as live streams.
    fn resolve(
        &self,
        target: &Target,
        quality: &str,
    ) -> impl Future<Output = Result<PlaylistUrl, ResolveError>> + Send;
}
