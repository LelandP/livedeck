//! `AudioTap`: a second, headless libmpv in encode mode that writes a stream's audio as 16 kHz mono
//! WAV to a file, and a thread that tails the file and delivers it as [`PcmChunk`]s stamped with
//! the stream's clock (captions-plan.md, phase 2).
//!
//! The clock: the tap runs with `rebase-start-time=no` and starts paused, so its first
//! `audio-pts` is the timestamp of the first sample in the file. Sample `n` plays at that plus
//! `n / 16000`. Once a second the mapping is checked against `audio-pts`, which runs up to one
//! encoder packet (1.024 s) ahead of the file; outside that twice in a row by the same amount is a
//! jump in the stream, and sets a new anchor.
//!
//! The file would grow by 115 MB an hour, so every [`ROTATE_AFTER`] of audio a new session starts
//! on the same playlist with a new file. It begins some seconds back, so it overlaps the old one:
//! once it has audio the old one is closed and the overlap cut off ([`Splice`]).
//!
//! **A video or clip** (`TapOptions::from_ms`, phase 5) starts at a position (mpv's `start`) and
//! keeps mpv's default of rebasing the file's timestamps to 0, as the main player does: the stream
//! clock is then `audio-pts` plus `demuxer-start-time`, the same sum `PlayerHealth::stream_ms`
//! makes. Reading faster than real time would fetch the whole video, so the session is paused
//! whenever it has read `LOOKAHEAD_S` beyond the playhead (`TapPace`), and mpv's cache is kept
//! small. A rotated file starts at the newest audio, not at the start position.

use std::fs::File;
use std::io::Read;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant, SystemTime};

use libmpv2::events::Event;
use libmpv2::{mpv_end_file_reason, Mpv};

use crate::tap::{
    should_pause, AudioTaps, Chunker, PcmChunk, Splice, Spliced, TapEvent, TapHandle, TapOptions,
    TapPace, TapSink, Timeline, WavReader, TAP_RATE,
};

/// How often the tap's thread reads the file and mpv's events.
const TICK: Duration = Duration::from_millis(50);

/// The same for a paced tap, which reads video at up to 100 times real time: the sooner it looks,
/// the less it reads past the lookahead before it is paused.
const PACED_TICK: Duration = Duration::from_millis(10);

/// How often the clock is checked against `audio-pts`.
const CHECK_EVERY: Duration = Duration::from_secs(1);

/// Audio per file before the tap moves to a new one: 10 minutes is 19.2 MB.
pub const ROTATE_AFTER: Duration = Duration::from_secs(600);

/// A rotation that failed is tried again after this long.
const ROTATE_RETRY: Duration = Duration::from_secs(60);

/// A tap file not written for this long belongs to no running tap (a crash left it).
const STALE: Duration = Duration::from_secs(600);

/// The tap files' names: `livedeck-tap-<pid>-<n>.wav`.
const FILE_PREFIX: &str = "livedeck-tap-";

/// Starts real taps.
#[derive(Debug, Clone, Copy, Default)]
pub struct MpvTaps;

impl AudioTaps for MpvTaps {
    fn start(&self, url: &str, options: TapOptions, sink: TapSink) -> Result<TapHandle, String> {
        AudioTap::start_with(url, options, sink).map(TapHandle::new)
    }
}

/// A running tap. Dropping it stops it: mpv is closed and the file deleted on the tap's thread.
pub struct AudioTap {
    stop: Arc<AtomicBool>,
}

impl AudioTap {
    /// Starts tapping `url` (a playlist, or any file mpv plays). Fails only if the thread cannot
    /// start; a playlist that cannot be played is reported as [`TapEvent::Failed`].
    pub fn start(url: &str, sink: TapSink) -> Result<Self, String> {
        Self::start_rotating(url, sink, ROTATE_AFTER)
    }

    /// As [`AudioTap::start`], for a live stream or (with `options`) a video or clip.
    pub fn start_with(url: &str, options: TapOptions, sink: TapSink) -> Result<Self, String> {
        Self::start_rotating_with(url, options, sink, ROTATE_AFTER)
    }

    /// As [`AudioTap::start`], moving to a new file after `rotate_after` of audio (the probe
    /// tries a short one).
    pub fn start_rotating(
        url: &str,
        sink: TapSink,
        rotate_after: Duration,
    ) -> Result<Self, String> {
        Self::start_rotating_with(url, TapOptions::default(), sink, rotate_after)
    }

    /// [`AudioTap::start_with`] and [`AudioTap::start_rotating`] together.
    pub fn start_rotating_with(
        url: &str,
        options: TapOptions,
        sink: TapSink,
        rotate_after: Duration,
    ) -> Result<Self, String> {
        crate::mpv::use_c_numeric_locale();
        let stop = Arc::new(AtomicBool::new(false));
        let stopped = Arc::clone(&stop);
        let url = url.to_owned();
        std::thread::Builder::new()
            .name("audio-tap".into())
            .spawn(move || run(&url, &options, &sink, &stopped, rotate_after))
            .map_err(|e| format!("could not start the audio tap thread: {e}"))?;
        Ok(Self { stop })
    }
}

impl Drop for AudioTap {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::SeqCst);
    }
}

/// Where tap files go: the runtime folder on Linux (a RAM disk, private to the user), else the
/// temp folder.
fn tap_dir() -> PathBuf {
    std::env::var_os("XDG_RUNTIME_DIR")
        .map(PathBuf::from)
        .filter(|d| cfg!(target_os = "linux") && d.is_dir())
        .unwrap_or_else(std::env::temp_dir)
}

/// A new file name for a session.
fn tap_path(dir: &Path) -> PathBuf {
    static NEXT: AtomicU64 = AtomicU64::new(0);
    let n = NEXT.fetch_add(1, Ordering::Relaxed);
    dir.join(format!("{FILE_PREFIX}{}-{n}.wav", std::process::id()))
}

/// Deletes tap files a crash left behind: any not written for [`STALE`].
fn remove_stale(dir: &Path) {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        let name = entry.file_name();
        let Some(name) = name.to_str() else { continue };
        if !name.starts_with(FILE_PREFIX) || !name.ends_with(".wav") {
            continue;
        }
        let old = entry
            .metadata()
            .and_then(|m| m.modified())
            .ok()
            .and_then(|t| SystemTime::now().duration_since(t).ok())
            .is_some_and(|age| age > STALE);
        if old {
            let _ = std::fs::remove_file(entry.path());
        }
    }
}

/// How a session ended.
enum End {
    /// The stream ended.
    Eof,
    Failed(String),
}

/// One encode-mode libmpv and its file.
struct Session {
    mpv: Option<Mpv>,
    path: PathBuf,
    file: Option<File>,
    wav: WavReader,
    timeline: Timeline,
    chunker: Chunker,
    /// When the clock was last checked against `audio-pts`.
    checked: Instant,
    /// The biggest gap between `audio-pts` and the mapped time seen, for the log.
    most_ahead: f64,
    /// A video or clip: mpv rebases its timestamps to 0, and the stream clock is `audio-pts`
    /// plus `demuxer-start-time`. A live stream is read with `rebase-start-time=no` instead.
    rebased: bool,
    /// Paused by the pace (it has read the lookahead), not by the anchor.
    paced: bool,
    /// Where in the video it started, in the player's own position: the playhead is taken to be
    /// there while none is known.
    from_s: Option<f64>,
}

/// What a video's tap lets mpv read ahead, in seconds.
const READAHEAD_S: &str = "5";

/// Seconds as mpv's time options take them.
fn seconds_arg(s: f64) -> String {
    format!("{:.3}", s.max(0.0))
}

impl Session {
    /// A live stream (`from_s` `None`), or a video or clip from `from_s` seconds in.
    fn open(url: &str, path: PathBuf, from_s: Option<f64>) -> Result<Self, String> {
        let out = path.to_str().ok_or("the tap path is not UTF-8")?.to_owned();
        let mpv = Mpv::with_initializer(|init| {
            init.set_option("o", out.as_str())?;
            init.set_option("of", "wav")?;
            init.set_option("oac", "pcm_s16le")?;
            // Write each packet at once, not in 256 KiB blocks (8 s of audio).
            init.set_option("ofopts", "fflags=+flush_packets")?;
            init.set_option("af", "lavfi=[aresample=16000,aformat=channel_layouts=mono]")?;
            init.set_option("vid", "no")?;
            init.set_option("sid", "no")?;
            match from_s {
                // The stream's own timestamps, as `PlayerHealth::stream_ms` reports them.
                None => init.set_option("rebase-start-time", "no")?,
                Some(from) => {
                    init.set_option("start", seconds_arg(from).as_str())?;
                    // A small cache: while the tap is paused it would otherwise read on to the
                    // end of the video, and let go it would pour out as one burst.
                    init.set_option("cache-secs", READAHEAD_S)?;
                    init.set_option("demuxer-readahead-secs", READAHEAD_S)?;
                }
            }
            // Paused until the first `audio-pts` is read, so it is sample 0's.
            init.set_option("pause", "yes")?;
            // Streamlink already resolved the playlist. Only exists in builds with Lua.
            let _ = init.set_option("ytdl", false);
            Ok(())
        })
        .map_err(|e| format!("libmpv failed to start for the audio tap: {e}"))?;
        mpv.command("loadfile", &[url, "replace"])
            .map_err(|e| format!("the audio tap could not load the stream: {e}"))?;
        Ok(Self {
            mpv: Some(mpv),
            path,
            file: None,
            wav: WavReader::default(),
            timeline: Timeline::default(),
            chunker: Chunker::default(),
            checked: Instant::now(),
            most_ahead: 0.0,
            rebased: from_s.is_some(),
            paced: false,
            from_s,
        })
    }

    fn mpv(&self) -> &Mpv {
        self.mpv
            .as_ref()
            .expect("mpv lives until the session is dropped")
    }

    /// Where a new session of the same video picks up: the end of the audio read, 2 s back (it
    /// overlaps, and the splice cuts it), in the player's own position. `None` for a live stream.
    fn resume_at(&self) -> Option<f64> {
        if !self.rebased {
            return None;
        }
        let end = self.timeline.at(self.wav.samples())?;
        Some((end - self.start_time()? - 2.0).max(0.0))
    }

    /// The video's start time: what mpv took off its timestamps.
    fn start_time(&self) -> Option<f64> {
        self.mpv()
            .get_property::<f64>("demuxer-start-time")
            .ok()
            .filter(|s| s.is_finite())
    }

    /// Pauses the session while it has read the lookahead beyond the playhead, and lets it go
    /// again 5 s under it. Not before the clock is set.
    fn pace(&mut self, pace: &TapPace) {
        let Some(end) = self.timeline.at(self.wav.samples()) else {
            return;
        };
        // No report yet (a new tap, after a seek): the video is where the tap started.
        let playhead = pace
            .playhead_s()
            .or_else(|| Some(self.from_s? + self.start_time()?));
        let want = playhead.is_some_and(|p| should_pause(end - p, self.paced));
        if want != self.paced {
            match self.mpv().set_property("pause", want) {
                Ok(()) => {
                    self.paced = want;
                    let cached = self
                        .mpv()
                        .get_property::<f64>("demuxer-cache-duration")
                        .ok();
                    tracing::debug!(want, end, ?cached, "audio tap paced");
                }
                Err(e) => tracing::warn!(%e, "could not pace the audio tap"),
            }
        }
    }

    /// mpv's events since the last call. `Some` once the session has ended.
    fn events(&self) -> Option<End> {
        while let Some(event) = self.mpv().wait_event(0.0) {
            match event {
                Ok(Event::EndFile(reason)) if reason == mpv_end_file_reason::Eof => {
                    return Some(End::Eof)
                }
                Ok(Event::EndFile(_) | Event::Shutdown) => {
                    return Some(End::Failed("the audio tap stopped".into()))
                }
                Ok(_) => {}
                Err(e) => return Some(End::Failed(format!("the audio tap failed: {e}"))),
            }
        }
        None
    }

    /// `audio-pts` on the stream's clock, if mpv has one (and, for a video, its start time).
    fn audio_pts(&self) -> Option<f64> {
        let pts = self
            .mpv()
            .get_property::<f64>("audio-pts")
            .ok()
            .filter(|p| p.is_finite())?;
        if self.rebased {
            Some(pts + self.start_time()?)
        } else {
            Some(pts)
        }
    }

    /// Sets the clock once the paused tap has its first audio, and lets it run.
    fn anchor(&mut self) {
        if self.timeline.is_set() {
            return;
        }
        let Some(pts) = self.audio_pts() else { return };
        // Paused before anything was encoded, so `pts` is sample 0's. Anything already read is
        // counted back (not seen in practice).
        let read = self.wav.samples() as f64 / f64::from(TAP_RATE);
        self.timeline = Timeline::starting_at(pts - read);
        tracing::debug!(pts, "audio tap clock set");
        if let Err(e) = self.mpv().set_property("pause", false) {
            tracing::warn!(%e, "could not start the audio tap");
        }
    }

    /// The file's new bytes as chunks.
    fn read(&mut self) -> Result<Vec<PcmChunk>, String> {
        if self.file.is_none() {
            // mpv makes the file when the stream opens.
            match File::open(&self.path) {
                Ok(f) => self.file = Some(f),
                Err(_) => return Ok(Vec::new()),
            }
        }
        let file = self.file.as_mut().expect("opened above");
        let mut bytes = Vec::new();
        file.read_to_end(&mut bytes)
            .map_err(|e| format!("could not read the audio tap file: {e}"))?;
        let samples = self.wav.push(&bytes).map_err(|e| e.to_string())?;
        Ok(self.chunker.push(&samples, &self.timeline))
    }

    /// `audio-pts` if a clock check is due (once a second). Read before the file, so the file
    /// holds at least what it had then.
    fn check_due(&mut self) -> Option<f64> {
        if self.checked.elapsed() < CHECK_EVERY || !self.timeline.is_set() {
            return None;
        }
        self.checked = Instant::now();
        self.audio_pts()
    }

    /// Checks the sample clock against `now`, the `audio-pts` read before the file: the audio
    /// jumped if they keep disagreeing by more than the encoder holds back.
    fn check(&mut self, now: f64) {
        let read = self.wav.samples();
        let Some(mapped) = self.timeline.at(read) else {
            return;
        };
        let Some((ahead, jumped)) = self.timeline.check(read, now) else {
            return;
        };
        if jumped {
            tracing::warn!(ahead, "the tap's audio jumped: new clock anchor");
        } else {
            self.most_ahead = self.most_ahead.max(ahead);
            tracing::debug!(
                audio_pts = now,
                mapped,
                ahead,
                "audio tap clock checked (audio-pts vs first pts + samples / 16000)"
            );
        }
    }

    /// Seconds of audio read so far.
    fn seconds(&self) -> f64 {
        self.wav.samples() as f64 / f64::from(TAP_RATE)
    }
}

impl Drop for Session {
    fn drop(&mut self) {
        self.file = None;
        // Closing mpv finishes and closes the file, so it can be deleted (Windows refuses to
        // delete an open file).
        self.mpv = None;
        if let Err(e) = std::fs::remove_file(&self.path) {
            if e.kind() != std::io::ErrorKind::NotFound {
                tracing::warn!(%e, path = %self.path.display(), "could not delete the tap file");
            }
        }
        tracing::debug!(most_ahead = self.most_ahead, "audio tap session closed");
    }
}

/// One step of a session: its chunks, and whether it ended.
fn step(session: &mut Session, pace: Option<&TapPace>) -> (Vec<PcmChunk>, Option<End>) {
    let end = session.events();
    session.anchor();
    let now = session.check_due();
    let chunks = match session.read() {
        Ok(chunks) => chunks,
        Err(e) => return (Vec::new(), Some(End::Failed(e))),
    };
    if let Some(now) = now {
        session.check(now);
    }
    if let Some(pace) = pace {
        session.pace(pace);
    }
    (chunks, end)
}

/// The tap's thread: runs sessions until stopped or the stream ends.
fn run(url: &str, options: &TapOptions, sink: &TapSink, stop: &AtomicBool, rotate_after: Duration) {
    let dir = tap_dir();
    remove_stale(&dir);
    let deliver = |event: TapEvent| {
        if !stop.load(Ordering::SeqCst) {
            sink(event);
        }
    };
    let from_s = options.from_ms.map(|ms| ms as f64 / 1000.0);
    let pace = options.pace.as_ref();
    let mut current = match Session::open(url, tap_path(&dir), from_s) {
        Ok(s) => s,
        Err(e) => {
            tracing::warn!(%e, "audio tap not started");
            deliver(TapEvent::Failed(e));
            return;
        }
    };
    let mut splice = Splice::default();
    let pass = |chunks: Vec<PcmChunk>, splice: &mut Splice| {
        for chunk in chunks {
            if let Some(Spliced::Next(c) | Spliced::NewClock(c)) = splice.take(chunk) {
                deliver(TapEvent::Audio(c));
            }
        }
    };
    // The next session while rotating, and when to try rotating again after a failure.
    let mut next: Option<Session> = None;
    let mut rotate_at = rotate_after.as_secs_f64();
    while !stop.load(Ordering::SeqCst) {
        std::thread::sleep(if pace.is_some() { PACED_TICK } else { TICK });
        let (chunks, end) = step(&mut current, pace);
        pass(chunks, &mut splice);
        match end {
            None => {}
            Some(End::Eof) => {
                tracing::info!("the tapped stream ended");
                deliver(TapEvent::Ended);
                return;
            }
            Some(End::Failed(e)) => {
                tracing::warn!(%e, "audio tap failed");
                deliver(TapEvent::Failed(e));
                return;
            }
        }

        if next.is_none() && current.seconds() >= rotate_at {
            // A video's next file starts where this one has got to.
            let resume = current.resume_at().or(from_s.filter(|_| current.rebased));
            match Session::open(url, tap_path(&dir), resume) {
                Ok(s) => next = Some(s),
                Err(e) => {
                    tracing::warn!(%e, "audio tap could not move to a new file");
                    rotate_at = current.seconds() + ROTATE_RETRY.as_secs_f64();
                }
            }
        }
        if let Some(new) = next.as_mut() {
            // Paced too: the new file starts near where the old one is, so it has read the
            // lookahead already and stops after its first chunk, which is all it needs to take over.
            let (chunks, end) = step(new, pace);
            if let Some(end) = end {
                let why = match end {
                    End::Eof => "the stream ended".to_owned(),
                    End::Failed(e) => e,
                };
                tracing::warn!(why, "the audio tap's new file failed: keeping the old one");
                next = None;
                rotate_at = current.seconds() + ROTATE_RETRY.as_secs_f64();
            } else if !chunks.is_empty() {
                // The new file has audio: it takes over, the overlap cut off by the splice.
                tracing::debug!(after_s = current.seconds(), "audio tap moved to a new file");
                current = next.take().expect("checked above");
                rotate_at = rotate_after.as_secs_f64();
                pass(chunks, &mut splice);
            }
        }
    }
    tracing::debug!("audio tap stopped");
}
