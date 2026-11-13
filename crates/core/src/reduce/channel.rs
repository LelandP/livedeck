//! The channel page (vod-plan.md, phase 3): a Twitch channel's live stream, past broadcasts,
//! highlights and clips, in two tabs. Opened by Y on a stream or channel card, A on an offline
//! channel card or *Continue watching* card, and the live player's menu. Back returns to where it
//! was opened. It needs no account: GQL lists videos signed out.
//!
//! From the live player's menu the page goes over the Player and the stream keeps playing behind it
//! (`AppState::live_covered`): B is instant (`back_to_live`). A video or clip played from the page
//! stops the stream first and parks it (`park_live`), and B from the page then loads it again.
//!
//! It is a screen of its own, not a `FeedId`: its two tabs are the `Feed`s of
//! `AppState::channel_page`, each keeping its own focus. A on a video or a clip plays it in the
//! video player (`video.rs`) over the page, so B comes back to the page; a subscriber-only video
//! opens the subscribe card instead.

use crate::domain::{ChannelRef, Row, Tile};
use crate::msg::{Command, Intent};
use crate::state::{AppState, ChannelPage, Feed, Playback, Screen, RAID_SECONDS};
use crate::video::{ChannelInfo, VideoItem};

use super::{
    back, begin_stream, drop_live_behind, fetch_thumbnails, open_player, pop_screen, pulse_hud,
    push_screen, stop_live, subscribe_card_intent, video,
};

/// Y on the focused card of a browse screen: a Twitch stream or channel card opens its channel
/// page. Anything else, a Kick card included, does nothing.
pub(super) fn open_from_card(s: &mut AppState, cmds: &mut Vec<Command>) {
    if s.rail.focused {
        return;
    }
    match s.focused_tile().cloned() {
        Some(Tile::Stream(stream)) if stream.channel.features().videos => {
            let login = stream.channel.login.clone();
            let page = ChannelPage::opening(stream.channel.clone(), login, Some(stream));
            open(s, page, cmds);
        }
        Some(Tile::Channel(card)) if card.channel.features().videos => {
            open(s, ChannelPage::opening(card.channel, card.name, None), cmds);
        }
        // A *Continue watching* card: the page of the video's channel.
        Some(Tile::Video(card)) if card.channel.features().videos => {
            open(
                s,
                ChannelPage::opening(card.channel, card.channel_name, None),
                cmds,
            );
        }
        _ => {}
    }
}

/// A on an offline channel card. `true` if it opened the channel page (Twitch), `false` if the
/// platform has none and the card should play as it always did.
pub(super) fn open_from_offline_card(
    s: &mut AppState,
    channel: &ChannelRef,
    name: &str,
    cmds: &mut Vec<Command>,
) -> bool {
    if !channel.features().videos {
        return false;
    }
    open(
        s,
        ChannelPage::opening(channel.clone(), name.to_owned(), None),
        cmds,
    );
    true
}

/// The *Channel videos* row of the live player's menu. The page goes over the Player
/// (`[…, Player, Channel]`) and the stream keeps playing behind it, sound and chat included (the
/// picture is not drawn: no video area on that screen), so B on the page is instant
/// (`back_to_live`). Does nothing in multiview, or for a platform without videos.
pub(super) fn open_from_player(s: &mut AppState, cmds: &mut Vec<Command>) {
    if s.multiview.is_some() {
        return;
    }
    let Some(stream) = s.now_playing.clone() else {
        return;
    };
    if !stream.channel.features().videos {
        return;
    }
    // A stream that was on the end screen is not live.
    let live = matches!(s.playback, Playback::Playing | Playback::Loading).then(|| stream.clone());
    s.player_menu = None;
    let login = stream.channel.login.clone();
    let page = ChannelPage::opening(stream.channel, login, live);
    let below = s.back_stack.iter().rposition(|x| *x == Screen::Channel);
    match (below, s.channel_page.as_mut()) {
        // The stream was opened from this very page: no second page, the one below is reused.
        (Some(_), Some(old)) if old.channel.is_same(&page.channel) => {
            old.live = page.live;
            s.rail.focused = false;
            s.subscribe_card = false;
            push_screen(s, Screen::Channel);
            reload(s, cmds);
        }
        // Another channel's page below (a raid was followed): this page replaces it.
        (below, _) => {
            if let Some(i) = below {
                s.back_stack.remove(i);
            }
            open(s, page, cmds);
        }
    }
}

/// B on the channel page over a live stream: back to the Player. A stream that kept playing is
/// just there again (the HUD shows briefly, and a raid that came meanwhile gets its card and
/// countdown now). One parked for a video (`park_live`) loads again. The page is dropped unless it
/// is also the one the stream was opened from, further down the stack. `false` if no live stream
/// is behind the page.
pub(super) fn back_to_live(s: &mut AppState, cmds: &mut Vec<Command>) -> bool {
    if s.live_behind().is_none() {
        return false;
    }
    s.subscribe_card = false;
    pop_screen(s);
    if !s.back_stack.contains(&Screen::Channel) {
        s.channel_page = None;
    }
    match s.parked_live.take() {
        Some(stream) => begin_stream(s, stream, cmds),
        None => {
            pulse_hud(s);
            if let Some(offer) = s.raid.as_mut() {
                offer.seconds_left = RAID_SECONDS;
                cmds.push(Command::StartRaidCountdown {
                    id: offer.id,
                    seconds: RAID_SECONDS,
                });
            }
        }
    }
    true
}

/// A video or clip is about to play over the page, and a video cannot share `now_playing` with a
/// live stream: the stream behind the page stops (player, chat, raids, hype train) and is
/// remembered, so B from the page can load it again. Nothing happens without a live stream
/// playing behind the page.
pub(super) fn park_live(s: &mut AppState, cmds: &mut Vec<Command>) {
    if !s.live_covered() {
        return;
    }
    let Some(stream) = s.now_playing.clone() else {
        return;
    };
    stop_live(s, cmds);
    s.parked_live = Some(stream);
}

/// Opens `channel`'s page over the current screen (a video's *Channel videos* with no page
/// below it).
pub(super) fn open_channel(
    s: &mut AppState,
    channel: ChannelRef,
    name: String,
    cmds: &mut Vec<Command>,
) {
    open(s, ChannelPage::opening(channel, name, None), cmds);
}

/// Shows `page` over the current screen and asks for its rows.
fn open(s: &mut AppState, page: ChannelPage, cmds: &mut Vec<Command>) {
    s.rail.focused = false;
    s.subscribe_card = false;
    let channel = page.channel.clone();
    s.channel_page = Some(page);
    push_screen(s, Screen::Channel);
    cmds.push(Command::LoadChannelPage { channel });
}

/// Asks for the page again (X after a failure). Rows already on screen stay until the answer.
fn reload(s: &mut AppState, cmds: &mut Vec<Command>) {
    let Some(page) = s.channel_page.as_mut() else {
        return;
    };
    for feed in [&mut page.videos, &mut page.clips] {
        feed.loading = true;
        feed.error = None;
    }
    cmds.push(Command::LoadChannelPage {
        channel: page.channel.clone(),
    });
}

/// Input on the channel page while the rail is closed. The D-pad moves over the rows (Left from
/// the first card opens the rail), A plays the card, X starts a video over or retries a failed
/// load, LB and RB switch tab, and B goes back.
pub(super) fn intent(s: &mut AppState, intent: Intent, cmds: &mut Vec<Command>) {
    if s.subscribe_card {
        subscribe_card_intent(s, intent, cmds);
        return;
    }
    match intent {
        Intent::Left => {
            let at_start = s.current_feed().is_none_or(|f| f.focus.col == 0);
            if at_start {
                s.rail.focused = true;
            } else if let Some(feed) = s.current_feed_mut() {
                feed.focus.col -= 1;
            }
        }
        Intent::Right => {
            if let Some(feed) = s.current_feed_mut() {
                if let Some(len) = feed.row_len(feed.focus.row) {
                    feed.focus.col = (feed.focus.col + 1).min(len.saturating_sub(1));
                }
            }
        }
        Intent::Up => {
            if let Some(feed) = s.current_feed_mut() {
                feed.focus_row(feed.focus.row.saturating_sub(1), true);
            }
        }
        Intent::Down => {
            if let Some(feed) = s.current_feed_mut() {
                if !feed.rows.is_empty() {
                    feed.focus_row((feed.focus.row + 1).min(feed.rows.len() - 1), true);
                }
            }
        }
        Intent::Select => select(s, cmds),
        Intent::Retry => retry(s, cmds),
        // Two tabs: either shoulder button goes to the other one.
        Intent::PageLeft | Intent::PageRight => {
            if let Some(page) = s.channel_page.as_mut() {
                page.feed_mut().remember_col();
                page.tab = page.tab.other();
            }
        }
        Intent::Back => back(s, cmds),
        _ => {}
    }
}

/// A on the focused card: the live stream plays, a video plays from its saved place (or the
/// start), a clip plays, and a subscriber-only video opens the subscribe card. The player goes
/// over the page, so B (and the end card's *Channel videos*) lands here again.
pub(super) fn select(s: &mut AppState, cmds: &mut Vec<Command>) {
    let Some(tile) = s.current_feed().and_then(Feed::focused_tile).cloned() else {
        return;
    };
    match tile {
        Tile::Stream(stream) => match s.live_behind() {
            // That stream is behind the page already: it is just B.
            Some(behind) if behind.is_same(&stream.channel) => {
                back_to_live(s, cmds);
            }
            // Another one (a raid was followed): the old stream ends.
            Some(_) => {
                drop_live_behind(s, cmds);
                open_player(s, stream, cmds);
            }
            None => open_player(s, stream, cmds),
        },
        Tile::Video(video) if video.sub_only => {
            s.subscribe_card = video.channel.features().subscribe;
        }
        Tile::Video(card) => {
            let saved = s.progress_of(&card.id).map(|p| u64::from(p.pos_s) * 1_000);
            let start_ms = saved.unwrap_or(0);
            video::open(s, VideoItem::Video(card), start_ms, saved.is_some(), cmds);
        }
        Tile::Clip(clip) => video::open(s, VideoItem::Clip(clip), 0, false, cmds),
        Tile::Category(_) | Tile::Channel(_) | Tile::Resume(_) => {}
    }
}

/// X: after a failed load, try again. On a video that is in *Continue watching*, *Start over*:
/// it plays from 0:00 (the hint is only shown when there is a saved place to start over from).
fn retry(s: &mut AppState, cmds: &mut Vec<Command>) {
    let failed = s
        .channel_page
        .as_ref()
        .is_some_and(|p| p.error().is_some() && !p.loading());
    if failed {
        reload(s, cmds);
        return;
    }
    if s.focused_progress().is_none() {
        return;
    }
    // A subscriber-only video opens the subscribe card instead (`video::open`).
    if let Some(Tile::Video(card)) = s.focused_tile().cloned() {
        video::open(s, VideoItem::Video(card), 0, false, cmds);
    }
}

/// The page arrived: the header, and the rows of each tab. A late answer for a page that was left
/// (or replaced by another channel's) is dropped.
pub(super) fn loaded(s: &mut AppState, info: ChannelInfo, rows: Vec<Row>, cmds: &mut Vec<Command>) {
    let Some(page) = s
        .channel_page
        .as_mut()
        .filter(|p| p.channel.is_same(&info.channel))
    else {
        return;
    };
    if !info.name.is_empty() {
        page.name = info.name;
    }
    if let Some(avatar) = info.avatar.clone() {
        cmds.push(Command::FetchThumbnails { urls: vec![avatar] });
    }
    page.avatar = info.avatar;
    page.live = info.live;
    page.last_live_at = info.last_live_at;
    page.now = info.loaded_at;
    let (clips, videos): (Vec<Row>, Vec<Row>) = rows.into_iter().partition(|r| r.kind.is_clips());
    for (feed, rows) in [(&mut page.videos, videos), (&mut page.clips, clips)] {
        feed.rows = rows;
        feed.loading = false;
        feed.error = None;
        feed.row_cols.clear();
        feed.clamp_focus();
    }
    fetch_thumbnails(
        page.videos
            .rows
            .iter()
            .chain(&page.clips.rows)
            .flat_map(|r| &r.tiles),
        cmds,
    );
}

/// The page could not be loaded. Rows already on screen (a retry) stay next to the error.
pub(super) fn failed(s: &mut AppState, channel: &ChannelRef, message: String) {
    let Some(page) = s
        .channel_page
        .as_mut()
        .filter(|p| p.channel.is_same(channel))
    else {
        return;
    };
    for feed in [&mut page.videos, &mut page.clips] {
        feed.loading = false;
        feed.error = Some(message.clone());
    }
}
