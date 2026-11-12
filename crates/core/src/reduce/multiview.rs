//! Multiview's side of the reducer (plan.md, section 2): the View popup, the grid, full screen,
//! the picker, moving the sound, and the quality settle timer.
//!
//! The stream with sound is `now_playing`, and its tile's state is mirrored into `playback` and
//! `paused`, so the single player's code (chat, raids, the player menu, the full-screen HUD) works
//! on it unchanged. Full screen is the Player screen while `multiview` is set.

use crate::domain::{ChannelRef, ResumeCard, Stream, Tile, RESUME_MIN_LIVE};
use crate::layout::{self, Layout};
use crate::msg::{Command, Intent, Slot, StreamKind, MAIN_SLOT};
use crate::multiview::{
    Multiview, MvTile, Picker, PickerTab, PopupRow, SavedSet, TileQuality, ViewPopup,
};
use crate::netlimit::Tier;
use crate::quality::{self, FrameRate, Rendition};
use crate::state::{AppState, Playback, Screen, ToastKind};

use super::{
    favorites, open_login, pop_screen, pulse_hud, push_screen, push_toast, raid_intent, resume,
    stop_watching, switch_now_playing, watch_raids,
};

/// The tile is offline or failed: A offers *Retry* or *Remove*.
fn is_down(state: &Playback) -> bool {
    matches!(state, Playback::Offline | Playback::Failed(_))
}

// ---------------------------------------------------------------------------------------------
// The View popup
// ---------------------------------------------------------------------------------------------

/// Signed out, the popup only offers to sign in: multiview is for signed-in users.
fn sign_in_popup(s: &mut AppState) {
    s.popup = Some(ViewPopup::new(vec![PopupRow::SignIn], None));
}

/// View on a browse screen. A live stream card can be picked or unpicked, the picked set (with
/// this card) started, or the set cleared (plan.md, section 2). It, and an offline channel card,
/// can be starred or unstarred. Category cards do nothing.
pub(super) fn browse_view(s: &mut AppState) {
    let on_keys = s.screen == Screen::Search && !s.search.in_results;
    let card = match s.focused_tile() {
        Some(t @ (Tile::Stream(_) | Tile::Channel(_))) => t.clone(),
        // Videos never go into multiview (vod-plan.md section 1).
        Some(Tile::Category(_) | Tile::Resume(_) | Tile::Video(_) | Tile::Clip(_)) | None => return,
    };
    if on_keys {
        return;
    }
    if !s.is_signed_in() {
        sign_in_popup(s);
        return;
    }
    let mut rows = Vec::new();
    if let Tile::Stream(stream) = &card {
        if s.is_picked(&stream.channel) {
            rows.push(PopupRow::Unpick);
        } else if s.picked.len() < layout::MAX_STREAMS {
            rows.push(PopupRow::Pick);
        }
        if s.watch_set(Some(stream)).len() >= 2 {
            rows.push(PopupRow::WatchPicked);
        }
        if !s.picked.is_empty() {
            rows.push(PopupRow::ClearPicked);
        }
    }
    rows.push(PopupRow::Favorite);
    s.popup = Some(ViewPopup::on_card(rows, card));
}

/// View on the Player screen with nothing in front of the stream. The single player offers to
/// add a stream, to watch it with the picked ones, or to star it; multiview's full screen offers
/// to add one, remove or star this one, or leave.
pub(super) fn player_view(s: &mut AppState) {
    let popup = match &s.multiview {
        None if !s.is_signed_in() => ViewPopup::new(vec![PopupRow::SignIn], None),
        None => {
            let mut rows = vec![PopupRow::AddStream];
            if s.watch_set(None).len() >= 2 {
                rows.push(PopupRow::WatchPicked);
            }
            rows.push(PopupRow::Favorite);
            ViewPopup::new(rows, None)
        }
        Some(mv) => {
            let mut rows = Vec::new();
            if !mv.is_full() {
                rows.push(PopupRow::AddStream);
            }
            rows.extend([PopupRow::Remove, PopupRow::Favorite, PopupRow::Leave]);
            ViewPopup::new(rows, Some(mv.sound))
        }
    };
    s.popup = Some(popup);
    pulse_hud(s);
}

/// View in the grid: about the focused place.
fn grid_view(s: &mut AppState) {
    let Some(mv) = &s.multiview else {
        return;
    };
    let mut rows = Vec::new();
    if !mv.is_full() {
        rows.push(PopupRow::AddStream);
    }
    let focused = mv.focused_tile();
    if let Some(tile) = focused {
        if is_down(&tile.state) {
            rows.push(PopupRow::Retry);
        }
        rows.extend([PopupRow::Remove, PopupRow::Favorite]);
    }
    rows.extend([PopupRow::Layout, PopupRow::Leave]);
    let place = focused.is_some().then_some(mv.focus);
    s.popup = Some(ViewPopup::new(rows, place));
}

/// Input while the View popup is open. Up and Down pick a row, A runs it, Left and Right step the
/// layout row, B or View close it.
pub(super) fn popup_intent(s: &mut AppState, intent: Intent, cmds: &mut Vec<Command>) {
    let Some(popup) = s.popup.as_mut() else {
        return;
    };
    match intent {
        Intent::Up => popup.focus = popup.focus.saturating_sub(1),
        Intent::Down => popup.focus = (popup.focus + 1).min(popup.rows.len().saturating_sub(1)),
        Intent::Left | Intent::Right if popup.row() == Some(PopupRow::Layout) => {
            cycle_layout(s, intent == Intent::Right, cmds);
        }
        Intent::Select => {
            if let Some(row) = popup.row() {
                let popup = popup.clone();
                run_row(s, row, &popup, cmds);
            }
        }
        Intent::Back | Intent::View => s.popup = None,
        _ => {}
    }
    if matches!(s.screen, Screen::Player | Screen::Multiview) {
        pulse_hud(s);
    }
}

/// A on a popup row of `popup`. Every row but the layout closes the popup.
fn run_row(s: &mut AppState, row: PopupRow, popup: &ViewPopup, cmds: &mut Vec<Command>) {
    if row != PopupRow::Layout {
        s.popup = None;
    }
    let place = popup.place;
    let card = popup.card_stream().cloned();
    match row {
        PopupRow::SignIn => open_login(s, None, cmds),
        PopupRow::Pick => {
            if let Some(card) = card {
                pick(s, card);
            }
        }
        PopupRow::Unpick => {
            if let Some(card) = card {
                s.picked.retain(|p| !p.channel.is_same(&card.channel));
            }
        }
        PopupRow::ClearPicked => s.picked.clear(),
        PopupRow::Favorite => {
            if let Some(target) = s.favorite_target(popup) {
                favorites::toggle(s, target, cmds);
            }
        }
        PopupRow::WatchPicked if s.multiview.is_some() => {}
        PopupRow::WatchPicked => match card {
            Some(card) => {
                let set = s.watch_set(Some(&card));
                watch_together(s, set, 0, None, cmds);
            }
            None if popup.card.is_some() => {}
            None => watch_with_picked(s, cmds),
        },
        PopupRow::AddStream if s.multiview.is_none() => {
            if start_from_player(s, cmds) {
                open_picker(s, None, cmds);
            }
        }
        PopupRow::AddStream => {
            if s.screen == Screen::Player {
                to_grid(s, cmds);
            }
            open_picker(s, None, cmds);
        }
        PopupRow::Retry => {
            if let Some(place) = place {
                play_tile(s, place, cmds);
            }
        }
        PopupRow::Remove => {
            if let Some(place) = place {
                remove(s, place, cmds);
            }
        }
        PopupRow::Layout => cycle_layout(s, true, cmds),
        PopupRow::Leave => leave(s, cmds),
    }
}

// ---------------------------------------------------------------------------------------------
// Starting and leaving
// ---------------------------------------------------------------------------------------------

/// Adds a live browse card to the picked set, unless it is there already or the set is full.
fn pick(s: &mut AppState, card: Stream) {
    if !s.is_signed_in() || s.is_picked(&card.channel) || s.picked.len() >= layout::MAX_STREAMS {
        return;
    }
    s.picked.push(card);
}

/// *Watch the N together* on a browse card, or *Resume multiview* on Home: multiview starts with
/// `streams` on top of the browse screen (B returns to it), the one at `sound` with sound, in
/// `layout` (or, without one or when it is too small, the smallest grid that fits them). Each
/// stream loads on its own slot at its tile's quality; the app resolves a few at a time.
fn watch_together(
    s: &mut AppState,
    streams: Vec<Stream>,
    sound: usize,
    layout: Option<Layout>,
    cmds: &mut Vec<Command>,
) {
    if streams.len() < 2 || !s.is_signed_in() || s.multiview.is_some() {
        return;
    }
    s.rail.focused = false;
    s.chat_open = false;
    s.audio_only = false;
    s.paused = false;
    s.background_paused = false;
    s.picked = streams.clone();
    let mut mv = Multiview::starting_with_all(streams);
    if let Some(layout) = layout.filter(|l| l.fits(mv.count())) {
        mv.layout = layout;
        mv.fit_places();
    }
    let sound = if mv.tile(sound).is_some() { sound } else { 0 };
    mv.focus = sound;
    s.multiview = Some(mv);
    push_screen(s, Screen::Multiview);
    // `now_playing` (chat, raids, hype train) follows the stream with sound.
    give_sound(s, sound, cmds);
    let places: Vec<(usize, Slot)> = s
        .multiview
        .as_ref()
        .map(|mv| {
            mv.occupied()
                .filter_map(|p| mv.tile(p).map(|t| (p, t.slot)))
                .collect()
        })
        .unwrap_or_default();
    // The stream with sound loads first: the app resolves a few at a time, in order.
    let order = places
        .iter()
        .filter(|(p, _)| *p == sound)
        .chain(places.iter().filter(|(p, _)| *p != sound));
    for &(place, slot) in order {
        if place != sound {
            cmds.push(Command::SetMuted { slot, muted: true });
        }
        play_tile(s, place, cmds);
    }
}

/// A on Home's *Resume multiview* card: the last session starts again with the streams of it
/// that are live, in its layout, the one that had sound with sound again (or the first). A toast
/// says how many channels were left out because they were offline.
pub(super) fn resume(s: &mut AppState, card: ResumeCard, cmds: &mut Vec<Command>) {
    if !s.is_signed_in() || s.multiview.is_some() {
        return;
    }
    let mut streams = card.streams;
    streams.truncate(layout::MAX_STREAMS);
    if streams.len() < RESUME_MIN_LIVE {
        return;
    }
    let saved = s.last_multiview.clone();
    let has = |c: &ChannelRef| streams.iter().any(|st| st.channel.is_same(c));
    let skipped = saved
        .as_ref()
        .map_or(0, |set| set.channels.iter().filter(|c| !has(c)).count());
    let sound = saved
        .as_ref()
        .and_then(SavedSet::sound_channel)
        .and_then(|c| streams.iter().position(|st| st.channel.is_same(c)))
        .unwrap_or(0);
    let layout = saved.as_ref().map(|set| set.layout);
    watch_together(s, streams, sound, layout, cmds);
    if skipped > 0 && s.multiview.is_some() {
        push_toast(s, ToastKind::OfflineSkipped(skipped), cmds);
    }
}

/// After every message: a running session that changed (streams, layout, sound) is saved, for
/// Home's *Resume multiview* card.
pub(super) fn track_saved(s: &mut AppState, cmds: &mut Vec<Command>) {
    let Some(set) = s.multiview.as_ref().and_then(SavedSet::of) else {
        return;
    };
    if s.last_multiview.as_ref() == Some(&set) {
        return;
    }
    s.last_multiview = Some(set.clone());
    cmds.push(Command::SaveMultiview(set));
}

/// *Watch with the N picked* on the single player: multiview starts with the stream on screen
/// (which keeps playing, with sound) and the picked ones join it, muted.
fn watch_with_picked(s: &mut AppState, cmds: &mut Vec<Command>) {
    let others: Vec<Stream> = s.watch_set(None).into_iter().skip(1).collect();
    if others.is_empty() || !start_from_player(s, cmds) {
        return;
    }
    for stream in others {
        add_stream(s, stream, None, cmds);
    }
    if let Some(mv) = s.multiview.as_mut() {
        mv.focus = mv.sound;
    }
}

/// Multiview starts with the single player's stream, which keeps playing on its slot with sound.
/// The grid replaces the Player screen, so B returns to browsing. `false` if it could not start.
fn start_from_player(s: &mut AppState, cmds: &mut Vec<Command>) -> bool {
    if s.screen != Screen::Player || !s.is_signed_in() {
        return false;
    }
    let Some(stream) = s.now_playing.clone() else {
        return false;
    };
    // A tile is video and never paused: sound only, or a pause, means loading it again.
    let reload = s.audio_only || s.paused;
    s.audio_only = false;
    s.paused = false;
    s.background_paused = false;
    s.rechecking = false;
    // A recheck timer of the single player's offline screen finds nothing to do.
    s.offline_recheck = s.offline_recheck.wrapping_add(1);
    s.player_menu = None;
    s.subscribe_card = false;
    s.compose.open = false;
    s.chat_open = false;
    // What the single player plays now, which the network limit may have lowered.
    let rendition = match s.stream_kind() {
        StreamKind::Tile { rendition, .. } => rendition,
        _ => Rendition::ceiling(s.quality()),
    };
    s.multiview = Some(Multiview::starting_with(
        stream,
        MAIN_SLOT,
        s.playback.clone(),
        rendition,
        FrameRate::Smooth,
    ));
    s.screen = Screen::Multiview;
    if reload {
        reload_tile(s, 0, cmds);
    }
    schedule_settle(s, cmds);
    true
}

/// B in the grid, *Leave multiview*, or the sleep timer: every stream stops, and the screen it was
/// started from comes back. The streams stay picked (the one with sound first, offline ones
/// dropped), so the set can be grown and started again.
pub(super) fn leave(s: &mut AppState, cmds: &mut Vec<Command>) {
    let Some(mv) = s.multiview.take() else {
        return;
    };
    let fresher = |t: &MvTile| match &s.now_playing {
        // The stream with sound knows more by now (title, user id).
        Some(now) if now.channel.is_same(&t.stream.channel) => now.clone(),
        _ => t.stream.clone(),
    };
    let live = |t: &&MvTile| t.state != Playback::Offline;
    s.picked = mv
        .streams_sound_first()
        .into_iter()
        .filter(live)
        .map(fresher)
        .collect();
    // Home's resume card shows the set just left, its streams in place order.
    let in_order: Vec<Stream> = mv
        .occupied()
        .filter_map(|p| mv.tile(p))
        .filter(live)
        .map(fresher)
        .collect();
    resume::place(s, resume::card(in_order));
    let sound_slot = mv.sound_tile().map(|t| t.slot);
    for tile in mv.tiles.iter().flatten() {
        cmds.push(Command::StopPlayback { slot: tile.slot });
        // The single player plays there next, and must be heard.
        if tile.slot == MAIN_SLOT && sound_slot != Some(MAIN_SLOT) {
            cmds.push(Command::SetMuted {
                slot: MAIN_SLOT,
                muted: false,
            });
        }
    }
    if let (true, Some(slot)) = (s.session.stats, sound_slot) {
        cmds.push(Command::SetStats { slot, on: false });
    }
    s.popup = None;
    stop_watching(s, cmds);
    s.chat_open = false;
    if matches!(s.screen, Screen::Multiview | Screen::Player) {
        pop_screen(s);
    }
}

// ---------------------------------------------------------------------------------------------
// The grid
// ---------------------------------------------------------------------------------------------

/// Input on the grid. The raid card and the picker go first when open.
pub(super) fn grid_intent(s: &mut AppState, intent: Intent, cmds: &mut Vec<Command>) {
    if s.raid.is_some() {
        raid_intent(s, intent, cmds);
        return;
    }
    let Some(mv) = s.multiview.as_mut() else {
        return;
    };
    if mv.picker.is_some() {
        picker_intent(s, intent, cmds);
        return;
    }
    match intent {
        Intent::Up | Intent::Down | Intent::Left | Intent::Right => {
            if let Some(place) = mv.neighbour(mv.focus, intent) {
                mv.focus = place;
            }
        }
        Intent::Select => grid_select(s, cmds),
        Intent::Back => leave(s, cmds),
        // The chat of the stream with sound, as a side panel.
        Intent::ToggleChat => s.chat_open = !s.chat_open,
        Intent::PageLeft | Intent::PageRight => {
            cycle_layout(s, intent == Intent::PageRight, cmds);
        }
        Intent::View => grid_view(s),
        Intent::Retry => {
            let focus = mv.focus;
            if mv.focused_tile().is_some_and(|t| is_down(&t.state)) {
                play_tile(s, focus, cmds);
            }
        }
        Intent::Menu
        | Intent::Text(_)
        | Intent::Erase
        | Intent::Pick { .. }
        | Intent::RailPick(_)
        | Intent::Released(_) => {}
    }
}

/// A in the grid: on an empty slot the picker opens, on a muted tile the sound moves there, on
/// the tile with sound it goes full screen, and on an offline or failed tile *Retry* or *Remove*
/// is offered.
fn grid_select(s: &mut AppState, cmds: &mut Vec<Command>) {
    let Some(mv) = &s.multiview else {
        return;
    };
    let focus = mv.focus;
    let (exists, down) = match mv.focused_tile() {
        Some(tile) => (true, is_down(&tile.state)),
        None => (false, false),
    };
    let sound = focus == mv.sound;
    if !exists {
        open_picker(s, Some(focus), cmds);
    } else if down {
        let rows = vec![PopupRow::Retry, PopupRow::Remove];
        s.popup = Some(ViewPopup::new(rows, Some(focus)));
    } else if sound {
        s.screen = Screen::Player;
        pulse_hud(s);
        schedule_settle(s, cmds);
    } else {
        move_sound(s, focus, cmds);
    }
}

/// LB / RB in the grid, or the popup's layout row: the next layout that fits the streams.
fn cycle_layout(s: &mut AppState, forward: bool, cmds: &mut Vec<Command>) {
    let Some(mv) = s.multiview.as_mut() else {
        return;
    };
    mv.layout = mv.layout.cycled(mv.count(), forward);
    mv.fit_places();
    schedule_settle(s, cmds);
}

// ---------------------------------------------------------------------------------------------
// Full screen
// ---------------------------------------------------------------------------------------------

/// Input in multiview's full screen, once the player's own overlays had their turn. Returns
/// `false` for what works as on the single player (Start, Y, X, the D-pad).
pub(super) fn full_screen_intent(
    s: &mut AppState,
    intent: Intent,
    cmds: &mut Vec<Command>,
) -> bool {
    let Some(mv) = &s.multiview else {
        return false;
    };
    let sound = mv.sound;
    match intent {
        Intent::Select => match s.playback {
            Playback::Failed(_) | Playback::Offline => play_tile(s, sound, cmds),
            // Play jumps back to live: the stream is loaded again.
            Playback::Playing if s.paused => reload_tile(s, sound, cmds),
            Playback::Playing => {
                s.paused = true;
                cmds.push(Command::SetPaused {
                    slot: s.sound_slot(),
                    paused: true,
                });
            }
            Playback::Idle | Playback::Loading => {}
        },
        Intent::Retry if is_down(&s.playback) => play_tile(s, sound, cmds),
        Intent::Back => to_grid(s, cmds),
        Intent::PageLeft | Intent::PageRight => {
            let next = mv.cycled_stream(sound, intent == Intent::PageRight);
            move_sound(s, next, cmds);
        }
        Intent::View => player_view(s),
        _ => return false,
    }
    pulse_hud(s);
    true
}

/// B in full screen: back to the grid, the stream playing again (from live, if it was paused).
fn to_grid(s: &mut AppState, cmds: &mut Vec<Command>) {
    s.screen = Screen::Multiview;
    s.player_menu = None;
    s.subscribe_card = false;
    s.compose.open = false;
    let Some(mv) = s.multiview.as_mut() else {
        return;
    };
    let sound = mv.sound;
    mv.focus = sound;
    if s.paused {
        reload_tile(s, sound, cmds);
    }
    schedule_settle(s, cmds);
}

// ---------------------------------------------------------------------------------------------
// The sound
// ---------------------------------------------------------------------------------------------

/// The sound moves to the stream at `place`: the old one is muted, the new one heard, and then
/// `now_playing` follows (chat, raids, hype train, stream info). Nothing is loaded again, except
/// a stream left paused in full screen, which goes back to live.
fn move_sound(s: &mut AppState, place: usize, cmds: &mut Vec<Command>) {
    let Some(mv) = s.multiview.as_mut() else {
        return;
    };
    let old = mv.sound;
    if place == old || mv.tile(place).is_none() {
        return;
    }
    if let Some(tile) = mv.tile_mut(old) {
        // Keep what was learnt about it meanwhile (title, user id).
        if let Some(now) = s
            .now_playing
            .as_ref()
            .filter(|n| n.channel.is_same(&tile.stream.channel))
        {
            tile.stream = now.clone();
        }
        cmds.push(Command::SetMuted {
            slot: tile.slot,
            muted: true,
        });
        if s.session.stats {
            cmds.push(Command::SetStats {
                slot: tile.slot,
                on: false,
            });
        }
    }
    let was_paused = s.paused;
    give_sound(s, place, cmds);
    if was_paused {
        reload_tile(s, old, cmds);
    }
    schedule_settle(s, cmds);
}

/// The stream at `place` gets the sound and becomes `now_playing`. The tile that had it is the
/// caller's (muted, or gone).
fn give_sound(s: &mut AppState, place: usize, cmds: &mut Vec<Command>) {
    let Some(mv) = s.multiview.as_mut() else {
        return;
    };
    let Some(tile) = mv.tile(place).cloned() else {
        return;
    };
    mv.sound = place;
    cmds.push(Command::SetMuted {
        slot: tile.slot,
        muted: false,
    });
    if s.session.stats {
        cmds.push(Command::SetStats {
            slot: tile.slot,
            on: true,
        });
    }
    s.playback = tile.state;
    s.paused = false;
    let known_id = !tile.stream.user_id.is_empty();
    let channel = tile.stream.channel.clone();
    switch_now_playing(s, tile.stream, cmds);
    cmds.push(Command::FetchStreamInfo { channel });
    // Without the user id, raids are watched once the stream info brings it.
    if known_id {
        watch_raids(s, cmds);
    }
    pulse_hud(s);
}

// ---------------------------------------------------------------------------------------------
// Adding and removing
// ---------------------------------------------------------------------------------------------

/// Opens the picker, to fill `place` (an empty slot) or the first empty place. It opens on
/// Favorites when there are any. Who is live is asked again, and the lists on screen stay
/// meanwhile.
fn open_picker(s: &mut AppState, place: Option<usize>, cmds: &mut Vec<Command>) {
    let tab = if s.favorites.is_empty() {
        PickerTab::Following
    } else {
        PickerTab::Favorites
    };
    let Some(mv) = s.multiview.as_mut() else {
        return;
    };
    if mv.is_full() {
        return;
    }
    mv.picker = Some(Picker {
        tab,
        focus: 0,
        place,
    });
    if !s.following.loading {
        s.following.loading = true;
        s.following.error = None;
        cmds.push(Command::LoadFeed {
            feed: crate::domain::FeedId::Following,
        });
    }
    favorites::reload(s, cmds);
}

/// Input while the picker is open. Up and Down pick a row, A adds its stream (muted), Y stars or
/// unstars its channel, LB and RB switch tabs, B closes the picker. It stays open after adding,
/// until multiview is full. Offline favorites are listed, but A does nothing on them.
fn picker_intent(s: &mut AppState, intent: Intent, cmds: &mut Vec<Command>) {
    let Some(picker) = s.multiview.as_ref().and_then(|m| m.picker) else {
        return;
    };
    let rows = s.picker_rows(picker.tab);
    let last = rows.len().saturating_sub(1);
    let focused = rows.get(picker.focus.min(last)).map(|t| (*t).clone());
    let mut next = picker;
    match intent {
        Intent::Up => next.focus = picker.focus.saturating_sub(1),
        Intent::Down => next.focus = (picker.focus + 1).min(last),
        Intent::PageLeft | Intent::PageRight => {
            next.tab = picker.tab.other();
            next.focus = 0;
        }
        Intent::Select => {
            if let Some(Tile::Stream(stream)) = focused {
                add_stream(s, stream, picker.place, cmds);
            }
            return;
        }
        Intent::ToggleChat => {
            if let Some(tile) = focused {
                favorites::toggle(s, tile, cmds);
            }
            // Unstarring on Favorites takes the row away.
            next.focus = picker
                .focus
                .min(s.picker_rows(picker.tab).len().saturating_sub(1));
        }
        _ => {}
    }
    if let Some(mv) = s.multiview.as_mut() {
        mv.picker = (intent != Intent::Back).then_some(next);
    }
}

/// Adds `stream`, muted, to the empty place `hint` (or the first empty one). With no empty place
/// a grid grows to the next size; Spotlight makes room by itself. A stream already watched, or a
/// tenth, is not added.
fn add_stream(s: &mut AppState, stream: Stream, hint: Option<usize>, cmds: &mut Vec<Command>) {
    let Some(mv) = s.multiview.as_mut() else {
        return;
    };
    if mv.is_full() || mv.is_watching(&stream.channel) {
        return;
    }
    let Some(slot) = mv.free_slot() else {
        return;
    };
    let hinted = hint.filter(|p| mv.tiles.get(*p).is_some_and(Option::is_none));
    let place = match hinted.or_else(|| mv.first_empty()) {
        Some(place) => place,
        None => {
            let streams = mv.count() + 1;
            if !mv.layout.fits(streams) {
                mv.layout = Layout::smallest_grid(streams);
            }
            mv.tiles.resize(mv.layout.places(streams), None);
            let Some(place) = mv.first_empty() else {
                return;
            };
            place
        }
    };
    mv.tiles[place] = Some(MvTile {
        stream,
        slot,
        state: Playback::Loading,
        // Set by `play_tile` below.
        rendition: Rendition::P160,
        rate: FrameRate::Light,
        lowered: false,
    });
    mv.fit_places();
    let Some(place) = mv.place_of_slot(slot) else {
        return;
    };
    mv.focus = place;
    let full = mv.is_full();
    if let Some(picker) = mv.picker.as_mut() {
        picker.place = None;
    }
    if full {
        mv.picker = None;
    }
    cmds.push(Command::SetMuted { slot, muted: true });
    play_tile(s, place, cmds);
    // The other tiles may have changed size.
    schedule_settle(s, cmds);
}

/// Stops the stream at `place` and leaves its place empty. If it had the sound, the first stream
/// left gets it (and full screen returns to the grid). The last stream removed leaves multiview.
fn remove(s: &mut AppState, place: usize, cmds: &mut Vec<Command>) {
    let Some(mv) = s.multiview.as_mut() else {
        return;
    };
    let Some(tile) = mv.tiles.get_mut(place).and_then(Option::take) else {
        return;
    };
    let was_sound = place == mv.sound;
    cmds.push(Command::StopPlayback { slot: tile.slot });
    if tile.slot == MAIN_SLOT && !was_sound {
        cmds.push(Command::SetMuted {
            slot: MAIN_SLOT,
            muted: false,
        });
    }
    if mv.count() == 0 {
        leave(s, cmds);
        return;
    }
    mv.fit_places();
    if was_sound {
        let Some(next) = mv.occupied().next() else {
            return;
        };
        if s.screen == Screen::Player {
            s.screen = Screen::Multiview;
        }
        give_sound(s, next, cmds);
    }
    schedule_settle(s, cmds);
}

// ---------------------------------------------------------------------------------------------
// Playing tiles and their quality
// ---------------------------------------------------------------------------------------------

/// Which streams a network step lowers the stream at `place` with: the one with sound, a muted
/// tile bigger than the other muted ones, or a muted small one (every muted tile of a grid).
pub(super) fn tier(mv: &Multiview, place: usize) -> Tier {
    if place == mv.sound {
        return Tier::Sound;
    }
    let rects = mv.rects();
    let height = |p: usize| rects.get(p).map_or(0.0, |r| r.h);
    let smallest = mv
        .occupied()
        .filter(|p| *p != mv.sound)
        .map(height)
        .fold(f32::INFINITY, f32::min);
    if height(place) > smallest + 0.5 {
        Tier::MutedBig
    } else {
        Tier::MutedSmall
    }
}

/// The quality the stream at `place` should play at now: from its height on screen, never above
/// the quality setting, then lowered as far as the network limit lowered its tier. The stream with
/// sound in full screen gets the whole screen and 60 fps; tiles take 30 fps where the stream has
/// both.
pub(super) fn wanted(s: &AppState, place: usize) -> TileQuality {
    let full = layout::FULL_SCREEN.h;
    let Some(mv) = &s.multiview else {
        return TileQuality {
            rendition: Rendition::ceiling(s.quality()),
            rate: FrameRate::Smooth,
            lowered: false,
        };
    };
    let full_screen = s.screen == Screen::Player && place == mv.sound;
    let height = if full_screen {
        full
    } else {
        mv.rects().get(place).map_or(full, |r| r.h)
    };
    let pixels = layout::pixel_height(height, s.display);
    let by_size = quality::rendition(pixels, s.quality(), 0);
    let rendition = by_size.lowered(s.net.steps(tier(mv, place)));
    TileQuality {
        rendition,
        rate: if full_screen {
            FrameRate::Smooth
        } else {
            FrameRate::Light
        },
        lowered: rendition < by_size,
    }
}

/// Loads the stream at `place` at the quality it should have now, showing "Starting…" until it
/// plays. How a tile starts and is retried.
pub(super) fn play_tile(s: &mut AppState, place: usize, cmds: &mut Vec<Command>) {
    load_tile(s, place, false, cmds);
}

/// The stream at `place` again: at the quality it should have now, or back to live from a pause.
/// A tile that plays stays `Playing`: the app loads the new one behind it and swaps at its first
/// frame, so the picture never stops. Any other tile loads as in [`play_tile`].
pub(super) fn reload_tile(s: &mut AppState, place: usize, cmds: &mut Vec<Command>) {
    load_tile(s, place, true, cmds);
}

fn load_tile(s: &mut AppState, place: usize, seamless: bool, cmds: &mut Vec<Command>) {
    let want = wanted(s, place);
    let latency = s.session.latency;
    let Some(mv) = s.multiview.as_mut() else {
        return;
    };
    let sound = place == mv.sound;
    let Some(tile) = mv.tile_mut(place) else {
        return;
    };
    let seamless = seamless && tile.state == Playback::Playing;
    if !seamless {
        tile.state = Playback::Loading;
    }
    tile.rendition = want.rendition;
    tile.rate = want.rate;
    tile.lowered = want.lowered;
    let slot: Slot = tile.slot;
    cmds.push(Command::Play {
        slot,
        channel: tile.stream.channel.clone(),
        stream: StreamKind::Tile {
            rendition: want.rendition,
            rate: want.rate,
        },
        latency,
    });
    if sound {
        s.paused = false;
        if !seamless {
            s.playback = Playback::Loading;
            s.stats = None;
        }
        if s.session.stats {
            cmds.push(Command::SetStats { slot, on: true });
        }
    }
}

/// `start_playback` in multiview: the stream with sound, again (a raid was followed, or the
/// player menu changed the quality or latency).
pub(super) fn play_sound(s: &mut AppState, cmds: &mut Vec<Command>) {
    let Some(place) = s.multiview.as_ref().map(|m| m.sound) else {
        return;
    };
    play_tile(s, place, cmds);
    // A new quality cap may change the other tiles too.
    schedule_settle(s, cmds);
}

/// The stream with sound again, without a gap (the player menu changed the quality or latency).
pub(super) fn reload_sound(s: &mut AppState, cmds: &mut Vec<Command>) {
    let Some(place) = s.multiview.as_ref().map(|m| m.sound) else {
        return;
    };
    reload_tile(s, place, cmds);
    schedule_settle(s, cmds);
}

/// The stream at `place` plays at another quality than it should now. Offline and failed tiles
/// wait for a retry, and a paused one is left alone.
fn needs_reload(s: &AppState, place: usize) -> bool {
    let Some(mv) = &s.multiview else {
        return false;
    };
    let Some(tile) = mv.tile(place) else {
        return false;
    };
    if is_down(&tile.state) || (place == mv.sound && s.paused) {
        return false;
    }
    let want = wanted(s, place);
    (tile.rendition, tile.rate) != (want.rendition, want.rate)
}

/// Something changed the tiles' sizes (layout, full screen, the sound in Spotlight, the window).
/// If a tile now plays at the wrong quality, a 2 s timer starts, replacing any running one; the
/// tiles reload when it runs out (`settle_due`). Back to how it was before then, nothing reloads.
pub(super) fn schedule_settle(s: &mut AppState, cmds: &mut Vec<Command>) {
    let Some(mv) = &s.multiview else {
        return;
    };
    if !mv.occupied().any(|p| needs_reload(s, p)) {
        return;
    }
    s.last_settle_id = s.last_settle_id.wrapping_add(1);
    let id = s.last_settle_id;
    if let Some(mv) = s.multiview.as_mut() {
        mv.settle = id;
    }
    cmds.push(Command::SettleQuality { id });
}

/// The settle timer `id` ran out: if it is the latest, every tile at the wrong quality reloads.
pub(super) fn settle_due(s: &mut AppState, id: u32, cmds: &mut Vec<Command>) {
    let Some(mv) = &s.multiview else {
        return;
    };
    if mv.settle != id {
        return;
    }
    reload_stale(s, cmds);
}

/// Every tile at the wrong quality reloads now (the settle timer ran out, or the network limit
/// moved). A settle timer still running then finds nothing to do.
pub(super) fn reload_stale(s: &mut AppState, cmds: &mut Vec<Command>) {
    let Some(mv) = &s.multiview else {
        return;
    };
    let stale: Vec<usize> = mv.occupied().filter(|p| needs_reload(s, *p)).collect();
    for place in stale {
        reload_tile(s, place, cmds);
    }
}

/// Whether some stream of `tier` on screen can still go a rendition lower.
pub(super) fn has_room(s: &AppState, tier: Tier) -> bool {
    let Some(mv) = &s.multiview else {
        return false;
    };
    mv.occupied()
        .any(|p| self::tier(mv, p) == tier && wanted(s, p).rendition > Rendition::P160)
}

// ---------------------------------------------------------------------------------------------
// Player reports
// ---------------------------------------------------------------------------------------------

/// Moves the tile at `place` to the state `next` gives (`None`: the report does not apply in
/// the tile's state). The stream with sound mirrors it into `playback`.
fn update_tile(
    s: &mut AppState,
    place: Option<usize>,
    next: impl FnOnce(&Playback) -> Option<Playback>,
) {
    let (Some(mv), Some(place)) = (s.multiview.as_mut(), place) else {
        return;
    };
    let sound = place == mv.sound;
    let Some(tile) = mv.tile_mut(place) else {
        return;
    };
    let Some(state) = next(&tile.state) else {
        return;
    };
    tile.state = state.clone();
    if sound {
        if state != Playback::Playing {
            s.paused = false;
        }
        s.playback = state;
        pulse_hud(s);
    }
}

fn place_of_slot(s: &AppState, slot: Slot) -> Option<usize> {
    s.multiview.as_ref().and_then(|m| m.place_of_slot(slot))
}

fn place_of_channel(s: &AppState, channel: &ChannelRef) -> Option<usize> {
    s.multiview
        .as_ref()
        .and_then(|m| m.place_of_channel(channel))
}

pub(super) fn tile_started(s: &mut AppState, slot: Slot) {
    let place = place_of_slot(s, slot);
    update_tile(s, place, |st| {
        (*st == Playback::Loading).then_some(Playback::Playing)
    });
}

pub(super) fn tile_ended(s: &mut AppState, slot: Slot) {
    let place = place_of_slot(s, slot);
    update_tile(s, place, |st| {
        (*st == Playback::Playing).then_some(Playback::Offline)
    });
}

pub(super) fn tile_error(s: &mut AppState, slot: Slot, message: String) {
    let place = place_of_slot(s, slot);
    update_tile(s, place, |st| {
        matches!(st, Playback::Loading | Playback::Playing).then_some(Playback::Failed(message))
    });
}

pub(super) fn tile_offline(s: &mut AppState, channel: &ChannelRef) {
    let place = place_of_channel(s, channel);
    update_tile(s, place, |st| {
        (*st == Playback::Loading).then_some(Playback::Offline)
    });
}

pub(super) fn tile_failed(s: &mut AppState, channel: &ChannelRef, message: String) {
    let place = place_of_channel(s, channel);
    update_tile(s, place, |st| {
        (*st == Playback::Loading).then_some(Playback::Failed(message))
    });
}
