//! The audio tap's data (captions-plan.md, phase 2): 16 kHz mono PCM stamped with the stream's
//! own clock, and the pure parts of reading it from the WAV file the encode-mode libmpv writes.
//!
//! - [`PcmChunk`] and [`TapEvent`]: what a tap delivers.
//! - [`AudioTaps`]: starts taps. `MpvTaps` (feature `mpv`) for real, [`FakeTaps`] for tests,
//!   [`NoTaps`] for builds without libmpv.
//! - [`TapOptions`]: where a video's tap starts and how far ahead of the playhead ([`TapPace`])
//!   it may read.
//! - [`Splice`]: joins the audio of two taps of the same stream that overlap (a restart), so each
//!   moment is delivered once.
//! - `WavReader`, `Timeline`, `Chunker`: the WAV tailing and timestamp maths `AudioTap` uses.

use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};

/// The tap's sample rate: what the speech model takes.
pub const TAP_RATE: u32 = 16_000;

/// Samples in a delivered chunk: 0.5 s.
#[cfg(any(feature = "mpv", test))]
const CHUNK_SAMPLES: usize = TAP_RATE as usize / 2;

/// Mono 16 kHz samples in -1.0..1.0, and when the first one plays on the stream's own clock
/// (mpv's `time-pos` with `rebase-start-time=no`, the same as `PlayerHealth::stream_ms`).
#[derive(Debug, Clone, PartialEq)]
pub struct PcmChunk {
    /// Seconds, on the stream's clock.
    pub start_s: f64,
    pub samples: Vec<f32>,
}

impl PcmChunk {
    /// When the sample after the last one plays.
    pub fn end_s(&self) -> f64 {
        self.start_s + self.samples.len() as f64 / f64::from(TAP_RATE)
    }
}

/// What a tap reports, from its own thread.
#[derive(Debug, Clone, PartialEq)]
pub enum TapEvent {
    /// About every half second while the stream plays. Starts go up, without overlaps.
    Audio(PcmChunk),
    /// The stream ended (the channel went offline). The tap has stopped.
    Ended,
    /// The tap could not start or stopped with an error. It has stopped.
    Failed(String),
}

/// Where a tap's events go. Called on the tap's thread, so it must return quickly.
pub type TapSink = Box<dyn Fn(TapEvent) + Send + Sync>;

/// A running tap. Dropping it stops the tap: its libmpv is closed and its file deleted on the
/// tap's own thread, so dropping never waits.
pub struct TapHandle(#[allow(dead_code)] Box<dyn Send + Sync>);

impl TapHandle {
    /// Keeps `running` until the handle is dropped.
    pub fn new(running: impl Send + Sync + 'static) -> Self {
        Self(Box::new(running))
    }
}

/// How far ahead of the playhead a video's tap reads, in seconds of the stream: the time a caption
/// has to be made in (a 5 s segment, the pause that ends it and ~2 s of the model take up to 10 s,
/// so 30 s leaves room for a slow machine and a speed of 2×).
pub const LOOKAHEAD_S: f64 = 30.0;

/// A paced tap that reached the lookahead reads on again once it is this much under it: the
/// audio comes in bursts of 5 s instead of a trickle.
#[cfg(any(feature = "mpv", test))]
pub const RESUME_BELOW_S: f64 = 5.0;

/// Whether a paced tap should be paused: it has read `ahead_s` seconds beyond the playhead, and
/// is `paused` already (pausing at [`LOOKAHEAD_S`], reading on [`RESUME_BELOW_S`] under it).
#[cfg(any(feature = "mpv", test))]
pub fn should_pause(ahead_s: f64, paused: bool) -> bool {
    if paused {
        ahead_s > LOOKAHEAD_S - RESUME_BELOW_S
    } else {
        ahead_s >= LOOKAHEAD_S
    }
}

/// The playhead of the video a tap reads, for [`TapOptions::pace`]. The app sets it from the
/// player's health reports; clones share it.
#[derive(Debug, Clone)]
pub struct TapPace(Arc<AtomicU64>);

/// No playhead known: the tap reads on.
const NO_PLAYHEAD: u64 = u64::MAX;

impl Default for TapPace {
    fn default() -> Self {
        Self(Arc::new(AtomicU64::new(NO_PLAYHEAD)))
    }
}

impl TapPace {
    /// The playhead on the stream's clock (`PlayerHealth::stream_ms`), in milliseconds.
    pub fn set_playhead_ms(&self, ms: u64) {
        self.0.store(ms, Ordering::Relaxed);
    }

    /// The playhead is not known (the video jumped): a tap takes it to be where the tap started,
    /// until the next report.
    pub fn clear(&self) {
        self.0.store(NO_PLAYHEAD, Ordering::Relaxed);
    }

    /// The playhead in seconds, if known.
    pub fn playhead_s(&self) -> Option<f64> {
        let ms = self.0.load(Ordering::Relaxed);
        (ms != NO_PLAYHEAD).then(|| ms as f64 / 1000.0)
    }
}

/// What a tap reads (captions-plan.md, phase 5). The default is a live stream: from its live edge,
/// as fast as it comes.
#[derive(Debug, Clone, Default)]
pub struct TapOptions {
    /// A video or clip: where in it to start, in the player's own position (`PlayerHealth::pos_ms`,
    /// from the start of the video). The tap's audio is on the stream clock all the same: the
    /// position plus the video's start time.
    pub from_ms: Option<u64>,
    /// A video's tap reads at most [`LOOKAHEAD_S`] beyond this playhead, so it neither downloads
    /// the whole video nor floods the speech engine. `None`: as fast as it comes.
    pub pace: Option<TapPace>,
}

/// Starts audio taps on a playlist (Twitch's 160p30 rendition, whose clock matches every video
/// rendition's, captions-plan.md section 1).
pub trait AudioTaps: Send + Sync + 'static {
    /// Starts tapping `url`. Returns at once; the audio follows through `sink`.
    fn start(&self, url: &str, options: TapOptions, sink: TapSink) -> Result<TapHandle, String>;
}

/// For builds without libmpv: every tap fails.
#[derive(Debug, Clone, Copy, Default)]
pub struct NoTaps;

impl AudioTaps for NoTaps {
    fn start(&self, _url: &str, _options: TapOptions, _sink: TapSink) -> Result<TapHandle, String> {
        Err("this build has no audio tap (built without the `mpv` feature)".into())
    }
}

/// One tap a [`FakeTaps`] started.
struct FakeTap {
    url: String,
    options: TapOptions,
    sink: TapSink,
    /// Dropped with the handle.
    alive: Arc<()>,
}

/// Records the taps started, lets a test send their events ([`FakeTaps::emit`]) and tells which
/// are still running. Clones share everything.
#[derive(Clone, Default)]
pub struct FakeTaps {
    taps: Arc<Mutex<Vec<FakeTap>>>,
}

impl FakeTaps {
    pub fn new() -> Self {
        Self::default()
    }

    /// The URL of every tap started, oldest first.
    pub fn started(&self) -> Vec<String> {
        let taps = self.taps.lock().expect("taps lock");
        taps.iter().map(|t| t.url.clone()).collect()
    }

    /// The options of every tap started, oldest first.
    pub fn options(&self) -> Vec<TapOptions> {
        let taps = self.taps.lock().expect("taps lock");
        taps.iter().map(|t| t.options.clone()).collect()
    }

    /// The URLs of the taps whose handle is still alive.
    pub fn running(&self) -> Vec<String> {
        let taps = self.taps.lock().expect("taps lock");
        taps.iter()
            .filter(|t| Arc::strong_count(&t.alive) > 1)
            .map(|t| t.url.clone())
            .collect()
    }

    /// Sends `event` from tap number `index` (in the order started), running or not: a stopped
    /// tap's thread may still deliver one last chunk.
    pub fn emit(&self, index: usize, event: TapEvent) {
        let taps = self.taps.lock().expect("taps lock");
        if let Some(tap) = taps.get(index) {
            (tap.sink)(event);
        }
    }
}

impl AudioTaps for FakeTaps {
    fn start(&self, url: &str, options: TapOptions, sink: TapSink) -> Result<TapHandle, String> {
        let alive = Arc::new(());
        let handle = TapHandle::new(Arc::clone(&alive));
        self.taps.lock().expect("taps lock").push(FakeTap {
            url: url.to_owned(),
            options,
            sink,
            alive,
        });
        Ok(handle)
    }
}

/// What [`Splice::take`] makes of a chunk.
#[derive(Debug, Clone, PartialEq)]
pub enum Spliced {
    /// The next audio: after what came before, with any overlap cut off.
    Next(PcmChunk),
    /// Audio from much earlier than what came before: a new clock (the stream restarted, or
    /// another one). Whoever queues audio or captions by time starts afresh.
    NewClock(PcmChunk),
}

/// Joins audio that may overlap: a restarted tap begins some seconds before the live edge the
/// old one had reached. Each moment is passed on once.
#[derive(Debug, Clone, Default)]
pub struct Splice {
    /// The end of the audio passed on so far.
    until: Option<f64>,
}

impl Splice {
    /// More than this far back is not an overlap but another clock. A restarted tap begins at
    /// most about 30 s back (Twitch's playlist, measured 2026-11-13).
    pub const MAX_OVERLAP_S: f64 = 120.0;

    /// Forgets what came before: the next chunk is passed on whole.
    pub fn reset(&mut self) {
        self.until = None;
    }

    /// The end of the audio passed on so far, if any.
    pub fn until(&self) -> Option<f64> {
        self.until
    }

    /// `chunk` without the part already passed on. `None` if all of it was.
    pub fn take(&mut self, mut chunk: PcmChunk) -> Option<Spliced> {
        let Some(until) = self.until else {
            self.until = Some(chunk.end_s());
            return Some(Spliced::Next(chunk));
        };
        if chunk.end_s() < until - Self::MAX_OVERLAP_S {
            self.until = Some(chunk.end_s());
            return Some(Spliced::NewClock(chunk));
        }
        if chunk.start_s < until {
            let skip = ((until - chunk.start_s) * f64::from(TAP_RATE)).round() as usize;
            if skip >= chunk.samples.len() {
                return None;
            }
            chunk.samples.drain(..skip);
            chunk.start_s += skip as f64 / f64::from(TAP_RATE);
        }
        self.until = Some(chunk.end_s());
        Some(Spliced::Next(chunk))
    }
}

/// Why the WAV cannot be read.
#[cfg(any(feature = "mpv", test))]
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum WavError {
    NotWav,
    /// Not 16-bit mono 16 kHz PCM.
    Format(String),
}

#[cfg(any(feature = "mpv", test))]
impl std::fmt::Display for WavError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::NotWav => f.write_str("the tap file is not a WAV file"),
            Self::Format(what) => write!(f, "the tap writes {what}, not 16 kHz mono 16-bit PCM"),
        }
    }
}

/// Reads a WAV file as it grows, in whatever pieces it is read: skips the header (whatever
/// chunks it has before `data`; lavf writes a `LIST` one) and turns 16-bit samples to `f32`. The
/// sizes in the header are ignored, as they are only filled in when the file is finished.
#[cfg(any(feature = "mpv", test))]
#[derive(Debug, Default)]
pub(crate) struct WavReader {
    /// Bytes not used yet: the header until `data` starts, or half a sample after it.
    pending: Vec<u8>,
    /// Past the header, reading samples.
    in_data: bool,
    /// Samples read so far.
    samples: u64,
}

#[cfg(any(feature = "mpv", test))]
impl WavReader {
    /// The samples in `bytes`, the file's next bytes.
    pub(crate) fn push(&mut self, bytes: &[u8]) -> Result<Vec<f32>, WavError> {
        self.pending.extend_from_slice(bytes);
        if !self.in_data {
            let Some(start) = self.data_start()? else {
                return Ok(Vec::new());
            };
            self.pending.drain(..start);
            self.in_data = true;
        }
        let whole = self.pending.len() / 2 * 2;
        let samples: Vec<f32> = self.pending[..whole]
            .as_chunks::<2>()
            .0
            .iter()
            .map(|b| f32::from(i16::from_le_bytes(*b)) / 32768.0)
            .collect();
        self.pending.drain(..whole);
        self.samples += samples.len() as u64;
        Ok(samples)
    }

    /// Samples read so far.
    pub(crate) fn samples(&self) -> u64 {
        self.samples
    }

    /// Where the samples start in `pending`, once the whole header is there. Checks the format
    /// on the way.
    fn data_start(&self) -> Result<Option<usize>, WavError> {
        let b = &self.pending;
        if b.len() < 12 {
            return Ok(None);
        }
        if &b[0..4] != b"RIFF" || &b[8..12] != b"WAVE" {
            return Err(WavError::NotWav);
        }
        let mut at = 12;
        loop {
            let Some(head) = b.get(at..at + 8) else {
                return Ok(None);
            };
            let size = u32::from_le_bytes([head[4], head[5], head[6], head[7]]) as usize;
            let body = at + 8;
            if &head[0..4] == b"data" {
                return Ok(Some(body));
            }
            if &head[0..4] == b"fmt " {
                let Some(fmt) = b.get(body..body + 16) else {
                    return Ok(None);
                };
                check_format(fmt)?;
            }
            // Chunks are padded to an even size.
            at = body + size + (size & 1);
        }
    }
}

/// The `fmt ` chunk must say 16-bit mono 16 kHz PCM (plain, or `WAVE_FORMAT_EXTENSIBLE`).
#[cfg(any(feature = "mpv", test))]
fn check_format(fmt: &[u8]) -> Result<(), WavError> {
    let word = |i: usize| u16::from_le_bytes([fmt[i], fmt[i + 1]]);
    let tag = word(0);
    let channels = word(2);
    let rate = u32::from_le_bytes([fmt[4], fmt[5], fmt[6], fmt[7]]);
    let bits = word(14);
    if !matches!(tag, 1 | 0xFFFE) || channels != 1 || rate != TAP_RATE || bits != 16 {
        return Err(WavError::Format(format!(
            "format {tag:#x}, {channels} channel(s), {rate} Hz, {bits} bits"
        )));
    }
    Ok(())
}

/// Maps a sample's index in the tap's output to the stream's clock: the timestamp of an anchor
/// sample plus the samples since it. The first anchor is the tap's first `audio-pts` (sample 0). A
/// later one is only set if the audio jumps (a discontinuity in the playlist).
#[cfg(any(feature = "mpv", test))]
#[derive(Debug, Clone, Default)]
pub(crate) struct Timeline {
    /// `(sample index, its timestamp)`, in order.
    anchors: Vec<(u64, f64)>,
    /// The last check's gap, if it was out of bounds. A jump is only believed when the next check
    /// finds the same gap: a tap catching up (it reads seconds of audio at a time) can look out
    /// of bounds for a moment.
    suspect: Option<f64>,
}

/// How much two out-of-bounds checks may differ and still be the same jump.
#[cfg(any(feature = "mpv", test))]
const SAME_JUMP_S: f64 = 0.5;

/// How far mpv's `audio-pts` may run ahead of the audio in the file: the encoder holds back up to
/// one packet (16384 samples, 1.024 s, measured 2026-11-13), and some slack.
#[cfg(any(feature = "mpv", test))]
pub(crate) const MAX_HELD_S: f64 = 1.5;

/// How far `audio-pts` may lag the audio in the file before it counts as a jump.
#[cfg(any(feature = "mpv", test))]
pub(crate) const MAX_BEHIND_S: f64 = 0.25;

#[cfg(any(feature = "mpv", test))]
impl Timeline {
    /// Sample 0 plays at `pts`.
    pub(crate) fn starting_at(pts: f64) -> Self {
        Self {
            anchors: vec![(0, pts)],
            suspect: None,
        }
    }

    #[cfg_attr(not(feature = "mpv"), allow(dead_code))]
    pub(crate) fn is_set(&self) -> bool {
        !self.anchors.is_empty()
    }

    /// When sample `index` plays. `None` before the first anchor is set.
    pub(crate) fn at(&self, index: u64) -> Option<f64> {
        let (from, pts) = self.anchors.iter().rev().find(|(i, _)| *i <= index)?;
        Some(pts + (index - from) as f64 / f64::from(TAP_RATE))
    }

    /// The next anchor after `index`, if any: a chunk must not run across it.
    fn next_anchor(&self, index: u64) -> Option<u64> {
        self.anchors.iter().map(|(i, _)| *i).find(|i| *i > index)
    }

    /// Checks the mapping against mpv's `audio-pts` (`now`, read before the file was) once
    /// `written` samples are read. Returns how far `audio-pts` is ahead of the mapped time
    /// (normally 0 to 1.024 s, what the encoder holds back), and whether that was a jump: out of
    /// bounds twice in a row by the same amount. Then sample `written` gets a new anchor halfway
    /// into the held-back range.
    pub(crate) fn check(&mut self, written: u64, now: f64) -> Option<(f64, bool)> {
        let ahead = now - self.at(written)?;
        if (-MAX_BEHIND_S..=MAX_HELD_S).contains(&ahead) {
            self.suspect = None;
            return Some((ahead, false));
        }
        let confirmed = self
            .suspect
            .is_some_and(|before| (before - ahead).abs() <= SAME_JUMP_S);
        if !confirmed {
            self.suspect = Some(ahead);
            return Some((ahead, false));
        }
        self.suspect = None;
        let last = self.anchors.last().map_or(0, |(i, _)| *i);
        let pts = now - 0.5;
        if written > last {
            self.anchors.push((written, pts));
        } else if let Some(anchor) = self.anchors.last_mut() {
            // Nothing was read since the last anchor: correct it instead.
            anchor.1 = pts - (written - anchor.0) as f64 / f64::from(TAP_RATE);
        }
        Some((ahead, true))
    }
}

/// Cuts the samples read into chunks of [`CHUNK_SAMPLES`] stamped by a [`Timeline`]. A chunk never
/// runs across an anchor, so its samples are evenly spaced.
#[cfg(any(feature = "mpv", test))]
#[derive(Debug, Default)]
pub(crate) struct Chunker {
    /// Samples not delivered yet.
    pending: Vec<f32>,
    /// The index of `pending[0]`.
    first: u64,
}

#[cfg(any(feature = "mpv", test))]
impl Chunker {
    /// Adds `samples` (the next ones read) and returns the chunks now complete.
    pub(crate) fn push(&mut self, samples: &[f32], timeline: &Timeline) -> Vec<PcmChunk> {
        self.pending.extend_from_slice(samples);
        let mut out = Vec::new();
        loop {
            let mut take = CHUNK_SAMPLES;
            if let Some(next) = timeline.next_anchor(self.first) {
                let until = usize::try_from(next - self.first).unwrap_or(usize::MAX);
                take = take.min(until);
            }
            // Before the clock is known the audio waits (the tap starts paused, so it has none).
            let Some(start_s) = timeline.at(self.first) else {
                return out;
            };
            if self.pending.len() < take {
                return out;
            }
            let samples: Vec<f32> = self.pending.drain(..take).collect();
            self.first += take as u64;
            out.push(PcmChunk { start_s, samples });
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A WAV header as lavf writes it: `fmt `, a `LIST` chunk, then `data` with a size to fill in
    /// later.
    fn header(rate: u32, channels: u16) -> Vec<u8> {
        let mut h = Vec::new();
        h.extend_from_slice(b"RIFF\xff\xff\xff\xffWAVE");
        h.extend_from_slice(b"fmt \x10\0\0\0");
        h.extend_from_slice(&1u16.to_le_bytes());
        h.extend_from_slice(&channels.to_le_bytes());
        h.extend_from_slice(&rate.to_le_bytes());
        h.extend_from_slice(&(rate * 2).to_le_bytes());
        h.extend_from_slice(&2u16.to_le_bytes());
        h.extend_from_slice(&16u16.to_le_bytes());
        // An odd-sized chunk, padded.
        h.extend_from_slice(b"LIST\x0d\0\0\0INFOISFT\x01\0\0\0x\0");
        h.extend_from_slice(b"data\xff\xff\xff\xff");
        h
    }

    fn pcm(samples: &[i16]) -> Vec<u8> {
        samples.iter().flat_map(|s| s.to_le_bytes()).collect()
    }

    #[test]
    fn the_header_is_skipped_whatever_its_chunks() {
        let mut file = header(16_000, 1);
        file.extend(pcm(&[0, 16384, -32768, 32767]));
        let mut wav = WavReader::default();
        let samples = wav.push(&file).unwrap();
        assert_eq!(samples, [0.0, 0.5, -1.0, 32767.0 / 32768.0]);
        assert_eq!(wav.samples(), 4);
    }

    #[test]
    fn the_file_can_arrive_a_byte_at_a_time() {
        let mut file = header(16_000, 1);
        file.extend(pcm(&[100, -100, 200]));
        let mut wav = WavReader::default();
        let mut samples = Vec::new();
        for byte in &file {
            samples.extend(wav.push(std::slice::from_ref(byte)).unwrap());
        }
        assert_eq!(samples.len(), 3);
        assert_eq!(samples[1], -100.0 / 32768.0);
        // Half a sample waits for its other byte.
        let mut wav = WavReader::default();
        let cut = file.len() - 3;
        assert_eq!(wav.push(&file[..cut]).unwrap().len(), 1);
        assert_eq!(wav.push(&file[cut..]).unwrap().len(), 2);
    }

    #[test]
    fn a_wrong_format_or_no_wav_is_an_error() {
        let mut wav = WavReader::default();
        assert!(matches!(
            wav.push(&header(48_000, 1)),
            Err(WavError::Format(_))
        ));
        let mut wav = WavReader::default();
        assert!(matches!(
            wav.push(&header(16_000, 2)),
            Err(WavError::Format(_))
        ));
        let mut wav = WavReader::default();
        assert_eq!(wav.push(b"OggS0000000000"), Err(WavError::NotWav));
        // Too short to tell yet.
        let mut wav = WavReader::default();
        assert_eq!(wav.push(b"RIFF"), Ok(Vec::new()));
    }

    #[test]
    fn a_sample_plays_at_the_first_pts_plus_its_index() {
        let t = Timeline::starting_at(4520.06);
        assert_eq!(t.at(0), Some(4520.06));
        assert!((t.at(16_000).unwrap() - 4521.06).abs() < 1e-9);
        assert!((t.at(24_000).unwrap() - 4521.56).abs() < 1e-9);
        assert_eq!(Timeline::default().at(5), None);
    }

    #[test]
    fn audio_pts_ahead_by_a_held_packet_keeps_the_clock() {
        // Measured: audio-pts runs 0 to 1.024 s ahead of the audio in the file.
        let mut t = Timeline::starting_at(4520.06);
        let written = 95_232 * 16; // 95.232 s
        let (ahead, jumped) = t.check(written, 4616.059).unwrap();
        assert!((ahead - 0.767).abs() < 1e-3);
        assert!(!jumped);
        assert_eq!(t.anchors.len(), 1);
    }

    #[test]
    fn a_jump_in_the_audio_sets_a_new_anchor_once_it_holds() {
        let mut t = Timeline::starting_at(100.0);
        // 10 s read, but audio-pts says 60.5 s: the playlist jumped. Not believed at once.
        let (ahead, jumped) = t.check(160_000, 160.5).unwrap();
        assert!((ahead - 50.5).abs() < 1e-9);
        assert!(!jumped);
        assert_eq!(t.at(160_000), Some(110.0));
        // A second later, the same gap: a jump.
        assert!(t.check(176_000, 161.5).unwrap().1);
        assert_eq!(t.at(176_000), Some(161.0));
        // Samples before the jump keep the old clock.
        assert_eq!(t.at(80_000), Some(105.0));
        // A jump back works the same.
        t.check(192_000, 50.0);
        assert!(t.check(192_000, 50.0).unwrap().1);
        assert_eq!(t.at(192_000), Some(49.5));
    }

    #[test]
    fn a_tap_catching_up_is_no_jump() {
        // Out of bounds once (audio-pts ran ahead while seconds were written), then fine.
        let mut t = Timeline::starting_at(100.0);
        assert!(!t.check(160_000, 112.96).unwrap().1);
        assert!(!t.check(176_000, 111.3).unwrap().1);
        // Out once, then out by a different amount: still no jump.
        assert!(!t.check(192_000, 115.0).unwrap().1);
        assert!(!t.check(208_000, 109.0).unwrap().1);
        assert_eq!(t.anchors.len(), 1);
    }

    #[test]
    fn chunks_are_half_a_second_and_never_cross_an_anchor() {
        let mut t = Timeline::starting_at(10.0);
        let mut c = Chunker::default();
        // 0.75 s: one chunk, a quarter second waits.
        let out = c.push(&vec![0.1; 12_000], &t);
        assert_eq!(out.len(), 1);
        assert_eq!(out[0].start_s, 10.0);
        assert_eq!(out[0].samples.len(), 8_000);
        // A jump at sample 12 000 (0.75 s).
        t.check(12_000, 20.5);
        assert!(t.check(12_000, 20.5).unwrap().1);
        let out = c.push(&vec![0.2; 8_000], &t);
        assert_eq!(out.len(), 2);
        assert_eq!((out[0].start_s, out[0].samples.len()), (10.5, 4_000));
        assert_eq!((out[1].start_s, out[1].samples.len()), (20.0, 8_000));
        assert!((out[1].end_s() - 20.5).abs() < 1e-9);
    }

    #[test]
    fn audio_waits_for_the_clock() {
        let mut c = Chunker::default();
        assert!(c.push(&vec![0.0; 9_000], &Timeline::default()).is_empty());
        let out = c.push(&[], &Timeline::starting_at(3.0));
        assert_eq!(out.len(), 1);
        assert_eq!(out[0].start_s, 3.0);
    }

    fn chunk(start_s: f64, seconds: f64) -> PcmChunk {
        PcmChunk {
            start_s,
            samples: vec![0.0; (seconds * 16_000.0) as usize],
        }
    }

    #[test]
    fn a_splice_cuts_the_overlap_of_a_restarted_tap() {
        let mut s = Splice::default();
        assert_eq!(
            s.take(chunk(100.0, 0.5)),
            Some(Spliced::Next(chunk(100.0, 0.5)))
        );
        assert_eq!(
            s.take(chunk(100.5, 0.5)),
            Some(Spliced::Next(chunk(100.5, 0.5)))
        );
        // A restart from 20 s back: all old, then the part after 101.0.
        assert_eq!(s.take(chunk(81.0, 0.5)), None);
        assert_eq!(
            s.take(chunk(100.75, 0.5)),
            Some(Spliced::Next(chunk(101.0, 0.25)))
        );
        // A gap is passed on as it is.
        assert_eq!(
            s.take(chunk(103.0, 0.5)),
            Some(Spliced::Next(chunk(103.0, 0.5)))
        );
    }

    #[test]
    fn audio_from_long_before_is_a_new_clock() {
        let mut s = Splice::default();
        s.take(chunk(5000.0, 0.5));
        assert_eq!(
            s.take(chunk(2.0, 0.5)),
            Some(Spliced::NewClock(chunk(2.0, 0.5)))
        );
        assert_eq!(
            s.take(chunk(2.5, 0.5)),
            Some(Spliced::Next(chunk(2.5, 0.5)))
        );
        s.reset();
        assert_eq!(
            s.take(chunk(1.0, 0.5)),
            Some(Spliced::Next(chunk(1.0, 0.5)))
        );
    }

    #[test]
    fn fake_taps_record_and_stop_on_drop() {
        let taps = FakeTaps::new();
        let seen = Arc::new(Mutex::new(Vec::new()));
        let sink = Arc::clone(&seen);
        let handle = taps
            .start(
                "u",
                TapOptions::default(),
                Box::new(move |e| sink.lock().unwrap().push(e)),
            )
            .unwrap();
        assert_eq!(taps.running(), ["u"]);
        taps.emit(0, TapEvent::Ended);
        drop(handle);
        assert!(taps.running().is_empty());
        assert_eq!(taps.started(), ["u"]);
        assert_eq!(*seen.lock().unwrap(), [TapEvent::Ended]);
        assert_eq!(taps.options().len(), 1);
        assert!(NoTaps
            .start("u", TapOptions::default(), Box::new(|_| {}))
            .is_err());
    }

    #[test]
    fn a_paced_tap_pauses_at_the_lookahead_and_reads_on_5_s_under_it() {
        assert!(!should_pause(0.0, false));
        assert!(!should_pause(29.9, false));
        assert!(should_pause(30.0, false));
        // Paused: stays paused until it is 5 s under.
        assert!(should_pause(29.0, true));
        assert!(should_pause(25.1, true));
        assert!(!should_pause(25.0, true));
        assert!(!should_pause(-3.0, true));
    }

    #[test]
    fn the_playhead_is_shared_and_unknown_until_set() {
        let pace = TapPace::default();
        let tap_side = pace.clone();
        assert_eq!(tap_side.playhead_s(), None);
        pace.set_playhead_ms(62_050);
        assert_eq!(tap_side.playhead_s(), Some(62.05));
        pace.clear();
        assert_eq!(tap_side.playhead_s(), None);
    }
}
