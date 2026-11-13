//! The live stream's broadcast as a video, and waking up (vod-plan.md, phase 7).
//!
//! While a Twitch stream plays, Twitch records it as a past broadcast (`live_archive`). The app
//! looks it up when the stream starts, so that when the stream ends under the viewer the end card
//! can offer it: from where they joined, from the start, or (after the device slept) from where
//! they left off. A raid card that was turned down stays as the card's first row.
//!
//! Waking up: the reducer has no clock, so [`WakeCheck`] runs in the app's store, in front of the
//! reducer. It is fed the wall clock with every player report of the single player and turns a
//! gap of more than [`WAKE_GAP_MS`] between two health reports into `Event::DeviceWoke`, reduced
//! before the report after the gap, so the state still holds the last report before it.

use crate::domain::ChannelRef;
use crate::msg::{Command, Event, Msg, MAIN_SLOT};

/// How far the picture of a live stream runs behind the broadcast, in seconds (Twitch's latency,
/// plus the few seconds the recording starts after the stream). A point watched at wall time `t`
/// is about `t - started_at - LIVE_BEHIND_S` into the broadcast's video. Rather a little early
/// than late: seeing a few seconds again beats missing them.
pub const LIVE_BEHIND_S: u64 = 8;

/// Health reports of a playing player come about once a second. Two further apart than this on
/// the wall clock mean the device slept in between (`Event::DeviceWoke`). It has to be the wall
/// clock: on Linux `Instant` does not advance during suspend.
pub const WAKE_GAP_MS: u64 = 30_000;

/// After a wake, a video that has played on this far past where it was is fine: a failure later
/// is an ordinary one, not an address that expired in the sleep.
pub const WAKE_SETTLE_MS: u64 = 60_000;

/// A video that ends more than this before its length after a wake ended early: its address
/// probably expired in the sleep.
pub const EARLY_END_MS: u64 = 30_000;

/// The past broadcast being recorded from the live stream (`Event::LiveArchive`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LiveArchive {
    /// The Twitch video id (`https://www.twitch.tv/videos/<id>`).
    pub video_id: String,
    /// When the live stream started (`stream.createdAt`), in Unix seconds.
    pub started_at: u64,
    /// When the app looked it up, in Unix seconds: the reducer's clock for *from where you
    /// joined* (it is looked up when the stream starts playing).
    pub checked_at: u64,
}

/// A raid the viewer turned down: the end card offers it again (*Follow the raid to X*).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RaidTarget {
    pub login: String,
    /// The name as the streamer writes it.
    pub name: String,
}

/// What the live player knows about the broadcast on screen, for the end card and the *You were
/// away* toast (`AppState::broadcast`). It belongs to one channel, and to one watch of it: a
/// stream opened again starts a new one. It outlives a stream parked for a video, so B from the
/// broadcast's video brings back the same end card.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Broadcast {
    pub channel: ChannelRef,
    /// The broadcast's video, once looked up. Kept when a later lookup finds none (the stream
    /// ended, so Twitch no longer calls it recording).
    pub archive: Option<LiveArchive>,
    /// When the viewer joined this broadcast (Unix seconds): the first lookup of its video.
    pub joined_at: u64,
    /// A raid card the viewer turned down.
    pub raid: Option<RaidTarget>,
    /// The device slept while the stream played: the wall time (Unix seconds) of the last health
    /// report before it.
    pub left_off_at: Option<u64>,
    /// A wake whose toast waits for the broadcast's video to be looked up again: how long the
    /// device slept, in ms.
    pub away_ms: Option<u64>,
    /// The device just woke and the player has not reported since: an error now is the sleep's
    /// doing (the connection dropped), and is treated as the stream ending.
    pub just_woke: bool,
}

impl Broadcast {
    pub fn new(channel: ChannelRef) -> Self {
        Self {
            channel,
            archive: None,
            joined_at: 0,
            raid: None,
            left_off_at: None,
            away_ms: None,
            just_woke: false,
        }
    }

    /// Seconds into the broadcast's video of the moment watched at wall time `at`. `None` without
    /// the video, or for a moment before it started (another broadcast).
    pub fn offset_of(&self, at: u64) -> Option<u32> {
        let a = self.archive.as_ref()?;
        if at < a.started_at || at == 0 {
            return None;
        }
        let s = (at - a.started_at).saturating_sub(LIVE_BEHIND_S);
        Some(u32::try_from(s).unwrap_or(u32::MAX))
    }

    /// *Watch from where you left off*: the moment before the device slept.
    pub fn left_off_s(&self) -> Option<u32> {
        self.offset_of(self.left_off_at?)
    }

    /// *Watch from where you joined*.
    pub fn joined_s(&self) -> Option<u32> {
        self.offset_of(self.joined_at)
    }
}

/// The rows of the live end card (vod-plan.md section 2), top to bottom. Which are shown depends
/// on what is known (`AppState::live_end_rows`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LiveEndRow {
    /// *Follow the raid to X*: a raid card was turned down.
    FollowRaid,
    /// *Watch from where you left off*: after the device slept.
    LeftOff,
    /// *Watch from where you joined*: once the broadcast's video is known.
    Joined,
    /// *Watch it from the start*: once the broadcast's video is known.
    FromStart,
    /// *Channel videos*: the channel page over the end card (the recheck runs on).
    ChannelVideos,
    Back,
}

impl LiveEndRow {
    /// The row `by` rows away in `rows` (negative: up), stopping at the ends. A row not in `rows`
    /// goes to the default one.
    pub fn step(self, rows: &[LiveEndRow], by: isize) -> Self {
        let Some(at) = rows.iter().position(|r| *r == self) else {
            return Self::default_in(rows);
        };
        let to = (at as isize + by).clamp(0, rows.len() as isize - 1);
        rows[to as usize]
    }

    /// Where the focus starts: the first row that watches something, not the raid the viewer
    /// just turned down.
    pub fn default_in(rows: &[LiveEndRow]) -> Self {
        rows.iter()
            .copied()
            .find(|r| *r != LiveEndRow::FollowRaid)
            .unwrap_or(LiveEndRow::Back)
    }
}

/// Tells a sleep from a gap that has another reason, in front of the reducer (see the module
/// docs). The store feeds it every message with the wall clock (`before`) and every batch of
/// commands (`after`).
///
/// Only the single player's slot counts (no videos and no positions in multiview). A gap counts
/// only between two health reports of one playing load: a pause (a paused player sends no
/// health), a new load, a stop, an end or an error starts over. Paused videos send no health, so a
/// long pause is never taken for a sleep.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct WakeCheck {
    /// Wall-clock ms of the last health report since the player last started, paused or stopped.
    last_ms: Option<u64>,
}

impl WakeCheck {
    /// `msg` is about to be reduced at wall-clock ms `wall_ms`. A health report, an end or an
    /// error after a gap returns `Event::DeviceWoke`, to reduce first.
    pub fn before(&mut self, msg: &Msg, wall_ms: u64) -> Option<Event> {
        let Msg::Event(event) = msg else {
            return None;
        };
        let (gap_counts, playing_after) = match event {
            Event::PlayerHealth { slot, .. } if *slot == MAIN_SLOT => (true, true),
            Event::PlaybackEnded { slot } | Event::PlaybackError { slot, .. }
                if *slot == MAIN_SLOT =>
            {
                (true, false)
            }
            Event::PlaybackStarted { slot } if *slot == MAIN_SLOT => (false, false),
            _ => return None,
        };
        let woke = self
            .last_ms
            .filter(|last| gap_counts && wall_ms.saturating_sub(*last) > WAKE_GAP_MS)
            .map(|last| Event::DeviceWoke {
                asleep_ms: wall_ms - last,
                before_s: last / 1_000,
            });
        self.last_ms = playing_after.then_some(wall_ms);
        woke
    }

    /// The reducer asked for these: a pause, a load or a stop of the single player starts over.
    pub fn after(&mut self, cmds: &[Command]) {
        let restarts = cmds.iter().any(|c| {
            matches!(
                c,
                Command::SetPaused {
                    slot: MAIN_SLOT,
                    paused: true
                } | Command::Play {
                    slot: MAIN_SLOT,
                    ..
                } | Command::PlayVideo {
                    slot: MAIN_SLOT,
                    ..
                } | Command::StopPlayback { slot: MAIN_SLOT }
            )
        });
        if restarts {
            self.last_ms = None;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn archive(started_at: u64) -> LiveArchive {
        LiveArchive {
            video_id: "1".into(),
            started_at,
            checked_at: started_at + 600,
        }
    }

    #[test]
    fn offsets_count_from_the_stream_start_less_the_latency() {
        let mut b = Broadcast::new(ChannelRef::twitch("c"));
        assert_eq!(b.offset_of(5_000), None, "no video yet");
        b.archive = Some(archive(1_000));
        assert_eq!(b.offset_of(1_000 + 3_600), Some(3_600 - 8));
        assert_eq!(b.offset_of(1_003), Some(0), "never before the start");
        assert_eq!(b.offset_of(999), None, "another broadcast");
        assert_eq!(b.left_off_s(), None);
        b.left_off_at = Some(1_000 + 7_800);
        assert_eq!(b.left_off_s(), Some(7_792));
        b.joined_at = 1_600;
        assert_eq!(b.joined_s(), Some(592));
    }

    #[test]
    fn rows_step_and_start_past_the_raid() {
        use LiveEndRow::*;
        let rows = [FollowRaid, Joined, FromStart, ChannelVideos, Back];
        assert_eq!(LiveEndRow::default_in(&rows), Joined);
        assert_eq!(Joined.step(&rows, -1), FollowRaid);
        assert_eq!(FollowRaid.step(&rows, -1), FollowRaid);
        assert_eq!(Back.step(&rows, 1), Back);
        assert_eq!(
            LeftOff.step(&rows, 1),
            Joined,
            "a row that went: the default"
        );
        assert_eq!(
            LiveEndRow::default_in(&[ChannelVideos, Back]),
            ChannelVideos
        );
    }
}
