//! Application state. Owned by the store and replaced on every `reduce`.

use crate::chat::ChatBuffer;
use crate::domain::{ChatStatus, DeviceCode, FeedId, LoginProblem, Row, Stream, Tile, User};
use crate::settings::Settings;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Screen {
    #[default]
    Home,
    /// The category directory (rail entry "Browse").
    Browse,
    /// The live streams of one category. Not a root: Back returns to where it was opened.
    Category,
    /// Followed channels, live first. Signed in only.
    Following,
    Player,
    /// Settings: the account (sign in / sign out), chat layout and stream quality.
    Settings,
    Login,
}

impl Screen {
    /// Root screens are rail destinations. Opening one resets the back stack.
    pub fn is_root(self) -> bool {
        matches!(
            self,
            Screen::Home | Screen::Browse | Screen::Following | Screen::Settings
        )
    }
}

/// Entries of the left rail, top to bottom. The Slint rail draws them in the same order.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum RailEntry {
    #[default]
    Home,
    Following,
    Browse,
    Settings,
    /// The account / sign-in entry pinned to the bottom.
    Account,
}

impl RailEntry {
    pub const ALL: [RailEntry; 5] = [
        RailEntry::Home,
        RailEntry::Following,
        RailEntry::Browse,
        RailEntry::Settings,
        RailEntry::Account,
    ];

    pub fn index(self) -> usize {
        self as usize
    }

    pub fn from_index(index: usize) -> Option<Self> {
        Self::ALL.get(index).copied()
    }

    /// The entry above, stopping at the top.
    pub fn prev(self) -> Self {
        Self::from_index(self.index().saturating_sub(1)).unwrap_or(self)
    }

    /// The entry below, stopping at the bottom.
    pub fn next(self) -> Self {
        Self::from_index(self.index() + 1).unwrap_or(self)
    }
}

/// The rows of the Settings screen, top to bottom.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum SettingsRow {
    /// Sign in or out.
    #[default]
    Account,
    ChatLayout,
    Quality,
}

impl SettingsRow {
    pub const ALL: [SettingsRow; 3] = [
        SettingsRow::Account,
        SettingsRow::ChatLayout,
        SettingsRow::Quality,
    ];

    pub fn index(self) -> usize {
        self as usize
    }

    /// The row above, stopping at the top.
    pub fn prev(self) -> Self {
        Self::ALL[self.index().saturating_sub(1)]
    }

    /// The row below, stopping at the bottom.
    pub fn next(self) -> Self {
        Self::ALL[(self.index() + 1).min(Self::ALL.len() - 1)]
    }
}

/// Card focus on the home rows.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Focus {
    pub row: usize,
    pub col: usize,
}

/// One scrolling list of rows (Home, Browse or a category) with its own focus and load state.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Feed {
    pub rows: Vec<Row>,
    pub focus: Focus,
    /// A full (re)load is in flight. With no rows yet, the view shows skeleton cards.
    pub loading: bool,
    /// The last full-load error. Cleared when a load starts or succeeds. Rows already on screen
    /// stay visible next to it.
    pub error: Option<String>,
}

impl Feed {
    /// A feed whose first load is about to start.
    pub fn loading() -> Self {
        Self {
            loading: true,
            ..Self::default()
        }
    }

    /// Number of tiles in a row, or `None` if the row does not exist.
    pub fn row_len(&self, row: usize) -> Option<usize> {
        self.rows.get(row).map(|r| r.tiles.len())
    }

    pub fn tile(&self, row: usize, col: usize) -> Option<&Tile> {
        self.rows.get(row).and_then(|r| r.tiles.get(col))
    }

    pub fn focused_tile(&self) -> Option<&Tile> {
        self.tile(self.focus.row, self.focus.col)
    }

    /// Keeps the focus inside the rows that exist (used after rows are replaced).
    pub fn clamp_focus(&mut self) {
        self.focus.row = self.focus.row.min(self.rows.len().saturating_sub(1));
        self.clamp_col();
    }

    /// Keeps the column inside the focused row (used after a row change).
    pub fn clamp_col(&mut self) {
        let len = self.row_len(self.focus.row).unwrap_or(0);
        self.focus.col = self.focus.col.min(len.saturating_sub(1));
    }
}

/// The category screen's content: which category, and its streams.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CategoryFeed {
    /// Twitch game id.
    pub id: String,
    pub name: String,
    pub feed: Feed,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct RailState {
    /// True while the rail is expanded and owns controller focus.
    pub focused: bool,
    /// The rail cursor. Kept when the rail closes, like the scaffold.
    pub index: RailEntry,
}

/// How the stream on the Player screen is doing.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub enum Playback {
    /// No stream is open.
    #[default]
    Idle,
    /// Resolving the playlist, or waiting for the first frame.
    Loading,
    Playing,
    /// The channel is not live (or went offline while watching).
    Offline,
    /// The stream could not be started. Carries a message for the user.
    Failed(String),
}

#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct AppState {
    pub screen: Screen,
    /// Screens to return to on Back. The current screen is not on the stack.
    pub back_stack: Vec<Screen>,
    pub rail: RailState,
    /// Chat is shown on the Player screen (Y toggles it). The connection stays up while it is
    /// hidden, so showing it again is instant and nothing is missed.
    pub chat_open: bool,
    /// Home rows.
    pub home: Feed,
    /// The Following tab: live followed channels, then the offline ones. Empty while signed out.
    pub following: Feed,
    /// The category directory.
    pub browse: Feed,
    /// The open category, while the Category screen is on the back stack or on screen.
    pub category: Option<CategoryFeed>,
    /// The newest chat lines of the channel being watched.
    pub chat: ChatBuffer,
    /// How the chat connection of the channel being watched is doing.
    pub chat_status: ChatStatus,
    pub now_playing: Option<Stream>,
    pub playback: Playback,
    pub paused: bool,
    /// Counts the inputs that should bring the HUD back. The reducer has no clock, so the window
    /// shows the HUD on every change and hides it again after a timeout.
    pub hud_pulse: u32,
    pub user: Option<User>,
    /// The pending device code while the sign-in screen is open.
    pub login: Option<DeviceCode>,
    /// Why the last login attempt ended without signing in. Shown until the next attempt.
    pub login_problem: Option<LoginProblem>,
    /// Where to go once the login on screen succeeds (Following, when that is what asked for it).
    /// `None` returns to the screen underneath.
    pub after_login: Option<Screen>,
    /// User settings, loaded from the config file at startup.
    pub settings: Settings,
    /// The focused row of the Settings screen.
    pub settings_focus: SettingsRow,
    /// The "Quit LiveDeck?" prompt is open (B on a root screen). It takes all input: A quits,
    /// B closes it.
    pub exit_prompt: bool,
}

impl AppState {
    pub fn is_signed_in(&self) -> bool {
        self.user.is_some()
    }

    /// The feed a screen shows, or `None` for screens without one (Player, Login).
    pub fn feed_id_of(&self, screen: Screen) -> Option<FeedId> {
        match screen {
            Screen::Home => Some(FeedId::Home),
            Screen::Following => Some(FeedId::Following),
            Screen::Browse => Some(FeedId::Browse),
            Screen::Category => self
                .category
                .as_ref()
                .map(|c| FeedId::Category(c.id.clone())),
            Screen::Player | Screen::Login | Screen::Settings => None,
        }
    }

    /// The feed of the current screen.
    pub fn current_feed_id(&self) -> Option<FeedId> {
        self.feed_id_of(self.screen)
    }

    /// The feed to draw: the current screen's, or for an overlay (Player, Login) the one
    /// underneath it, so the cards stay put while the overlay is open.
    pub fn visible_feed_id(&self) -> FeedId {
        self.current_feed_id()
            .or_else(|| {
                self.back_stack
                    .iter()
                    .rev()
                    .find_map(|s| self.feed_id_of(*s))
            })
            .unwrap_or(FeedId::Home)
    }

    pub fn feed(&self, id: &FeedId) -> Option<&Feed> {
        match id {
            FeedId::Home => Some(&self.home),
            FeedId::Following => Some(&self.following),
            FeedId::Browse => Some(&self.browse),
            FeedId::Category(game) => self
                .category
                .as_ref()
                .filter(|c| c.id == *game)
                .map(|c| &c.feed),
        }
    }

    pub fn feed_mut(&mut self, id: &FeedId) -> Option<&mut Feed> {
        match id {
            FeedId::Home => Some(&mut self.home),
            FeedId::Following => Some(&mut self.following),
            FeedId::Browse => Some(&mut self.browse),
            FeedId::Category(game) => self
                .category
                .as_mut()
                .filter(|c| c.id == *game)
                .map(|c| &mut c.feed),
        }
    }

    pub fn current_feed(&self) -> Option<&Feed> {
        self.current_feed_id().and_then(|id| self.feed(&id))
    }

    pub fn current_feed_mut(&mut self) -> Option<&mut Feed> {
        let id = self.current_feed_id()?;
        self.feed_mut(&id)
    }

    /// Number of tiles in a row of the current feed, or `None` if the row does not exist.
    pub fn row_len(&self, row: usize) -> Option<usize> {
        self.current_feed().and_then(|f| f.row_len(row))
    }

    pub fn tile(&self, row: usize, col: usize) -> Option<&Tile> {
        self.current_feed().and_then(|f| f.tile(row, col))
    }

    pub fn focused_tile(&self) -> Option<&Tile> {
        self.current_feed().and_then(Feed::focused_tile)
    }
}
