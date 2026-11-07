//! Multiview's state: the tiles, which one has sound, the layout, and the View popup and picker.
//! The reducer's side is in `reduce/multiview.rs`.
//!
//! The stream with sound is also `AppState::now_playing`, and its state is mirrored into
//! `AppState::playback` and `AppState::paused`, so chat, raids, hype trains, the player menu and the
//! full-screen player work on it as on the single player. The tiles here are the authority.

use crate::domain::{ChannelRef, Stream, Tile};
use crate::layout::{self, Layout, Rect, MAX_STREAMS};
use crate::msg::{Intent, Slot};
use crate::quality::{FrameRate, Rendition};
use crate::state::Playback;

/// One stream in multiview.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MvTile {
    pub stream: Stream,
    /// The player it plays on. Never shared with another tile.
    pub slot: Slot,
    /// Loading, playing, offline or failed. A tile is never paused in the grid.
    pub state: Playback,
    /// The height it was last asked to play at, and the frame rate tried first. The settle timer
    /// compares it with what its size on screen calls for.
    pub rendition: Rendition,
    pub rate: FrameRate,
    /// The network limit made `rendition` lower than the tile's size calls for (its quality chip
    /// turns amber).
    pub lowered: bool,
}

/// The quality a tile should play at now: from its size on screen (plan.md, section 3), lowered
/// by the network limit (section 4).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TileQuality {
    pub rendition: Rendition,
    pub rate: FrameRate,
    /// Lower than the size alone calls for.
    pub lowered: bool,
}

/// A running multiview session.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Multiview {
    /// The layout's places in order: a stream, or an empty *Add a stream* slot. There are always
    /// `layout.places(count)` of them (`fit_places`).
    pub tiles: Vec<Option<MvTile>>,
    pub layout: Layout,
    /// The place of the stream with sound (`AppState::now_playing`). Always holds a stream.
    pub sound: usize,
    /// The focused place in the grid (D-pad). Can be an empty slot.
    pub focus: usize,
    /// The picker panel is open (A on an empty slot, or *Add a stream*).
    pub picker: Option<Picker>,
    /// The id of the latest `Command::SettleQuality`. An older timer finds nothing to do.
    pub settle: u32,
}

impl Multiview {
    /// A session with one stream, on `slot`, with sound. `rendition` and `rate` are what that
    /// stream already plays at.
    pub fn starting_with(
        stream: Stream,
        slot: Slot,
        state: Playback,
        rendition: Rendition,
        rate: FrameRate,
    ) -> Self {
        let mut mv = Self {
            tiles: vec![Some(MvTile {
                stream,
                slot,
                state,
                rendition,
                rate,
                lowered: false,
            })],
            layout: Layout::smallest_grid(2),
            sound: 0,
            focus: 0,
            picker: None,
            settle: 0,
        };
        mv.fit_places();
        mv
    }

    /// A session with `streams` (at most `MAX_STREAMS`) on slots 0, 1, ..., in the smallest grid
    /// that fits them. The first has the sound. Every tile is loading; `play_tile` sets its quality.
    pub fn starting_with_all(streams: Vec<Stream>) -> Self {
        let tiles: Vec<Option<MvTile>> = streams
            .into_iter()
            .take(MAX_STREAMS)
            .enumerate()
            .map(|(i, stream)| {
                Some(MvTile {
                    stream,
                    slot: i as Slot,
                    state: Playback::Loading,
                    rendition: Rendition::P160,
                    rate: FrameRate::Light,
                    lowered: false,
                })
            })
            .collect();
        let mut mv = Self {
            layout: Layout::smallest_grid(tiles.len()),
            tiles,
            sound: 0,
            focus: 0,
            picker: None,
            settle: 0,
        };
        mv.fit_places();
        mv
    }

    /// The streams, the one with sound first and then the others in place order. Leaving
    /// multiview keeps them picked in this order, so starting again gives the sound back.
    pub fn streams_sound_first(&self) -> Vec<&MvTile> {
        self.sound_tile()
            .into_iter()
            .chain(
                self.occupied()
                    .filter(|p| *p != self.sound)
                    .filter_map(|p| self.tile(p)),
            )
            .collect()
    }

    /// How many streams there are.
    pub fn count(&self) -> usize {
        self.tiles.iter().flatten().count()
    }

    /// No room for another stream.
    pub fn is_full(&self) -> bool {
        self.count() >= MAX_STREAMS
    }

    pub fn tile(&self, place: usize) -> Option<&MvTile> {
        self.tiles.get(place).and_then(Option::as_ref)
    }

    pub fn tile_mut(&mut self, place: usize) -> Option<&mut MvTile> {
        self.tiles.get_mut(place).and_then(Option::as_mut)
    }

    /// The stream with sound.
    pub fn sound_tile(&self) -> Option<&MvTile> {
        self.tile(self.sound)
    }

    pub fn focused_tile(&self) -> Option<&MvTile> {
        self.tile(self.focus)
    }

    /// The place of the tile playing on `slot`.
    pub fn place_of_slot(&self, slot: Slot) -> Option<usize> {
        self.tiles
            .iter()
            .position(|t| t.as_ref().is_some_and(|t| t.slot == slot))
    }

    /// The place of the tile showing `channel`.
    pub fn place_of_channel(&self, channel: &ChannelRef) -> Option<usize> {
        self.tiles.iter().position(|t| {
            t.as_ref()
                .is_some_and(|t| t.stream.channel.is_same(channel))
        })
    }

    /// `channel` is one of the streams (the picker tags it *Watching*).
    pub fn is_watching(&self, channel: &ChannelRef) -> bool {
        self.place_of_channel(channel).is_some()
    }

    /// The places holding a stream, in order.
    pub fn occupied(&self) -> impl Iterator<Item = usize> + '_ {
        self.tiles
            .iter()
            .enumerate()
            .filter(|(_, t)| t.is_some())
            .map(|(i, _)| i)
    }

    /// The first empty place.
    pub fn first_empty(&self) -> Option<usize> {
        self.tiles.iter().position(Option::is_none)
    }

    /// The lowest slot no tile plays on.
    pub fn free_slot(&self) -> Option<Slot> {
        (0..MAX_STREAMS as Slot).find(|s| self.place_of_slot(*s).is_none())
    }

    /// Where each place is drawn in the grid (or Spotlight).
    pub fn rects(&self) -> Vec<Rect> {
        layout::rects(self.layout, self.count(), self.sound)
    }

    /// The occupied place after (or, with `forward == false`, before) `from`, wrapping around.
    /// `from` itself when it is the only stream.
    pub fn cycled_stream(&self, from: usize, forward: bool) -> usize {
        let places: Vec<usize> = self.occupied().collect();
        let Some(at) = places.iter().position(|p| *p == from) else {
            return places.first().copied().unwrap_or(from);
        };
        let len = places.len() as isize;
        let next = at as isize + if forward { 1 } else { -1 };
        places[next.rem_euclid(len) as usize]
    }

    /// Makes the places match the layout and the number of streams. More places than the layout
    /// has drops the empty ones (streams keep their order); fewer adds empty ones at the end. The
    /// sound and the focus stay on their streams.
    pub fn fit_places(&mut self) {
        let want = self.layout.places(self.count());
        if self.tiles.len() > want {
            let slot_at = |tiles: &[Option<MvTile>], place: usize| {
                tiles.get(place).and_then(Option::as_ref).map(|t| t.slot)
            };
            let sound_slot = slot_at(&self.tiles, self.sound);
            let focus_slot = slot_at(&self.tiles, self.focus);
            self.tiles.retain(Option::is_some);
            self.sound = sound_slot.and_then(|s| self.place_of_slot(s)).unwrap_or(0);
            self.focus = focus_slot
                .and_then(|s| self.place_of_slot(s))
                .unwrap_or(self.focus);
        }
        self.tiles.resize(want, None);
        self.focus = self.focus.min(want.saturating_sub(1));
    }

    /// The place next to `from` in the direction of a D-pad press, by where the places are drawn:
    /// the nearest one that way, favouring places in line. `None` at an edge.
    pub fn neighbour(&self, from: usize, dir: Intent) -> Option<usize> {
        let rects = self.rects();
        let (fx, fy) = rects.get(from)?.center();
        rects
            .iter()
            .enumerate()
            .filter(|(i, _)| *i != from)
            .filter_map(|(i, r)| {
                let (x, y) = r.center();
                let (dx, dy) = (x - fx, y - fy);
                let (along, across) = match dir {
                    Intent::Right => (dx, dy),
                    Intent::Left => (-dx, dy),
                    Intent::Down => (dy, dx),
                    Intent::Up => (-dy, dx),
                    _ => return None,
                };
                (along > 1.0).then_some((i, along + 2.0 * across.abs()))
            })
            .min_by(|a, b| a.1.total_cmp(&b.1))
            .map(|(i, _)| i)
    }
}

/// A multiview session as `multiview.toml` keeps it, for the Home *Resume multiview* card: the
/// channels in place order (empty places left out), the layout, and which channel had sound.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SavedSet {
    pub channels: Vec<ChannelRef>,
    pub layout: Layout,
    /// Index into `channels` of the stream with sound. Kept within `channels`.
    pub sound: usize,
}

impl SavedSet {
    /// What `mv` would save. `None` without a stream (never the case while it runs).
    pub fn of(mv: &Multiview) -> Option<Self> {
        let channels: Vec<ChannelRef> = mv
            .occupied()
            .filter_map(|p| mv.tile(p))
            .map(|t| t.stream.channel.clone())
            .collect();
        if channels.is_empty() {
            return None;
        }
        let sound = mv.occupied().position(|p| p == mv.sound).unwrap_or(0);
        Some(Self {
            channels,
            layout: mv.layout,
            sound,
        })
    }

    /// The channel that had sound.
    pub fn sound_channel(&self) -> Option<&ChannelRef> {
        self.channels.get(self.sound)
    }
}

/// The picker's tabs.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum PickerTab {
    /// LiveDeck's own list of channels, Kick and Twitch: live ones first, offline ones greyed.
    Favorites,
    /// Followed Twitch channels that are live.
    #[default]
    Following,
}

impl PickerTab {
    pub fn other(self) -> Self {
        match self {
            PickerTab::Favorites => PickerTab::Following,
            PickerTab::Following => PickerTab::Favorites,
        }
    }
}

/// The panel that adds a stream to multiview.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Picker {
    pub tab: PickerTab,
    /// The focused row.
    pub focus: usize,
    /// The empty place it was opened on: the stream goes there. `None` takes the first empty
    /// place, or a bigger layout.
    pub place: Option<usize>,
}

/// A row of the View popup.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PopupRow {
    /// Signed out: the only row. Multiview is for signed-in users.
    SignIn,
    /// Single player: *Add a stream to watch with this one*. Multiview: *Add a stream*.
    AddStream,
    /// Try a failed or offline tile again.
    Retry,
    /// *Remove <channel>*: the popup's tile.
    Remove,
    /// *Layout ◀ ▶*: Left and Right (and A) step through the layouts.
    Layout,
    /// Stop every stream and go back to browsing.
    Leave,
    /// A browse card: *Add to multiview* (pick it).
    Pick,
    /// A browse card: *Remove from multiview* (unpick it).
    Unpick,
    /// A browse card: *Watch the N together*. The single player: *Watch with the N picked*.
    /// Starts multiview with `AppState::watch_set`.
    WatchPicked,
    /// A browse card: *Clear picked*.
    ClearPicked,
    /// *Add to favorites* or *Remove from favorites*: the popup's card, the multiview place, or
    /// the single player's stream.
    Favorite,
}

/// The popup the View button opens.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ViewPopup {
    pub rows: Vec<PopupRow>,
    pub focus: usize,
    /// The multiview place the popup is about (*Remove*, *Retry*).
    pub place: Option<usize>,
    /// The browse card the popup was opened on: a live stream (*Add to multiview*, ...) or an
    /// offline channel (only *Add to favorites*).
    pub card: Option<Tile>,
}

impl ViewPopup {
    pub fn new(rows: Vec<PopupRow>, place: Option<usize>) -> Self {
        Self {
            rows,
            focus: 0,
            place,
            card: None,
        }
    }

    /// A popup about the browse card `card`.
    pub fn on_card(rows: Vec<PopupRow>, card: Tile) -> Self {
        Self {
            card: Some(card),
            ..Self::new(rows, None)
        }
    }

    pub fn row(&self) -> Option<PopupRow> {
        self.rows.get(self.focus).copied()
    }

    /// The card's stream, when the popup is about a live stream card.
    pub fn card_stream(&self) -> Option<&Stream> {
        match &self.card {
            Some(Tile::Stream(s)) => Some(s),
            _ => None,
        }
    }
}
