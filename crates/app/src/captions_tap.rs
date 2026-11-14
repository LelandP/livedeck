//! The single player's captions audio tap (captions-plan.md, phases 2 and 5): which tap runs, and
//! the one feed of audio it makes for the speech engine.
//!
//! `Playback` drives it. **A live stream:** a `Play` of the single player claims the channel (a new
//! channel stops the old tap at once), the load starts a tap on the same channel's 160p30
//! rendition, and a stop, multiview or a failed resolve stops it. A reload or quality change of the
//! same channel starts a new tap and drops the old one; the new one begins some seconds back, and
//! the overlap is cut off ([`Splice`]), so the feed goes on without a gap or a repeat.
//!
//! **A video or clip:** a `PlayVideo` claims it with its start position, and the tap starts there
//! and reads no more than 30 s beyond the playhead (`TapOptions`). Every load and every seek stops
//! the tap and starts a new one at the new place, with no overlap to cut: the feed says
//! [`CaptionAudio::Start`] again, so the engine starts afresh. The tap's address is kept for a
//! while, so a seek does not resolve it again.
//!
//! Nothing runs until a feed is set (`Playback::set_captions_feed`).

use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use htpc_core::{ChannelRef, VideoTarget};
use htpc_player::{
    AudioTaps, NoTaps, PcmChunk, Splice, Spliced, TapEvent, TapHandle, TapOptions, TapPace,
    TAP_RATE,
};

/// How long a video's tap address is used again for the next tap (a seek) before it is resolved
/// anew. A video's playlist has no expiry that was seen; a clip's address does (about a day).
const URL_KEPT: Duration = Duration::from_secs(30 * 60);

/// Silence added after the last audio of a video that ran to its end, so the engine's last speech
/// segment is closed and made.
const END_PAD_S: f64 = 2.0;

/// What the engine is fed.
#[derive(Debug, Clone, PartialEq)]
pub enum CaptionAudio {
    /// A new stream or a new clock begins: drop the audio and captions queued so far. Always
    /// before a stream's first chunk. `video`: the audio is a video's, read ahead of its playhead
    /// (a longer backlog fits), not a live stream's.
    Start { video: bool },
    /// The next audio, on the stream clock the single player's `PlayerHealth::stream_ms` uses.
    /// Starts go up without gaps or overlaps, unless the stream itself skipped.
    Chunk(PcmChunk),
    /// The tap stopped: playback stopped, the stream went offline, or the tap failed. A
    /// [`CaptionAudio::Start`] comes before any more audio. (A video that plays to its end sends
    /// none: its last audio is followed by silence, and the engine finishes what it has.)
    Stop,
}

/// Where the tap's audio goes. Called on the tap's thread, or the store's for a
/// [`CaptionAudio::Stop`], under a lock: it must return quickly and never call back into
/// `Playback`.
pub type CaptionFeed = Arc<dyn Fn(CaptionAudio) + Send + Sync>;

/// What the single player plays that gets a tap.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum Tapped {
    /// A Twitch channel's live stream.
    Live(ChannelRef),
    /// A Twitch video or clip.
    Video(VideoTarget),
}

impl Tapped {
    fn is_same(&self, other: &Tapped) -> bool {
        match (self, other) {
            (Tapped::Live(a), Tapped::Live(b)) => a.is_same(b),
            (Tapped::Video(a), Tapped::Video(b)) => a == b,
            _ => false,
        }
    }

    pub(crate) fn is_video(&self) -> bool {
        matches!(self, Tapped::Video(_))
    }
}

/// The single player's tap.
pub(crate) struct MainTap {
    taps: Arc<dyn AudioTaps>,
    state: Mutex<State>,
    /// Shared with the running tap's sink.
    feed: Arc<Mutex<Feed>>,
    /// The playhead of the video the tap reads, for its pace. Shared with the running tap.
    pace: TapPace,
    /// The video's position in the player's own terms (`pos_ms`), in ms: where a tap that starts
    /// now begins. Kept up to date by [`MainTap::follow`] and by loads and seeks.
    position_ms: AtomicU64,
}

struct State {
    /// What the single player plays as a picture (not sound only, not a multiview tile), tapped
    /// or not.
    what: Option<Tapped>,
    /// The tap running for it. Dropping it stops the tap.
    handle: Option<TapHandle>,
    /// Goes up with every load or seek of a video: a tap that is about to start for an older one
    /// is dropped.
    ticket: u64,
    /// The address a video's tap was started on, and when it was resolved.
    url: Option<(String, Instant)>,
}

struct Feed {
    to: Option<CaptionFeed>,
    /// The running tap's number. Each start and stop takes a new one, so the sink of a tap that
    /// was replaced or stopped is ignored.
    current: u64,
    splice: Splice,
    /// Audio went out since the last `Start`.
    flowing: bool,
    /// The running tap reads a video.
    video: bool,
}

impl Feed {
    fn send(&self, audio: CaptionAudio) {
        if let Some(to) = &self.to {
            to(audio);
        }
    }

    /// The tap stopped: say so if audio had gone out.
    fn stopped(&mut self) {
        if self.flowing {
            self.send(CaptionAudio::Stop);
        }
        self.flowing = false;
        self.splice.reset();
    }

    /// A video's audio is all in. Silence closes the last speech, and the engine is not reset,
    /// so it finishes the chunks it has.
    fn video_ended(&mut self) {
        if self.flowing {
            if let Some(start_s) = self.splice.until() {
                self.send(CaptionAudio::Chunk(PcmChunk {
                    start_s,
                    samples: vec![0.0; (END_PAD_S * f64::from(TAP_RATE)) as usize],
                }));
            }
        }
        self.flowing = false;
        self.splice.reset();
    }

    /// An event from tap number `tap`.
    fn on_event(&mut self, tap: u64, event: TapEvent) {
        if tap != self.current {
            return;
        }
        match event {
            TapEvent::Audio(chunk) => match self.splice.take(chunk) {
                Some(Spliced::Next(chunk)) if self.flowing => self.send(CaptionAudio::Chunk(chunk)),
                Some(Spliced::Next(chunk) | Spliced::NewClock(chunk)) => {
                    self.flowing = true;
                    self.send(CaptionAudio::Start { video: self.video });
                    self.send(CaptionAudio::Chunk(chunk));
                }
                None => {}
            },
            TapEvent::Ended => {
                tracing::info!("the captions tap's stream ended");
                self.current += 1;
                if self.video {
                    self.video_ended();
                } else {
                    self.stopped();
                }
            }
            TapEvent::Failed(e) => {
                tracing::warn!(%e, "the captions tap failed");
                self.current += 1;
                self.stopped();
            }
        }
    }
}

impl Default for MainTap {
    fn default() -> Self {
        Self::new(Arc::new(NoTaps))
    }
}

impl MainTap {
    pub(crate) fn new(taps: Arc<dyn AudioTaps>) -> Self {
        Self {
            taps,
            state: Mutex::new(State {
                what: None,
                handle: None,
                ticket: 0,
                url: None,
            }),
            feed: Arc::new(Mutex::new(Feed {
                to: None,
                current: 0,
                splice: Splice::default(),
                flowing: false,
                video: false,
            })),
            pace: TapPace::default(),
            position_ms: AtomicU64::new(0),
        }
    }

    fn state(&self) -> std::sync::MutexGuard<'_, State> {
        self.state.lock().expect("tap state lock")
    }

    fn feed(&self) -> std::sync::MutexGuard<'_, Feed> {
        self.feed.lock().expect("tap feed lock")
    }

    /// The single player starts playing `what` (`None`: sound only, a multiview tile, nothing),
    /// a video from `from_ms`. Another stream or none stops the tap at once; the same channel
    /// keeps it until the new one starts. A video always stops it: its place changed, and the new
    /// tap begins there. Returns the ticket for [`MainTap::start`] (which a newer one cancels).
    pub(crate) fn claim(&self, what: Option<Tapped>, from_ms: u64) -> u64 {
        let mut state = self.state();
        let same = match (&state.what, &what) {
            (Some(a), Some(b)) => a.is_same(b),
            _ => false,
        };
        let video = what.as_ref().is_some_and(Tapped::is_video);
        if !same {
            state.url = None;
        }
        state.what = what;
        state.ticket += 1;
        if video {
            self.position_ms.store(from_ms, Ordering::Relaxed);
            self.pace.clear();
        }
        if !same || video {
            self.stop_locked(&mut state);
        }
        state.ticket
    }

    /// The video jumped to `to_ms`: the tap stops now, and starts again there ([`MainTap::start`]
    /// with the ticket this returns). `None` if the single player plays no video with a tap.
    pub(crate) fn seeked(&self, to_ms: u64) -> Option<u64> {
        let mut state = self.state();
        if !state.what.as_ref().is_some_and(Tapped::is_video) {
            return None;
        }
        self.position_ms.store(to_ms, Ordering::Relaxed);
        self.pace.clear();
        state.ticket += 1;
        self.stop_locked(&mut state);
        Some(state.ticket)
    }

    /// Where the video is: its position in the player's own terms, and the stream clock the
    /// reducer shows captions by (`None` right after a jump: not known yet). A new tap starts at
    /// the position, and a running one is paced by the clock.
    pub(crate) fn follow(&self, position_ms: u64, clock_ms: Option<u64>) {
        self.position_ms.store(position_ms, Ordering::Relaxed);
        match clock_ms {
            Some(ms) => self.pace.set_playhead_ms(ms),
            None => self.pace.clear(),
        }
    }

    /// Stops the tap, keeping the claim (a failed tap resolve).
    pub(crate) fn stop(&self) {
        let mut state = self.state();
        self.stop_locked(&mut state);
    }

    fn stop_locked(&self, state: &mut State) {
        let mut feed = self.feed();
        feed.current += 1;
        state.handle = None;
        feed.stopped();
    }

    /// What to tap now, if captions are on and the single player plays it.
    pub(crate) fn wanted(&self) -> Option<Tapped> {
        let on = self.feed().to.is_some();
        on.then(|| self.state().what.clone()).flatten()
    }

    /// Whether something is wanted but has no tap yet.
    pub(crate) fn untapped(&self) -> Option<Tapped> {
        let state = self.state();
        if state.handle.is_some() {
            return None;
        }
        drop(state);
        self.wanted()
    }

    /// The tap's address for the video claimed, if it was resolved lately.
    pub(crate) fn kept_url(&self, what: &Tapped) -> Option<String> {
        let state = self.state();
        let (url, at) = state.url.as_ref()?;
        (state.what.as_ref().is_some_and(|w| w.is_same(what)) && at.elapsed() < URL_KEPT)
            .then(|| url.clone())
    }

    /// Starts a tap on `url`, the 160p30 rendition of `what`, replacing any running one. A video's
    /// tap begins at the position last known. Nothing if the single player moved on from `what`
    /// meanwhile, a newer load or seek (`ticket`) came, or captions went off.
    pub(crate) fn start(&self, what: &Tapped, url: &str, ticket: Option<u64>) {
        let mut state = self.state();
        if !state.what.as_ref().is_some_and(|c| c.is_same(what)) {
            return;
        }
        if ticket.is_some_and(|t| t != state.ticket) {
            return;
        }
        let video = what.is_video();
        let tap = {
            let mut feed = self.feed();
            if feed.to.is_none() {
                return;
            }
            feed.current += 1;
            feed.video = video;
            feed.current
        };
        let options = if video {
            TapOptions {
                from_ms: Some(self.position_ms.load(Ordering::Relaxed)),
                pace: Some(self.pace.clone()),
            }
        } else {
            TapOptions::default()
        };
        let shared = Arc::clone(&self.feed);
        let sink = Box::new(move |event| {
            shared.lock().expect("tap feed lock").on_event(tap, event);
        });
        match self.taps.start(url, options.clone(), sink) {
            Ok(handle) => {
                match what {
                    Tapped::Live(channel) => {
                        tracing::info!(channel = %channel.login, "captions tap started");
                    }
                    Tapped::Video(target) => {
                        tracing::info!(?target, from_ms = ?options.from_ms, "captions tap started");
                        if state.url.as_ref().is_none_or(|(u, _)| u != url) {
                            state.url = Some((url.to_owned(), Instant::now()));
                        }
                    }
                }
                // The old tap (same channel) stops here; its late audio has an old number.
                state.handle = Some(handle);
            }
            Err(e) => {
                tracing::warn!(%e, "could not start the captions tap");
                self.stop_locked(&mut state);
            }
        }
    }

    /// Sets where the audio goes, or turns the tap off (`None`).
    pub(crate) fn set_feed(&self, to: Option<CaptionFeed>) {
        let off = to.is_none();
        if off {
            self.stop();
        }
        self.feed().to = to;
    }
}
