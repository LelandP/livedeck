//! Chat replay of a video (vod-plan.md, phase 6; the types and the idea are in `replay.rs`).
//!
//! | When                         | What happens |
//! |------------------------------|--------------|
//! | Y, panel closed              | The panel opens and a window is fetched at the playhead |
//! | A health tick                | Lines whose second has passed are shown, and the next window is fetched while less than `LOOKAHEAD_MS` of lines are left, asked `LOOK_BACK_S` past the newest line |
//! | Any jump (`video::seek_to`)  | The shown lines go, and a window is fetched at the new spot |
//! | Y, panel open                | The panel closes and everything is forgotten |
//! | An answer                    | Dropped unless it is for this video and for the request still on its way |
//! | A failure with no line ever  | The panel closes and *No chat replay for this video* shows, once while the tries keep failing (not in the last minutes: the chat may just have ended) |
//!
//! The lines are shown in `AppState::chat`, the buffer live chat uses. A live stream clears it
//! when it starts and `video::open` and `video::leave` clear it too, so one never shows the
//! other's lines.

use crate::msg::Command;
use crate::replay::{
    ReplayLine, LOOKAHEAD_MS, LOOK_BACK_S, NEAR_END_MS, NUDGE_MAX_S, NUDGE_STEP_S, WINDOW_LINES,
};
use crate::state::{AppState, ToastKind};
use crate::video::{VideoItem, VideoSession};

use super::{fetch_chat_images, pulse_hud, push_toast};

/// Y on a video: the panel opens at the playhead, or closes. Only a broadcast or highlight has a
/// replay (a clip has none), and after a failure Y tries again.
pub(super) fn toggle(s: &mut AppState, cmds: &mut Vec<Command>) {
    let Some(v) = s.video.as_ref() else {
        return;
    };
    if !matches!(v.item, VideoItem::Video(_)) {
        return;
    }
    if s.chat_open {
        s.chat_open = false;
        forget(s);
    } else {
        s.chat_open = true;
        restart(s, cmds);
    }
    pulse_hud(s);
}

/// Throws away the lines (shown and waiting) and the progress of the windows. Whatever was on its
/// way is dropped when it arrives.
fn forget(s: &mut AppState) {
    s.chat.clear();
    if let Some(v) = s.video.as_mut() {
        v.replay.clear();
    }
}

/// The video jumped (or starts again): the lines of the old spot are gone, and with the panel
/// open a window is fetched at the new one. The playhead (`pos_ms`) is already there.
pub(super) fn restart(s: &mut AppState, cmds: &mut Vec<Command>) {
    forget(s);
    if s.chat_open {
        let at = s.video.as_ref().map_or(0, |v| v.pos_ms / 1_000);
        fetch(s, u32::try_from(at).unwrap_or(u32::MAX), cmds);
    }
}

/// Asks for the window at `offset_s`. Only that answer will count.
fn fetch(s: &mut AppState, offset_s: u32, cmds: &mut Vec<Command>) {
    let Some(v) = s.video.as_mut() else {
        return;
    };
    let VideoItem::Video(video) = &v.item else {
        return;
    };
    v.replay.in_flight = Some(offset_s);
    cmds.push(Command::LoadChatReplay {
        id: video.id.clone(),
        offset_s,
    });
}

/// A health tick moved the playhead: lines come due, and the next window is asked for when the
/// fetched ones run low.
pub(super) fn tick(s: &mut AppState, cmds: &mut Vec<Command>) {
    show_due(s, cmds);
    top_up(s, cmds);
}

/// Moves the lines the playhead has passed from the waiting list into the chat.
fn show_due(s: &mut AppState, cmds: &mut Vec<Command>) {
    let Some(v) = s.video.as_mut() else {
        return;
    };
    let pos_ms = v.pos_ms;
    let mut due = Vec::new();
    while v
        .replay
        .pending
        .front()
        .is_some_and(|l| l.at_ms() <= pos_ms)
    {
        if let Some(line) = v.replay.pending.pop_front() {
            due.push(line.message);
        }
    }
    if due.is_empty() {
        return;
    }
    fetch_chat_images(&due, cmds);
    s.chat.extend(due);
}

/// The next window, from the newest line fetched, once less than `LOOKAHEAD_MS` of lines are
/// left. Only from a tick (a second apart), so a very busy chat cannot make requests back to
/// back, and only one request is out at a time.
fn top_up(s: &mut AppState, cmds: &mut Vec<Command>) {
    if !s.chat_open {
        return;
    }
    let Some(v) = s.video.as_ref() else {
        return;
    };
    let r = &v.replay;
    if r.in_flight.is_some() || r.exhausted || r.unavailable {
        return;
    }
    // The window starts `LOOK_BACK_S` before its offset. Asked at the newest line, a busy chat
    // would fill all of it with lines already held, so it is asked that far past the newest line:
    // the window then starts at it, repeats a few lines (told apart by id) and goes on from there.
    let Some(last_s) = r.last_s else {
        fetch(s, u32::try_from(v.pos_ms / 1_000).unwrap_or(u32::MAX), cmds);
        return;
    };
    if u64::from(last_s) * 1_000 < v.pos_ms.saturating_add(LOOKAHEAD_MS) {
        let offset = last_s.saturating_add(LOOK_BACK_S).saturating_add(r.nudge_s);
        fetch(s, offset, cmds);
    }
}

/// True when `line` is not in the lines taken in so far (the windows overlap).
fn is_new(s: &AppState, line: &ReplayLine) -> bool {
    let Some(v) = s.video.as_ref() else {
        return false;
    };
    match v.replay.last_s {
        None => true,
        Some(last) => {
            line.offset_s > last
                || (line.offset_s == last && !v.replay.last_ids.contains(&line.message.id))
        }
    }
}

/// Whether an answer is for this video and for the request on its way.
fn current(s: &AppState, id: &str, offset_s: u32) -> bool {
    s.video
        .as_ref()
        .is_some_and(|v| v.item.video_id() == Some(id) && v.replay.in_flight == Some(offset_s))
}

/// `Event::ChatReplay`: a window arrived. What is new in it waits for its second, and what is
/// due already (the lines just before a jump) shows at once.
pub(super) fn loaded(
    s: &mut AppState,
    id: &str,
    offset_s: u32,
    mut lines: Vec<ReplayLine>,
    cmds: &mut Vec<Command>,
) {
    if !current(s, id, offset_s) {
        return;
    }
    lines.sort_by_key(|l| l.offset_s);
    let full = lines.len() >= WINDOW_LINES;
    let fresh: Vec<ReplayLine> = lines.into_iter().filter(|l| is_new(s, l)).collect();
    let Some(v) = s.video.as_mut() else {
        return;
    };
    v.replay.in_flight = None;
    if fresh.is_empty() {
        if full && v.replay.nudge_s < NUDGE_MAX_S {
            // A full window, and all of it at or before the newest line: not the end, just a very
            // busy chat. The next tick asks a little further on.
            v.replay.nudge_s += NUDGE_STEP_S;
            return;
        }
        // The end of the chat, as far as Twitch tells: nothing more until the next jump.
        v.replay.exhausted = true;
        if !v.replay.had_lines && !near_the_end(v, offset_s) {
            unavailable(s, cmds);
        }
        return;
    }
    v.replay.had_lines = true;
    v.replay.unavailable = false;
    v.replay.nudge_s = 0;
    for line in fresh {
        match v.replay.last_s {
            Some(last) if line.offset_s == last => v.replay.last_ids.push(line.message.id.clone()),
            _ => {
                v.replay.last_s = Some(line.offset_s);
                v.replay.last_ids = vec![line.message.id.clone()];
            }
        }
        v.replay.pending.push_back(line);
    }
    show_due(s, cmds);
}

/// `Event::ChatReplayFailed` (also an empty answer). With no line ever shown for this video there
/// may be no replay, and the viewer is told once; Y tries again. After lines had come, or in the
/// last minutes of the video, it is more likely a bad moment (or the end), so the panel stays and
/// the next jump tries again.
pub(super) fn failed(s: &mut AppState, id: &str, offset_s: u32, cmds: &mut Vec<Command>) {
    if !current(s, id, offset_s) {
        return;
    }
    let Some(v) = s.video.as_mut() else {
        return;
    };
    v.replay.in_flight = None;
    if v.replay.had_lines || near_the_end(v, offset_s) {
        v.replay.exhausted = true;
    } else {
        unavailable(s, cmds);
    }
}

/// The request at `offset_s` was for the last minutes of the video, where nothing may have been
/// said (an unknown length counts as not near).
fn near_the_end(v: &VideoSession, offset_s: u32) -> bool {
    v.length_ms > 0 && u64::from(offset_s) * 1_000 + NEAR_END_MS >= v.length_ms
}

/// Nothing came for a video that never gave a line: the panel closes and the viewer is told, once
/// for as long as the tries keep failing. Y tries again.
fn unavailable(s: &mut AppState, cmds: &mut Vec<Command>) {
    s.chat_open = false;
    forget(s);
    let told = s.video.as_mut().is_some_and(|v| {
        let told = v.replay.unavailable;
        v.replay.unavailable = true;
        told
    });
    if !told {
        push_toast(s, ToastKind::NoChatReplay, cmds);
    }
}
