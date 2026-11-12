//! `MpvPlayer`: libmpv behind the [`Player`] trait.
//!
//! Threading follows the rules in mpv's `render.h`:
//! - The Slint UI thread is the mpv *render thread*. It only calls `mpv_render_*` functions, and
//!   only from inside the rendering notifier (see `output.rs`).
//! - The control thread made here makes every other libmpv call, so the UI thread never waits on
//!   the mpv core. [`MpvPlayer`]'s methods only queue a command for it.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{channel, Receiver, RecvTimeoutError, Sender};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use libmpv2::events::Event;
use libmpv2::{mpv_end_file_reason, Mpv};

use crate::output::VideoOutput;
use crate::recent::RecentLog;
use crate::{
    Latency, LoadOptions, Player, PlayerEvent, PlayerHealth, PlayerOption, PlayerStats, Zoom,
};

/// How often the control thread looks for mpv events when no command arrives.
const POLL: Duration = Duration::from_millis(50);

/// How often stats are read while they are on.
const STATS_EVERY: Duration = Duration::from_secs(1);

/// How often health is reported while a stream plays.
const HEALTH_EVERY: Duration = Duration::from_secs(1);

/// How long a load waits for the player's render context (`VideoOutput`'s `ready`). It is made at
/// the next frame the window draws, so this only runs out if nothing is drawn: then the stream
/// loads anyway, without a picture.
const RENDER_WAIT: Duration = Duration::from_secs(2);

/// Buffered less than this, a playing stream has run dry. With low latency the buffer reads 0.00
/// while the picture is frozen; anything that still plays had at least 0.1 s (live Twitch, slowed
/// to half its bitrate, 2026-11-07).
const DRY_BUFFER_MS: u32 = 50;

/// What a latency means in mpv options: `(option, low, normal)`. The low values are mpv's own
/// `low-latency` profile (set once at start), the normal ones mpv's defaults plus a longer
/// read-ahead and a short wait to refill the cache after a stall. Applied before each load, as
/// most of them are only read when a file opens.
const LATENCY_OPTIONS: &[(&str, &str, &str)] = &[
    ("audio-buffer", "0", "0.2"),
    ("vd-lavc-threads", "1", "0"),
    ("cache-pause", "no", "yes"),
    ("cache-pause-wait", "1", "3"),
    ("demuxer-lavf-o", "fflags=+nobuffer", ""),
    ("demuxer-lavf-probe-info", "nostreams", "auto"),
    ("demuxer-lavf-analyzeduration", "0.1", "0"),
    ("demuxer-readahead-secs", "1", "10"),
    ("video-latency-hacks", "yes", "no"),
    ("stream-buffer-size", "4k", "128k"),
];

enum Control {
    Load(String, LoadOptions),
    Pause(bool),
    /// Jump to this many milliseconds from the start of the video.
    Seek(u64),
    Stop,
    Option(PlayerOption),
    Stats(bool),
    /// Fade the volume to silence over this long.
    FadeOut(Duration),
    /// End a fade: full volume again.
    RestoreVolume,
}

/// mpv's normal volume (its `volume` property is a percentage).
const FULL_VOLUME: f64 = 100.0;

/// The volume `elapsed` into a fade of `length`: a straight line from full to silent.
fn fade_volume(elapsed: Duration, length: Duration) -> f64 {
    if length.is_zero() {
        return 0.0;
    }
    let done = (elapsed.as_secs_f64() / length.as_secs_f64()).clamp(0.0, 1.0);
    FULL_VOLUME * (1.0 - done)
}

fn set_volume(mpv: &Mpv, volume: f64) {
    if let Err(e) = mpv.set_property("volume", volume) {
        tracing::warn!(%e, volume, "could not change the volume");
    }
}

pub struct MpvPlayer {
    tx: Mutex<Sender<Control>>,
    /// Lives as long as the player. Its `VideoOutput` holds a weak reference, so the renderer
    /// frees the render context once the player is dropped (see `VideoOutputs`).
    _alive: Arc<()>,
}

impl MpvPlayer {
    /// Starts mpv and its control thread. `on_event` runs on that thread. Fails if libmpv cannot
    /// be loaded or initialised. The [`VideoOutput`] goes to `VideoOutputs::insert`.
    pub fn new(
        on_event: impl Fn(PlayerEvent) + Send + 'static,
    ) -> Result<(Self, VideoOutput), String> {
        use_c_numeric_locale();
        let mpv = Mpv::with_initializer(|init| {
            // Required: without these there is no video at all.
            init.set_option("vo", "libmpv")
                .inspect_err(|e| tracing::error!(%e, "mpv rejected vo=libmpv"))?;
            // Keep the core alive between streams (and after one ends).
            init.set_option("idle", "yes")
                .inspect_err(|e| tracing::error!(%e, "mpv rejected idle=yes"))?;

            // Everything else is best effort. Which options exist depends on how libmpv was built
            // (the Flatpak's has no Lua, so no `ytdl`), and a missing one must not stop the video.
            let optional = |name: &str, result: libmpv2::Result<()>| {
                if let Err(e) = result {
                    tracing::warn!(option = name, %e, "mpv option not applied");
                }
            };
            // Live streaming: small buffers, no waiting to refill the cache.
            optional("profile", init.set_option("profile", "low-latency"));
            // Hardware decode where mpv considers it safe (VAAPI on the Deck, D3D11VA on Windows).
            optional("hwdec", init.set_option("hwdec", "auto-safe"));
            // Without this, mpv_render_context_render() blocks until the frame's display time,
            // which would stall the Slint UI thread.
            optional(
                "video-timing-offset",
                init.set_option("video-timing-offset", 0.0),
            );
            // Streamlink already resolved the playlist. Only exists in builds with Lua.
            optional("ytdl", init.set_option("ytdl", false));
            if let Ok(path) = std::env::var("HTPC_MPV_LOG") {
                optional("log-file", init.set_option("log-file", path.as_str()));
            }
            Ok(())
        })
        .map_err(|e| format!("libmpv failed to start: {}", describe(&e)))?;
        request_log_messages(&mpv, c"warn");
        let mpv = Arc::new(mpv);

        let (tx, rx) = channel();
        let core = Arc::clone(&mpv);
        let ready = Arc::new(AtomicBool::new(false));
        let can_show = Arc::clone(&ready);
        std::thread::Builder::new()
            .name("mpv-control".into())
            .spawn(move || control_loop(&core, &rx, &can_show, &on_event))
            .map_err(|e| format!("could not start the mpv thread: {e}"))?;

        let alive = Arc::new(());
        let output = VideoOutput::new(mpv, Arc::downgrade(&alive), ready);
        Ok((
            Self {
                tx: Mutex::new(tx),
                _alive: alive,
            },
            output,
        ))
    }

    fn send(&self, control: Control) {
        let tx = self.tx.lock().expect("mpv sender lock");
        // Fails only once the control thread is gone (mpv shut down), when nothing can play.
        if tx.send(control).is_err() {
            tracing::debug!("mpv control thread is gone");
        }
    }
}

impl Player for MpvPlayer {
    fn load(&self, url: &str, options: LoadOptions) {
        self.send(Control::Load(url.to_owned(), options));
    }

    fn set_paused(&self, paused: bool) {
        self.send(Control::Pause(paused));
    }

    fn seek(&self, to_ms: u64) {
        self.send(Control::Seek(to_ms));
    }

    fn stop(&self) {
        self.send(Control::Stop);
    }

    fn set_option(&self, option: PlayerOption) {
        self.send(Control::Option(option));
    }

    fn set_stats(&self, on: bool) {
        self.send(Control::Stats(on));
    }

    fn fade_out(&self, duration: Duration) {
        self.send(Control::FadeOut(duration));
    }

    fn restore_volume(&self) {
        self.send(Control::RestoreVolume);
    }
}

/// Writes `source` (anything mpv plays, such as a `lavfi` graph with a duration) to the Matroska
/// file `path`, as fast as libmpv can encode it. For `--mock`'s videos: a generated `lavfi`
/// picture cannot seek backwards and has no known length, but a file made from it can and has.
/// Blocks until the file is written. A small picture at a low rate encodes at hundreds of times
/// real time.
pub fn encode_file(source: &str, path: &std::path::Path) -> Result<(), String> {
    use_c_numeric_locale();
    let out = path.to_str().ok_or("the path is not UTF-8")?;
    let mpv = Mpv::with_initializer(|init| {
        init.set_option("o", out)?;
        init.set_option("of", "matroska")?;
        init.set_option("ovc", "libx264")?;
        // A keyframe a second, so a seek lands quickly anywhere.
        init.set_option("ovcopts", "preset=ultrafast,crf=32,g=15")?;
        init.set_option("oac", "aac")?;
        init.set_option("ytdl", false)?;
        Ok(())
    })
    .map_err(|e| format!("libmpv failed to start: {}", describe(&e)))?;
    mpv.command("loadfile", &[source, "replace"])
        .map_err(|e| format!("loadfile failed: {}", describe(&e)))?;
    loop {
        match mpv.wait_event(1.0) {
            Some(Ok(Event::EndFile(reason))) if reason == mpv_end_file_reason::Eof => break,
            Some(Ok(Event::EndFile(_) | Event::Shutdown)) => {
                return Err("the encoder stopped before the end".into())
            }
            Some(Err(e)) => return Err(describe(&e)),
            _ => {}
        }
    }
    // Dropping the handle finishes the file.
    drop(mpv);
    Ok(())
}

/// Sets the buffering options of `latency` for the next load. Best effort: an option this mpv
/// build lacks is logged and skipped.
fn apply_latency(mpv: &Mpv, latency: Latency) {
    for (name, low, normal) in LATENCY_OPTIONS {
        let value = match latency {
            Latency::Low => *low,
            Latency::Normal => *normal,
        };
        if let Err(e) = mpv.set_property(name, value) {
            tracing::warn!(option = name, value, %e, "mpv latency option not applied");
        }
    }
}

/// Applies an option that changes while playing. mpv keeps it for later streams as well.
fn apply_option(mpv: &Mpv, option: PlayerOption) {
    let result = match option {
        PlayerOption::Hwdec(on) => mpv.set_property("hwdec", if on { "auto-safe" } else { "no" }),
        // `panscan` 1.0 crops the picture until it fills the window, 0.0 shows all of it.
        PlayerOption::Zoom(zoom) => mpv.set_property(
            "panscan",
            match zoom {
                Zoom::Fit => 0.0,
                Zoom::Fill => 1.0,
            },
        ),
        // `vid=no` leaves the video track unselected: nothing is decoded or rendered. As an
        // option it also holds for the next `loadfile`.
        PlayerOption::Video(on) => mpv.set_property("vid", if on { "auto" } else { "no" }),
        // `mute` moves the sound between tiles at once and without dropped frames (phase 0 of
        // plan.md). The audio is still decoded; `aid=no` would save that but is untested.
        PlayerOption::Muted(muted) => mpv.set_property("mute", muted),
        // mpv keeps the pitch (`audio-pitch-correction` is on by default).
        PlayerOption::Speed(milli) => mpv.set_property("speed", speed(milli)),
    };
    if let Err(e) = result {
        tracing::warn!(?option, %e, "mpv option not changed");
    }
}

/// mpv's `speed` for [`PlayerOption::Speed`]'s thousandths. mpv accepts 0.01 to 100.
fn speed(milli: u32) -> f64 {
    (f64::from(milli) / 1000.0).clamp(0.01, 100.0)
}

/// Milliseconds as the seconds mpv's time options and commands take (`start=`, `seek`), with
/// the fraction kept.
fn seconds(ms: u64) -> String {
    format!("{}.{:03}", ms / 1000, ms % 1000)
}

/// Seconds from mpv to whole milliseconds.
fn millis(seconds: f64) -> u64 {
    (seconds * 1000.0).round() as u64
}

/// Sets where the next file starts: `start_ms` for a video, or mpv's default (the beginning, or
/// the live edge) for a live stream. Set as the `start` option rather than `loadfile`'s per-file
/// options, whose place among `loadfile`'s arguments changed in mpv 0.38, so it works with older
/// libmpv builds too.
fn apply_start(mpv: &Mpv, start_ms: Option<u64>) {
    let value = start_ms.map_or_else(|| "none".to_owned(), seconds);
    if let Err(e) = mpv.set_property("start", value.as_str()) {
        tracing::warn!(%e, value, "mpv start position not applied");
    }
}

/// A numeric property, whether mpv keeps it as a whole number or a fraction. `None` while it
/// has no value (nothing loaded yet).
fn number(mpv: &Mpv, name: &str) -> Option<f64> {
    mpv.get_property::<f64>(name)
        .ok()
        .or_else(|| mpv.get_property::<i64>(name).ok().map(|v| v as f64))
        .filter(|v| v.is_finite() && *v >= 0.0)
}

/// A text property, `None` if it is empty or "no" (mpv's word for "none" in `hwdec-current`).
fn text(mpv: &Mpv, name: &str) -> Option<String> {
    mpv.get_property::<String>(name)
        .ok()
        .filter(|v| !v.is_empty() && v != "no")
}

/// Reads the stats box's numbers from mpv's properties.
fn read_stats(mpv: &Mpv) -> PlayerStats {
    let whole = |name: &str| number(mpv, name).map(|v| v.round() as u64);
    let small = |v: f64| u32::try_from(v.round() as u64).ok();
    PlayerStats {
        width: number(mpv, "video-params/w").and_then(small),
        height: number(mpv, "video-params/h").and_then(small),
        fps_milli: number(mpv, "estimated-vf-fps")
            .or_else(|| number(mpv, "container-fps"))
            .and_then(|fps| small(fps * 1000.0)),
        // `video-format` is the short codec name ("h264"), `video-codec` the long one.
        codec: text(mpv, "video-format").or_else(|| text(mpv, "video-codec")),
        hwdec: text(mpv, "hwdec-current"),
        video_bitrate: whole("video-bitrate"),
        audio_bitrate: whole("audio-bitrate"),
        audio_codec: text(mpv, "audio-codec-name"),
        buffer_ms: number(mpv, "demuxer-cache-duration").and_then(|s| small(s * 1000.0)),
        dropped_frames: whole("frame-drop-count"),
        decoder_dropped_frames: whole("decoder-frame-drop-count"),
    }
}

/// Reads the health report. `None` while paused by the user: a paused stream only fills its
/// buffer, which says nothing about the network. For a video (`video`), also its position and
/// length.
///
/// A video is stalled only when mpv waits for its cache: it always has `cache-pause` on (normal
/// latency), and its buffer runs dry at its end anyway, which is no stall. While it seeks it is
/// never stalled: the buffer refills from the new spot, which says nothing about the network.
fn read_health(mpv: &Mpv, video: bool) -> Option<PlayerHealth> {
    if mpv.get_property::<bool>("pause").unwrap_or(false) {
        return None;
    }
    let buffer_ms = number(mpv, "demuxer-cache-duration").map_or(0, |s| {
        u32::try_from((s * 1000.0).round() as u64).unwrap_or(u32::MAX)
    });
    let waiting = mpv
        .get_property::<bool>("paused-for-cache")
        .unwrap_or(false);
    let stalled = if video {
        waiting && !mpv.get_property::<bool>("seeking").unwrap_or(false)
    } else {
        waiting || buffer_ms < DRY_BUFFER_MS
    };
    let at = |name| video.then(|| number(mpv, name).map(millis)).flatten();
    Some(PlayerHealth {
        stalled,
        buffer_ms,
        pos_ms: at("time-pos"),
        length_ms: at("duration"),
    })
}

/// libmpv refuses to start (`mpv_create` returns NULL) unless `LC_NUMERIC` is "C". Rust never
/// changes the locale, but a library in the process may have set it from the environment (a
/// Flatpak or SteamOS session has `LANG` set). Only number formatting changes: text stays UTF-8.
#[cfg(target_os = "linux")]
fn use_c_numeric_locale() {
    use std::ffi::{c_char, c_int};
    // glibc and musl both use 1 for LC_NUMERIC.
    const LC_NUMERIC: c_int = 1;
    extern "C" {
        fn setlocale(category: c_int, locale: *const c_char) -> *mut c_char;
    }
    // SAFETY: a valid category and a NUL-terminated string. Runs once, before mpv starts, and
    // nothing else in the app changes the locale, so no other `setlocale` call races with it.
    unsafe {
        setlocale(LC_NUMERIC, c"C".as_ptr());
    }
}

#[cfg(not(target_os = "linux"))]
fn use_c_numeric_locale() {}

/// Asks mpv to send its log messages at `min_level` and above as events, so a failure can show
/// what mpv said (see `RecentLog`). libmpv2 has no wrapper for this call.
fn request_log_messages(mpv: &Mpv, min_level: &std::ffi::CStr) {
    use std::ffi::{c_char, c_int, c_void};
    extern "C" {
        fn mpv_request_log_messages(ctx: *mut c_void, min_level: *const c_char) -> c_int;
    }
    // SAFETY: `ctx` is the live handle `mpv` owns, and `min_level` is NUL-terminated. mpv copies
    // the string.
    let err = unsafe { mpv_request_log_messages(mpv.ctx.as_ptr().cast(), min_level.as_ptr()) };
    if err < 0 {
        tracing::warn!(err, "mpv log messages unavailable");
    }
}

/// An mpv error in words, for the player's error screen: "Raw(-13)" tells nobody anything. The
/// code stays in brackets for bug reports.
fn describe(e: &libmpv2::Error) -> String {
    use libmpv2::mpv_error as code;
    let libmpv2::Error::Raw(raw) = e else {
        return e.to_string();
    };
    let what = match *raw {
        code::LoadingFailed => "the stream could not be opened",
        code::NothingToPlay => "the stream has no audio or video",
        code::UnknownFormat => "the stream's format is not supported",
        code::AoInitFailed => "no audio output could be opened",
        code::VoInitFailed => "no video output could be opened",
        code::OptionNotFound => "mpv was given an option this build does not have",
        code::Unsupported | code::NotImplemented => "this mpv build does not support it",
        _ => return format!("mpv error {raw}"),
    };
    format!("{what} (mpv error {raw})")
}

fn control_loop(
    mpv: &Mpv,
    rx: &Receiver<Control>,
    can_show: &AtomicBool,
    on_event: &impl Fn(PlayerEvent),
) {
    // True from `Load` until the first frame, so `Playing` is reported once per stream.
    let mut starting = false;
    // A load waiting for the render context, and since when.
    let mut waiting: Option<(String, LoadOptions, Instant)> = None;
    // What mpv warned about since the current stream started, shown with a failure.
    let mut recent = RecentLog::default();
    // When the next stats report is due, while stats are on.
    let mut stats_due: Option<Instant> = None;
    // A volume fade in progress: when it started and how long it takes.
    let mut fade: Option<(Instant, Duration)> = None;
    // When the next health report is due: from the first frame of a stream until it stops.
    let mut health_due: Option<Instant> = None;
    // The loaded file is a video or clip, so health carries its position and length.
    let mut video = false;
    loop {
        let mut load = None;
        match rx.recv_timeout(POLL) {
            // Held until the render context exists (see `RENDER_WAIT`). A newer load replaces it.
            Ok(Control::Load(url, options)) => waiting = Some((url, options, Instant::now())),
            Ok(Control::Pause(paused)) => {
                if let Err(e) = mpv.set_property("pause", paused) {
                    tracing::warn!(%e, "could not change the pause state");
                }
            }
            // Precise, as mpv's `hr-seek=default` makes absolute seeks. A paused video stays
            // paused, showing the new spot.
            Ok(Control::Seek(to_ms)) => {
                if let Err(e) = mpv.command("seek", &[seconds(to_ms).as_str(), "absolute"]) {
                    tracing::warn!(%e, to_ms, "could not seek");
                }
            }
            Ok(Control::Stop) => {
                waiting = None;
                starting = false;
                health_due = None;
                if let Err(e) = mpv.command("stop", &[]) {
                    tracing::warn!(%e, "could not stop mpv");
                }
            }
            Ok(Control::Option(option)) => apply_option(mpv, option),
            // The first report comes at once, so the box does not sit empty for a second.
            Ok(Control::Stats(on)) => stats_due = on.then(Instant::now),
            Ok(Control::FadeOut(length)) => fade = Some((Instant::now(), length)),
            Ok(Control::RestoreVolume) => {
                fade = None;
                set_volume(mpv, FULL_VOLUME);
            }
            Err(RecvTimeoutError::Timeout) => {}
            // The player was dropped: the app is quitting.
            Err(RecvTimeoutError::Disconnected) => break,
        }

        if waiting.as_ref().is_some_and(|(_, _, since)| {
            can_show.load(Ordering::SeqCst) || since.elapsed() >= RENDER_WAIT
        }) {
            load = waiting.take();
        }
        if let Some((url, options, since)) = load {
            if !can_show.load(Ordering::SeqCst) {
                tracing::warn!(waited = ?since.elapsed(), "loading without a render context");
            }
            recent.clear();
            health_due = None;
            video = options.is_video();
            // Never the low-latency buffers for a video (`effective_latency`).
            apply_latency(mpv, options.effective_latency());
            apply_start(mpv, options.start_ms);
            // A live stream always plays at normal speed. A video keeps the speed set for it.
            if !video {
                apply_option(mpv, PlayerOption::Speed(crate::NORMAL_SPEED));
            }
            let _ = mpv.set_property("pause", false);
            // A fade only quietens the stream it ran on.
            fade = None;
            set_volume(mpv, FULL_VOLUME);
            match mpv.command("loadfile", &[url.as_str(), "replace"]) {
                Ok(()) => starting = true,
                Err(e) => on_event(PlayerEvent::Failed(
                    recent.report(&format!("loadfile failed: {}", describe(&e))),
                )),
            }
        }

        while let Some(event) = mpv.wait_event(0.0) {
            match event {
                Ok(Event::PlaybackRestart) if starting => {
                    starting = false;
                    health_due = Some(Instant::now() + HEALTH_EVERY);
                    on_event(PlayerEvent::Playing);
                }
                Ok(Event::EndFile(reason)) if reason == mpv_end_file_reason::Eof => {
                    starting = false;
                    health_due = None;
                    on_event(PlayerEvent::Ended);
                }
                // `Stop` and `Redirect` are our own doing (a new load or `stop`).
                Ok(Event::EndFile(_)) => {}
                Ok(Event::Shutdown) => return,
                Ok(Event::LogMessage {
                    prefix,
                    level,
                    text,
                    ..
                }) => {
                    tracing::debug!(target: "mpv", "[{prefix}] {level}: {}", text.trim_end());
                    recent.push(prefix, level, text);
                }
                Ok(_) => {}
                // An end-of-file with an error code arrives as an `Err`.
                Err(e) => {
                    starting = false;
                    health_due = None;
                    tracing::warn!(%e, "mpv could not play the stream");
                    on_event(PlayerEvent::Failed(recent.report(&describe(&e))));
                }
            }
        }

        // One step per pass (about every 50 ms), so the loop never waits on the fade.
        if let Some((start, length)) = fade {
            let elapsed = start.elapsed();
            set_volume(mpv, fade_volume(elapsed, length));
            if elapsed >= length {
                // Stays silent until the next load or a restore.
                fade = None;
            }
        }

        if let Some(due) = stats_due {
            let now = Instant::now();
            if now >= due {
                on_event(PlayerEvent::Stats(read_stats(mpv)));
                stats_due = Some(now + STATS_EVERY);
            }
        }

        if let Some(due) = health_due {
            let now = Instant::now();
            if now >= due {
                if let Some(health) = read_health(mpv, video) {
                    on_event(PlayerEvent::Health(health));
                }
                health_due = Some(now + HEALTH_EVERY);
            }
        }
    }
    tracing::debug!("mpv control thread finished");
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_fade_goes_from_full_to_silent() {
        let five = Duration::from_secs(5);
        assert_eq!(fade_volume(Duration::ZERO, five), 100.0);
        assert_eq!(fade_volume(Duration::from_millis(2500), five), 50.0);
        assert_eq!(fade_volume(five, five), 0.0);
        assert_eq!(fade_volume(Duration::from_secs(9), five), 0.0);
        assert_eq!(fade_volume(Duration::ZERO, Duration::ZERO), 0.0);
    }

    #[test]
    fn times_and_speeds_in_mpvs_units() {
        assert_eq!(seconds(0), "0.000");
        assert_eq!(seconds(4_360_250), "4360.250");
        assert_eq!(seconds(7), "0.007");
        assert_eq!(millis(4360.2504), 4_360_250);
        assert_eq!(speed(1500), 1.5);
        assert_eq!(speed(750), 0.75);
        assert_eq!(speed(0), 0.01);
    }
}
