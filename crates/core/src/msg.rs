//! Messages into the reducer (`Intent`, `Event`) and side effects out of it (`Command`).

use crate::domain::{
    ChannelRef, ChatMessage, ChatStatus, DeviceCode, FeedId, HypeTrain, LoginProblem, Moderation,
    PlayerStats, RoomModeChange, Row, RowId, SendProblem, Stream, Tile, User,
};
use crate::multiview::SavedSet;
use crate::netlimit::Health;
use crate::quality::{FrameRate, Rendition};
use crate::settings::{Hwdec, Latency, Quality, Settings, Zoom};
use crate::state::RailEntry;

/// What the user wants, independent of the input device.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Intent {
    Up,
    Down,
    Left,
    Right,
    /// A button / Enter.
    Select,
    /// B button / Escape / Backspace.
    Back,
    /// Y button / `y`.
    ToggleChat,
    /// Start button: toggles the rail on root screens.
    Menu,
    /// X button / `r`: retry a failed load on the current screen.
    Retry,
    /// Mouse fallback: a card was clicked. Moves focus there and selects it.
    Pick {
        row: usize,
        col: usize,
    },
    /// Mouse fallback: a rail entry was clicked.
    RailPick(RailEntry),
    /// A character typed on a physical keyboard. Search types it. Elsewhere `y` and `r` are the
    /// chat and retry shortcuts and everything else is ignored (the reducer decides).
    Text(char),
    /// Backspace on a physical keyboard: deletes a character on Search, Back everywhere else.
    Erase,
    /// LB / Page Up: the previous page of an on-screen keyboard (chat compose).
    PageLeft,
    /// RB / Page Down: the next page of an on-screen keyboard (chat compose).
    PageRight,
    /// The View button (View, Back/Select or Create/Share, left of the face buttons) / `v`:
    /// multiview things on every screen (the View popup).
    View,
}

/// Which of the app's players a playback command or event is about: 0 is the single player, 1 to
/// 8 the other multiview tiles (`layout::MAX_STREAMS` in all).
pub type Slot = u8;

/// The single player's slot. Multiview started from the single player keeps that stream here.
pub const MAIN_SLOT: Slot = 0;

/// Side effects requested by the reducer. The app runs them off the UI thread.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Command {
    /// Fetch the first page of every row of a feed (for `FeedId::Following`, the live channels
    /// first and the offline ones after them). Answered with `Event::FeedLoaded` or
    /// `Event::FeedFailed`.
    LoadFeed { feed: FeedId },
    /// Fetch the page after `cursor` for one row. Answered with `Event::MoreLoaded`.
    LoadMore {
        feed: FeedId,
        row: RowId,
        cursor: String,
    },
    /// Download and decode these thumbnails and hand them to the window. There is no `Event`:
    /// images never enter `AppState`, a card just shows its image once it has arrived.
    FetchThumbnails { urls: Vec<String> },
    /// Resolve the channel's playlist and play it on the player of `slot`, replacing what it
    /// plays. Answered with `Event::StreamOffline` or `Event::PlaybackFailed` if that fails, and
    /// later with the player's own events (`PlaybackStarted`, `PlaybackEnded`, `PlaybackError`)
    /// carrying `slot`. `stream` says which rendition to play, `latency` sets the player's
    /// buffers for this stream.
    Play {
        slot: Slot,
        channel: ChannelRef,
        stream: StreamKind,
        latency: Latency,
    },
    /// Pause or resume the player of `slot`.
    SetPaused { slot: Slot, paused: bool },
    /// Silence (or let be heard) the player of `slot`, which keeps playing. It lasts for the
    /// streams loaded there later, so a reloaded muted tile stays muted.
    SetMuted { slot: Slot, muted: bool },
    /// Change a player option that applies at once, without reloading the stream. It lasts until
    /// the app quits (or the next change), and applies to every slot.
    SetPlayerOption(PlayerOption),
    /// Start or stop the stats reports of the player of `slot` (`Event::PlayerStats`, about once
    /// a second).
    SetStats { slot: Slot, on: bool },
    /// Stop the player of `slot` and drop its stream.
    StopPlayback { slot: Slot },
    /// Send `Event::QualitySettleDue(id)` in 2 s. Multiview reloads tiles at their new quality
    /// only once the layout has stayed put that long (plan.md, section 3).
    SettleQuality { id: u32 },
    /// Start receiving chat for a channel, leaving any other one. The client stays connected
    /// (and reconnects on its own) until `LeaveChat`. Answered over time with
    /// `Event::ChatStatus`, `Event::ChatMessages` and `Event::ChatModerated`.
    JoinChat { channel: ChannelRef },
    /// Stop receiving chat.
    LeaveChat,
    /// Download the badge and emote images of new chat lines and hand them to the window. Like
    /// `FetchThumbnails`, there is no `Event`. The app skips URLs it has already fetched.
    FetchChatImages { urls: Vec<String> },
    /// Start a device-code login. Answered with `Event::DeviceCodeIssued`, and later with
    /// `Event::SignedIn` or `Event::LoginFailed` once the user has (or has not) approved it.
    StartLogin,
    /// Abandon a pending login (stop polling).
    CancelLogin,
    /// Forget the stored tokens and go back to anonymous browsing. Answered with
    /// `Event::SignedOut`, after which feeds are reloaded.
    SignOut,
    /// Write these settings to the config file. There is no `Event`: the state already holds them.
    SaveSettings(Settings),
    /// The favorites changed: this is the whole list, in the order starred. The store hands it to
    /// the provider before the commands after it (so a `LoadFeed { feed: FeedId::Favorites }`
    /// that follows already checks it) and writes it to `favorites.toml`. There is no `Event`.
    SaveFavorites(Vec<ChannelRef>),
    /// The running multiview session changed (a stream added, removed or replaced by a raid, the
    /// layout, the sound): this is the whole set. The store hands its channels to the provider
    /// (Home's next load checks which are live for the *Resume multiview* card) and writes it to
    /// `multiview.toml`. There is no `Event`.
    SaveMultiview(SavedSet),
    /// Start a sleep timer of `minutes`, replacing any running one. Answered over time with
    /// `Event::SleepTimerTick` once a minute, `Event::SleepTimerWarning` 60 s before the end, and
    /// `Event::SleepTimerFired` (after fading the sound out), all carrying `id`.
    StartSleepTimer { id: u32, minutes: u16 },
    /// Stop the running sleep timer. Sound it had started fading comes back.
    CancelSleepTimer,
    /// Suspend the device (the sleep timer ran out). Failure is only logged.
    Suspend,
    /// Open this web page in the system browser. Only sent where `AppState::can_open_browser`
    /// is set. There is no `Event`: failure is only logged.
    OpenUrl(String),
    /// The search text changed: look for channels and categories matching `query` (already
    /// trimmed). Sent on every change. The app waits for typing to pause and drops a search a
    /// newer one replaced, and a `query` shorter than `SEARCH_MIN_CHARS` only cancels. Answered
    /// with `Event::SearchResults` or `Event::SearchFailed` carrying `id`.
    Search { id: u32, query: String },
    /// Start watching for followed channels going live (replacing a running watch, whose
    /// memory of who was live is dropped). Polls every 30 s. The first poll, and the first after
    /// the device slept, only notes who is live. Answered over time with `Event::WentLive`.
    StartLiveWatch,
    /// Stop watching: no more requests.
    StopLiveWatch,
    /// Send `Event::ToastExpired(id)` once the toast has been on screen long enough.
    ExpireToast { id: u32 },
    /// The stream on screen is offline: send `Event::OfflineRecheckDue(id)` in a few seconds, so
    /// the reducer can look again (a streamer restarting after Twitch's 24 h limit is back soon).
    ScheduleOfflineRecheck { id: u32 },
    /// Watch the channel with this Twitch user id for raids, replacing the channel watched so far.
    /// Signed in only. The EventSub connection opens on the first one and stays open until
    /// `DisconnectRaids`. Answered over time with `Event::Raided`.
    WatchRaids { broadcaster_id: String },
    /// Watch no channel for raids (the player was left). The connection stays for the next one.
    StopRaids,
    /// Close the EventSub connection (signed out).
    DisconnectRaids,
    /// Count the raid offer `id` down from `seconds`: `Event::RaidTick` every second, then
    /// `Event::RaidCountdownDone`. Replaces any running countdown.
    StartRaidCountdown { id: u32, seconds: u8 },
    /// Watch the Twitch login `channel` for hype trains (a Twitch-only feature), replacing the
    /// channel watched so far. Signed in or not
    /// (the website's GQL API shows them to anyone). The app checks about once a minute, and every
    /// 10 s while a train runs. Answered over time with `Event::HypeTrain` whenever it changes.
    WatchHypeTrain { channel: String },
    /// Stop watching for hype trains (the player was left): no more requests.
    StopHypeTrain,
    /// Look up the live stream of `channel` (title, category, viewers) and its user id. Answered
    /// with `Event::StreamInfo`, or nothing if the lookup fails.
    FetchStreamInfo { channel: ChannelRef },
    /// Write `text` in the chat of `channel` as the signed-in user. `broadcaster_id` is the
    /// channel's user id, or empty when it is not known yet (the app looks it up). The app keeps
    /// to Twitch's rate limit itself. Answered with `Event::ChatSent` or `Event::ChatDropped`
    /// carrying `id`; the message itself comes back over the chat connection like any other line.
    SendChat {
        id: u32,
        channel: ChannelRef,
        broadcaster_id: String,
        text: String,
    },
    /// Kick on or off ("Show Kick"). Off, the app asks Kick nothing: no Kick row, no Kick search
    /// results. The store applies it before the commands after it, so a `LoadFeed` that follows
    /// already sees it. Sent at startup with the saved setting. There is no `Event`.
    SetKick(bool),
    /// Close the window and end the app (confirmed on the quit prompt).
    Quit,
}

/// Which rendition of a stream `Command::Play` asks for.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StreamKind {
    /// Video (and sound) at this quality.
    Video(Quality),
    /// Sound only: the stream's audio rendition, with video decoding off in the player. A stream
    /// without one falls back to video, still without decoding it.
    AudioOnly,
    /// A multiview tile: this height (a fallback list, see `Rendition::streamlink_list`), with
    /// this frame rate first.
    Tile {
        rendition: Rendition,
        rate: FrameRate,
    },
}

/// A player option that can change while a stream plays.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PlayerOption {
    Hwdec(Hwdec),
    Zoom(Zoom),
}

/// The result of fetching one more page of a row.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PageOutcome {
    /// The next tiles and, if there is one, the cursor after them.
    Page {
        tiles: Vec<Tile>,
        next: Option<String>,
    },
    /// The source has nothing more to give. Anonymous GQL answers `IntegrityRequired` to every
    /// later page, which the provider reports as this.
    End,
    /// The request failed (the provider logs why). The row can be retried with `Intent::Retry`.
    Failed,
}

/// Results of commands, fed back into the reducer.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Event {
    FeedLoaded {
        feed: FeedId,
        rows: Vec<Row>,
    },
    /// `LoadFeed` failed completely. Rows already on screen are kept next to the error.
    FeedFailed {
        feed: FeedId,
        message: String,
    },
    /// The answer to a `LoadMore`. `cursor` is the one that was requested: the reducer ignores
    /// the answer unless that page is still the one the row waits for.
    MoreLoaded {
        feed: FeedId,
        row: RowId,
        cursor: String,
        outcome: PageOutcome,
    },
    /// New chat lines, oldest first. Ignored unless `channel` is the one being watched.
    ChatMessages {
        channel: ChannelRef,
        messages: Vec<ChatMessage>,
    },
    /// A moderator removed lines (cleared chat, timeout or ban, deleted message).
    ChatModerated {
        channel: ChannelRef,
        action: Moderation,
    },
    /// The chat connection changed state (connecting, live, reconnecting after a drop).
    ChatStatus {
        channel: ChannelRef,
        status: ChatStatus,
    },
    /// The resolver found no live stream for `channel`.
    StreamOffline {
        channel: ChannelRef,
    },
    /// The resolver failed for another reason (Streamlink missing, timeout, ...).
    PlaybackFailed {
        channel: ChannelRef,
        message: String,
    },
    /// The player of `slot` shows the first frame.
    PlaybackStarted {
        slot: Slot,
    },
    /// The stream of `slot` ended by itself: the channel went offline or the connection dropped.
    PlaybackEnded {
        slot: Slot,
    },
    /// The player of `slot` could not play the stream.
    PlaybackError {
        slot: Slot,
        message: String,
    },
    /// The `Command::SettleQuality` timer `id` ran out. Ignored unless it is the latest.
    QualitySettleDue(u32),
    /// The player's latest numbers for the stats box. Ignored unless the box is on.
    PlayerStats(PlayerStats),
    /// How the player of `slot` is doing: sent about once a second while it plays (not while it
    /// loads or is paused). `at_ms` is when, in milliseconds on the app's own clock, which only
    /// goes forward. The slow-network rule (`NetLimit`) judges from these.
    PlayerHealth {
        slot: Slot,
        at_ms: u64,
        health: Health,
    },
    DeviceCodeIssued(DeviceCode),
    /// The login finished and the credentials are in place. Feeds are reloaded for the account.
    SignedIn(User),
    /// The login did not finish. The sign-in screen says why and offers another try.
    LoginFailed(LoginProblem),
    /// The session is over and the credentials are gone: the user signed out, or Twitch refused
    /// to refresh the tokens. Feeds are reloaded without the account.
    SignedOut,
    /// A minute passed on the sleep timer `id`: `left_min` whole minutes are left.
    SleepTimerTick {
        id: u32,
        left_min: u16,
    },
    /// The sleep timer `id` ends in 60 s.
    SleepTimerWarning {
        id: u32,
    },
    /// The sleep timer `id` ran out (the sound has already faded).
    SleepTimerFired {
        id: u32,
    },
    /// The rows for search `id`. Ignored unless it is the latest search.
    SearchResults {
        id: u32,
        rows: Vec<Row>,
    },
    /// Search `id` failed completely. Ignored unless it is the latest search.
    SearchFailed {
        id: u32,
        message: String,
    },
    /// These followed channels went live since the last poll. Each becomes a toast, except the
    /// channel being watched. Ignored while the watch should not run (signed out, setting off).
    WentLive(Vec<Stream>),
    /// The toast `id` has been shown long enough.
    ToastExpired(u32),
    /// Time to look again whether the offline stream is back. Ignored unless `id` is the latest
    /// recheck and the offline screen is still up.
    OfflineRecheckDue(u32),
    /// The window gained (`true`) or lost (`false`) focus: the user switched to or from another
    /// app. Without `Settings::background_play`, losing it pauses the stream and regaining it
    /// resumes.
    WindowFocus(bool),
    /// The window is `w` x `h` physical pixels: sent at startup and on every resize. Multiview
    /// picks each tile's quality from it. An empty (minimised) window is ignored.
    DisplaySize {
        w: u32,
        h: u32,
    },
    /// `from_login` raided `to_login`, taking `viewers` along. Ignored unless `from_login` is the
    /// channel on screen and the user is signed in.
    Raided {
        from_login: String,
        to_login: String,
        to_name: String,
        viewers: u64,
    },
    /// The raid countdown `id` has `left` whole seconds to go.
    RaidTick {
        id: u32,
        left: u8,
    },
    /// The raid countdown `id` ran out: follow the raid.
    RaidCountdownDone {
        id: u32,
    },
    /// The hype train in the Twitch login `channel` now: `None` when none runs (any more). Ignored unless
    /// `channel` is the one on screen.
    HypeTrain {
        channel: String,
        train: Option<HypeTrain>,
    },
    /// The answer to `FetchStreamInfo`: what Twitch knows of the channel. A channel that is not
    /// live has an empty title and category. Ignored unless it is the channel on screen.
    StreamInfo(Stream),
    /// Twitch posted the chat message `id`.
    ChatSent {
        id: u32,
    },
    /// The chat message `id` was not posted, and why.
    ChatDropped {
        id: u32,
        problem: SendProblem,
    },
    /// The chat room's rules changed (or were sent on joining). Ignored unless `channel` is the
    /// one being watched.
    ChatRoomMode {
        channel: ChannelRef,
        change: RoomModeChange,
    },
}

/// Anything the reducer accepts.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Msg {
    Intent(Intent),
    Event(Event),
}

impl From<Intent> for Msg {
    fn from(intent: Intent) -> Self {
        Msg::Intent(intent)
    }
}

impl From<Event> for Msg {
    fn from(event: Event) -> Self {
        Msg::Event(event)
    }
}
