//! The slow-network rule's side of the reducer (plan.md, section 4): players' health reports go to
//! `AppState::net`, and when the limit moves the streams it changed reload at once. The rule
//! itself is in `netlimit.rs`.

use crate::msg::{Command, Slot, StreamKind, MAIN_SLOT};
use crate::netlimit::{Change, Health, Tier};
use crate::quality::Rendition;
use crate::state::{AppState, Playback, ToastKind};

use super::{multiview, push_toast, reload_playback};

/// A health report from the player on `slot`, taken at `at_ms`.
pub(super) fn health(
    s: &mut AppState,
    slot: Slot,
    at_ms: u64,
    health: Health,
    cmds: &mut Vec<Command>,
) {
    if !counts(s, slot) {
        return;
    }
    let room: Vec<(Tier, u8)> = Tier::LOWERED_FIRST
        .into_iter()
        .map(|t| (t, room(s, t)))
        .collect();
    let single_before = s.stream_kind();
    let Some(change) = s.net.report(slot, health, at_ms, |t| {
        room.iter()
            .find(|(tier, _)| *tier == t)
            .map_or(0, |(_, room)| *room)
    }) else {
        return;
    };
    if let Change::Lowered(tier) = change {
        if !s.net_told {
            s.net_told = true;
            push_toast(
                s,
                ToastKind::SlowNetwork {
                    muted: tier != Tier::Sound,
                },
                cmds,
            );
        }
    }
    // Either way the old quality plays on until the new one's first frame.
    if s.multiview.is_some() {
        multiview::reload_stale(s, cmds);
    } else if s.stream_kind() != single_before {
        reload_playback(s, cmds);
    }
}

/// Only a stream that plays is judged: not one loading (its buffer is still filling), offline,
/// failed, paused or sound only. Never a video (vod-plan.md): its buffer runs minutes ahead and
/// empties at every seek, which says nothing about the network (`video::health` takes its
/// reports instead).
fn counts(s: &AppState, slot: Slot) -> bool {
    if s.video.is_some() {
        return false;
    }
    match &s.multiview {
        Some(mv) => mv.place_of_slot(slot).is_some_and(|place| {
            mv.tile(place).is_some_and(|t| t.state == Playback::Playing)
                && !(place == mv.sound && s.paused)
        }),
        None => {
            slot == MAIN_SLOT
                && s.on_live_player()
                && s.playback == Playback::Playing
                && !s.paused
                && !s.rechecking
                && !s.audio_only
        }
    }
}

/// How many renditions lower the streams of `tier` on screen can still go (the most of any). The
/// single player's stream is the only one of its tier `Sound`.
fn room(s: &AppState, tier: Tier) -> u8 {
    if s.multiview.is_some() {
        return multiview::room(s, tier);
    }
    let now = match s.stream_kind() {
        StreamKind::AudioOnly => return 0,
        StreamKind::Tile { rendition, .. } => rendition,
        StreamKind::Video(quality) => Rendition::ceiling(quality),
    };
    if tier == Tier::Sound {
        now.above_lowest()
    } else {
        0
    }
}
