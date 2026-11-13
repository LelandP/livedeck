//! Seeking in a video with ◀ and ▶ (vod-plan.md section 2, *Seeking, in detail*), modelled on the
//! YouTube TV app. Every constant that decides how it feels is here, to tune on a real TV.
//!
//! - **Tap:** each press moves the target [`SEEK_STEP_MS`], and presses less than
//!   [`TAP_JOIN_MS`] apart add up (the chip says *+30 s*). The seek goes out
//!   [`SEEK_SETTLE_MS`] after the last tap is let go, so a burst of taps is one jump. The video
//!   plays on meanwhile.
//! - **Hold:** a press still down after [`HOLD_AFTER_MS`] scrubs. The video pauses on its frame and
//!   only the playhead moves, [`SCRUB_TICK_MS`] apart: [`SCRUB_FIRST_STEP_MS`] of video per tick
//!   at first, doubling every [`SCRUB_DOUBLING_TICKS`] ticks (0.8 s), never more than
//!   [`SCRUB_CAP_PER_MILLE`] thousandths of the video's length per tick.
//! - **Release:** the video jumps to the playhead [`SEEK_SETTLE_MS`] later, and plays again unless
//!   it was paused before. The wait lets a rocker D-pad, which passes through neither direction,
//!   reverse the scrub: the other direction within it carries on the other way.
//! - **While scrubbing:** the other direction reverses (the speed-up starts over), B cancels back
//!   to where it started, and A jumps at once. A scrub ends by itself after
//!   [`SCRUB_MAX_TICKS`] ticks held, in case a release was lost.
//! - **Again:** a press of the same direction within [`SCRUB_CARRY_MS`] of letting go of a scrub
//!   that jumped starts its hold at about [`SCRUB_CARRY_PERCENT`]% of the speed that scrub
//!   reached ([`carried_ticks`]), and speeds up from there. The other direction, a scrub B
//!   cancelled, a tap in between or a longer gap start from the slowest step.
//!
//! The reducer has no clock: the waits are timer commands with ids (`Command::SettleSeek`,
//! `HoldCheck`, `ScrubTick`), and only the latest id counts. The end of the carry-over window
//! is a `SettleSeek` with its own id ([`Carry::id`]), so the other timers do not stop it.

use crate::msg::Dir;

/// How far one tap moves.
pub const SEEK_STEP_MS: u64 = 10_000;
/// Taps less than this apart (from letting go of one to pressing the next) add up into one run.
pub const TAP_JOIN_MS: u64 = 600;
/// The seek of a run of taps goes out this long after the last one is let go. A scrub jumps this
/// long after its release.
pub const SEEK_SETTLE_MS: u64 = 300;
/// A press still down after this long is a hold: it scrubs.
pub const HOLD_AFTER_MS: u64 = 400;
/// Time between two scrub steps (10 a second).
pub const SCRUB_TICK_MS: u64 = 100;
/// The first scrub steps move this much video each.
pub const SCRUB_FIRST_STEP_MS: u64 = 1_000;
/// The step doubles after this many ticks (0.8 s). Was 10 (a second) until 2026-11-13, which felt
/// about 20% too slow on a TV: 8 reaches each speed 25% sooner. The cap is unchanged.
pub const SCRUB_DOUBLING_TICKS: u32 = 8;
/// A new hold of the same direction within this long of letting go of a scrub (counted from the
/// release to the press) picks up near the speed that scrub had. For tuning on a TV.
pub const SCRUB_CARRY_MS: u64 = 1_500;
/// How much of that speed it picks up, in percent: the new scrub starts at the fastest step of
/// the ramp at or under this share of the last step, and speeds up from there. For tuning on a
/// TV.
pub const SCRUB_CARRY_PERCENT: u64 = 50;
/// The largest step, in thousandths of the video's length: 1%. On an 18 h broadcast about 11 min
/// a tick, on a 10 min highlight 6 s.
pub const SCRUB_CAP_PER_MILLE: u64 = 10;
/// A scrub held this many ticks (30 s) ends by itself, as if let go.
pub const SCRUB_MAX_TICKS: u32 = 300;
/// LB goes to the previous chapter instead of the start of this one within this long of its start.
pub const CHAPTER_BACK_MS: u64 = 5_000;
/// A seek never goes closer to the end than this, so it lands on a picture, not on the end.
pub const END_GUARD_MS: u64 = 1_000;
/// After a seek, health reports still on their way from before it are ignored, up to this many,
/// unless they are near the new spot: the playhead must not flick back.
pub const SEEK_LAND_REPORTS: u8 = 3;
/// A report this far before the target of a seek still counts as landed (mpv lands on a frame).
pub const SEEK_LAND_BEFORE_MS: u64 = 2_000;
/// A report this far after the target of a seek still counts as landed (the video played on).
pub const SEEK_LAND_AFTER_MS: u64 = 8_000;
/// A quality change loads behind the picture. If no first frame came after this many health
/// reports, the new quality failed quietly (the pool drops it and the old one plays on), and
/// seeks go to the player again.
pub const RELOAD_GIVE_UP_REPORTS: u32 = 20;
/// The speeds the player menu offers, in thousandths. Every video starts at 1×.
pub const SPEEDS: [u32; 5] = [750, 1_000, 1_250, 1_500, 2_000];
/// Normal speed, in thousandths.
pub const NORMAL_SPEED: u32 = 1_000;

/// The speed after (or, with `forward == false`, before) `milli` in [`SPEEDS`], wrapping around.
/// A speed that is not one of them goes to normal.
pub fn cycled_speed(milli: u32, forward: bool) -> u32 {
    let len = SPEEDS.len() as isize;
    let Some(at) = SPEEDS.iter().position(|s| *s == milli) else {
        return NORMAL_SPEED;
    };
    let next = (at as isize + if forward { 1 } else { -1 }).rem_euclid(len);
    SPEEDS[next as usize]
}

/// How far the playhead moves on scrub tick `tick` (0 for the first) of a video `length_ms`
/// long. A video of unknown length (0) has no cap.
pub fn scrub_step(tick: u32, length_ms: u64) -> u64 {
    let doublings = (tick / SCRUB_DOUBLING_TICKS).min(40);
    let step = SCRUB_FIRST_STEP_MS.saturating_mul(1u64 << doublings);
    if length_ms == 0 {
        return step;
    }
    step.min((length_ms * SCRUB_CAP_PER_MILLE / 1_000).max(1))
}

/// Where the ramp of a new scrub starts (its first `Scrub::ticks`) when it carries on from one
/// that was `ticks` into its ramp: the start of the fastest doubling whose step is at most
/// [`SCRUB_CARRY_PERCENT`] of the last step taken. 0, the slowest, when even the first step is
/// faster than that. The cap still holds, as every step goes through [`scrub_step`].
pub fn carried_ticks(ticks: u32, length_ms: u64) -> u32 {
    let Some(last) = ticks.checked_sub(1) else {
        return 0;
    };
    let target = scrub_step(last, length_ms).saturating_mul(SCRUB_CARRY_PERCENT) / 100;
    let mut start = 0;
    let mut tick = 0;
    while tick <= last && scrub_step(tick, length_ms) <= target {
        start = tick;
        tick += SCRUB_DOUBLING_TICKS;
    }
    start
}

/// Which way, as a signed step.
fn signed(dir: Dir, ms: u64) -> i64 {
    let ms = i64::try_from(ms).unwrap_or(i64::MAX);
    match dir {
        Dir::Left => -ms,
        Dir::Right => ms,
    }
}

/// Taps adding up: where the run started and how far its taps have moved so far.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TapRun {
    /// Where the video was at the first tap, in ms.
    pub from_ms: u64,
    /// The taps added up, in ms: +30 000 after three taps forward.
    pub delta_ms: i64,
    /// The last target sent for this run. A tap after the seek went out (within
    /// [`TAP_JOIN_MS`]) still adds up, and sends the new total.
    pub sent_ms: Option<u64>,
}

impl TapRun {
    /// A run starting at `from_ms` with its first tap.
    pub fn new(from_ms: u64, dir: Dir) -> Self {
        Self {
            from_ms,
            delta_ms: signed(dir, SEEK_STEP_MS),
            sent_ms: None,
        }
    }

    /// One more tap.
    pub fn tap(&mut self, dir: Dir) {
        self.delta_ms = self.delta_ms.saturating_add(signed(dir, SEEK_STEP_MS));
    }

    /// Takes a tap back (the press turned out to be a hold).
    pub fn untap(&mut self, dir: Dir) {
        self.delta_ms = self.delta_ms.saturating_sub(signed(dir, SEEK_STEP_MS));
    }

    /// Where the taps lead, before keeping it inside the video.
    pub fn raw_target(&self) -> u64 {
        let to = i128::from(self.from_ms) + i128::from(self.delta_ms);
        u64::try_from(to.max(0)).unwrap_or(u64::MAX)
    }

    /// Where the video is now as far as this run knows: the last target sent, or the start.
    pub fn video_at(&self) -> u64 {
        self.sent_ms.unwrap_or(self.from_ms)
    }
}

/// A scrub: the video is paused and only the playhead moves.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Scrub {
    pub dir: Dir,
    /// The button is still held. Once let go the playhead stops and the jump waits
    /// [`SEEK_SETTLE_MS`] for the other direction.
    pub held: bool,
    /// Where it started: B goes back there.
    pub from_ms: u64,
    /// The playhead.
    pub at_ms: u64,
    /// Steps since this direction was pressed: the speed-up.
    pub ticks: u32,
    /// Steps since the scrub began, for [`SCRUB_MAX_TICKS`].
    pub total_ticks: u32,
}

impl Scrub {
    /// Moves the playhead one step, staying inside `[0, end_ms]`.
    pub fn step(&mut self, length_ms: u64, end_ms: u64) {
        let step = scrub_step(self.ticks, length_ms);
        self.ticks = self.ticks.saturating_add(1);
        self.at_ms = match self.dir {
            Dir::Left => self.at_ms.saturating_sub(step),
            Dir::Right => self.at_ms.saturating_add(step).min(end_ms),
        };
    }
}

/// A scrub that was let go and jumped, for [`SCRUB_CARRY_MS`]: a hold of the same direction
/// soon after picks up near its speed (`VideoSession::carry`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Carry {
    pub dir: Dir,
    /// Where the next scrub's ramp starts ([`carried_ticks`]).
    pub ticks: u32,
    /// The id of the `SettleSeek` that ends the window.
    pub id: u32,
}

/// Where ◀ ▶ are in a video.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Seeking {
    #[default]
    Idle,
    /// ◀ or ▶ is down, and may turn out to be a hold (`Command::HoldCheck` is running). Its tap
    /// is already in `run`. `carry` is where the scrub's ramp starts if it is a hold: above 0
    /// when it came soon after a scrub the same way ([`Carry`]).
    Pressed { dir: Dir, run: TapRun, carry: u32 },
    /// Let go: the run waits for more taps. Not `settled`: the seek goes out at the next
    /// `SeekSettleDue`. `settled`: it went out, and the run ends at the next one.
    Taps { run: TapRun, settled: bool },
    /// Held: scrubbing.
    Scrub(Scrub),
}

impl Seeking {
    /// The tap run under way, if any.
    pub fn run(&self) -> Option<&TapRun> {
        match self {
            Seeking::Pressed { run, .. } | Seeking::Taps { run, .. } => Some(run),
            Seeking::Idle | Seeking::Scrub(_) => None,
        }
    }

    pub fn scrub(&self) -> Option<&Scrub> {
        match self {
            Seeking::Scrub(scrub) => Some(scrub),
            _ => None,
        }
    }

    pub fn is_idle(&self) -> bool {
        *self == Seeking::Idle
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const HOUR: u64 = 3_600_000;

    #[test]
    fn the_scrub_speeds_up_every_0_8_seconds_up_to_one_percent() {
        // 18 h: 1 s, 2 s, 4 s ... until 1% (648 s) caps it, 8 s in (10 s before 2026-11-13).
        let long = 18 * HOUR;
        assert_eq!(scrub_step(0, long), 1_000);
        assert_eq!(scrub_step(7, long), 1_000);
        assert_eq!(scrub_step(8, long), 2_000);
        assert_eq!(scrub_step(16, long), 4_000);
        assert_eq!(scrub_step(72, long), 512_000);
        assert_eq!(scrub_step(79, long), 512_000);
        assert_eq!(scrub_step(80, long), 648_000, "about 11 minutes a tick");
        assert_eq!(scrub_step(10_000, long), 648_000, "and never more");
        // A 10 min highlight never passes 6 s a tick.
        let short = 600_000;
        assert_eq!(scrub_step(16, short), 4_000);
        assert_eq!(scrub_step(24, short), 6_000);
        assert_eq!(scrub_step(300, short), 6_000);
        // A 30 s clip: 0.3 s from the first tick.
        assert_eq!(scrub_step(0, 30_000), 300);
        // Unknown length: no cap.
        assert_eq!(scrub_step(24, 0), 8_000);
    }

    #[test]
    fn a_carried_scrub_starts_near_half_the_speed_it_had() {
        let long = 18 * HOUR;
        // Not into the ramp, or still at the first steps: from the slowest.
        assert_eq!(carried_ticks(0, long), 0);
        assert_eq!(carried_ticks(8, long), 0);
        // Last step 2 s: back to the 1 s steps, which is the start anyway.
        assert_eq!(carried_ticks(9, long), 0);
        // Last step 4 s: from the start of the 2 s steps.
        assert_eq!(carried_ticks(17, long), 8);
        assert_eq!(scrub_step(carried_ticks(17, long), long), 2_000);
        // Capped at 648 s: the fastest doubling at or under 324 s is 256 s.
        assert_eq!(carried_ticks(150, long), 64);
        assert_eq!(scrub_step(64, long), 256_000);
        // A 10 min highlight capped at 6 s: from the 2 s steps.
        assert_eq!(carried_ticks(100, 600_000), 8);
        // A clip whose cap is under the first step: from the start.
        assert_eq!(carried_ticks(50, 30_000), 0);
    }

    #[test]
    fn taps_add_up_and_never_go_before_the_start() {
        let mut run = TapRun::new(30_000, Dir::Right);
        run.tap(Dir::Right);
        run.tap(Dir::Right);
        assert_eq!(run.delta_ms, 30_000);
        assert_eq!(run.raw_target(), 60_000);
        run.untap(Dir::Right);
        assert_eq!(run.raw_target(), 50_000);
        let mut back = TapRun::new(15_000, Dir::Left);
        back.tap(Dir::Left);
        assert_eq!(back.delta_ms, -20_000);
        assert_eq!(back.raw_target(), 0);
        assert_eq!(back.video_at(), 15_000);
    }

    #[test]
    fn speeds_step_and_wrap() {
        assert_eq!(cycled_speed(1_000, true), 1_250);
        assert_eq!(cycled_speed(2_000, true), 750);
        assert_eq!(cycled_speed(750, false), 2_000);
        assert_eq!(cycled_speed(1_100, true), NORMAL_SPEED);
    }

    #[test]
    fn a_scrub_step_stays_inside_the_video() {
        let mut scrub = Scrub {
            dir: Dir::Right,
            held: true,
            from_ms: 0,
            at_ms: 598_500,
            ticks: 0,
            total_ticks: 0,
        };
        scrub.step(600_000, 599_000);
        assert_eq!(scrub.at_ms, 599_000);
        scrub.dir = Dir::Left;
        scrub.at_ms = 500;
        scrub.step(600_000, 599_000);
        assert_eq!(scrub.at_ms, 0);
        assert_eq!(scrub.ticks, 2);
    }
}
