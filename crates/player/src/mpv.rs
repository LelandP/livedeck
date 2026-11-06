//! `MpvPlayer`: libmpv behind the [`Player`] trait.
//!
//! Threading follows the rules in mpv's `render.h`:
//! - The Slint UI thread is the mpv *render thread*. It only calls `mpv_render_*` functions, and
//!   only from inside the rendering notifier (see `output.rs`).
//! - The control thread made here makes every other libmpv call, so the UI thread never waits on
//!   the mpv core. [`MpvPlayer`]'s methods only queue a command for it.

use std::sync::mpsc::{channel, Receiver, RecvTimeoutError, Sender};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use libmpv2::events::Event;
use libmpv2::{mpv_end_file_reason, Mpv};

use crate::output::VideoOutput;
use crate::recent::RecentLog;
use crate::{Latency, LoadOptions, Player, PlayerEvent, PlayerOption, PlayerStats, Zoom};

/// How often the control thread looks for mpv events when no command arrives.
const POLL: Duration = Duration::from_millis(50);

/// How often stats are read while they are on.
const STATS_EVERY: Duration = Duration::from_secs(1);

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
}

impl MpvPlayer {
    /// Starts mpv and its control thread. `on_event` runs on that thread. Fails if libmpv cannot
    /// be loaded or initialised.
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
        std::thread::Builder::new()
            .name("mpv-control".into())
            .spawn(move || control_loop(&core, &rx, &on_event))
            .map_err(|e| format!("could not start the mpv thread: {e}"))?;

        Ok((Self { tx: Mutex::new(tx) }, VideoOutput::new(mpv)))
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
    };
    if let Err(e) = result {
        tracing::warn!(?option, %e, "mpv option not changed");
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

fn control_loop(mpv: &Mpv, rx: &Receiver<Control>, on_event: &impl Fn(PlayerEvent)) {
    // True from `Load` until the first frame, so `Playing` is reported once per stream.
    let mut starting = false;
    // What mpv warned about since the current stream started, shown with a failure.
    let mut recent = RecentLog::default();
    // When the next stats report is due, while stats are on.
    let mut stats_due: Option<Instant> = None;
    // A volume fade in progress: when it started and how long it takes.
    let mut fade: Option<(Instant, Duration)> = None;
    loop {
        match rx.recv_timeout(POLL) {
            Ok(Control::Load(url, options)) => {
                recent.clear();
                apply_latency(mpv, options.latency);
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
            Ok(Control::Pause(paused)) => {
                if let Err(e) = mpv.set_property("pause", paused) {
                    tracing::warn!(%e, "could not change the pause state");
                }
            }
            Ok(Control::Stop) => {
                starting = false;
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

        while let Some(event) = mpv.wait_event(0.0) {
            match event {
                Ok(Event::PlaybackRestart) if starting => {
                    starting = false;
                    on_event(PlayerEvent::Playing);
                }
                Ok(Event::EndFile(reason)) if reason == mpv_end_file_reason::Eof => {
                    starting = false;
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
}
