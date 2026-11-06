//! Keeps the screen on while a stream plays. Watching is not input, so without this the desktop
//! dims, locks (KDE shows its wallpaper) or sleeps mid-stream. mpv would do it for its own window,
//! but it draws into ours.
//!
//! The work runs on a thread of its own: the calls block (D-Bus round trips), and on Windows the
//! request belongs to the thread that made it, so that thread must outlive it.

use std::sync::mpsc::channel;
use std::sync::Arc;

/// Tells the system whether to stay awake: `true` while a stream plays, `false` otherwise. The
/// store calls it only when the answer changes.
pub type KeepAwake = Arc<dyn Fn(bool) + Send + Sync>;

/// Starts the thread and returns the switch. `None` if the thread could not start, in which case
/// the system just keeps its usual idle behaviour.
pub fn keep_awake() -> Option<KeepAwake> {
    let (tx, rx) = channel::<bool>();
    let spawned = std::thread::Builder::new()
        .name("keep-awake".into())
        .spawn(move || {
            let mut system = imp::System::new();
            let mut holding = false;
            while let Ok(want) = rx.recv() {
                if want && !holding {
                    system.hold();
                } else if !want && holding {
                    system.release();
                }
                holding = want;
            }
            if holding {
                system.release();
            }
        });
    match spawned {
        Ok(_) => Some(Arc::new(move |want| {
            // Fails only once the thread is gone, which ends the request anyway.
            let _ = tx.send(want);
        })),
        Err(err) => {
            tracing::warn!(%err, "could not start the keep-awake thread");
            None
        }
    }
}

/// Linux: the Inhibit portal (works inside the Flatpak sandbox, and KDE honours it) plus the
/// older org.freedesktop.ScreenSaver interface, which other sessions (such as Steam's Game Mode)
/// may listen to instead. Each is tried, and whichever is there is held.
#[cfg(target_os = "linux")]
mod imp {
    use std::collections::HashMap;

    use zbus::blocking::Connection;
    use zbus::zvariant::{OwnedObjectPath, Value};

    const REASON: &str = "Watching a stream";
    /// Portal flags: suspend (4) and idle (8).
    const SUSPEND_AND_IDLE: u32 = 4 | 8;
    const PORTAL: &str = "org.freedesktop.portal.Desktop";
    const SCREENSAVER: &str = "org.freedesktop.ScreenSaver";

    #[derive(Default)]
    pub(super) struct System {
        /// Opened on first use and kept: the portal ends a request when its connection closes.
        bus: Option<Connection>,
        /// The portal's request handle, closed to release it.
        portal: Option<OwnedObjectPath>,
        /// The ScreenSaver cookie, handed back to release it.
        cookie: Option<u32>,
    }

    impl System {
        pub(super) fn new() -> Self {
            Self::default()
        }

        pub(super) fn hold(&mut self) {
            if self.bus.is_none() {
                match Connection::session() {
                    Ok(bus) => self.bus = Some(bus),
                    Err(e) => {
                        tracing::warn!(%e, "no session bus, the screen may sleep during streams");
                        return;
                    }
                }
            }
            let Some(bus) = &self.bus else { return };

            let options = HashMap::from([("reason", Value::from(REASON))]);
            let portal = bus
                .call_method(
                    Some(PORTAL),
                    "/org/freedesktop/portal/desktop",
                    Some("org.freedesktop.portal.Inhibit"),
                    "Inhibit",
                    &("", SUSPEND_AND_IDLE, options),
                )
                .and_then(|reply| reply.body().deserialize::<OwnedObjectPath>());
            match portal {
                Ok(handle) => self.portal = Some(handle),
                Err(e) => tracing::debug!(%e, "inhibit portal unavailable"),
            }

            let screensaver = bus
                .call_method(
                    Some(SCREENSAVER),
                    "/org/freedesktop/ScreenSaver",
                    Some(SCREENSAVER),
                    "Inhibit",
                    &("LiveDeck", REASON),
                )
                .and_then(|reply| reply.body().deserialize::<u32>());
            match screensaver {
                Ok(cookie) => self.cookie = Some(cookie),
                Err(e) => tracing::debug!(%e, "ScreenSaver inhibit unavailable"),
            }

            if self.portal.is_none() && self.cookie.is_none() {
                tracing::warn!("nothing accepted the keep-awake request, the screen may sleep");
            } else {
                tracing::debug!(
                    portal = self.portal.is_some(),
                    screensaver = self.cookie.is_some(),
                    "keeping the screen awake"
                );
            }
        }

        pub(super) fn release(&mut self) {
            let Some(bus) = &self.bus else { return };
            if let Some(handle) = self.portal.take() {
                let closed = bus.call_method(
                    Some(PORTAL),
                    handle.as_ref(),
                    Some("org.freedesktop.portal.Request"),
                    "Close",
                    &(),
                );
                if let Err(e) = closed {
                    tracing::debug!(%e, "could not close the inhibit request");
                }
            }
            if let Some(cookie) = self.cookie.take() {
                let released = bus.call_method(
                    Some(SCREENSAVER),
                    "/org/freedesktop/ScreenSaver",
                    Some(SCREENSAVER),
                    "UnInhibit",
                    &(cookie,),
                );
                if let Err(e) = released {
                    tracing::debug!(%e, "could not release the ScreenSaver inhibit");
                }
            }
            tracing::debug!("the screen may sleep again");
        }
    }
}

/// Windows: the thread's execution state keeps the display and the system on until reset.
#[cfg(windows)]
mod imp {
    const ES_CONTINUOUS: u32 = 0x8000_0000;
    const ES_SYSTEM_REQUIRED: u32 = 0x0000_0001;
    const ES_DISPLAY_REQUIRED: u32 = 0x0000_0002;

    #[link(name = "kernel32")]
    extern "system" {
        fn SetThreadExecutionState(flags: u32) -> u32;
    }

    pub(super) struct System;

    impl System {
        pub(super) fn new() -> Self {
            Self
        }

        pub(super) fn hold(&mut self) {
            // SAFETY: a plain Win32 call with valid flags. It applies to this (long-lived) thread.
            if unsafe {
                SetThreadExecutionState(ES_CONTINUOUS | ES_SYSTEM_REQUIRED | ES_DISPLAY_REQUIRED)
            } == 0
            {
                tracing::warn!("Windows refused the keep-awake request");
            }
        }

        pub(super) fn release(&mut self) {
            // SAFETY: as above. ES_CONTINUOUS alone clears this thread's request.
            unsafe { SetThreadExecutionState(ES_CONTINUOUS) };
        }
    }
}

/// Elsewhere: nothing to do.
#[cfg(not(any(target_os = "linux", windows)))]
mod imp {
    pub(super) struct System;

    impl System {
        pub(super) fn new() -> Self {
            Self
        }

        pub(super) fn hold(&mut self) {}
        pub(super) fn release(&mut self) {}
    }
}
