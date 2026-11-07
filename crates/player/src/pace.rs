//! Frame pacing: video frames ask for a redraw at most once per display refresh.
//!
//! Every mpv player reports each new frame, and nine unsynchronised streams report hundreds a
//! second. Redrawing for each made the UI draw at about 200 fps in the multiview spike, which was
//! most of the GPU load. The [`Pacer`] lets one redraw be pending at a time, and delays it until a
//! refresh interval has passed since the last one.

use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::Arc;
use std::time::Duration;

/// The display's refresh rate, shared between the app (which reads it from the monitor) and the
/// video renderer (which paces redraws by it). Starts at 60 Hz.
#[derive(Clone, Debug)]
pub struct RefreshRate(Arc<AtomicU32>);

impl RefreshRate {
    /// 60 Hz: most TVs, monitors and the Steam Deck.
    pub const DEFAULT_MILLIHERTZ: u32 = 60_000;
    /// Below this a reported rate is ignored as bogus.
    const MIN_MILLIHERTZ: u32 = 20_000;

    pub fn new() -> Self {
        Self(Arc::new(AtomicU32::new(Self::DEFAULT_MILLIHERTZ)))
    }

    /// Sets the rate from what the monitor reports, in millihertz (59 940 for 59.94 Hz). An
    /// unusable value keeps the current one.
    pub fn set_millihertz(&self, millihertz: u32) {
        if millihertz >= Self::MIN_MILLIHERTZ {
            self.0.store(millihertz, Ordering::Relaxed);
        }
    }

    pub fn millihertz(&self) -> u32 {
        self.0.load(Ordering::Relaxed)
    }

    /// The time between two refreshes.
    pub fn interval(&self) -> Duration {
        Duration::from_secs(1000) / self.millihertz()
    }
}

impl Default for RefreshRate {
    fn default() -> Self {
        Self::new()
    }
}

/// How long a redraw asked for now waits, `since_last` after the last frame was drawn.
#[cfg(any(feature = "mpv", test))]
fn wait(since_last: Duration, interval: Duration) -> Duration {
    interval.saturating_sub(since_last)
}

#[cfg(any(feature = "mpv", test))]
pub(crate) use pacer::Pacer;

#[cfg(any(feature = "mpv", test))]
mod pacer {
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::sync::Mutex;
    use std::time::{Duration, Instant};

    use super::{wait, RefreshRate};

    type Redraw = Box<dyn Fn(Duration) + Send + Sync>;

    /// Turns "a frame is ready" from any player thread into at most one redraw per refresh.
    pub(crate) struct Pacer {
        rate: RefreshRate,
        /// A redraw was asked for and has not been drawn yet.
        pending: AtomicBool,
        last_drawn: Mutex<Instant>,
        /// Asks the window to redraw after the given delay. Must not block or call mpv.
        redraw: Redraw,
    }

    impl Pacer {
        pub(crate) fn new(
            rate: RefreshRate,
            redraw: impl Fn(Duration) + Send + Sync + 'static,
        ) -> Self {
            Self {
                rate,
                pending: AtomicBool::new(false),
                last_drawn: Mutex::new(Instant::now()),
                redraw: Box::new(redraw),
            }
        }

        /// A player has a new frame (any thread). Asks for a redraw unless one is pending.
        pub(crate) fn frame_ready(&self) {
            if self.pending.swap(true, Ordering::AcqRel) {
                return;
            }
            let since = self.last_drawn.lock().expect("pacer lock").elapsed();
            (self.redraw)(wait(since, self.rate.interval()));
        }

        /// The window is drawing (UI thread). Frames from now on ask for the next redraw.
        pub(crate) fn drawing(&self) {
            *self.last_drawn.lock().expect("pacer lock") = Instant::now();
            self.pending.store(false, Ordering::Release);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Mutex;

    #[test]
    fn the_rate_gives_the_interval_and_ignores_bogus_values() {
        let rate = RefreshRate::new();
        assert_eq!(rate.interval().as_micros(), 16_666);
        rate.set_millihertz(120_000);
        assert_eq!(rate.interval().as_micros(), 8_333);
        rate.set_millihertz(0);
        rate.set_millihertz(5_000);
        assert_eq!(rate.millihertz(), 120_000);
    }

    #[test]
    fn a_redraw_waits_out_the_rest_of_the_interval() {
        let interval = Duration::from_millis(16);
        assert_eq!(
            wait(Duration::from_millis(4), interval),
            Duration::from_millis(12)
        );
        assert_eq!(wait(Duration::from_millis(16), interval), Duration::ZERO);
        assert_eq!(wait(Duration::from_secs(3), interval), Duration::ZERO);
    }

    #[test]
    fn only_one_redraw_is_pending_at_a_time() {
        let asked = Arc::new(Mutex::new(Vec::new()));
        let sink = Arc::clone(&asked);
        let pacer = Pacer::new(RefreshRate::new(), move |d| sink.lock().unwrap().push(d));

        // Nine players report a frame before the window draws: one redraw.
        for _ in 0..9 {
            pacer.frame_ready();
        }
        assert_eq!(asked.lock().unwrap().len(), 1);

        // Drawn: the next frame asks again, waiting most of an interval.
        pacer.drawing();
        pacer.frame_ready();
        pacer.frame_ready();
        let asked = asked.lock().unwrap();
        assert_eq!(asked.len(), 2);
        assert!(asked[1] > Duration::from_millis(10), "{:?}", asked[1]);
    }
}
