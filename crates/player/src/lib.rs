//! Video playback behind the [`Player`] trait.
//!
//! - `MpvPlayer` (feature `mpv`): libmpv renders into an OpenGL texture that Slint shows. Needs a
//!   system libmpv.
//! - [`FakePlayer`]: records calls and lets tests send [`PlayerEvent`]s.
//!
//! Options that can change while a stream plays go through [`Player::set_option`]. Latency is
//! fixed per stream ([`LoadOptions`]). Stats are polled only while [`Player::set_stats`] is on.
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
    /// About once a second while stats are on (see [`Player::set_stats`]).
    Stats(PlayerStats),
}

/// How the picture fills a screen of another shape.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Zoom {
    /// The whole picture, with black bars.
    #[default]
    Fit,
    /// Cropped to fill the screen.
    Fill,
}

/// A player option that can change while a stream plays. It lasts until it is changed again.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PlayerOption {
    /// Hardware decoding where it is safe (`true`), or decoding on the CPU.
    Hwdec(bool),
    Zoom(Zoom),
}

/// How much the player buffers. Set when a stream is loaded.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Latency {
    /// Small buffers, close to live.
    #[default]
    Low,
    /// Bigger buffers: further behind live, fewer stalls.
    Normal,
}

/// Options for one [`Player::load`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct LoadOptions {
    pub latency: Latency,
}

/// What the player knows about the stream it plays. `None` for what it cannot tell (yet). Whole
/// numbers in small units, so events stay `Eq`.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct PlayerStats {
    pub width: Option<u32>,
    pub height: Option<u32>,
    /// Frames per second, times 1000.
    pub fps_milli: Option<u32>,
    pub codec: Option<String>,
    /// The hardware decoder in use. `None` when decoding on the CPU.
    pub hwdec: Option<String>,
    /// Bits per second.
    pub video_bitrate: Option<u64>,
    pub audio_bitrate: Option<u64>,
    /// Buffered ahead, in milliseconds.
    pub buffer_ms: Option<u32>,
    pub dropped_frames: Option<u64>,
    pub decoder_dropped_frames: Option<u64>,
}

/// Plays one stream at a time. Every method returns at once: the work happens on the player's
/// own thread, and the outcome comes back as a [`PlayerEvent`].
pub trait Player: Send + Sync + 'static {
    /// Starts `url`, replacing whatever is loaded.
    fn load(&self, url: &str, options: LoadOptions);
    /// Pauses or resumes.
    fn set_paused(&self, paused: bool);
    /// Stops playback and clears the picture.
    fn stop(&self);
    /// Changes an option of the stream that plays now and of the ones after it.
    fn set_option(&self, option: PlayerOption);
    /// Starts (`true`) or stops sending [`PlayerEvent::Stats`].
    fn set_stats(&self, on: bool);
}
