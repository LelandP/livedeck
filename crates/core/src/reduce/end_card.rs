//! The live end card, raids at the end of a stream, and waking up while a stream plays
//! (vod-plan.md, phase 7).
//!
//! - **The broadcast.** When a Twitch stream starts playing on the single player, its past
//!   broadcast (the video Twitch records while it is live) is looked up (`LoadLiveArchive`), and
//!   kept in `AppState::broadcast` with what else the end card needs: a raid turned down, and
//!   where the viewer left off before the device slept.
//! - **The end card** replaces the offline screen of a Twitch stream that ended (`offline`):
//!   *Follow the raid to X*, *Watch from where you left off*, *Watch from where you joined*,
//!   *Watch it from the start*, *Channel videos*, *Back*. The quiet recheck runs on behind it; the
//!   stream coming back plays and the card goes. Kick keeps the offline screen.
//! - **Raids:** the raid card goes first, over the stream or the card; B turns it down and the
//!   raid becomes the card's first row (now, or when the stream ends).
//! - **Waking up** (`Event::DeviceWoke`): still live, a toast offers the part slept through (X
//!   opens the broadcast there); ended in the sleep, the end card has *Watch from where you left
//!   off*. A video keeps its place (`video::woke`).
//!
//! The broadcast's video plays like any video, over the live player, whose stream is parked
//! (`AppState::parked_live`): B from the video loads the stream again, which brings back the end
//! card if it is still offline.

use crate::broadcast::{Broadcast, LiveArchive, LiveEndRow, RaidTarget};
use crate::domain::ChannelRef;
use crate::msg::{Command, Intent};
use crate::state::{AppState, Playback, RaidOffer, ToastKind, RAID_SECONDS};
use crate::video::{Video, VideoItem, VideoKind};

use super::{back, channel, follow_raid, pulse_hud, push_toast, stop_live, video};

/// The live stream's broadcast, made fresh when there is none for its channel yet.
fn broadcast_mut(s: &mut AppState) -> Option<&mut Broadcast> {
    let channel = s.now_playing.as_ref()?.channel.clone();
    let fresh = s
        .broadcast
        .as_ref()
        .is_none_or(|b| !b.channel.is_same(&channel));
    if fresh {
        s.broadcast = Some(Broadcast::new(channel));
    }
    s.broadcast.as_mut()
}

/// A Twitch stream on the single player (not multiview): the only one with a broadcast to offer.
fn single_twitch(s: &AppState) -> Option<ChannelRef> {
    let stream = s.now_playing.as_ref()?;
    (s.multiview.is_none() && stream.channel.features().videos).then(|| stream.channel.clone())
}

/// The live stream started playing (its first frame, or a recheck brought it back): the end card
/// goes, and the broadcast is looked up (it may be a new one). What belonged to the broadcast
/// that ended goes with the card.
pub(super) fn started(s: &mut AppState, cmds: &mut Vec<Command>) {
    s.end_card = None;
    let Some(channel) = single_twitch(s) else {
        return;
    };
    if let Some(b) = broadcast_mut(s) {
        b.raid = None;
        b.left_off_at = None;
        b.away_ms = None;
        b.just_woke = false;
    }
    cmds.push(Command::LoadLiveArchive { channel });
}

/// The stream on screen ended (the offline screen is up): a Twitch stream gets the end card,
/// focused on the first row that watches something. Rechecks that fail keep it as it is.
pub(super) fn offline(s: &mut AppState) {
    if s.end_card.is_some() || !s.on_live_player() || single_twitch(s).is_none() {
        return;
    }
    s.end_card = Some(LiveEndRow::default_in(&s.live_end_rows()));
}

/// B on the raid card: the viewer stays. The raid is kept for the end card's first row.
pub(super) fn declined(s: &mut AppState) {
    let Some(offer) = s.raid.clone() else {
        return;
    };
    if let Some(b) = broadcast_mut(s) {
        b.raid = Some(RaidTarget {
            login: offer.to_login,
            name: offer.to_name,
        });
    }
}

/// Input on the end card, after the raid card, the menu and the like had their turn. Up and Down
/// pick a row and A runs it; everything else is the offline screen's (`true` if it was taken).
pub(super) fn intent(s: &mut AppState, intent: Intent, cmds: &mut Vec<Command>) -> bool {
    let Some(row) = s.end_card else {
        return false;
    };
    if s.playback != Playback::Offline || s.multiview.is_some() {
        return false;
    }
    let rows = s.live_end_rows();
    match intent {
        Intent::Up => s.end_card = Some(row.step(&rows, -1)),
        Intent::Down => s.end_card = Some(row.step(&rows, 1)),
        Intent::Select => {
            // The row on screen: one that went meanwhile counts as the default.
            let row = row.step(&rows, 0);
            select(s, row, cmds);
            return true;
        }
        _ => return false,
    }
    pulse_hud(s);
    true
}

/// A on a row of the end card.
fn select(s: &mut AppState, row: LiveEndRow, cmds: &mut Vec<Command>) {
    let Some(b) = s.live_broadcast().cloned() else {
        match row {
            LiveEndRow::ChannelVideos => channel::open_from_player(s, cmds),
            LiveEndRow::Back => back(s, cmds),
            _ => {}
        }
        return;
    };
    match row {
        LiveEndRow::FollowRaid => {
            if let Some(raid) = b.raid {
                follow(s, raid, cmds);
            }
        }
        LiveEndRow::LeftOff => {
            if let Some(at) = b.left_off_s() {
                watch(s, at, cmds);
            }
        }
        LiveEndRow::Joined => {
            if let Some(at) = b.joined_s() {
                watch(s, at, cmds);
            }
        }
        LiveEndRow::FromStart => watch(s, 0, cmds),
        // Over the end card, as from the menu while live: the recheck runs on, B comes back.
        LiveEndRow::ChannelVideos => channel::open_from_player(s, cmds),
        LiveEndRow::Back => back(s, cmds),
    }
}

/// *Follow the raid to X*: as A on the raid card would have.
fn follow(s: &mut AppState, raid: RaidTarget, cmds: &mut Vec<Command>) {
    let Some(from) = s.now_playing.as_ref().map(|p| p.channel.login.clone()) else {
        return;
    };
    s.last_raid_id = s.last_raid_id.wrapping_add(1);
    s.raid = Some(RaidOffer {
        id: s.last_raid_id,
        from,
        to_login: raid.login,
        to_name: raid.name,
        viewers: 0,
        seconds_left: RAID_SECONDS,
    });
    follow_raid(s, cmds);
}

/// Plays the broadcast's video from `at_s` seconds in, over the live player. The live stream
/// stops and is parked: B from the video loads it again (still offline: the end card again).
fn watch(s: &mut AppState, at_s: u32, cmds: &mut Vec<Command>) {
    let (Some(b), Some(stream)) = (s.live_broadcast().cloned(), s.now_playing.clone()) else {
        return;
    };
    let Some(archive) = b.archive else {
        return;
    };
    let video = Video {
        id: archive.video_id,
        channel: stream.channel.clone(),
        channel_name: stream.channel.login.clone(),
        kind: VideoKind::Archive,
        title: stream.title.clone(),
        category: stream.category.clone(),
        // The player says once it plays (a broadcast still recording keeps growing).
        length_s: 0,
        created_at: archive.started_at,
        views: 0,
        tint: stream.tint,
        thumbnail: None,
        muted: Vec::new(),
        sub_only: false,
        recording: s.playback == Playback::Playing,
    };
    stop_live(s, cmds);
    s.parked_live = Some(stream);
    video::open(
        s,
        VideoItem::Video(video),
        u64::from(at_s) * 1_000,
        false,
        cmds,
    );
    // The clock for *Streamed Nov 10*: when the broadcast was looked up.
    if let Some(v) = s.video.as_mut().filter(|v| v.now == 0) {
        v.now = archive.checked_at;
    }
}

/// X on the live player while the *You were away* toast is up: the broadcast from where the
/// viewer left off. `false` when there is no such toast (X does what it always did).
pub(super) fn away_x(s: &mut AppState, cmds: &mut Vec<Command>) -> bool {
    let toast = s
        .toasts
        .iter()
        .find(|t| matches!(t.kind, ToastKind::Away { .. }))
        .map(|t| t.id);
    let Some(id) = toast else {
        return false;
    };
    let Some(at) = s.live_broadcast().and_then(Broadcast::left_off_s) else {
        return false;
    };
    s.toasts.retain(|t| t.id != id);
    watch(s, at, cmds);
    true
}

/// The *You were away* toasts go when the stream they are about does.
pub(super) fn drop_away_toasts(s: &mut AppState) {
    s.toasts
        .retain(|t| !matches!(t.kind, ToastKind::Away { .. }));
}

/// `Event::LiveArchive`: the broadcast's video. A lookup that finds none keeps the one known
/// (the stream ended, so it is not "recording" any more). A wake waiting for it gets its toast if
/// the stream still plays and the place before the sleep is in this broadcast.
pub(super) fn archive_loaded(
    s: &mut AppState,
    channel: &ChannelRef,
    archive: Option<LiveArchive>,
    cmds: &mut Vec<Command>,
) {
    let live = s
        .now_playing
        .as_ref()
        .is_some_and(|p| p.channel.is_same(channel));
    if !live {
        return;
    }
    let Some(b) = broadcast_mut(s) else {
        return;
    };
    if let Some(a) = archive {
        let same = b
            .archive
            .as_ref()
            .is_some_and(|old| old.video_id == a.video_id);
        if !same {
            b.joined_at = a.checked_at;
        }
        b.archive = Some(a);
    }
    let Some(away_ms) = b.away_ms.take() else {
        return;
    };
    let offer = b.left_off_s().is_some();
    if offer && s.playback == Playback::Playing && s.on_live_player() {
        push_toast(s, ToastKind::Away { away_ms }, cmds);
    }
}

/// `Event::DeviceWoke`: the device slept while the single player played. A video keeps the place
/// before the sleep. A live Twitch stream remembers it (the end card's *from where you left off*
/// if the stream ended in the sleep) and looks the broadcast up again, for the toast if it is
/// still live (it may also have restarted as another broadcast).
pub(super) fn woke(s: &mut AppState, asleep_ms: u64, before_s: u64, cmds: &mut Vec<Command>) {
    if s.multiview.is_some() || s.paused {
        return;
    }
    if s.video.is_some() {
        video::woke(s);
        return;
    }
    if !s.on_live_player() || s.playback != Playback::Playing {
        return;
    }
    let Some(channel) = single_twitch(s) else {
        return;
    };
    if let Some(b) = broadcast_mut(s) {
        b.left_off_at = Some(before_s);
        b.away_ms = Some(asleep_ms);
        b.just_woke = true;
    }
    cmds.push(Command::LoadLiveArchive { channel });
}

/// A health report of the live stream: it plays on after a wake.
pub(super) fn ticked(s: &mut AppState) {
    if let Some(b) = s.broadcast.as_mut() {
        b.just_woke = false;
    }
}

/// The live player failed just after a wake, with no report in between: the sleep dropped the
/// connection, or the stream ended in it. Treated as the end of the stream (the end card, and the
/// recheck plays it again if it is still live) rather than an error.
pub(super) fn failed_on_waking(s: &AppState) -> bool {
    s.live_broadcast().is_some_and(|b| b.just_woke)
}
