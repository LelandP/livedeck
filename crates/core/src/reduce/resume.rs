//! Home's *Resume multiview* card (plan.md, phase 7): the last multiview session, at the start of
//! Home's first row of cards, while at least two of its streams are live. A starts that set again
//! (`multiview::resume`).
//!
//! The provider checks which of the saved channels are live with every Home load and sends them
//! as a `RowKind::Resume` row; `home_loaded` turns that row into the card. Leaving multiview
//! replaces the card with the set just left (`multiview::leave`), as its streams are known live.
//! After every message `settle` keeps the card where it belongs while other rows come and go
//! (a starred favorite adds a row, an unstarred one can take a row away), and takes it away when
//! signed out.

use crate::domain::{ResumeCard, Row, RowKind, Tile, RESUME_MIN_LIVE};
use crate::state::{AppState, Feed};

/// Rows of stream cards the card can open. The hero and categories are other shapes.
fn takes_card(kind: RowKind) -> bool {
    matches!(
        kind,
        RowKind::FollowedLive | RowKind::FavoritesLive | RowKind::TopLive
    )
}

/// The row the card goes in: the first row of stream cards. `None` when Home has none, and the
/// card gets a row of its own.
fn target_row(rows: &[Row]) -> Option<usize> {
    rows.iter().position(|r| takes_card(r.kind))
}

/// Where the card is (row, column), if anywhere.
fn card_spot(home: &Feed) -> Option<(usize, usize)> {
    home.rows.iter().enumerate().find_map(|(r, row)| {
        row.tiles
            .iter()
            .position(|t| matches!(t, Tile::Resume(_)))
            .map(|c| (r, c))
    })
}

/// A card for `streams`, the live streams of the set in order. `None` with fewer than two.
pub(super) fn card(streams: Vec<crate::domain::Stream>) -> Option<ResumeCard> {
    (streams.len() >= RESUME_MIN_LIVE).then_some(ResumeCard { streams })
}

/// A Home load arrived (`rows` are the provider's): its `RowKind::Resume` row becomes the card,
/// at the start of the first row of stream cards. The focus is the caller's: it is clamped next,
/// and already counted the card before the reload.
pub(super) fn home_loaded(s: &AppState, rows: &mut Vec<Row>) {
    let checked = rows.iter().position(|r| r.kind == RowKind::Resume);
    let live = checked
        .map(|at| rows.remove(at))
        .map_or_else(Vec::new, |row| {
            row.tiles
                .into_iter()
                .filter_map(|t| match t {
                    Tile::Stream(s) => Some(s),
                    _ => None,
                })
                .collect()
        });
    let Some(card) = card(live).filter(|_| s.is_signed_in()) else {
        return;
    };
    match target_row(rows) {
        Some(t) => rows[t].tiles.insert(0, Tile::Resume(card)),
        None => rows.insert(
            holder_at(rows),
            Row::new(RowKind::Resume, vec![Tile::Resume(card)]),
        ),
    }
}

/// Where a row holding only the card goes: right after the hero.
fn holder_at(rows: &[Row]) -> usize {
    rows.iter()
        .rposition(|r| r.kind == RowKind::Featured)
        .map_or(0, |i| i + 1)
}

/// Replaces the card (wherever it is) with `card`, or just takes it away with `None`. The focus
/// stays on the card it was on, and on the card itself if it was there.
pub(super) fn place(s: &mut AppState, card: Option<ResumeCard>) {
    let card = card.filter(|_| s.is_signed_in());
    let home = &mut s.home;
    let mut on_card = false;
    while let Some((r, c)) = card_spot(home) {
        on_card |= home.focus.row == r && home.focus.col == c;
        remove_tile(home, r, c);
    }
    let Some(card) = card else {
        home.clamp_focus();
        return;
    };
    if home.rows.is_empty() {
        // Home is still loading: its rows bring the card.
        return;
    }
    let row = match target_row(&home.rows) {
        Some(t) => {
            home.rows[t].tiles.insert(0, Tile::Resume(card));
            if home.focus.row == t {
                home.focus.col += 1;
            }
            if let Some(col) = home.row_cols.get_mut(t) {
                *col += 1;
            }
            t
        }
        None => {
            let at = holder_at(&home.rows);
            home.rows
                .insert(at, Row::new(RowKind::Resume, vec![Tile::Resume(card)]));
            if at <= home.row_cols.len() {
                home.row_cols.insert(at, 0);
            }
            if home.rows.len() > 1 && home.focus.row >= at {
                home.focus.row += 1;
            }
            at
        }
    };
    if on_card {
        home.focus.row = row;
        home.focus.col = 0;
    }
    home.clamp_focus();
}

/// Takes the tile at (`r`, `c`) out of Home, and the row too if that leaves it empty, keeping
/// the focus on the card it was on.
fn remove_tile(home: &mut Feed, r: usize, c: usize) {
    home.rows[r].tiles.remove(c);
    if home.focus.row == r && home.focus.col > c {
        home.focus.col -= 1;
    }
    if let Some(col) = home.row_cols.get_mut(r) {
        if *col > c {
            *col -= 1;
        }
    }
    if !home.rows[r].tiles.is_empty() {
        return;
    }
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

/// Keeps the card at the start of the first row of stream cards as Home's rows change, and takes
/// it away when signed out. Nothing happens while it is where it belongs.
pub(super) fn settle(s: &mut AppState) {
    let Some((r, c)) = card_spot(&s.home) else {
        return;
    };
    // A row of its own holds only the card, and goes once the card moves out of it.
    let in_place = match target_row(&s.home.rows) {
        Some(t) => r == t && c == 0,
        None => s.home.rows[r].kind == RowKind::Resume,
    };
    if in_place && s.is_signed_in() {
        return;
    }
    let card = match &s.home.rows[r].tiles[c] {
        Tile::Resume(card) => Some(card.clone()),
        _ => None,
    };
    place(s, card);
}
