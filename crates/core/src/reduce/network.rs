//! The slow-network rule's side of the reducer (plan.md, section 4): players' health reports go to
//! `AppState::net`, and when the limit moves the streams it changed reload at once. The rule
//! itself is in `netlimit.rs`.

use crate::msg::{Command, Slot, StreamKind, MAIN_SLOT};
use crate::netlimit::{Change, Health, Tier};
use crate::quality::Rendition;
use crate::state::{AppState, Playback, Screen, ToastKind};

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
    let room: Vec<(Tier, bool)> = Tier::LOWERED_FIRST
        .into_iter()
        .map(|t| (t, has_room(s, t)))
        .collect();
    let single_before = s.stream_kind();
    let Some(change) = s.net.report(slot, health, at_ms, |t| {
        room.iter().any(|(tier, room)| *tier == t && *room)
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
/// failed, paused or sound only.
fn counts(s: &AppState, slot: Slot) -> bool {
    match &s.multiview {
        Some(mv) => mv.place_of_slot(slot).is_some_and(|place| {
            mv.tile(place).is_some_and(|t| t.state == Playback::Playing)
                && !(place == mv.sound && s.paused)
        }),
        None => {
            slot == MAIN_SLOT
                && s.screen == Screen::Player
                && s.playback == Playback::Playing
                && !s.paused
                && !s.rechecking
                && !s.audio_only
        }
    }
}

/// Whether a stream of `tier` on screen can still go a rendition lower. The single player's
/// stream is the only one of its tier `Sound`.
fn has_room(s: &AppState, tier: Tier) -> bool {
    if s.multiview.is_some() {
        return multiview::has_room(s, tier);
    }
    let now = match s.stream_kind() {
        StreamKind::AudioOnly => return false,
        StreamKind::Tile { rendition, .. } => rendition,
        StreamKind::Video(quality) => Rendition::ceiling(quality),
    };
    tier == Tier::Sound && now > Rendition::P160
}
