//! Video playback behind the [`Player`] trait.
//!
//! - `MpvPlayer` (feature `mpv`): libmpv renders into an OpenGL texture that Slint shows. Needs a
//!   system libmpv.
//! - [`FakePlayer`]: records calls and lets tests send [`PlayerEvent`]s.
//!
//! Options that can change while a stream plays go through [`Player::set_option`]. Latency is
//! fixed per stream ([`LoadOptions`]). Stats are polled only while [`Player::set_stats`] is on.
//! Videos and clips load with a start position ([`LoadOptions::video`]), seek with
//! [`Player::seek`], and report their position and length in [`PlayerHealth`].
//! - [`UnavailablePlayer`]: what a build without the `mpv` feature uses, or any build when libmpv
//!   failed to start. Every load fails with the reason.
//!
//! Several streams play at once through a [`PlayerPool`]: one player per [`Slot`], made on demand.
//! Slot 0 ([`MAIN_SLOT`]) is the single player. With `mpv`, `VideoOutputs` draws every slot's
//! frames from the window's one rendering notifier. A new quality of the stream on screen loads in
//! a hidden spare player and takes over at its first frame ([`PlayerPool::spare`]).
//!
//! This crate does not depend on `htpc-core`. The app maps its types.

mod fake;
mod pace;
mod pool;

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
pub use mpv::{encode_file, MpvPlayer};
#[cfg(feature = "mpv")]
pub use output::{VideoOutput, VideoOutputs};
pub use pace::RefreshRate;
pub use pool::{FakePool, PlayerPool, Report, Seat, IDLE_DROP};

/// Which of the pool's players: 0 is the single player, 1 to 8 the other multiview tiles.
pub type Slot = u8;

/// How many players can exist at once (multiview's largest grid is 3×3).
pub const SLOTS: usize = 9;

/// The single player's slot. It is never dropped for being idle.
pub const MAIN_SLOT: Slot = 0;

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
    /// About once a second from the first frame of a stream until it stops, except while paused:
    /// a paused video sends none, so its last position is the one from before the pause.
    Health(PlayerHealth),
}

/// How well the stream keeps up: the slow-network rule's input (plan.md, section 4). For a video,
/// also where it is: the position to save and to draw the seek bar from (vod-plan.md).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct PlayerHealth {
    /// The picture froze for lack of data: `paused-for-cache` (normal latency), or nothing at all
    /// buffered (low latency turns `cache-pause` off, so mpv plays on into an empty buffer).
    pub stalled: bool,
    /// Buffered ahead (`demuxer-cache-duration`), in milliseconds.
    pub buffer_ms: u32,
    /// Where a video or clip is (`time-pos`), in milliseconds from its start. `None` for a live
    /// stream, and while mpv cannot tell.
    pub pos_ms: Option<u64>,
    /// How long a video or clip is (`duration`), in milliseconds. `None` for a live stream, and
    /// while mpv cannot tell (a broadcast still being recorded may have none).
    pub length_ms: Option<u64>,
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
    /// Decode and show video (`true`), or play the sound only and send no frames. Kept for the
    /// streams loaded after it, so it is set before each load.
    Video(bool),
    /// Silent (`true`) but still playing, or heard. Multiview mutes every tile but the one with
    /// sound. Kept for the streams loaded after it, so a muted tile stays muted on a reload.
    Muted(bool),
    /// How fast a video plays (mpv's `speed`), in thousandths: 1000 is normal, 1500 is 1.5×.
    /// Thousandths keep the option `Eq`. Kept for the videos loaded after it, but a live stream
    /// always loads at normal speed.
    Speed(u32),
}

/// [`PlayerOption::Speed`]'s normal speed.
pub const NORMAL_SPEED: u32 = 1000;

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
    /// Ignored for a video: it always gets the normal buffers.
    pub latency: Latency,
    /// `Some` for a video or clip: where to start, in milliseconds from its start (mpv's
    /// `start=`). It also makes it a video: it loads with [`Latency::Normal`] whatever `latency`
    /// says, it seeks, and its health reports carry its position and length. `None` for a live
    /// stream.
    pub start_ms: Option<u64>,
}

impl LoadOptions {
    /// A video or clip from `start_ms`, with the normal buffers.
    pub fn video(start_ms: u64) -> Self {
        Self {
            latency: Latency::Normal,
            start_ms: Some(start_ms),
        }
    }

    /// A video or clip, rather than a live stream.
    pub fn is_video(&self) -> bool {
        self.start_ms.is_some()
    }

    /// The latency the load really gets: always [`Latency::Normal`] for a video, as the
    /// low-latency buffers would only make it stall more and gain nothing.
    pub fn effective_latency(&self) -> Latency {
        if self.is_video() {
            Latency::Normal
        } else {
            self.latency
        }
    }
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
    pub audio_codec: Option<String>,
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
    /// Jumps to `to_ms` milliseconds from the start of the video that plays (mpv's `seek
    /// <secs> absolute`). A paused video stays paused. Does nothing useful on a live stream.
    fn seek(&self, to_ms: u64);
    /// Stops playback and clears the picture.
    fn stop(&self);
    /// Changes an option of the stream that plays now and of the ones after it.
    fn set_option(&self, option: PlayerOption);
    /// Starts (`true`) or stops sending [`PlayerEvent::Stats`].
    fn set_stats(&self, on: bool);
    /// Lowers the volume from full to silent over `duration` (the sleep timer's gentle stop).
    /// The next [`Player::load`] or [`Player::restore_volume`] brings it back to full.
    fn fade_out(&self, duration: std::time::Duration);
    /// Stops a fade and sets the volume back to full.
    fn restore_volume(&self);
}
