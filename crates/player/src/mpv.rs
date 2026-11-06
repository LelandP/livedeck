//! `MpvPlayer`: libmpv behind the [`Player`] trait.
//!
//! Threading follows the rules in mpv's `render.h`:
//! - The Slint UI thread is the mpv *render thread*. It only calls `mpv_render_*` functions, and
//!   only from inside the rendering notifier (see `output.rs`).
//! - The control thread made here makes every other libmpv call, so the UI thread never waits on
//!   the mpv core. [`MpvPlayer`]'s methods only queue a command for it.

use std::sync::mpsc::{channel, Receiver, RecvTimeoutError, Sender};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use libmpv2::events::Event;
use libmpv2::{mpv_end_file_reason, Mpv};

use crate::output::VideoOutput;
use crate::{Player, PlayerEvent};

/// How often the control thread looks for mpv events when no command arrives.
const POLL: Duration = Duration::from_millis(50);

enum Control {
    Load(String),
    Pause(bool),
    Stop,
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
    fn load(&self, url: &str) {
        self.send(Control::Load(url.to_owned()));
    }

    fn set_paused(&self, paused: bool) {
        self.send(Control::Pause(paused));
    }

    fn stop(&self) {
        self.send(Control::Stop);
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
    loop {
        match rx.recv_timeout(POLL) {
            Ok(Control::Load(url)) => {
                let _ = mpv.set_property("pause", false);
                match mpv.command("loadfile", &[url.as_str(), "replace"]) {
                    Ok(()) => starting = true,
                    Err(e) => on_event(PlayerEvent::Failed(format!(
                        "loadfile failed: {}",
                        describe(&e)
                    ))),
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
                Ok(_) => {}
                // An end-of-file with an error code arrives as an `Err`.
                Err(e) => {
                    starting = false;
                    tracing::warn!(%e, "mpv could not play the stream");
                    on_event(PlayerEvent::Failed(describe(&e)));
                }
            }
        }
    }
    tracing::debug!("mpv control thread finished");
}
