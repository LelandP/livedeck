//! *Continue watching* (vod-plan.md, phase 5): where each video was left, at most `CONTINUE_MAX`,
//! newest first. `AppState::continue_watching` holds it and `Command::SaveContinue` writes it to
//! `continue.toml` after every change.
//!
//! - A video is saved when it is left (B, *Back* and *Channel videos* on the end card, the sleep
//!   timer, the window closing). Another video can only start from the page or Home, never over a
//!   playing one. Clips are never saved.
//! - It is dropped when it plays to its end or is left within `CONTINUE_END_MARGIN_S` of it, and
//!   when Twitch says it no longer exists (`Event::VideoGone`).
//! - Home shows the list as a row after Followed (`RowKind::ContinueWatching`): A resumes, X removes.
//!   `sync_home` keeps the row true to the list as it changes, so it needs no Home reload.

use crate::domain::{Row, RowKind, Tile};
use crate::msg::Command;
use crate::state::{AppState, Feed, Screen};
use crate::video::{
    Progress, Video, VideoItem, CONTINUE_END_MARGIN_S, CONTINUE_MAX, CONTINUE_MIN_S,
};

use super::video;

/// The list after leaving `video` `pos_s` seconds in. A video left near its start keeps the list
/// as it was, one left near its end drops out, any other moves to the front.
pub fn remembered(list: &[Progress], video: &Video, pos_s: u32, saved_at: u64) -> Vec<Progress> {
    if pos_s < CONTINUE_MIN_S {
        // Not worth a place of its own: whatever place it had stays.
        return list.to_vec();
    }
    let mut out: Vec<Progress> = list
        .iter()
        .filter(|p| p.video.id != video.id)
        .cloned()
        .collect();
    let watched =
        video.length_s > 0 && pos_s.saturating_add(CONTINUE_END_MARGIN_S) >= video.length_s;
    if !watched {
        out.insert(
            0,
            Progress {
                video: video.clone(),
                pos_s,
                saved_at,
            },
        );
        out.truncate(CONTINUE_MAX);
    }
    out
}

/// Replaces the list and asks for it to be written, if it changed.
fn set(s: &mut AppState, list: Vec<Progress>, cmds: &mut Vec<Command>) {
    if list == s.continue_watching {
        return;
    }
    s.continue_watching = list;
    cmds.push(Command::SaveContinue(s.continue_watching.clone()));
}

/// The video on screen is being left: its place goes into the list, or out of it. Call before the
/// session is dropped. A clip, and a video Twitch says is gone, change nothing.
pub(super) fn save_leaving(s: &mut AppState, cmds: &mut Vec<Command>) {
    let Some(v) = s.video.as_ref() else {
        return;
    };
    let VideoItem::Video(card) = &v.item else {
        return;
    };
    if v.gone {
        return;
    }
    let mut video = card.clone();
    // The player's length is the truth (a broadcast still recording grows).
    if v.length_ms > 0 {
        video.length_s = u32::try_from(v.length_ms / 1_000).unwrap_or(u32::MAX);
    }
    let pos_s = u32::try_from(v.shown_ms() / 1_000).unwrap_or(u32::MAX);
    let list = remembered(&s.continue_watching, &video, pos_s, v.now);
    set(s, list, cmds);
}

/// The video played to its end: it leaves the list.
pub(super) fn finished(s: &mut AppState, cmds: &mut Vec<Command>) {
    let Some(VideoItem::Video(card)) = s.video.as_ref().map(|v| &v.item) else {
        return;
    };
    forget(s, &card.id.clone(), cmds);
}

/// Takes the video `id` out of the list.
pub(super) fn forget(s: &mut AppState, id: &str, cmds: &mut Vec<Command>) {
    let list = s
        .continue_watching
        .iter()
        .filter(|p| p.video.id != id)
        .cloned()
        .collect();
    set(s, list, cmds);
}

/// A on a card of Home's row: the video from where it was left.
pub(super) fn resume_card(s: &mut AppState, card: Video, cmds: &mut Vec<Command>) {
    let saved = s.progress_of(&card.id).map(|p| u64::from(p.pos_s) * 1_000);
    video::open(
        s,
        VideoItem::Video(card),
        saved.unwrap_or(0),
        saved.is_some(),
        cmds,
    );
}

/// X on Home's row: the focused card leaves the list. `false` when the focus is not on one.
pub(super) fn remove_focused(s: &mut AppState, cmds: &mut Vec<Command>) -> bool {
    if s.screen != Screen::Home || s.rail.focused {
        return false;
    }
    let focus = s.home.focus;
    let Some(row) = s.home.rows.get(focus.row) else {
        return false;
    };
    if row.kind != RowKind::ContinueWatching {
        return false;
    }
    let Some(Tile::Video(card)) = row.tiles.get(focus.col) else {
        return false;
    };
    let id = card.id.clone();
    forget(s, &id, cmds);
    true
}

/// The row for the list, `None` while it is empty.
fn row_of(list: &[Progress]) -> Option<Row> {
    (!list.is_empty()).then(|| {
        Row::new(
            RowKind::ContinueWatching,
            list.iter().map(|p| Tile::Video(p.video.clone())).collect(),
        )
    })
}

/// Where the row goes: after Followed, else after the hero, else first.
fn insert_at(rows: &[Row]) -> usize {
    rows.iter()
        .rposition(|r| r.kind == RowKind::FollowedLive)
        .or_else(|| rows.iter().rposition(|r| r.kind == RowKind::Featured))
        .map_or(0, |i| i + 1)
}

/// A Home load arrived: the row goes in with the provider's.
pub(super) fn home_loaded(s: &AppState, rows: &mut Vec<Row>) {
    rows.retain(|r| r.kind != RowKind::ContinueWatching);
    if let Some(row) = row_of(&s.continue_watching) {
        let at = insert_at(rows);
        rows.insert(at, row);
    }
}

/// Keeps Home's row true to the list after every message: it appears with the first saved video,
/// follows each change and goes with the last. Nothing happens while Home has not loaded, or while
/// the row already shows the list. The focus stays on its card.
pub(super) fn sync_home(s: &mut AppState, cmds: &mut Vec<Command>) {
    let list = &s.continue_watching;
    let home: &mut Feed = &mut s.home;
    let at = home
        .rows
        .iter()
        .position(|r| r.kind == RowKind::ContinueWatching);
    let shown = at.map(|r| {
        home.rows[r]
            .tiles
            .iter()
            .filter_map(|t| match t {
                Tile::Video(v) => Some(v),
                _ => None,
            })
            .collect::<Vec<_>>()
    });
    let same = match &shown {
        Some(shown) => {
            shown.len() == list.len() && shown.iter().zip(list).all(|(a, b)| **a == b.video)
        }
        None => list.is_empty(),
    };
    if same || (home.rows.is_empty() && at.is_none()) {
        return;
    }
    let row = row_of(list);
    if let Some(row) = &row {
        let urls: Vec<String> = row
            .tiles
            .iter()
            .flat_map(Tile::thumbnails)
            .map(str::to_owned)
            .collect();
        if !urls.is_empty() {
            cmds.push(Command::FetchThumbnails { urls });
        }
    }
    match (at, row) {
        // In place, so the focus stays on its card (or the one that took its place).
        (Some(r), Some(row)) => home.rows[r].tiles = row.tiles,
        (Some(r), None) => {
            home.rows.remove(r);
            if r < home.row_cols.len() {
                home.row_cols.remove(r);
            }
            if home.focus.row > r {
                home.focus.row -= 1;
            } else if home.focus.row == r {
                home.focus.col = home.col_of(r);
            }
        }
        (None, Some(row)) => {
            let at = insert_at(&home.rows);
            home.rows.insert(at, row);
            if at <= home.row_cols.len() {
                home.row_cols.insert(at, 0);
            }
            if home.rows.len() > 1 && home.focus.row >= at {
                home.focus.row += 1;
            }
        }
        (None, None) => {}
    }
    home.clamp_focus();
}
