//! Application state. Owned by the store and replaced on every `reduce`.

use crate::chat::ChatBuffer;
use crate::domain::{
    ChatStatus, DeviceCode, Features, FeedId, HypeTrain, LoginProblem, Platform, PlayerStats,
    RoomMode, Row, SendProblem, Stream, Tile, User,
};
use crate::keyboard::{Board, KeyboardState, Shift};
use crate::msg::StreamKind;
use crate::settings::{Hwdec, Latency, Quality, Settings, Zoom};

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
    /// Find channels and categories by name with the on-screen keyboard. Signed in only.
    Search,
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
            Screen::Home | Screen::Browse | Screen::Following | Screen::Search | Screen::Settings
        )
    }

    /// Screens that only exist while signed in. Signing out leaves them for Home.
    pub fn needs_account(self) -> bool {
        matches!(self, Screen::Following | Screen::Search)
    }
}

/// Entries of the left rail, top to bottom. The Slint rail draws them in the same order.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum RailEntry {
    #[default]
    Home,
    Following,
    Browse,
    Search,
    Settings,
    /// The account / sign-in entry pinned to the bottom.
    Account,
}

impl RailEntry {
    pub const ALL: [RailEntry; 6] = [
        RailEntry::Home,
        RailEntry::Following,
        RailEntry::Browse,
        RailEntry::Search,
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
    /// On or off: a popup when a followed channel goes live.
    LiveNotifications,
    /// On or off: keep playing while another app has focus.
    BackgroundPlay,
    /// On or off: Kick streams next to Twitch.
    ShowKick,
}

impl SettingsRow {
    pub const ALL: [SettingsRow; 6] = [
        SettingsRow::Account,
        SettingsRow::ChatLayout,
        SettingsRow::Quality,
        SettingsRow::LiveNotifications,
        SettingsRow::BackgroundPlay,
        SettingsRow::ShowKick,
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

/// The rows of the player menu (Start on the Player screen), top to bottom.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum PlayerMenuRow {
    #[default]
    Quality,
    Hwdec,
    Latency,
    Zoom,
    Stats,
    /// Sound only, for this stream (see `AppState::audio_only`).
    AudioOnly,
    /// Stop and suspend the device after a while (see `AppState::sleep`).
    SleepTimer,
    /// An action row: A opens the subscribe card (see `AppState::subscribe_card`).
    Subscribe,
    /// An action row: A opens the chat compose panel (see `AppState::compose`). Signed in only.
    WriteChat,
}

impl PlayerMenuRow {
    pub const ALL: [PlayerMenuRow; 9] = [
        PlayerMenuRow::Quality,
        PlayerMenuRow::Hwdec,
        PlayerMenuRow::Latency,
        PlayerMenuRow::Zoom,
        PlayerMenuRow::Stats,
        PlayerMenuRow::AudioOnly,
        PlayerMenuRow::SleepTimer,
        PlayerMenuRow::Subscribe,
        PlayerMenuRow::WriteChat,
    ];

    pub fn index(self) -> usize {
        self as usize
    }

    /// A runs it, Left and Right do nothing. The other rows are values that A, Left and Right
    /// step through.
    pub fn is_action(self) -> bool {
        matches!(self, PlayerMenuRow::Subscribe | PlayerMenuRow::WriteChat)
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

/// What the player menu changed for this run of the app. Never saved: it starts from the
/// settings (or built-in defaults) and is gone when the app quits.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct SessionOptions {
    /// The quality picked in the player menu. `None` follows `Settings::quality`, and changing
    /// the quality on the Settings screen sets it back to `None`.
    pub quality: Option<Quality>,
    pub hwdec: Hwdec,
    pub latency: Latency,
    pub zoom: Zoom,
    /// The stats box is on. It stays on from stream to stream until it is turned off.
    pub stats: bool,
}

/// The lengths the sleep timer offers, in minutes. Off comes before the first.
pub const SLEEP_STEPS: [u16; 6] = [15, 30, 45, 60, 90, 120];

/// A running sleep timer. It runs on wall-clock time in the app, whatever is on screen, and is
/// never saved.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SleepTimer {
    /// Goes up every time a timer is set, so late events from an older one are ignored.
    pub id: u32,
    /// The length it was set to.
    pub minutes: u16,
    /// Whole minutes left, for the display. 0 once less than a minute is left.
    pub left_min: u16,
    /// "Still watching?" is up: the next button press only extends the timer.
    pub warning: bool,
}

impl SleepTimer {
    /// The length after (or, with `forward == false`, before) `minutes`, where `None` is off.
    /// Wraps around through off.
    pub fn cycled(minutes: Option<u16>, forward: bool) -> Option<u16> {
        let all: Vec<Option<u16>> = std::iter::once(None)
            .chain(SLEEP_STEPS.iter().copied().map(Some))
            .collect();
        let len = all.len() as isize;
        let at = all.iter().position(|m| *m == minutes).unwrap_or(0) as isize;
        all[(at + if forward { 1 } else { -1 }).rem_euclid(len) as usize]
    }
}

/// Seconds the raid card counts down before following the raid.
pub const RAID_SECONDS: u8 = 10;

/// The channel on screen raided another one: the card offering to follow it, while it counts down.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RaidOffer {
    /// Goes up with every offer, so ticks of an older countdown are ignored.
    pub id: u32,
    /// The channel that raided (the one on screen when it came).
    pub from: String,
    /// Login of the raided channel.
    pub to_login: String,
    /// The raided channel's name as the streamer writes it.
    pub to_name: String,
    /// How many viewers came along.
    pub viewers: u64,
    /// Whole seconds left before following.
    pub seconds_left: u8,
}

/// The quick messages offered while composing when `config.toml` names none.
pub const DEFAULT_QUICK_MESSAGES: [&str; 5] = ["GG", "LUL", "Hi!", "o7", "Thanks for the stream!"];

/// The quick messages as `AppState::quick_messages` starts.
pub fn default_quick_messages() -> Vec<String> {
    DEFAULT_QUICK_MESSAGES
        .iter()
        .map(|m| (*m).to_owned())
        .collect()
}

/// A chat message on its way to Twitch.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Sending {
    /// The id of its `Command::SendChat`, which the answer carries.
    pub id: u32,
    /// What was sent (trimmed). The box is cleared on success only while it still holds this, so
    /// a quick message, or words typed meanwhile, are not lost.
    pub text: String,
}

/// Writing in the chat of the stream on screen: the text, the keyboard and the quick messages.
/// Signed in only. The text is kept while the panel is closed, until it is sent or the channel
/// changes.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Compose {
    /// The panel is open over the stream. It takes the D-pad, A, B, X, Y, Start and the shoulder
    /// buttons; the video keeps playing.
    pub open: bool,
    /// The message so far.
    pub text: String,
    /// The chat keyboard (one of `Board::CHAT`) and its cursor.
    pub keys: KeyboardState,
    pub shift: Shift,
    /// Focus is on the quick-message row above the keys, on this one.
    pub chip: Option<usize>,
    /// The message waiting for Twitch's answer. One at a time.
    pub sending: Option<Sending>,
    /// Why the last message was not posted. Shown above the box until the next edit or send.
    pub problem: Option<SendProblem>,
}

impl Default for Compose {
    fn default() -> Self {
        Self {
            open: false,
            text: String::new(),
            keys: KeyboardState::on(Board::Letters),
            shift: Shift::Off,
            chip: None,
            sending: None,
            problem: None,
        }
    }
}

/// A "<channel> is live" popup. It only informs: no button acts on it, and the app removes it
/// after a few seconds (`Event::ToastExpired`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Toast {
    /// Goes up with every toast, never repeats.
    pub id: u32,
    pub stream: Stream,
}

/// Toasts drawn at once. More are summed up as "+N more".
pub const TOASTS_SHOWN: usize = 3;
/// Toasts kept at most (each goes after a few seconds anyway). A burst beyond this drops the
/// oldest.
pub const TOASTS_KEPT: usize = 20;

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
    /// Home and Following only: the column each row was last left on, so a row keeps its
    /// scroll position while focus is on another row (grids keep the column instead).
    pub row_cols: Vec<usize>,
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

    /// Moves focus to `row`. With `per_row_cols` (separate rows, not a grid) the row being left
    /// remembers its column and the new row gets back the one it was left on (0 the first time).
    pub fn focus_row(&mut self, row: usize, per_row_cols: bool) {
        if per_row_cols && row != self.focus.row {
            self.remember_col();
            self.focus.col = self.col_of(row);
        }
        self.focus.row = row;
        self.clamp_col();
    }

    /// The column `row` was last left on (0 if never).
    pub fn col_of(&self, row: usize) -> usize {
        self.row_cols.get(row).copied().unwrap_or(0)
    }

    /// Stores the focused column as the focused row's own.
    pub fn remember_col(&mut self) {
        let row = self.focus.row;
        if self.row_cols.len() <= row {
            self.row_cols.resize(row + 1, 0);
        }
        self.row_cols[row] = self.focus.col;
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

/// Shortest search text (letters and digits, spaces trimmed) that is looked up.
pub const SEARCH_MIN_CHARS: usize = 2;

/// The Search screen: the text, the keyboard and the results. Kept while a stream is watched
/// from it, and while other screens are open.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct SearchState {
    /// The text as typed (the keyboard's characters only).
    pub query: String,
    pub keys: KeyboardState,
    /// The results: live channels, categories, offline channels. Each row pages on its own.
    pub feed: Feed,
    /// Goes up with every search sent, so answers to older ones are ignored. Never reset.
    pub id: u32,
    /// The trimmed text of the latest search sent.
    pub asked: String,
    /// The trimmed text the rows on screen answer. Pages are fetched for it.
    pub results_for: String,
    /// Focus is on the result rows instead of the keyboard.
    pub in_results: bool,
}

impl SearchState {
    /// The search text is long enough to be looked up.
    pub fn is_searchable(text: &str) -> bool {
        text.trim().chars().count() >= SEARCH_MIN_CHARS
    }
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
    /// The Search screen.
    pub search: SearchState,
    /// The newest chat lines of the channel being watched.
    pub chat: ChatBuffer,
    /// How the chat connection of the channel being watched is doing.
    pub chat_status: ChatStatus,
    pub now_playing: Option<Stream>,
    /// The stream on screen plays sound only (player menu). Unlike `session`, it belongs to this
    /// stream: every stream opens as video.
    pub audio_only: bool,
    pub playback: Playback,
    /// While the offline screen is up, the app quietly asks for the stream again every few
    /// seconds. This is the id of the latest such recheck (ids never repeat), so a timer from an
    /// earlier offline spell finds nothing.
    pub offline_recheck: u32,
    /// A quiet recheck is resolving the stream. The offline screen stays up meanwhile: only the
    /// first frame replaces it, and any failure just schedules the next recheck.
    pub rechecking: bool,
    pub paused: bool,
    /// Another app has focus (the window lost it). Starts false: the window opens in front.
    pub in_background: bool,
    /// The stream was paused because the app went to the background (`Settings::background_play`
    /// off), so it resumes when the app comes back. A pause by the user is never undone.
    pub background_paused: bool,
    /// The player menu is open on this row (Start on the Player screen). It takes the D-pad.
    pub player_menu: Option<PlayerMenuRow>,
    /// The player menu's choices for this run of the app.
    pub session: SessionOptions,
    /// The latest numbers for the stats box. Only kept while it is on and a stream is on screen.
    pub stats: Option<PlayerStats>,
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
    /// The sleep timer, while one runs (player menu).
    pub sleep: Option<SleepTimer>,
    /// The id of the last sleep timer set, kept after it ends so ids never repeat.
    pub last_sleep_id: u32,
    /// The subscribe card is open over the stream (player menu): a QR code of the channel's
    /// subscribe page. It takes A and B and ignores the other buttons.
    pub subscribe_card: bool,
    /// The platform can open a web page in a browser the user will see. False in Game Mode on
    /// the Deck, where a browser opens behind gamescope or not at all. Set once at startup.
    pub can_open_browser: bool,
    /// "<channel> is live" popups, newest first. Drawn over every screen.
    pub toasts: Vec<Toast>,
    /// The id of the last toast, kept so ids never repeat.
    pub last_toast_id: u32,
    /// The raid card over the stream, while it is open. Signed in only. It takes A and B and
    /// ignores the other buttons.
    pub raid: Option<RaidOffer>,
    /// The id of the last raid offer, kept so ids never repeat.
    pub last_raid_id: u32,
    /// Writing in chat on the Player screen.
    pub compose: Compose,
    /// The id of the last chat message sent, kept so ids never repeat.
    pub last_send_id: u32,
    /// The chat room's rules (slow mode, followers only, ...) of the channel on screen.
    pub room_mode: RoomMode,
    /// The ready-made messages offered while composing (`quick_messages` in `config.toml`).
    pub quick_messages: Vec<String>,
    /// The hype train in the channel on screen, while one runs.
    pub hype_train: Option<HypeTrain>,
}

/// The Twitch page to subscribe to `channel`. The channel is a Twitch login (letters, digits and
/// underscores), so it needs no escaping.
pub fn subscribe_url(channel: &str) -> String {
    format!("https://www.twitch.tv/subs/{channel}")
}

impl AppState {
    pub fn is_signed_in(&self) -> bool {
        self.user.is_some()
    }

    /// Signed in with a token that may write in chat.
    pub fn can_chat(&self) -> bool {
        self.user.as_ref().is_some_and(|u| u.can_chat)
    }

    /// What the platform of the stream on screen offers (Twitch's everything when none is).
    pub fn on_screen_features(&self) -> Features {
        self.now_playing
            .as_ref()
            .map_or(Platform::Twitch.features(), |p| p.channel.features())
    }

    /// The compose panel is open over the stream on screen.
    pub fn composing(&self) -> bool {
        self.screen == Screen::Player && self.compose.open
    }

    /// The app should be watching for followed channels going live: signed in, with the
    /// setting on. `main` checks it at startup; the reducer keeps it true after that.
    pub fn wants_live_watch(&self) -> bool {
        self.is_signed_in() && self.settings.live_notifications
    }

    /// The quality streams are played at: the player menu's pick, or else the saved setting.
    pub fn quality(&self) -> Quality {
        self.session.quality.unwrap_or(self.settings.quality)
    }

    /// What the stream on screen is played as: sound only, or video at `quality()`.
    pub fn stream_kind(&self) -> StreamKind {
        if self.audio_only {
            StreamKind::AudioOnly
        } else {
            StreamKind::Video(self.quality())
        }
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
            Screen::Search => Some(FeedId::Search(self.search.results_for.clone())),
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
            FeedId::Search(text) => (self.search.results_for == *text).then_some(&self.search.feed),
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
            FeedId::Search(text) => {
                (self.search.results_for == *text).then_some(&mut self.search.feed)
            }
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
