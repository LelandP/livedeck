//! Video playback behind the [`Player`] trait.
//!
//! - `MpvPlayer` (feature `mpv`): libmpv renders into an OpenGL texture that Slint shows. Needs a
//!   system libmpv.
//! - [`FakePlayer`]: records calls and lets tests send [`PlayerEvent`]s.
//! - [`UnavailablePlayer`]: what a build without the `mpv` feature uses, or any build when libmpv
//!   failed to start. Every load fails with the reason.
//!
//! This crate does not depend on `htpc-core`. The app maps its types.

mod fake;

#[cfg(feature = "mpv")]
mod gl_state;
#[cfg(feature = "mpv")]
mod gpu;
#[cfg(feature = "mpv")]
mod mpv;
#[cfg(feature = "mpv")]
mod output;
#[cfg(any(feature = "mpv", test))]
mod recent;
#[cfg(feature = "mpv")]
mod target;

pub use fake::{FakePlayer, PlayerCall, UnavailablePlayer};
#[cfg(feature = "mpv")]
pub use mpv::MpvPlayer;
#[cfg(feature = "mpv")]
pub use output::VideoOutput;

/// What the player reports. Sent from the player's own thread.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PlayerEvent {
    /// The first frame of the loaded stream is ready to show.
    Playing,
    /// The stream stopped by itself: the channel went offline or the connection dropped.
    Ended,
    /// The stream could not be played at all (bad playlist, decode failure).
    Failed(String),
    /// The pause state changed.
    Paused(bool),
}

/// Plays one stream at a time. Every method returns at once: the work happens on the player's
/// own thread, and the outcome comes back as a [`PlayerEvent`].
pub trait Player: Send + Sync + 'static {
    /// Starts `url`, replacing whatever is loaded.
    fn load(&self, url: &str);
    /// Pauses or resumes.
    fn set_paused(&self, paused: bool);
    /// Stops playback and clears the picture.
    fn stop(&self);
}
