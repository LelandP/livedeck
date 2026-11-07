//! Multiview's part of the view-model: the grid's places, the header, the picker, the View popup
//! and the strip over full screen (plan.md, phase 4).

use htpc_core::{
    AppState, Layout, Multiview, MvTile, PickerTab, Platform, Playback, PopupRow, Screen, Stream,
};

use super::{format_viewers, tile_card, to_i32, CardVm, PlaybackId};

/// One place of the grid, in the 1280x800 layout's pixels (`htpc_core::layout::rects`).
#[derive(Debug, Clone, PartialEq, Default)]
pub struct MvTileVm {
    pub x: f32,
    pub y: f32,
    pub w: f32,
    pub h: f32,
    /// An *Add a stream* slot: the fields below are unused.
    pub empty: bool,
    pub slot: i32,
    pub channel: String,
    /// "Twitch · Just Chatting · 4.2K watching".
    pub line: String,
    pub kick: bool,
    pub tint: htpc_core::Rgb,
    /// The rendition it was last asked to play at ("480p").
    pub quality: String,
    /// The slow-network rule lowered it (plan.md, section 4): drawn amber with a down arrow.
    pub lowered: bool,
    /// The stream with sound.
    pub sound: bool,
    pub focused: bool,
    pub state: PlaybackId,
    /// What A does on the focused place. Empty on the others.
    pub action: String,
}

/// A chip of the header's layout row.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct LayoutChipVm {
    pub label: String,
    pub on: bool,
    /// Big enough for the streams. LB/RB skip the others.
    pub fits: bool,
}

/// A thumbnail of the strip over full screen.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct StripVm {
    pub slot: i32,
    pub channel: String,
    pub kick: bool,
    /// The stream on screen.
    pub on: bool,
}

/// A row of the View popup.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct PopupRowVm {
    pub label: String,
    /// Shown on the right (the layout row's current layout).
    pub value: String,
}

/// Everything multiview adds to the view-model.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct MultiviewVm {
    pub active: bool,
    pub tiles: Vec<MvTileVm>,
    pub layouts: Vec<LayoutChipVm>,
    /// "3 streams".
    pub count_text: String,
    pub count: i32,
    /// "2 of 3 in multiview", for the info box over full screen.
    pub position: String,
    pub strip: Vec<StripVm>,
    pub picker_open: bool,
    /// 0 Favorites, 1 Following.
    pub picker_tab: i32,
    /// Live streams, and on Favorites the offline channels after them (`CardVm::offline`).
    pub picker_rows: Vec<CardVm>,
    /// Same order as `picker_rows`: that channel is already in multiview (*Watching*).
    pub picker_watching: Vec<bool>,
    /// Same order as `picker_rows`: that channel is a favorite (a star).
    pub picker_starred: Vec<bool>,
    /// What Y does on the focused row: "Star" or "Unstar". Empty with no rows.
    pub picker_star_label: String,
    pub picker_focus: i32,
    /// What the picker says while it lists nothing.
    pub picker_empty: String,
    pub popup_open: bool,
    pub popup_title: String,
    pub popup_sub: String,
    pub popup_rows: Vec<PopupRowVm>,
    pub popup_focus: i32,
    /// The focused popup row is the layout: Left/Right step it.
    pub popup_on_layout: bool,
}

/// "Grid of 4", "Spotlight".
pub fn layout_name(layout: Layout) -> String {
    match layout {
        Layout::Grid(n) => format!("Grid of {n}"),
        Layout::Spotlight => "Spotlight".to_owned(),
    }
}

fn platform_name(p: Platform) -> &'static str {
    match p {
        Platform::Twitch => "Twitch",
        Platform::Kick => "Kick",
    }
}

fn streams_text(n: usize) -> String {
    if n == 1 {
        "1 stream".to_owned()
    } else {
        format!("{n} streams")
    }
}

/// "Twitch · Just Chatting · 4.2K watching", leaving out what is not known.
fn caption_line(s: &Stream) -> String {
    let mut parts = vec![platform_name(s.channel.platform).to_owned()];
    if !s.category.is_empty() {
        parts.push(s.category.clone());
    }
    if s.viewers > 0 {
        parts.push(format!("{} watching", format_viewers(s.viewers)));
    }
    parts.join(" · ")
}

pub(super) fn playback_id(p: &Playback) -> PlaybackId {
    match p {
        Playback::Idle | Playback::Loading => PlaybackId::Loading,
        Playback::Playing => PlaybackId::Playing,
        Playback::Offline => PlaybackId::Offline,
        Playback::Failed(_) => PlaybackId::Failed,
    }
}

fn is_down(p: &Playback) -> bool {
    matches!(p, Playback::Offline | Playback::Failed(_))
}

/// The stream as the view-model shows it: the tile's, or `now_playing` for the one with sound
/// (which knows more by now, such as the title from the stream info).
fn tile_stream<'a>(
    state: &'a AppState,
    mv: &Multiview,
    place: usize,
    tile: &'a MvTile,
) -> &'a Stream {
    match &state.now_playing {
        Some(now) if place == mv.sound && now.channel.is_same(&tile.stream.channel) => now,
        _ => &tile.stream,
    }
}

/// What A does on the focused place.
fn action(mv: &Multiview, place: usize) -> &'static str {
    match mv.tile(place) {
        None => "Add a stream",
        Some(t) if is_down(&t.state) => "Retry or remove",
        Some(_) if place == mv.sound => "Full screen",
        Some(_) => "Listen",
    }
}

fn tiles(state: &AppState, mv: &Multiview) -> Vec<MvTileVm> {
    // The picker and the popup take the focus: no tile shows it meanwhile.
    let show_focus =
        state.screen == Screen::Multiview && mv.picker.is_none() && state.popup.is_none();
    mv.rects()
        .iter()
        .enumerate()
        .map(|(place, r)| {
            let focused = show_focus && place == mv.focus;
            let base = MvTileVm {
                x: r.x,
                y: r.y,
                w: r.w,
                h: r.h,
                focused,
                action: if focused {
                    action(mv, place).to_owned()
                } else {
                    String::new()
                },
                ..MvTileVm::default()
            };
            match mv.tile(place) {
                None => MvTileVm {
                    empty: true,
                    ..base
                },
                Some(tile) => {
                    let s = tile_stream(state, mv, place, tile);
                    MvTileVm {
                        slot: i32::from(tile.slot),
                        channel: s.channel.login.clone(),
                        line: caption_line(s),
                        kick: s.channel.platform == Platform::Kick,
                        tint: s.tint,
                        quality: tile.rendition.label().to_owned(),
                        lowered: tile.lowered,
                        sound: place == mv.sound,
                        state: playback_id(&tile.state),
                        ..base
                    }
                }
            }
        })
        .collect()
}

fn layouts(mv: &Multiview) -> Vec<LayoutChipVm> {
    let count = mv.count();
    Layout::ALL
        .iter()
        .map(|l| LayoutChipVm {
            label: match l {
                Layout::Grid(n) => n.to_string(),
                Layout::Spotlight => "Spotlight".to_owned(),
            },
            on: *l == mv.layout,
            fits: l.fits(count),
        })
        .collect()
}

fn strip(mv: &Multiview) -> Vec<StripVm> {
    mv.occupied()
        .filter_map(|place| {
            let t = mv.tile(place)?;
            Some(StripVm {
                slot: i32::from(t.slot),
                channel: t.stream.channel.login.clone(),
                kick: t.stream.channel.platform == Platform::Kick,
                on: place == mv.sound,
            })
        })
        .collect()
}

/// "2 of 3 in multiview": where the stream with sound is among the streams.
fn position(mv: &Multiview) -> String {
    let at = mv.occupied().position(|p| p == mv.sound).unwrap_or(0);
    format!("{} of {} in multiview", at + 1, mv.count())
}

fn picker_empty(state: &AppState, tab: PickerTab) -> String {
    match tab {
        PickerTab::Favorites if state.favorites.is_empty() => {
            "No favorites yet. Press Y on a channel to star it.".to_owned()
        }
        PickerTab::Favorites if state.favorites_feed.loading => {
            "Checking which favorites are live…".to_owned()
        }
        PickerTab::Favorites => match &state.favorites_feed.error {
            Some(error) => format!("Could not check your favorites: {error}"),
            None => "None of your favorites can be shown right now.".to_owned(),
        },
        PickerTab::Following if state.following.loading => {
            "Looking for the channels you follow…".to_owned()
        }
        PickerTab::Following => match &state.following.error {
            Some(error) => format!("Could not load the channels you follow: {error}"),
            None => "Nobody you follow is live right now.".to_owned(),
        },
    }
}

/// The channel at a multiview place, for popup rows about it.
fn channel_at(state: &AppState, place: Option<usize>) -> String {
    state
        .multiview
        .as_ref()
        .zip(place)
        .and_then(|(mv, p)| mv.tile(p))
        .map(|t| t.stream.channel.login.clone())
        .unwrap_or_default()
}

fn popup_rows(state: &AppState) -> Vec<PopupRowVm> {
    let Some(popup) = &state.popup else {
        return Vec::new();
    };
    let channel = channel_at(state, popup.place);
    // How many *Watch together* starts, besides the stream on screen for the single player.
    let together = state.watch_set(popup.card_stream()).len();
    let starred = state
        .favorite_target(popup)
        .is_some_and(|t| t.channel().is_some_and(|c| state.is_favorite(c)));
    // In multiview the row names the stream it is about.
    let in_multiview = state.multiview.is_some() && popup.card.is_none();
    popup
        .rows
        .iter()
        .map(|row| {
            let label = match row {
                PopupRow::SignIn => "Sign in to watch several streams".to_owned(),
                PopupRow::Pick => "Add to multiview".to_owned(),
                PopupRow::Unpick => "Remove from multiview".to_owned(),
                PopupRow::WatchPicked if popup.card.is_some() => {
                    format!("Watch the {together} together")
                }
                PopupRow::WatchPicked => {
                    format!("Watch with the {} picked", together.saturating_sub(1))
                }
                PopupRow::ClearPicked => "Clear picked".to_owned(),
                PopupRow::Favorite if in_multiview && starred => {
                    format!("Remove {channel} from favorites")
                }
                PopupRow::Favorite if in_multiview => format!("Add {channel} to favorites"),
                PopupRow::Favorite if starred => "Remove from favorites".to_owned(),
                PopupRow::Favorite => "Add to favorites".to_owned(),
                PopupRow::AddStream if state.multiview.is_none() => {
                    "Add a stream to watch with this one".to_owned()
                }
                PopupRow::AddStream => "Add a stream".to_owned(),
                PopupRow::Retry => format!("Try {channel} again"),
                PopupRow::Remove => format!("Remove {channel}"),
                PopupRow::Layout => "Layout".to_owned(),
                PopupRow::Leave => "Leave multiview".to_owned(),
            };
            let value = match (row, &state.multiview) {
                (PopupRow::Layout, Some(mv)) => layout_name(mv.layout),
                _ => String::new(),
            };
            PopupRowVm { label, value }
        })
        .collect()
}

/// The line under the popup's title: what it is about.
fn popup_sub(state: &AppState) -> String {
    let Some(popup) = &state.popup else {
        return String::new();
    };
    if popup.rows == [PopupRow::SignIn] {
        return "Watch several streams at once".to_owned();
    }
    if let Some(card) = &popup.card {
        let login = card.channel().map(|c| c.login.as_str()).unwrap_or_default();
        return match state.picked.len() {
            _ if popup.card_stream().is_none() => format!("{login} · offline"),
            0 => login.to_owned(),
            n => format!("{login} · {n} picked"),
        };
    }
    let Some(mv) = &state.multiview else {
        let channel = state
            .now_playing
            .as_ref()
            .map(|s| s.channel.login.as_str())
            .unwrap_or_default();
        return format!("Watching {channel}");
    };
    if let Some(tile) = popup.place.and_then(|p| mv.tile(p)) {
        match tile.state {
            Playback::Offline => return format!("{} is offline", tile.stream.channel.login),
            Playback::Failed(_) => return format!("{} could not play", tile.stream.channel.login),
            _ => {}
        }
    }
    let sound = mv
        .sound_tile()
        .map(|t| t.stream.channel.login.as_str())
        .unwrap_or_default();
    format!(
        "{} · {} · sound from {sound}",
        streams_text(mv.count()),
        layout_name(mv.layout)
    )
}

pub(super) fn multiview_vm(state: &AppState) -> MultiviewVm {
    let mut vm = MultiviewVm {
        picker_tab: 1,
        ..MultiviewVm::default()
    };
    if let Some(popup) = &state.popup {
        vm.popup_open = true;
        // An offline card's popup can only star it.
        vm.popup_title = if popup.rows == [PopupRow::Favorite] {
            "Favorites"
        } else {
            "Multiview"
        }
        .to_owned();
        vm.popup_sub = popup_sub(state);
        vm.popup_rows = popup_rows(state);
        vm.popup_focus = to_i32(popup.focus);
        vm.popup_on_layout = popup.row() == Some(PopupRow::Layout);
    }
    let Some(mv) = &state.multiview else {
        return vm;
    };
    vm.active = true;
    vm.tiles = tiles(state, mv);
    vm.layouts = layouts(mv);
    vm.count = to_i32(mv.count());
    vm.count_text = streams_text(mv.count());
    vm.position = position(mv);
    vm.strip = strip(mv);
    if let Some(picker) = mv.picker.filter(|_| state.screen == Screen::Multiview) {
        let rows = state.picker_rows(picker.tab);
        vm.picker_open = true;
        vm.picker_tab = match picker.tab {
            PickerTab::Favorites => 0,
            PickerTab::Following => 1,
        };
        let channels: Vec<_> = rows.iter().filter_map(|t| t.channel()).collect();
        vm.picker_watching = channels.iter().map(|c| mv.is_watching(c)).collect();
        vm.picker_starred = channels.iter().map(|c| state.is_favorite(c)).collect();
        vm.picker_rows = rows
            .into_iter()
            .map(|t| CardVm {
                // The tick of picked cards means nothing here.
                picked: false,
                ..tile_card(state, t)
            })
            .collect();
        let focus = picker.focus.min(vm.picker_rows.len().saturating_sub(1));
        vm.picker_focus = to_i32(focus);
        vm.picker_star_label = match vm.picker_starred.get(focus) {
            Some(true) => "Unstar",
            Some(false) => "Star",
            None => "",
        }
        .to_owned();
        vm.picker_empty = picker_empty(state, picker.tab);
    }
    vm
}

/// What A does in the grid, the picker or the popup (the hints bar's A), and whether X retries.
/// `None` off those.
pub(super) fn select_label(state: &AppState, vm: &MultiviewVm) -> Option<(&'static str, bool)> {
    if vm.popup_open {
        return Some((
            if vm.popup_on_layout {
                "Next layout"
            } else {
                "Select"
            },
            false,
        ));
    }
    if state.screen != Screen::Multiview {
        return None;
    }
    let mv = state.multiview.as_ref()?;
    if vm.picker_open {
        let focus = usize::try_from(vm.picker_focus).unwrap_or(0);
        let offline = vm.picker_rows.get(focus).is_some_and(|c| c.offline);
        let label = match vm.picker_watching.get(focus) {
            Some(false) if !offline => "Add",
            // Already watched, offline, or nothing to add.
            Some(_) | None => "",
        };
        return Some((label, false));
    }
    let down = mv.focused_tile().is_some_and(|t| is_down(&t.state));
    Some((action(mv, mv.focus), down))
}
