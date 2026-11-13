//! Favorites (plan.md, phase 6): LiveDeck's own starred channels, Kick and Twitch. Starring works
//! from the View popup (on a card, the single player, multiview), the picker (Y), and View on a
//! channel page (vod-plan.md, phase 3: Twitch has no follow API for apps). The list is
//! saved on every change (`Command::SaveFavorites`); which favorites are live comes from
//! `FeedId::Favorites`, and Home has a row of the live ones.
//!
//! A change shows at once: an unstarred channel leaves the Favorites rows and Home's row, a
//! starred live stream joins the live rows and an offline channel the offline row. The next load
//! brings the real picture.

use crate::domain::{Channel, ChannelRef, FeedId, Rgb, Row, RowKind, Tile};
use crate::msg::Command;
use crate::state::{AppState, Feed, RailEntry, Screen};

/// The rows that list favorites, on the Favorites screen and on Home.
fn is_favorites_row(kind: RowKind) -> bool {
    matches!(kind, RowKind::FavoritesLive | RowKind::FavoritesOffline)
}

/// Shows Favorites as a root screen and asks again which favorites are live. Rows from an earlier
/// visit stay on screen until the answer.
pub(super) fn open(s: &mut AppState, cmds: &mut Vec<Command>) {
    super::reset_stack(s, cmds);
    s.category = None;
    s.channel_page = None;
    s.subscribe_card = false;
    s.screen = Screen::Favorites;
    s.rail.index = RailEntry::Favorites;
    reload(s, cmds);
}

/// Asks which favorites are live, unless that is already under way. With no favorites there is
/// nothing to ask.
pub(super) fn reload(s: &mut AppState, cmds: &mut Vec<Command>) {
    if s.favorites.is_empty() {
        s.favorites_feed = Feed::default();
        return;
    }
    if s.favorites_feed.loading {
        return;
    }
    s.favorites_feed.loading = true;
    s.favorites_feed.error = None;
    cmds.push(Command::LoadFeed {
        feed: FeedId::Favorites,
    });
}

/// Stars the channel of `tile` (a stream or an offline channel), or unstars it if it is a
/// favorite already, and saves the list. Signed in only.
pub(super) fn toggle(s: &mut AppState, tile: Tile, cmds: &mut Vec<Command>) {
    if !s.is_signed_in() {
        return;
    }
    switch(s, tile, cmds);
}

/// View on the channel page: stars the page's channel, or unstars it, and saves the list. Signed
/// out too: the list is LiveDeck's own (the Favorites screen shows it once signed in), so only
/// the list changes then, not the rows.
pub(super) fn toggle_page(s: &mut AppState, cmds: &mut Vec<Command>) {
    let Some(page) = s.channel_page.as_ref() else {
        return;
    };
    let tile = match &page.live {
        Some(stream) => Tile::Stream(stream.clone()),
        None => Tile::Channel(Channel {
            channel: page.channel.clone(),
            name: page.name.clone(),
            tint: Rgb::default(),
        }),
    };
    switch(s, tile, cmds);
}

/// Stars or unstars the channel of `tile`, and saves the list. The rows that list favorites
/// follow, but a star only adds to them signed in (they are not shown signed out).
fn switch(s: &mut AppState, tile: Tile, cmds: &mut Vec<Command>) {
    let Some(channel) = tile.channel().cloned() else {
        return;
    };
    if s.is_favorite(&channel) {
        s.favorites.retain(|f| !f.is_same(&channel));
        let gone = |c: &ChannelRef| !c.is_same(&channel);
        retain_tiles(&mut s.favorites_feed, gone);
        retain_tiles(&mut s.home, gone);
    } else {
        s.favorites.push(channel);
        if !s.is_signed_in() {
            cmds.push(Command::SaveFavorites(s.favorites.clone()));
            return;
        }
        match tile {
            Tile::Stream(stream) => {
                let tile = Tile::Stream(stream);
                add_to_row(
                    &mut s.favorites_feed,
                    RowKind::FavoritesLive,
                    0,
                    tile.clone(),
                );
                // Home's row of live favorites, after the hero and Followed (as the provider
                // places it). A Home still loading gets it with its rows.
                if !s.home.rows.is_empty() {
                    let at = s
                        .home
                        .rows
                        .iter()
                        .rposition(|r| matches!(r.kind, RowKind::Featured | RowKind::FollowedLive))
                        .map_or(0, |i| i + 1);
                    add_to_row(&mut s.home, RowKind::FavoritesLive, at, tile);
                }
            }
            Tile::Channel(_) => {
                let at = s.favorites_feed.rows.len();
                add_to_row(&mut s.favorites_feed, RowKind::FavoritesOffline, at, tile);
            }
            Tile::Category(_) | Tile::Resume(_) | Tile::Video(_) | Tile::Clip(_) => {}
        }
    }
    cmds.push(Command::SaveFavorites(s.favorites.clone()));
}

/// After a load: a channel unstarred while it was under way is left out.
pub(super) fn prune(s: &mut AppState) {
    let favorites = s.favorites.clone();
    let starred = |c: &ChannelRef| favorites.iter().any(|f| f.is_same(c));
    retain_tiles(&mut s.favorites_feed, starred);
    retain_tiles(&mut s.home, starred);
}

/// Keeps the tiles of the favorites rows whose channel passes `keep`. Rows left empty go.
fn retain_tiles(feed: &mut Feed, keep: impl Fn(&ChannelRef) -> bool) {
    for row in feed.rows.iter_mut().filter(|r| is_favorites_row(r.kind)) {
        row.tiles.retain(|t| t.channel().is_none_or(&keep));
    }
    while let Some(at) = feed
        .rows
        .iter()
        .position(|r| is_favorites_row(r.kind) && r.tiles.is_empty())
    {
        remove_row(feed, at);
    }
    feed.clamp_focus();
}

/// Adds `tile` at the end of the row of `kind`, or as a new row at `at` if there is none.
fn add_to_row(feed: &mut Feed, kind: RowKind, at: usize, tile: Tile) {
    match feed.rows.iter_mut().find(|r| r.kind == kind) {
        Some(row) => {
            let fresh = tile.key();
            if !row.tiles.iter().any(|t| t.key() == fresh) {
                row.tiles.push(tile);
            }
        }
        None => insert_row(feed, at.min(feed.rows.len()), Row::new(kind, vec![tile])),
    }
}

/// Inserts a row, keeping the focus (and the remembered columns) on the rows they were on.
fn insert_row(feed: &mut Feed, at: usize, row: Row) {
    feed.rows.insert(at, row);
    if at <= feed.row_cols.len() {
        feed.row_cols.insert(at, 0);
    }
    if feed.rows.len() > 1 && feed.focus.row >= at {
        feed.focus.row += 1;
    }
}

/// Removes a row, keeping the focus on the row it was on. Focus on the removed row moves to the
/// one taking its place.
fn remove_row(feed: &mut Feed, at: usize) {
    feed.rows.remove(at);
    if at < feed.row_cols.len() {
        feed.row_cols.remove(at);
    }
    if feed.focus.row > at {
        feed.focus.row -= 1;
    } else if feed.focus.row == at {
        feed.focus.col = feed.col_of(at);
    }
}
