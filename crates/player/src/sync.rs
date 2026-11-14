//! Taking over from another player at the same moment (plan 14): a new quality of the stream on
//! screen must never land ahead of the old picture. If it cannot reach the moment, it lands
//! behind (a short repeat), not in front.
//!
//! Every Twitch video rendition shares one clock (the captions spike: 160p30 and 1080p60 within
//! 0.2 s), so each player publishes where its picture is on that clock through a
//! [`PlayerClock`]. The spare that takes over ([`crate::Player::take_over`]) starts a few
//! segments further back, paused, and then [`sync_step`] tells it each moment what to do: wait,
//! seek to the old player's moment plus the time the seek takes, or go (unpause and report its
//! first frame, which swaps the two).

use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

/// A reading is carried forward by the time since it was taken, but no further than this: a
/// player that publishes stops doing so when it stops, and its last reading must not run on.
const CARRY_MAX: Duration = Duration::from_millis(100);

/// Behind the old picture by this much or less, the spare goes now: a repeat too short to see.
pub const CLOSE_MS: u64 = 100;

/// The most seeks a spare makes to reach the moment, before it goes from where it is (behind).
pub const MAX_SEEKS: u8 = 3;

/// How far ahead of the old picture the first seek aims: the time an exact seek takes to decode
/// from the keyframe before it (a 2 s Twitch segment at most). Landing early only means a wait.
pub const SEEK_LEAD_MS: u64 = 600;

/// A video's spare further ahead of the old picture than this seeks back to it rather than
/// waiting (the old one was seeked meanwhile). A live spare can only wait.
pub const WAIT_AHEAD_MAX_MS: u64 = 3_000;

/// With the old picture frozen (a stall, a hold), a spare this little ahead of it goes: a frame.
pub const FROZEN_SLACK_MS: u64 = 40;

/// The longest a spare waits, from its first frame, before it goes wherever it is.
pub const SYNC_MAX: Duration = Duration::from_secs(8);

/// Where a player's picture was at one moment.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ClockReading {
    /// Where the picture is, in milliseconds: on the stream's own clock for a live stream
    /// ([`crate::PlayerHealth::stream_ms`]), the position for a video
    /// ([`crate::PlayerHealth::pos_ms`]).
    pub ms: u64,
    /// When it was read.
    pub at: Instant,
    /// Paused by the user.
    pub paused: bool,
    /// The picture is frozen: waiting for data, or held for the live delay.
    pub frozen: bool,
    /// Buffered ahead of the picture, in milliseconds: about how far behind the live edge it is.
    pub ahead_ms: u32,
}

impl ClockReading {
    /// Where the picture is at `now`: the reading carried forward while it plays.
    pub fn ms_at(&self, now: Instant) -> u64 {
        if self.paused || self.frozen {
            return self.ms;
        }
        let carried = now.saturating_duration_since(self.at).min(CARRY_MAX);
        self.ms + u64::try_from(carried.as_millis()).unwrap_or(0)
    }
}

/// Where a player's picture is, readable from any thread at once. The player publishes to it
/// only while someone watches ([`PlayerClock::watch`]), so it costs nothing otherwise.
#[derive(Clone, Default)]
pub struct PlayerClock(Arc<ClockShared>);

#[derive(Default)]
struct ClockShared {
    reading: Mutex<Option<ClockReading>>,
    watchers: AtomicUsize,
}

impl std::fmt::Debug for PlayerClock {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_tuple("PlayerClock").field(&self.read()).finish()
    }
}

impl PlayerClock {
    pub fn new() -> Self {
        Self::default()
    }

    /// The latest reading. `None` while nothing plays, or nobody watched yet.
    pub fn read(&self) -> Option<ClockReading> {
        *self.0.reading.lock().expect("clock lock")
    }

    /// Where the picture is now, carried forward from the latest reading.
    pub fn now_ms(&self) -> Option<u64> {
        self.read().map(|r| r.ms_at(Instant::now()))
    }

    /// The player's side: a new reading, or `None` when nothing plays.
    pub fn publish(&self, reading: Option<ClockReading>) {
        *self.0.reading.lock().expect("clock lock") = reading;
    }

    /// Asks the player to publish, until the returned watch is dropped.
    pub fn watch(&self) -> ClockWatch {
        self.0.watchers.fetch_add(1, Ordering::SeqCst);
        ClockWatch(self.clone())
    }

    /// Someone watches: the player should publish (about every 10 ms).
    pub fn is_watched(&self) -> bool {
        self.0.watchers.load(Ordering::SeqCst) > 0
    }

    /// The same clock (the same player's).
    pub fn is(&self, other: &PlayerClock) -> bool {
        Arc::ptr_eq(&self.0, &other.0)
    }
}

/// Keeps a [`PlayerClock`] published while it lives.
pub struct ClockWatch(PlayerClock);

impl Drop for ClockWatch {
    fn drop(&mut self) {
        self.0 .0.watchers.fetch_sub(1, Ordering::SeqCst);
    }
}

/// What a spare that takes over sees, once its picture is ready (paused) and no seek runs.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SyncView {
    /// Where its own paused picture is, on the same clock as the old one's.
    pub own_ms: u64,
    /// The end of what it has buffered, on that clock: a live stream can only seek up to it.
    pub cached_until_ms: u64,
    /// The old player's latest reading. `None`: it stopped or cannot tell.
    pub old: Option<ClockReading>,
    pub now: Instant,
    /// A video, which can seek anywhere (a live stream only within its buffer).
    pub seek_anywhere: bool,
    /// Seeks made so far.
    pub seeks: u8,
    /// Since its first frame.
    pub ready_for: Duration,
    /// How far ahead of the old picture a seek aims.
    pub lead_ms: u64,
}

/// What the spare does next.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SyncStep {
    /// Stay paused: the old picture is still before its own, or its buffer is still too short.
    Wait,
    /// Seek (exactly) to this moment.
    Seek(u64),
    /// Unpause and take over.
    Go,
}

/// The rule: go once the old picture has reached its own (at most [`CLOSE_MS`] past it), never
/// before; seek forward when it is further behind; give up and go after [`SYNC_MAX`]. A paused
/// old player, or one that cannot tell, means no wait at all: going back to live from a pause
/// loads the stream again, and must not land where the pause was.
pub fn sync_step(v: &SyncView) -> SyncStep {
    if v.ready_for >= SYNC_MAX {
        return SyncStep::Go;
    }
    let Some(old) = v.old.filter(|o| !o.paused) else {
        return SyncStep::Go;
    };
    let old_ms = old.ms_at(v.now);
    if old_ms >= v.own_ms {
        let behind = old_ms - v.own_ms;
        if behind <= CLOSE_MS || v.seeks >= MAX_SEEKS {
            return SyncStep::Go;
        }
        // A frozen picture is caught exactly; a moving one where it will be once the seek is done.
        let target = if old.frozen {
            old_ms
        } else {
            old_ms + v.lead_ms
        };
        return if v.seek_anywhere || target <= v.cached_until_ms {
            SyncStep::Seek(target)
        } else {
            // A live buffer still filling up to it (the spare started further back).
            SyncStep::Wait
        };
    }
    let ahead = v.own_ms - old_ms;
    if old.frozen && ahead <= FROZEN_SLACK_MS {
        return SyncStep::Go;
    }
    if v.seek_anywhere && ahead > WAIT_AHEAD_MAX_MS && v.seeks < MAX_SEEKS {
        return SyncStep::Seek(old_ms + v.lead_ms);
    }
    SyncStep::Wait
}

/// How far ahead the next seek aims, after one that took `took`: long enough for a seek as slow
/// as that one, and never less than [`SEEK_LEAD_MS`].
pub fn next_lead_ms(took: Duration) -> u64 {
    let took = u64::try_from(took.as_millis()).unwrap_or(u64::MAX);
    (took.saturating_mul(3) / 2 + 100).max(SEEK_LEAD_MS)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn reading(ms: u64, at: Instant) -> ClockReading {
        ClockReading {
            ms,
            at,
            paused: false,
            frozen: false,
            ahead_ms: 4_000,
        }
    }

    /// A live spare paused at `own` with 10 s buffered, the old picture at `old`.
    fn view(own: u64, old: Option<u64>) -> SyncView {
        let now = Instant::now();
        SyncView {
            own_ms: own,
            cached_until_ms: own + 10_000,
            old: old.map(|ms| reading(ms, now)),
            now,
            seek_anywhere: false,
            seeks: 0,
            ready_for: Duration::from_millis(200),
            lead_ms: SEEK_LEAD_MS,
        }
    }

    #[test]
    fn a_spare_ahead_of_the_old_picture_waits_for_it_and_goes_once_it_is_reached() {
        // 1.5 s ahead: the old one plays on until it gets there.
        assert_eq!(sync_step(&view(101_500, Some(100_000))), SyncStep::Wait);
        assert_eq!(sync_step(&view(101_500, Some(101_499))), SyncStep::Wait);
        // Reached, or just past: go (a repeat of at most 0.1 s).
        assert_eq!(sync_step(&view(101_500, Some(101_500))), SyncStep::Go);
        assert_eq!(sync_step(&view(101_500, Some(101_600))), SyncStep::Go);
    }

    #[test]
    fn a_spare_behind_seeks_to_where_the_old_picture_will_be() {
        assert_eq!(
            sync_step(&view(95_000, Some(100_000))),
            SyncStep::Seek(100_000 + SEEK_LEAD_MS)
        );
        // Beyond what a live spare has buffered: it waits for more.
        let mut short = view(95_000, Some(100_000));
        short.cached_until_ms = 100_200;
        assert_eq!(sync_step(&short), SyncStep::Wait);
        // A video seeks anywhere.
        short.seek_anywhere = true;
        assert_eq!(sync_step(&short), SyncStep::Seek(100_600));
        // Out of seeks: it goes from where it is, behind, never ahead.
        let mut tired = view(99_500, Some(100_000));
        tired.seeks = MAX_SEEKS;
        assert_eq!(sync_step(&tired), SyncStep::Go);
    }

    #[test]
    fn a_frozen_old_picture_is_caught_exactly() {
        let mut v = view(95_000, Some(100_000));
        v.old.as_mut().unwrap().frozen = true;
        assert_eq!(sync_step(&v), SyncStep::Seek(100_000));
        // An exact seek lands on the next frame: close enough.
        let mut v = view(100_033, Some(100_000));
        v.old.as_mut().unwrap().frozen = true;
        assert_eq!(sync_step(&v), SyncStep::Go);
        let mut v = view(100_500, Some(100_000));
        v.old.as_mut().unwrap().frozen = true;
        assert_eq!(sync_step(&v), SyncStep::Wait);
    }

    #[test]
    fn a_paused_or_silent_old_player_or_a_long_wait_means_go() {
        // Back to live from a pause: not to where the pause was.
        let mut v = view(101_500, Some(100_000));
        v.old.as_mut().unwrap().paused = true;
        assert_eq!(sync_step(&v), SyncStep::Go);
        assert_eq!(sync_step(&view(101_500, None)), SyncStep::Go);
        let mut v = view(101_500, Some(100_000));
        v.ready_for = SYNC_MAX;
        assert_eq!(sync_step(&v), SyncStep::Go);
    }

    #[test]
    fn a_video_far_ahead_seeks_back_but_a_live_stream_waits() {
        let mut v = view(200_000, Some(100_000));
        assert_eq!(sync_step(&v), SyncStep::Wait);
        v.seek_anywhere = true;
        assert_eq!(sync_step(&v), SyncStep::Seek(100_000 + SEEK_LEAD_MS));
    }

    #[test]
    fn a_reading_is_carried_forward_a_little_while_it_plays() {
        let at = Instant::now();
        let r = reading(5_000, at);
        assert_eq!(r.ms_at(at + Duration::from_millis(30)), 5_030);
        assert_eq!(
            r.ms_at(at + Duration::from_secs(5)),
            5_100,
            "not past 0.1 s"
        );
        let paused = ClockReading { paused: true, ..r };
        assert_eq!(paused.ms_at(at + Duration::from_millis(30)), 5_000);
    }

    #[test]
    fn the_lead_grows_with_a_slow_seek() {
        assert_eq!(next_lead_ms(Duration::from_millis(100)), SEEK_LEAD_MS);
        assert_eq!(next_lead_ms(Duration::from_millis(800)), 1_300);
    }

    #[test]
    fn a_clock_is_published_while_watched() {
        let clock = PlayerClock::new();
        assert!(!clock.is_watched());
        let watch = clock.watch();
        let other = clock.watch();
        assert!(clock.is_watched());
        drop(watch);
        assert!(clock.is_watched());
        drop(other);
        assert!(!clock.is_watched());
        assert_eq!(clock.read(), None);
        let r = reading(7_000, Instant::now());
        clock.publish(Some(r));
        assert_eq!(clock.read(), Some(r));
        assert!(clock.is(&clock.clone()));
        assert!(!clock.is(&PlayerClock::new()));
    }
}
