//! The slow-network rule (plan.md, section 4): lower the quality of some streams when the network
//! cannot keep up, and raise it again once it can. Pure: health reports come in with the time they
//! were taken, and the answer is whether the limit moved.
//!
//! - **Struggling:** a stream stalled, or its buffer stayed under [`LOW_BUFFER_MS`] for
//!   [`LOW_FOR_MS`]. The limit then goes one step down, at most every [`STEP_GAP_MS`], lowering
//!   the first [`Tier`] that can still go lower: muted small tiles, then muted big ones, then the
//!   stream with sound. So the stream being listened to keeps its quality the longest.
//! - **Calm:** after [`CALM_MS`] of healthy reports (no stall, every buffer at least
//!   [`LOW_BUFFER_MS`]), one step comes back, the stream with sound first. A struggle within
//!   [`RELAPSE_MS`] of rising drops that step again at once and doubles the wait, up to
//!   [`CALM_MAX_MS`]. A rise that lasts brings the wait back to [`CALM_MS`].
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
/// The shortest time between two steps, so each reload can settle before the next judgement.
pub const STEP_GAP_MS: u64 = 10_000;
/// Healthy reports for this long bring one step back.
pub const CALM_MS: u64 = 120_000;
/// The longest the wait for a step back grows to.
pub const CALM_MAX_MS: u64 = 600_000;
/// A struggle this soon after a step back undoes it, and doubles the wait.
pub const RELAPSE_MS: u64 = 30_000;
/// Reports further apart than this do not join up: the stream reloaded or was paused, or nothing
/// was watched. A low-buffer spell, or a calm spell, starts again.
pub const REPORT_GAP_MS: u64 = 2_500;

/// The limit moved one step.
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
    /// When the limit last moved.
    last_change: Option<u64>,
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

    /// Takes the report of the player on `slot`, taken at `at`. `room` says whether a tier has a
    /// stream on screen that can still go lower. Returns the step the limit moved, if it did.
    pub fn report(
        &mut self,
        slot: Slot,
        health: Health,
        at: u64,
        room: impl Fn(Tier) -> bool,
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
            return self.struggle(at, room);
        }
        if low {
            // Not a struggle yet, but not calm either.
            self.calm_since = Some(at);
            return None;
        }
        self.calm(at)
    }

    fn struggle(&mut self, at: u64, room: impl Fn(Tier) -> bool) -> Option<Change> {
        self.calm_since = Some(at);
        if let Some((rose_at, tier)) = self.rose.take() {
            if at.saturating_sub(rose_at) < RELAPSE_MS {
                // The step back was too soon: drop it again now, and wait longer next time.
                self.calm_needed = (self.calm_needed * 2).min(CALM_MAX_MS);
                return Some(self.lower(tier, at));
            }
        }
        if self
            .last_change
            .is_some_and(|t| at.saturating_sub(t) < STEP_GAP_MS)
        {
            return None;
        }
        let tier = Tier::LOWERED_FIRST.into_iter().find(|t| room(*t))?;
        Some(self.lower(tier, at))
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
        if at.saturating_sub(since) < self.calm_needed {
            return None;
        }
        let tier = Tier::LOWERED_FIRST
            .into_iter()
            .rev()
            .find(|t| self.steps(*t) > 0)?;
        self.lowered[tier.index()] -= 1;
        self.rose = Some((at, tier));
        self.moved(at);
        Some(Change::Raised(tier))
    }

    fn lower(&mut self, tier: Tier, at: u64) -> Change {
        let steps = &mut self.lowered[tier.index()];
        *steps = steps.saturating_add(1);
        self.moved(at);
        Change::Lowered(tier)
    }

    /// The limit moved at `at`: streams reload, so spells start again.
    fn moved(&mut self, at: u64) {
        self.last_change = Some(at);
        self.calm_since = Some(at);
        self.low = [None; MAX_STREAMS];
    }
}
