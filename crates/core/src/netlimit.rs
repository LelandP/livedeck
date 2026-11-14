//! The slow-network rule (plan.md, section 4): lower the quality of some streams when the network
//! cannot keep up, and raise it again once it can. Pure: health reports come in with the time they
//! were taken, and the answer is whether the limit moved.
//!
//! - **Struggling:** a stream stalled, or its buffer stayed under [`LOW_BUFFER_MS`] for
//!   [`LOW_FOR_MS`]. The limit then goes down, lowering the first [`Tier`] that can still go
//!   lower: muted small tiles, then muted big ones, then the stream with sound. So the stream
//!   being listened to keeps its quality the longest. A low buffer lowers one step; a stall two
//!   at once where the tier has room, so one reload replaces two (plan 14).
//! - **Calm:** after [`CALM_MS`] of healthy reports (no stall, every buffer at least
//!   [`LOW_BUFFER_MS`]), one step comes back, the stream with sound first. A struggle within
//!   [`RELAPSE_MS`] of rising drops that step again at once and doubles the wait, up to
//!   [`CALM_MAX_MS`]. A rise that lasts brings the wait back to [`CALM_MS`].
//! - **The hold:** two changes are at least [`STEP_GAP_MS`] apart, and each change the rule makes
//!   doubles that gap for the next one, up to [`HOLD_MAX_MS`], so a quality that keeps changing
//!   is held longer. [`HOLD_RESET_MS`] without a change brings it back to [`STEP_GAP_MS`], and so
//!   does a quality picked by hand ([`NetLimit::picked`]). Only the relapse drop above ignores it.
//! - The limit lasts as long as the app runs (it lives in `AppState`).
//!
//! Not used: mpv's download speed (`cache-speed`). On live HLS it read about twice the real rate
//! while the network was slowed, and kept its last value while it was cut (measured 2026-11-07
//! with `crates/player/examples/health_probe.rs`), so it cannot tell whether a higher rendition
//! would fit. A rise into a stall is caught by the relapse rule instead.

use crate::layout::MAX_STREAMS;
use crate::msg::Slot;

/// How one player is doing, about once a second while it plays (`Event::PlayerHealth`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Health {
    /// It ran out of buffer and froze: mpv's `paused-for-cache` with normal latency, or nothing
    /// left buffered with low latency (which never pauses for the cache).
    pub stalled: bool,
    /// Seconds buffered ahead (`demuxer-cache-duration`), in milliseconds.
    pub buffer_ms: u32,
    /// Where a video or clip is, in milliseconds from its start. `None` for a live stream, and
    /// while the player cannot tell. The slow-network rule ignores it.
    pub pos_ms: Option<u64>,
    /// How long a video or clip is, in milliseconds. `None` for a live stream, and while the
    /// player cannot tell. The slow-network rule ignores it.
    pub length_ms: Option<u64>,
    /// Where the stream is on its own raw clock, in milliseconds: the clock the captions' audio
    /// is stamped with (`PlayerHealth::stream_ms`, captions-plan.md). `None` while the player
    /// cannot tell. The slow-network rule ignores it.
    pub stream_ms: Option<u64>,
}

/// Which streams a step lowers, in the order they are lowered.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Tier {
    /// Muted tiles that are not the biggest on screen (every muted tile of a grid).
    MutedSmall,
    /// Muted tiles bigger than the other muted ones.
    MutedBig,
    /// The stream with sound, and the single player's stream.
    Sound,
}

impl Tier {
    /// Lowered in this order, raised in the reverse.
    pub const LOWERED_FIRST: [Tier; 3] = [Tier::MutedSmall, Tier::MutedBig, Tier::Sound];

    fn index(self) -> usize {
        match self {
            Tier::MutedSmall => 0,
            Tier::MutedBig => 1,
            Tier::Sound => 2,
        }
    }
}

/// A buffer under this is low. Low latency keeps 4 to 5 s on a good network.
pub const LOW_BUFFER_MS: u32 = 1_000;
/// A stream struggles once its buffer has been low this long without a break.
pub const LOW_FOR_MS: u64 = 5_000;
/// The shortest time between two changes, so each reload can settle before the next judgement.
/// Each change the rule makes doubles it for the next one, up to [`HOLD_MAX_MS`].
pub const STEP_GAP_MS: u64 = 10_000;
/// The longest the gap between two changes grows to.
pub const HOLD_MAX_MS: u64 = 300_000;
/// This long without a change brings the gap back to [`STEP_GAP_MS`].
pub const HOLD_RESET_MS: u64 = 600_000;
/// The most steps one stall lowers a tier by at once.
pub const STALL_STEPS: u8 = 2;
/// Healthy reports for this long bring one step back.
pub const CALM_MS: u64 = 120_000;
/// The longest the wait for a step back grows to.
pub const CALM_MAX_MS: u64 = 600_000;
/// A struggle this soon after a step back undoes it, and doubles the wait.
pub const RELAPSE_MS: u64 = 30_000;
/// Reports further apart than this do not join up: the stream reloaded or was paused, or nothing
/// was watched. A low-buffer spell, or a calm spell, starts again.
pub const REPORT_GAP_MS: u64 = 2_500;

/// The limit moved: a tier went down (one step, or two on a stall: see [`NetLimit::steps`]) or one
/// step up.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Change {
    Lowered(Tier),
    Raised(Tier),
}

/// A run of low-buffer reports from one player: when it began, and the latest.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct Spell {
    since: u64,
    last: u64,
}

/// The network limit and what it needs to judge the next report. Times are milliseconds on the
/// app's clock (`Event::PlayerHealth::at_ms`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NetLimit {
    /// Steps each tier is lowered by, indexed by `Tier::index`.
    lowered: [u8; 3],
    /// Each slot's low-buffer spell.
    low: [Option<Spell>; MAX_STREAMS],
    /// The latest report of any player.
    last_report: Option<u64>,
    /// When the limit last moved, or a quality was picked by hand.
    last_change: Option<u64>,
    /// The changes the rule made one after the other, each within [`HOLD_RESET_MS`] of the one
    /// before, since the last pick by hand: what the gap after the latest has grown to.
    streak: u32,
    /// Since when every report has been healthy.
    calm_since: Option<u64>,
    /// The latest step back, until it has lasted `RELAPSE_MS`.
    rose: Option<(u64, Tier)>,
    /// How long it must be calm before the next step back.
    calm_needed: u64,
}

impl Default for NetLimit {
    fn default() -> Self {
        Self {
            lowered: [0; 3],
            low: [None; MAX_STREAMS],
            last_report: None,
            last_change: None,
            streak: 0,
            calm_since: None,
            rose: None,
            calm_needed: CALM_MS,
        }
    }
}

impl NetLimit {
    /// How many renditions the streams of `tier` are lowered by.
    pub fn steps(&self, tier: Tier) -> u8 {
        self.lowered[tier.index()]
    }

    /// Some stream is lowered.
    pub fn is_limited(&self) -> bool {
        self.lowered.iter().any(|s| *s > 0)
    }

    /// How long it must now be calm before a step back.
    pub fn calm_needed_ms(&self) -> u64 {
        self.calm_needed
    }

    /// The shortest gap after the latest change before the rule makes another (a relapse drop
    /// aside): [`STEP_GAP_MS`], doubled for each change the rule made since the hold was last
    /// reset, up to [`HOLD_MAX_MS`].
    pub fn hold_ms(&self) -> u64 {
        let doublings = self.streak.saturating_sub(1).min(16);
        (STEP_GAP_MS << doublings).min(HOLD_MAX_MS)
    }

    /// A quality was picked by hand. It loads at once whatever the hold, and resets the hold: the
    /// rule's next change waits [`STEP_GAP_MS`] from now (the latest report), so the new stream
    /// can settle, and the gaps grow from there again.
    pub fn picked(&mut self) {
        self.streak = 0;
        if let Some(at) = self.last_report {
            self.moved(at);
        }
    }

    /// Takes the report of the player on `slot`, taken at `at`. `room` says how many renditions
    /// lower the streams of a tier on screen can still go (the most of any of them; 0 when none
    /// can). Returns how the limit moved, if it did.
    pub fn report(
        &mut self,
        slot: Slot,
        health: Health,
        at: u64,
        room: impl Fn(Tier) -> u8,
    ) -> Option<Change> {
        let i = usize::from(slot);
        if i >= MAX_STREAMS {
            return None;
        }
        // Nobody reported for a while: what came before says nothing about now.
        if self
            .last_report
            .is_none_or(|t| at.saturating_sub(t) > REPORT_GAP_MS)
        {
            self.calm_since = Some(at);
        }
        self.last_report = Some(at);

        let low = health.buffer_ms < LOW_BUFFER_MS;
        let spell = &mut self.low[i];
        *spell = match *spell {
            _ if !low => None,
            Some(s) if at.saturating_sub(s.last) <= REPORT_GAP_MS => Some(Spell { last: at, ..s }),
            _ => Some(Spell {
                since: at,
                last: at,
            }),
        };
        let long_low = spell.is_some_and(|s| at.saturating_sub(s.since) >= LOW_FOR_MS);

        if health.stalled || long_low {
            return self.struggle(at, health.stalled, room);
        }
        if low {
            // Not a struggle yet, but not calm either.
            self.calm_since = Some(at);
            return None;
        }
        self.calm(at)
    }

    fn struggle(&mut self, at: u64, stalled: bool, room: impl Fn(Tier) -> u8) -> Option<Change> {
        self.calm_since = Some(at);
        if let Some((rose_at, tier)) = self.rose.take() {
            if at.saturating_sub(rose_at) < RELAPSE_MS {
                // The step back was too soon: drop it again now, whatever the hold, and wait
                // longer next time.
                self.calm_needed = (self.calm_needed * 2).min(CALM_MAX_MS);
                return Some(self.lower(tier, 1, at));
            }
        }
        if self.held(at) {
            return None;
        }
        let (tier, room) = Tier::LOWERED_FIRST
            .into_iter()
            .map(|t| (t, room(t)))
            .find(|(_, room)| *room > 0)?;
        let steps = if stalled { room.min(STALL_STEPS) } else { 1 };
        Some(self.lower(tier, steps, at))
    }

    fn calm(&mut self, at: u64) -> Option<Change> {
        if let Some((rose_at, _)) = self.rose {
            if at.saturating_sub(rose_at) >= RELAPSE_MS {
                // The step back held.
                self.rose = None;
                self.calm_needed = CALM_MS;
            }
        }
        let since = *self.calm_since.get_or_insert(at);
        if at.saturating_sub(since) < self.calm_needed || self.held(at) {
            return None;
        }
        let tier = Tier::LOWERED_FIRST
            .into_iter()
            .rev()
            .find(|t| self.steps(*t) > 0)?;
        self.lowered[tier.index()] -= 1;
        self.rose = Some((at, tier));
        self.changed(at);
        Some(Change::Raised(tier))
    }

    /// The latest change was less than the hold ago: the rule must not change anything yet.
    fn held(&self, at: u64) -> bool {
        self.last_change
            .is_some_and(|t| at.saturating_sub(t) < self.hold_ms())
    }

    fn lower(&mut self, tier: Tier, steps: u8, at: u64) -> Change {
        let lowered = &mut self.lowered[tier.index()];
        *lowered = lowered.saturating_add(steps);
        self.changed(at);
        Change::Lowered(tier)
    }

    /// The rule changed the limit at `at`: the hold doubles, unless the last change was so long
    /// ago that it starts again.
    fn changed(&mut self, at: u64) {
        if self
            .last_change
            .is_some_and(|t| at.saturating_sub(t) >= HOLD_RESET_MS)
        {
            self.streak = 0;
        }
        self.streak = self.streak.saturating_add(1);
        self.moved(at);
    }

    /// The streams reload at `at` (the limit moved, or a quality was picked), so spells start
    /// again.
    fn moved(&mut self, at: u64) {
        self.last_change = Some(at);
        self.calm_since = Some(at);
        self.low = [None; MAX_STREAMS];
    }
}
