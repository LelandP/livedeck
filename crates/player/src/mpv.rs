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
use crate::sync::{next_lead_ms, sync_step, ClockWatch, SyncStep, SyncView, SEEK_LEAD_MS};
use crate::{
    ClockReading, Latency, LoadOptions, Player, PlayerClock, PlayerEvent, PlayerHealth,
    PlayerOption, PlayerStats, Zoom,
};

/// How often the control thread looks for mpv events when no command arrives.
const POLL: Duration = Duration::from_millis(50);

/// The same while a load takes over from another player, or another player takes over from
/// this one: the swap lands within this of its moment.
const SYNC_POLL: Duration = Duration::from_millis(10);

/// How long a load that takes over waits for the old player's first reading, which says how far
/// back to start.
const READING_WAIT: Duration = Duration::from_millis(300);

/// A seek this old that has not finished ends the take-over: the spare goes from where it is.
const SEEK_MAX: Duration = Duration::from_secs(3);

/// A live load that takes over starts this much further back than the old picture, so it can
/// seek forward to it (a segment).
const TAKE_OVER_MARGIN_MS: u32 = 2_000;

/// How far behind the live edge the old picture is taken to be when it has not said: a low
/// latency stream's 4 to 5 s, with room.
const TAKE_OVER_GUESS_MS: u32 = 6_000;

/// The furthest back a live load that takes over starts, in segments. Twitch's playlists hold 14.
const TAKE_OVER_MAX_BACK: u32 = 12;

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
///
/// `video-latency-hacks` stays on for both: without it mpv estimates the frame rate from frame
/// timings to decide which frames are late, and as frames are drawn when the UI redraws (not on
/// mpv's schedule) that estimate drifts and it drops frames that were not late. On the Steam
/// Machine (0.7.1, 2026-11-15) every video, and a live stream set to Normal, stuttered with the
/// "dropped" count climbing even at 480p, while Low was smooth.
const LATENCY_OPTIONS: &[(&str, &str, &str)] = &[
    ("audio-buffer", "0", "0.2"),
    ("vd-lavc-threads", "1", "0"),
    ("cache-pause", "no", "yes"),
    ("cache-pause-wait", "1", "3"),
    ("demuxer-lavf-o", "fflags=+nobuffer", ""),
    ("demuxer-lavf-probe-info", "nostreams", "auto"),
    ("demuxer-lavf-analyzeduration", "0.1", "0"),
    ("demuxer-readahead-secs", "1", "10"),
    ("video-latency-hacks", "yes", "yes"),
    ("stream-buffer-size", "4k", "128k"),
];

/// How far ahead the demuxer reads while a live stream is delayed for captions: enough to keep
/// everything from the delayed picture up to the live edge (the latencies' own values stop
/// reading 1 or 10 s ahead). Changed while playing, mpv reads further at once.
const DELAY_READAHEAD_S: &str = "60";

/// The length of a live HLS segment (Twitch's are 2 s): a delayed load starts back by as many
/// segments as the delay needs, through lavf's `live_start_index`.
const SEGMENT_MS: u32 = 2_000;

/// A load `n` segments back has about `n` × 2 s − 1.5 s buffered once it plays (live Twitch,
/// 2026-11-13: index −3 gave 3.8–4.9 s, −7 gave 12.3–13.6 s, −8 gave 14.2–15.3 s).
const START_BACK_SHORT_MS: u32 = 1_500;

/// A delayed stream whose buffer falls this far under the delay (after a network stall, say) is
/// held again. Less than this is the saw of 2 s segments arriving (2.3 s under the release
/// point at worst, measured).
const REHOLD_SLACK_MS: u32 = 3_500;

/// The longest hold. A buffer that cannot grow (a network too slow, a playlist too short) plays
/// on after this, and is not held again until the next load or delay change.
const HOLD_MAX: Duration = Duration::from_secs(20);

/// Turning the delay off jumps ahead to leave about this much buffered: a keyframe seek lands up
/// to a segment past its target, which leaves the 4-5 s a low-latency stream keeps (measured
/// 2026-11-13 on live Twitch, `examples/caption_delay_probe.rs`).
const CATCH_UP_KEEP_MS: u32 = 6_000;

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
    /// The next load takes over from the player of this clock.
    TakeOver(PlayerClock),
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
    /// Where its picture is, while someone watches (a player taking over from it).
    clock: PlayerClock,
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
        Self::start(on_event, false)
    }

    /// A player that shows and plays nothing (`vo=null`, `ao=null`) but otherwise behaves the
    /// same, for probes (`examples/caption_delay_probe.rs`).
    pub fn headless(on_event: impl Fn(PlayerEvent) + Send + 'static) -> Result<Self, String> {
        Self::start(on_event, true).map(|(player, _)| player)
    }

    fn start(
        on_event: impl Fn(PlayerEvent) + Send + 'static,
        headless: bool,
    ) -> Result<(Self, VideoOutput), String> {
        use_c_numeric_locale();
        let mpv = Mpv::with_initializer(|init| {
            // Required: without these there is no video at all.
            let vo = if headless { "null" } else { "libmpv" };
            init.set_option("vo", vo)
                .inspect_err(|e| tracing::error!(%e, "mpv rejected vo={vo}"))?;
            if headless {
                init.set_option("ao", "null")?;
            }
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
        // Headless, nothing waits for a render context.
        let ready = Arc::new(AtomicBool::new(headless));
        let can_show = Arc::clone(&ready);
        let clock = PlayerClock::new();
        let publish = clock.clone();
        std::thread::Builder::new()
            .name("mpv-control".into())
            .spawn(move || control_loop(&core, &rx, &can_show, &publish, &on_event))
            .map_err(|e| format!("could not start the mpv thread: {e}"))?;

        let alive = Arc::new(());
        let output = VideoOutput::new(mpv, Arc::downgrade(&alive), ready);
        Ok((
            Self {
                tx: Mutex::new(tx),
                clock,
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

    fn clock(&self) -> Option<PlayerClock> {
        Some(self.clock.clone())
    }

    fn take_over(&self, from: PlayerClock) {
        self.send(Control::TakeOver(from));
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

/// A latency's own value of `option` in [`LATENCY_OPTIONS`].
fn latency_value(option: &str, latency: Latency) -> &'static str {
    LATENCY_OPTIONS
        .iter()
        .find(|(name, _, _)| *name == option)
        .map_or("", |(_, low, normal)| match latency {
            Latency::Low => low,
            Latency::Normal => normal,
        })
}

/// lavf's options for a live load `delay_ms` behind the edge: the latency's own, plus a start
/// enough segments back for that buffer. Twitch's playlists hold 14 segments (28 s).
fn delayed_lavf_options(latency: Latency, delay_ms: u32) -> String {
    lavf_start_back(
        latency,
        (delay_ms + START_BACK_SHORT_MS).div_ceil(SEGMENT_MS),
    )
}

/// lavf's options for a live load `back` segments behind the edge, with the latency's own.
fn lavf_start_back(latency: Latency, back: u32) -> String {
    let start = format!("live_start_index=-{back}");
    match latency_value("demuxer-lavf-o", latency) {
        "" => start,
        own => format!("{own},{start}"),
    }
}

/// How many segments back a live load that takes over starts: a segment or so behind the old
/// picture, which is about its buffer (`ahead_ms`, or a guess) or the live delay behind the edge,
/// so the new one seeks forward to it rather than waits.
fn take_over_back(ahead_ms: Option<u32>, delay_ms: Option<u32>) -> u32 {
    let behind = ahead_ms
        .unwrap_or(TAKE_OVER_GUESS_MS)
        .max(delay_ms.unwrap_or(0));
    (behind + START_BACK_SHORT_MS + TAKE_OVER_MARGIN_MS)
        .div_ceil(SEGMENT_MS)
        .clamp(2, TAKE_OVER_MAX_BACK)
}

/// Sets up the next load to take over from another player: a live one `back` segments behind
/// the edge, and either reading far enough ahead to seek to the old picture within its buffer.
fn apply_take_over_to_load(mpv: &Mpv, latency: Latency, back: Option<u32>) {
    let lavf = back.map(|back| lavf_start_back(latency, back));
    let options = lavf
        .as_deref()
        .map(|lavf| ("demuxer-lavf-o", lavf))
        .into_iter()
        .chain([("demuxer-readahead-secs", DELAY_READAHEAD_S)]);
    for (name, value) in options {
        if let Err(e) = mpv.set_property(name, value) {
            tracing::warn!(option = name, value, %e, "mpv take-over option not applied");
        }
    }
}

/// Where the picture is on the clock a take-over compares by: the stream's own for a live stream
/// ([`stream_ms`]), the position for a video (the app's `pos_ms`).
fn clock_ms(mpv: &Mpv, video: bool) -> Option<u64> {
    if video {
        number(mpv, "time-pos").map(millis)
    } else {
        stream_ms(mpv)
    }
}

/// Seeks exactly to `ms` on the clock of [`clock_ms`].
fn seek_to_clock(mpv: &Mpv, video: bool, ms: u64) {
    let start = if video {
        0.0
    } else {
        mpv.get_property::<f64>("demuxer-start-time")
            .ok()
            .filter(|s| s.is_finite())
            .unwrap_or(0.0)
    };
    let to = (ms as f64 / 1000.0 - start).max(0.0);
    if let Err(e) = mpv.command("seek", &[format!("{to:.3}").as_str(), "absolute+exact"]) {
        tracing::warn!(%e, ms, "could not seek to the old player's moment");
    }
}

/// This player's picture for its clock: where it is, and whether it moves.
fn read_clock(mpv: &Mpv, video: bool, user_paused: bool, held: bool) -> Option<ClockReading> {
    let ms = clock_ms(mpv, video)?;
    let ahead_ms = buffered_ms(mpv);
    let flag = |name| mpv.get_property::<bool>(name).unwrap_or(false);
    let frozen =
        held || flag("paused-for-cache") || flag("seeking") || (!video && ahead_ms < DRY_BUFFER_MS);
    Some(ClockReading {
        ms,
        at: Instant::now(),
        paused: user_paused,
        frozen,
        ahead_ms,
    })
}

/// A load taking over from another player ([`Player::take_over`]), from its load to the moment
/// it goes.
struct TakeOver {
    from: PlayerClock,
    /// Keeps `from` published.
    _watch: ClockWatch,
    /// Its first frame came, paused, at this moment.
    ready_since: Option<Instant>,
    /// A seek to the old picture runs, since then.
    seeking: Option<Instant>,
    seeks: u8,
    /// How far ahead of the old picture the next seek aims.
    lead_ms: u64,
}

impl TakeOver {
    fn new(from: PlayerClock, watch: ClockWatch) -> Self {
        Self {
            from,
            _watch: watch,
            ready_since: None,
            seeking: None,
            seeks: 0,
            lead_ms: SEEK_LEAD_MS,
        }
    }

    /// What to do now: `own` is where the paused picture is and `buffered_ms` its buffer ahead.
    fn step(&self, now: Instant, own: Option<u64>, buffered_ms: u32, video: bool) -> SyncStep {
        let Some(ready) = self.ready_since else {
            return SyncStep::Wait;
        };
        if let Some(at) = self.seeking {
            return if now.saturating_duration_since(at) < SEEK_MAX {
                SyncStep::Wait
            } else {
                SyncStep::Go
            };
        }
        let Some(own) = own else {
            return SyncStep::Go;
        };
        sync_step(&SyncView {
            own_ms: own,
            cached_until_ms: own + u64::from(buffered_ms),
            old: self.from.read(),
            now,
            seek_anywhere: video,
            seeks: self.seeks,
            ready_for: now.saturating_duration_since(ready),
            lead_ms: self.lead_ms,
        })
    }
}

/// Sets up the next live load to start `delay_ms` behind the live edge and to keep reading up to
/// it.
fn apply_delay_to_load(mpv: &Mpv, latency: Latency, delay_ms: u32) {
    let lavf = delayed_lavf_options(latency, delay_ms);
    for (name, value) in [
        ("demuxer-lavf-o", lavf.as_str()),
        ("demuxer-readahead-secs", DELAY_READAHEAD_S),
    ] {
        if let Err(e) = mpv.set_property(name, value) {
            tracing::warn!(option = name, value, %e, "mpv delay option not applied");
        }
    }
}

/// What the delay asks of a live stream now.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum DelayStep {
    /// Hold the picture: the buffer is too short.
    Hold,
    /// Let it go: the buffer is long enough, or the hold lasted [`HOLD_MAX`] (`gave_up`).
    Release {
        gave_up: bool,
    },
    Stay,
}

/// The delay's decision for a stream `buffered_ms` ahead, held for `held_for` (`None`: playing).
/// `may_hold`: it plays with nothing against a hold (not paused by the user, not given up).
fn delay_step(
    delay_ms: u32,
    buffered_ms: u32,
    held_for: Option<Duration>,
    may_hold: bool,
) -> DelayStep {
    match held_for {
        Some(_) if buffered_ms >= delay_ms => DelayStep::Release { gave_up: false },
        Some(held) if held >= HOLD_MAX => DelayStep::Release { gave_up: true },
        Some(_) => DelayStep::Stay,
        None if may_hold && buffered_ms + REHOLD_SLACK_MS < delay_ms => DelayStep::Hold,
        None => DelayStep::Stay,
    }
}

/// How far to jump ahead (seconds) when the delay is turned off with `buffered_ms` ahead: to near
/// the live edge, or nowhere if it is near already.
fn catch_up_s(buffered_ms: u32) -> Option<f64> {
    let by = buffered_ms.checked_sub(CATCH_UP_KEEP_MS)?;
    (by >= 1_000).then(|| f64::from(by) / 1000.0)
}

/// The demuxer's buffer ahead in milliseconds, 0 while mpv cannot tell.
fn buffered_ms(mpv: &Mpv) -> u32 {
    number(mpv, "demuxer-cache-duration").map_or(0, |s| {
        u32::try_from((s * 1000.0).round() as u64).unwrap_or(u32::MAX)
    })
}

/// The live delay on the control thread ([`PlayerOption::LiveDelay`]): what was asked, and the
/// hold that builds it.
#[derive(Debug, Default)]
struct Delay {
    /// The delay asked for, in milliseconds.
    ms: Option<u32>,
    /// Since when the picture is held: paused by the delay, whatever the user asked.
    held_since: Option<Instant>,
    /// The loaded stream's first frame came during the hold, so `Playing` waits for its end.
    restarted: bool,
    /// A hold ran out: no more until the next load or a new delay.
    gave_up: bool,
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
        // The control loop keeps it (`Delay`): it changes the next load and the pause.
        PlayerOption::LiveDelay(_) => Ok(()),
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
        stream_ms: stream_ms(mpv),
    })
}

/// `time-pos` on the stream's own clock. mpv rebases a file's timestamps to start at 0
/// (`rebase-start-time`) by taking its start time off, so adding `demuxer-start-time` back gives
/// the timestamps an audio tap with `rebase-start-time=no` sees. Measured 2026-11-13: 0 on live
/// Twitch (rebased and raw `time-pos` were equal), 62.05 s on a Twitch video (raw = rebased +
/// 62.05 s). Positions in the app (`pos_ms`, seeking) stay rebased.
fn stream_ms(mpv: &Mpv) -> Option<u64> {
    let pos = number(mpv, "time-pos")?;
    let start = mpv
        .get_property::<f64>("demuxer-start-time")
        .ok()
        .filter(|s| s.is_finite())
        .unwrap_or(0.0);
    Some(millis((pos + start).max(0.0)))
}

/// libmpv refuses to start (`mpv_create` returns NULL) unless `LC_NUMERIC` is "C". Rust never
/// changes the locale, but a library in the process may have set it from the environment (a
/// Flatpak or SteamOS session has `LANG` set). Only number formatting changes: text stays UTF-8.
#[cfg(target_os = "linux")]
pub(crate) fn use_c_numeric_locale() {
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
pub(crate) fn use_c_numeric_locale() {}

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
    clock: &PlayerClock,
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
    // A live stream is loaded (or loading), and has not ended, failed or been stopped.
    let mut live = false;
    // The loaded stream's latency, whose read-ahead comes back when the delay goes.
    let mut latency = Latency::Low;
    // The user's pause. mpv is paused while this or the delay's hold is on.
    let mut user_paused = false;
    let mut delay = Delay::default();
    // The next load takes over from this clock's player (`Player::take_over`), which publishes
    // while it is watched.
    let mut follow: Option<(PlayerClock, ClockWatch)> = None;
    // The load doing so, paused until it goes.
    let mut sync: Option<TakeOver> = None;
    loop {
        let mut load = None;
        let poll = if sync.is_some() || clock.is_watched() {
            SYNC_POLL
        } else {
            POLL
        };
        match rx.recv_timeout(poll) {
            // Held until the render context exists (see `RENDER_WAIT`). A newer load replaces it.
            Ok(Control::Load(url, options)) => waiting = Some((url, options, Instant::now())),
            Ok(Control::TakeOver(from)) => {
                let watch = from.watch();
                follow = Some((from, watch));
            }
            Ok(Control::Pause(paused)) => {
                user_paused = paused;
                let paused = paused || delay.held_since.is_some() || sync.is_some();
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
                live = false;
                delay.held_since = None;
                follow = None;
                sync = None;
                if let Err(e) = mpv.command("stop", &[]) {
                    tracing::warn!(%e, "could not stop mpv");
                }
            }
            Ok(Control::Option(PlayerOption::LiveDelay(ms))) => {
                let was = std::mem::replace(&mut delay.ms, ms);
                delay.gave_up = false;
                if was == ms || !live || sync.is_some() {
                    // Nothing plays to change: the next load reads it (a take-over, once it
                    // goes, follows the old player, which gets the same change).
                } else if ms.is_some() {
                    // Read up to the live edge; the check below holds the picture until the
                    // buffer is long enough.
                    let _ = mpv.set_property("demuxer-readahead-secs", DELAY_READAHEAD_S);
                    tracing::debug!(?ms, "live delay on");
                } else {
                    let readahead = latency_value("demuxer-readahead-secs", latency);
                    let _ = mpv.set_property("demuxer-readahead-secs", readahead);
                    let buffered = buffered_ms(mpv);
                    end_hold(
                        mpv,
                        &mut delay,
                        user_paused,
                        &mut starting,
                        &mut health_due,
                        on_event,
                    );
                    // Back to near the live edge, from the cache: no reload.
                    if let Some(by) = catch_up_s(buffered) {
                        if let Err(e) =
                            mpv.command("seek", &[format!("{by:.3}").as_str(), "relative"])
                        {
                            tracing::warn!(%e, by, "could not catch up with the live edge");
                        }
                    }
                    tracing::debug!(buffered, "live delay off");
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
            let shown = can_show.load(Ordering::SeqCst) || since.elapsed() >= RENDER_WAIT;
            // Taking over: the old player's first reading says how far back to start.
            let read = follow
                .as_ref()
                .is_none_or(|(from, _)| from.read().is_some())
                || since.elapsed() >= READING_WAIT;
            shown && read
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
            live = !video;
            latency = options.effective_latency();
            // Never the low-latency buffers for a video (`effective_latency`).
            apply_latency(mpv, latency);
            apply_start(mpv, options.start_ms);
            // A live stream always plays at normal speed. A video keeps the speed set for it.
            if !video {
                apply_option(mpv, PlayerOption::Speed(crate::NORMAL_SPEED));
            }
            // A delayed live stream starts that far back and paused: its first frame waits until
            // the buffer is long enough (at once, mostly), and `Playing` with it.
            delay.held_since = None;
            delay.restarted = false;
            delay.gave_up = false;
            user_paused = false;
            sync = None;
            if let Some((from, watch)) = follow.take() {
                // Taking over: paused, from behind the old picture, until it reaches it. The
                // delay comes with it, as the old picture is that far behind.
                let back = live.then(|| take_over_back(from.read().map(|r| r.ahead_ms), delay.ms));
                apply_take_over_to_load(mpv, latency, back);
                sync = Some(TakeOver::new(from, watch));
            } else if let (Some(ms), true) = (delay.ms, live) {
                apply_delay_to_load(mpv, latency, ms);
                delay.held_since = Some(Instant::now());
            }
            let _ = mpv.set_property("pause", delay.held_since.is_some() || sync.is_some());
            // A fade only quietens the stream it ran on.
            fade = None;
            set_volume(mpv, FULL_VOLUME);
            match mpv.command("loadfile", &[url.as_str(), "replace"]) {
                Ok(()) => starting = true,
                Err(e) => {
                    live = false;
                    delay.held_since = None;
                    sync = None;
                    on_event(PlayerEvent::Failed(
                        recent.report(&format!("loadfile failed: {}", describe(&e))),
                    ));
                }
            }
        }

        while let Some(event) = mpv.wait_event(0.0) {
            match event {
                // Taking over: the paused picture is ready (the first frame, or a seek's).
                Ok(Event::PlaybackRestart) if starting && sync.is_some() => {
                    if let Some(s) = sync.as_mut() {
                        let now = Instant::now();
                        if let Some(at) = s.seeking.take() {
                            s.lead_ms = next_lead_ms(now.saturating_duration_since(at));
                        }
                        s.ready_since.get_or_insert(now);
                    }
                }
                // Held for the delay: `Playing` comes when the hold ends (`end_hold`).
                Ok(Event::PlaybackRestart) if starting && delay.held_since.is_some() => {
                    delay.restarted = true;
                }
                Ok(Event::PlaybackRestart) if starting => {
                    starting = false;
                    health_due = Some(Instant::now() + HEALTH_EVERY);
                    on_event(PlayerEvent::Playing);
                }
                Ok(Event::EndFile(reason)) if reason == mpv_end_file_reason::Eof => {
                    starting = false;
                    health_due = None;
                    live = false;
                    delay.held_since = None;
                    sync = None;
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
                    live = false;
                    delay.held_since = None;
                    sync = None;
                    tracing::warn!(%e, "mpv could not play the stream");
                    on_event(PlayerEvent::Failed(recent.report(&describe(&e))));
                }
            }
        }

        // Taking over: wait, seek, or go once the old picture reaches this one's.
        if let Some(s) = sync.as_mut() {
            let now = Instant::now();
            let own = clock_ms(mpv, video);
            match s.step(now, own, buffered_ms(mpv), video) {
                SyncStep::Wait => {}
                SyncStep::Seek(to) => {
                    tracing::debug!(own, to, lead = s.lead_ms, "seeking to the old picture");
                    seek_to_clock(mpv, video, to);
                    s.seeks += 1;
                    s.seeking = Some(now);
                }
                SyncStep::Go => {
                    let old = s.from.now_ms();
                    let behind = old
                        .zip(own)
                        .map(|(old, own)| i128::from(old) - i128::from(own));
                    tracing::debug!(?behind, seeks = s.seeks, "taking over from the old player");
                    sync = None;
                    let readahead = if live && delay.ms.is_some() {
                        DELAY_READAHEAD_S
                    } else {
                        latency_value("demuxer-readahead-secs", latency)
                    };
                    let _ = mpv.set_property("demuxer-readahead-secs", readahead);
                    let _ = mpv.set_property("pause", user_paused);
                    starting = false;
                    health_due = Some(now + HEALTH_EVERY);
                    if clock.is_watched() {
                        clock.publish(read_clock(mpv, video, user_paused, false));
                    }
                    on_event(PlayerEvent::Playing);
                }
            }
        }

        // The delay: hold the picture while the buffer is too short, let it go once it is long
        // enough. Holds start only on a stream that plays; a load is held from the start.
        if let (Some(ms), true) = (delay.ms, live) {
            let playing = !starting && health_due.is_some();
            let may_hold = playing && !user_paused && !delay.gave_up;
            let held_for = delay.held_since.map(|since| since.elapsed());
            let buffered = buffered_ms(mpv);
            match delay_step(ms, buffered, held_for, may_hold) {
                DelayStep::Hold => {
                    tracing::debug!(buffered, ms, "holding the picture for the live delay");
                    delay.held_since = Some(Instant::now());
                    let _ = mpv.set_property("pause", true);
                    on_event(PlayerEvent::Held(true));
                }
                DelayStep::Release { gave_up } => {
                    if gave_up {
                        tracing::warn!(buffered, ms, "the live delay could not be built");
                        delay.gave_up = true;
                    }
                    tracing::debug!(buffered, ?held_for, "the live delay is built");
                    end_hold(
                        mpv,
                        &mut delay,
                        user_paused,
                        &mut starting,
                        &mut health_due,
                        on_event,
                    );
                }
                DelayStep::Stay => {}
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

        // A player taking over from this one is watching: where the picture is, every pass.
        if clock.is_watched() {
            let playing = !starting && health_due.is_some();
            let held = delay.held_since.is_some();
            clock.publish(
                playing
                    .then(|| read_clock(mpv, video, user_paused, held))
                    .flatten(),
            );
        }
    }
    tracing::debug!("mpv control thread finished");
}

/// Ends the delay's hold, if there is one: the picture goes on (unless the user paused it). A
/// load held from its start reports `Playing` now if its first frame came meanwhile (or later, at
/// that frame); a stream held while playing reports `Held(false)` and its health at once.
fn end_hold(
    mpv: &Mpv,
    delay: &mut Delay,
    user_paused: bool,
    starting: &mut bool,
    health_due: &mut Option<Instant>,
    on_event: &impl Fn(PlayerEvent),
) {
    if delay.held_since.take().is_none() {
        return;
    }
    let _ = mpv.set_property("pause", user_paused);
    if *starting {
        if std::mem::take(&mut delay.restarted) {
            *starting = false;
            *health_due = Some(Instant::now() + HEALTH_EVERY);
            on_event(PlayerEvent::Playing);
        }
    } else {
        *health_due = Some(Instant::now());
        on_event(PlayerEvent::Held(false));
    }
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

    #[test]
    fn a_delayed_load_starts_segments_back_with_the_latencys_own_options() {
        // 12.5 s: 7 segments of 2 s give about 12.5 s.
        assert_eq!(
            delayed_lavf_options(Latency::Low, 12_500),
            "fflags=+nobuffer,live_start_index=-7"
        );
        assert_eq!(
            delayed_lavf_options(Latency::Normal, 10_000),
            "live_start_index=-6"
        );
        assert_eq!(latency_value("demuxer-readahead-secs", Latency::Low), "1");
        assert_eq!(
            latency_value("demuxer-readahead-secs", Latency::Normal),
            "10"
        );
    }

    #[test]
    fn a_take_over_starts_a_segment_or_so_behind_the_old_picture() {
        // 4.5 s buffered (low latency): 4 segments, about 6.5 s behind the edge.
        assert_eq!(take_over_back(Some(4_500), None), 4);
        assert_eq!(
            lavf_start_back(Latency::Low, 4),
            "fflags=+nobuffer,live_start_index=-4"
        );
        // Delayed for captions: behind the delay, whatever the buffer says.
        assert_eq!(take_over_back(Some(3_000), Some(12_500)), 8);
        // No reading: a guess. Never further back than the playlist holds.
        assert_eq!(take_over_back(None, None), 5);
        assert_eq!(take_over_back(Some(60_000), None), TAKE_OVER_MAX_BACK);
        assert_eq!(take_over_back(Some(0), None), 2);
    }

    #[test]
    fn the_delay_holds_a_short_buffer_and_lets_go_of_a_long_one() {
        let s = Duration::from_secs;
        let step =
            |buffered, held: Option<u64>, may| delay_step(11_500, buffered, held.map(s), may);
        // Playing: held only well under the delay, and only when it may be.
        assert_eq!(step(4_500, None, true), DelayStep::Hold);
        assert_eq!(step(4_500, None, false), DelayStep::Stay);
        assert_eq!(
            step(9_000, None, true),
            DelayStep::Stay,
            "the saw of segments"
        );
        assert_eq!(step(7_900, None, true), DelayStep::Hold);
        // Held: until the buffer is long enough, or for so long.
        assert_eq!(step(11_000, Some(3), true), DelayStep::Stay);
        assert_eq!(
            step(11_500, Some(3), true),
            DelayStep::Release { gave_up: false }
        );
        assert_eq!(
            step(6_000, Some(20), false),
            DelayStep::Release { gave_up: true }
        );
    }

    #[test]
    fn turning_the_delay_off_jumps_to_near_the_live_edge() {
        assert_eq!(catch_up_s(12_400), Some(6.4));
        assert_eq!(catch_up_s(6_500), None, "near it already");
        assert_eq!(catch_up_s(0), None);
    }
}
