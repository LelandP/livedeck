//! Suspends the device when the sleep timer runs out.
//!
//! - Linux (Steam Deck, desktops): logind's `Suspend` over the system bus. The active local user
//!   may suspend without a password. Steam listens to logind's `PrepareForSleep`, so Game Mode
//!   treats it like a suspend from its power menu. The Flatpak needs
//!   `--system-talk-name=org.freedesktop.login1` for this.
//! - Windows: `SetSuspendState`. With hibernation turned on, some systems hibernate instead.
//!
//! A failure is logged and nothing else happens: the app stays on the browse screens, which is
//! what the sleep timer leaves behind anyway.

use std::sync::Arc;

/// Something that can put the device to sleep. A fake in tests.
pub trait Suspend: Send + Sync + 'static {
    /// Blocks while the request is made (on Windows, until the system wakes again).
    fn suspend(&self) -> Result<(), String>;
}

/// The real thing, for this platform.
pub struct SystemSuspend;

impl Suspend for SystemSuspend {
    fn suspend(&self) -> Result<(), String> {
        imp::suspend()
    }
}

/// For `--mock`: says what it would do and leaves the machine on.
pub struct LogOnly;

impl Suspend for LogOnly {
    fn suspend(&self) -> Result<(), String> {
        tracing::info!("--mock: the device would suspend now");
        Ok(())
    }
}

/// Asks `power` to suspend, off the async workers, and logs the outcome.
pub async fn suspend(power: Arc<dyn Suspend>) {
    tracing::info!("sleep timer ran out, suspending");
    match tokio::task::spawn_blocking(move || power.suspend()).await {
        Ok(Ok(())) => tracing::debug!("suspend requested"),
        Ok(Err(err)) => tracing::warn!(%err, "could not suspend the device"),
        Err(err) => tracing::warn!(%err, "the suspend task failed"),
    }
}

#[cfg(target_os = "linux")]
mod imp {
    use zbus::blocking::Connection;

    pub(super) fn suspend() -> Result<(), String> {
        let bus = Connection::system().map_err(|e| format!("no system bus: {e}"))?;
        // `false`: do not ask for a password (the active local user needs none).
        bus.call_method(
            Some("org.freedesktop.login1"),
            "/org/freedesktop/login1",
            Some("org.freedesktop.login1.Manager"),
            "Suspend",
            &(false,),
        )
        .map(|_| ())
        .map_err(|e| format!("logind refused: {e}"))
    }
}

#[cfg(windows)]
mod imp {
    #[link(name = "powrprof")]
    extern "system" {
        /// BOOLEAN arguments and result: 0 is false.
        fn SetSuspendState(hibernate: u8, force: u8, wakeup_events_disabled: u8) -> u8;
    }

    pub(super) fn suspend() -> Result<(), String> {
        // SAFETY: a plain Win32 call with valid arguments: sleep (not hibernate), let applications
        // object as usual, keep wake events.
        if unsafe { SetSuspendState(0, 0, 0) } == 0 {
            return Err(std::io::Error::last_os_error().to_string());
        }
        Ok(())
    }
}

#[cfg(not(any(target_os = "linux", windows)))]
mod imp {
    pub(super) fn suspend() -> Result<(), String> {
        Err("suspending is not supported on this platform".into())
    }
}

#[cfg(test)]
pub mod tests {
    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering};

    /// Counts suspend requests and answers with `result`.
    pub struct FakeSuspend {
        pub calls: AtomicUsize,
        pub result: Result<(), String>,
    }

    impl FakeSuspend {
        pub fn new(result: Result<(), String>) -> Arc<Self> {
            Arc::new(Self {
                calls: AtomicUsize::new(0),
                result,
            })
        }

        pub fn count(&self) -> usize {
            self.calls.load(Ordering::SeqCst)
        }
    }

    impl Suspend for FakeSuspend {
        fn suspend(&self) -> Result<(), String> {
            self.calls.fetch_add(1, Ordering::SeqCst);
            self.result.clone()
        }
    }

    #[tokio::test]
    async fn a_suspend_reaches_the_system() {
        let fake = FakeSuspend::new(Ok(()));
        suspend(fake.clone()).await;
        assert_eq!(fake.count(), 1);
    }

    #[tokio::test]
    async fn a_failed_suspend_is_only_logged() {
        let fake = FakeSuspend::new(Err("not allowed".into()));
        suspend(fake.clone()).await;
        assert_eq!(fake.count(), 1);
    }
}
