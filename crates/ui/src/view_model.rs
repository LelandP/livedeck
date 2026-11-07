//! `AppState` -> view-model -> Slint properties.
//!
//! The view-model is plain Rust data (`Send`, no Slint types), so the store can build it on any
//! thread and tests can check it without a window. [`Update::apply`] converts it to Slint models
//! on the UI thread. Row models are updated in place (cards are changed, added or removed one by
//! one) so appending a page or a thumbnail arriving never recreates the cards, which would skip
//! their focus animations.

use std::cell::RefCell;
use std::collections::{HashMap, VecDeque};

use htpc_core::{
    grid_cols, subscribe_url, AppState, ChatKind, ChatLayout, ChatMessage, ChatSpan, ChatStatus,
    Feed, FeedId, HypeTrain, Key, LoginProblem, Paging, Playback, PlayerMenuRow, Rgb, RoomMode,
    Row, RowKind, Screen, SearchState, SendProblem, SettingsRow, Shift, Stream, Tile, Toast,
    CHAT_MAX, TOASTS_SHOWN,
};
use slint::{Color, Image, Model, ModelRc, Rgba8Pixel, SharedPixelBuffer, VecModel};

use crate::{ChatMsg, ChatPiece, MainWindow, RowData, SearchKey, StreamItem, ToastItem};

/// Name colour for chatters who have not picked one. Matches the scaffold's mock colour.
pub const DEFAULT_CHAT_NAME: Rgb = Rgb::new(214, 255, 58);

/// Chat lines handed to the window. The newest ones: older lines are off screen anyway, and every
/// line is a row of elements the window has to lay out.
pub const CHAT_LINES_SHOWN: usize = 40;

/// Names darker than this (relative brightness, 0 to 1) are lightened. Twitch lets people pick
/// near-black colours that its own site corrects for dark themes.
const MIN_NAME_BRIGHTNESS: f32 = 0.45;

/// Decoded chat images (emotes, badges) kept for re-use. They are small, about 16 KB.
const CHAT_IMAGE_MEMORY_CAP: usize = 600;

/// Skeleton cards shown at the end of a Home row while its next page loads.
const SKELETON_TAIL: usize = 2;
/// Skeleton cards per row while a feed loads for the first time.
const SKELETON_HOME_COLS: usize = 5;

/// Decoded thumbnails kept for re-use when rows are rebuilt (about 0.4 MB each).
const IMAGE_MEMORY_CAP: usize = 160;

/// The screen ids `main.slint` switches on.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum ScreenId {
    #[default]
    Home = 0,
    Player = 1,
    SignIn = 2,
    /// Browse and Category: rows of cards that scroll vertically.
    Grid = 3,
    /// Settings: account, chat layout, quality, about.
    Settings = 4,
    /// Search: the text, the on-screen keyboard and the result rows.
    Search = 5,
}

/// What the Player screen shows. The values are what `main.slint` switches on.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum PlaybackId {
    /// Resolving or buffering: a skeleton.
    #[default]
    Loading = 0,
    Playing = 1,
    /// The channel is not live (the end screen).
    Offline = 2,
    Failed = 3,
}

/// The card shape of a row. The values are what `RowData.shape` carries in `types.slint`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Shape {
    #[default]
    Landscape = 0,
    Portrait = 1,
    Hero = 2,
}

#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct CardVm {
    pub channel: String,
    pub title: String,
    pub category: String,
    /// Formatted, e.g. "4.2K". Empty for category cards, which hides the live line.
    pub viewers: String,
    pub tint: Rgb,
    /// Cache key of the thumbnail. Empty if the card has none.
    pub thumb_url: String,
    /// A pulsing placeholder for data that has not arrived yet.
    pub loading: bool,
    /// The card at the end of a row whose next page failed.
    pub retry: bool,
    /// A followed channel that is not live: drawn dimmed, with an "Offline" label.
    pub offline: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct RowVm {
    pub title: String,
    pub shape: Shape,
    pub cards: Vec<CardVm>,
}

/// How a chat line is drawn. The values are what `ChatMsg.kind` carries in `types.slint`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum ChatLineKind {
    /// `name: words`
    #[default]
    Message = 0,
    /// `/me`: the whole line in the chatter's colour.
    Action = 1,
    /// From Twitch itself: muted, no name.
    Notice = 2,
}

/// A word of a chat line, or an emote.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct PieceVm {
    /// The word, or the emote's name (shown until its image has arrived).
    pub text: String,
    /// Cache key of the image. Empty for plain words.
    pub url: String,
    pub emote: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct ChatLineVm {
    pub user: String,
    pub tint: Rgb,
    pub kind: ChatLineKind,
    /// Badge image URLs, in display order.
    pub badges: Vec<String>,
    pub pieces: Vec<PieceVm>,
}

/// A "<channel> is live" popup.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct ToastVm {
    /// The toast's id: the window keeps each popup's element (and its animations) by it.
    pub id: i32,
    pub name: String,
    /// The stream title and category, as far as known.
    pub line: String,
    pub tint: Rgb,
    /// Cache key of the stream preview. Empty without one.
    pub thumb_url: String,
}

/// Everything `MainWindow` displays.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct ViewModel {
    pub screen: ScreenId,
    pub sel_row: i32,
    pub sel_col: i32,
    /// Home and Following: the column each row is scrolled to, focused or not. Empty on grids.
    pub row_cols: Vec<i32>,
    pub rail_index: i32,
    pub rail_focused: bool,
    /// Rail entry highlighted as the current destination.
    pub rail_current: i32,
    pub chat_open: bool,
    /// Chat is drawn over the video instead of in a side panel.
    pub chat_overlay: bool,
    /// The chat connection, when it is not simply working: "Connecting…", "Reconnecting…", or
    /// that no message has come yet. Empty otherwise.
    pub chat_status: String,
    pub user_name: String,
    pub device_code: String,
    /// The address to type, without the scheme and the code in the query.
    pub verification_url: String,
    /// What the sign-in screen says under the code: waiting, or why the login ended.
    pub login_status: String,
    /// The login ended without signing in (A asks for a new code).
    pub login_failed: bool,
    /// The QR code of the verification URL, row by row, `true` = dark module. Empty without one.
    pub qr: Vec<bool>,
    /// Modules per side of `qr`.
    pub qr_width: i32,
    /// The screen title above the rows.
    pub heading: String,
    /// The last load error, or empty.
    pub feed_error: String,
    /// The feed loaded fine and has nothing to show.
    pub feed_empty: bool,
    /// What to say about an empty feed.
    pub empty_text: String,
    /// What A does on the focused card: "Watch" or "Open".
    pub select_label: String,
    /// Something failed and X would try again.
    pub can_retry: bool,
    /// B has somewhere to go on this feed screen (only the Category screen).
    pub can_back: bool,
    /// Focused row of the Settings screen (`SettingsRow` order: account, chat layout, quality).
    pub settings_focus: i32,
    pub chat_layout_label: String,
    pub quality_label: String,
    /// "On" or "Off".
    pub live_notifications_label: String,
    /// "On" or "Off".
    pub background_play_label: String,
    /// The newest "is live" popups, at most `TOASTS_SHOWN`, newest first.
    pub toasts: Vec<ToastVm>,
    /// How many more there are than `toasts` shows ("+2 more"). 0 for none.
    pub toast_more: i32,
    pub playback: PlaybackId,
    /// Why playback failed, or empty.
    pub playback_error: String,
    /// What the player logged before the failure (the message after its first blank line), for a
    /// photo of the screen to be a useful bug report. Empty when there is nothing.
    pub playback_details: String,
    pub paused: bool,
    /// The player menu is open (Start on the Player screen).
    pub player_menu_open: bool,
    /// Its focused row (`PlayerMenuRow` order: quality, hardware decoding, latency, zoom, stats,
    /// audio only).
    pub player_menu_focus: i32,
    /// The current value of each player-menu row, in the same order.
    pub player_menu_values: Vec<String>,
    /// The stream on screen plays sound only: the Player screen shows the black audio-only screen,
    /// and the menu's Quality row is locked.
    pub audio_only: bool,
    /// The stats box, one fact per line. Empty while the box is off.
    pub stats_text: String,
    /// Changes whenever the HUD should come back (the window hides it again after a timeout).
    pub hud_pulse: i32,
    /// The "Quit LiveDeck?" prompt is open.
    pub exit_prompt: bool,
    /// The sleep timer in the HUD ("Sleep in 42 min", "Sleeping soon"). Empty without one.
    pub sleep_label: String,
    /// "Still watching?" is up: any button keeps going.
    pub sleep_warning: bool,
    /// The subscribe card is open over the stream.
    pub subscribe_open: bool,
    /// The channel's subscribe page as it is read out (no scheme), for typing. Empty while the
    /// card is closed.
    pub subscribe_url: String,
    /// The QR code of the subscribe page, like `qr`. Empty while the card is closed.
    pub subscribe_qr: Vec<bool>,
    pub subscribe_qr_width: i32,
    /// The card offers "A: Open in browser" (not in Game Mode).
    pub subscribe_can_open: bool,
    /// The raid card is open over the stream: A goes now, B stays.
    pub raid_open: bool,
    /// "<from> is raiding <to>". Empty while the card is closed.
    pub raid_title: String,
    /// "with 1,234 viewers". Empty while the card is closed, or when nobody came along.
    pub raid_viewers: String,
    /// Whole seconds left before following.
    pub raid_seconds: i32,
    /// A hype train runs in the channel on screen: the bar at the top of the video.
    pub hype_open: bool,
    /// The level being worked on. 0 without a train.
    pub hype_level: i32,
    /// How full the bar is toward the next level, in thousandths (0..=1000).
    pub hype_permille: i32,
    /// "1,250 / 2,000". Empty without a train.
    pub hype_points: String,
    /// Seconds left at the last check. The window counts down from it on its own.
    pub hype_seconds: i32,
    /// The Search screen's text as typed.
    pub search_query: String,
    /// The focused key, as an index into `htpc_core::keyboard::layout()`.
    pub search_key_focus: i32,
    /// The keyboard has focus (not the results, not the rail).
    pub search_on_keys: bool,
    /// The result rows have focus.
    pub search_in_results: bool,
    /// What Search says where the results go when there are none: type more, searching,
    /// nothing found, or the error. Empty while results are shown.
    pub search_status: String,
    /// The chat compose panel is open over the stream.
    pub compose_open: bool,
    /// It is open on "Chat needs a new Twitch sign-in" (the token may not chat).
    pub compose_needs_sign_in: bool,
    /// The message so far. Shown at the bottom of the chat while composing, and while closed
    /// when there is a draft.
    pub compose_text: String,
    /// "12/500".
    pub compose_count: String,
    /// The chat keyboard's current page, letters in capitals while shift is on.
    pub compose_keys: Vec<KeyVm>,
    /// The focused key, as an index into `compose_keys`. -1 while a quick message has focus.
    pub compose_key_focus: i32,
    /// 0 off, 1 the next letter, 2 locked (`htpc_core::Shift`), for the shift key's look.
    pub compose_shift: i32,
    /// The quick messages, in order.
    pub compose_chips: Vec<String>,
    /// The focused quick message, or -1.
    pub compose_chip_focus: i32,
    /// "Sending…", or why the last message was not posted. Empty otherwise.
    pub compose_status: String,
    /// `compose_status` is a problem (drawn as a warning).
    pub compose_problem: bool,
    /// The room's rules, e.g. "Slow mode 30 s · Followers only". Empty when anyone may write.
    pub room_mode: String,
    /// Under the chat while not composing: how to write, or that signing in is needed. Empty
    /// off the Player screen.
    pub compose_hint: String,
    pub rows: Vec<RowVm>,
    pub chat: Vec<ChatLineVm>,
    pub now_playing: CardVm,
}

/// One key of an on-screen keyboard: its text and where it sits on the 10-column grid.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct KeyVm {
    pub label: String,
    pub row: i32,
    pub col: i32,
    pub span: i32,
}

/// The chat keyboard's page as drawn: letters follow the shift key.
fn compose_keys(state: &AppState) -> Vec<KeyVm> {
    let shift = state.compose.shift;
    htpc_core::keyboard::layout_of(state.compose.keys.board())
        .into_iter()
        .map(|k| KeyVm {
            label: match k.key {
                Key::Char(c) => shift.apply(c).to_string(),
                other => other.label(),
            },
            row: i32::from(k.row),
            col: i32::from(k.col),
            span: i32::from(k.span),
        })
        .collect()
}

/// What A does on the focused key of the chat keyboard.
fn compose_key_label(key: Key) -> &'static str {
    match key {
        Key::Char(_) | Key::Emote(_) => "Type",
        Key::Space => "Space",
        Key::Backspace => "Delete",
        Key::Clear => "Clear",
        Key::Shift => "Shift",
        Key::Page(_) => "Page",
        Key::Send | Key::Done => "Send",
    }
}

/// The room's rules, as far as they keep someone from writing.
pub fn room_mode_text(mode: &RoomMode) -> String {
    let mut parts = Vec::new();
    if mode.slow_secs > 0 {
        parts.push(format!("Slow mode {} s", mode.slow_secs));
    }
    match mode.followers_min {
        None => {}
        Some(0) => parts.push("Followers only".to_owned()),
        Some(min) => parts.push(format!("Followers only ({})", minutes(min))),
    }
    if mode.subs_only {
        parts.push("Subscribers only".to_owned());
    }
    if mode.emote_only {
        parts.push("Emote only".to_owned());
    }
    parts.join(" · ")
}

/// A followers-only wait in the largest whole unit: "10 min", "3 h", "1 day", "7 days".
fn minutes(min: u32) -> String {
    const DAY: u32 = 60 * 24;
    match min {
        DAY => "1 day".to_owned(),
        m if m >= DAY && m % DAY == 0 => format!("{} days", m / DAY),
        m if m >= 60 && m % 60 == 0 => format!("{} h", m / 60),
        m => format!("{m} min"),
    }
}

/// Why a message was not posted, in a few words. Twitch's codes are the IRC NOTICE ids; the room's
/// rules fill in the numbers where Twitch's own sentence is long. Anything else shows Twitch's
/// sentence.
pub fn send_problem_text(problem: &SendProblem, mode: &RoomMode) -> String {
    match problem {
        SendProblem::TooFast => "Sending too fast: wait a few seconds".to_owned(),
        SendProblem::Failed(why) => format!("Could not send: {why}"),
        SendProblem::Dropped { code, message } => match code.as_str() {
            c if c.starts_with("msg_followersonly") => match mode.followers_min {
                Some(min) if min > 0 => format!("Followers-only chat ({})", minutes(min)),
                _ => "Followers-only chat".to_owned(),
            },
            "msg_slowmode" if mode.slow_secs > 0 => {
                format!("Slow mode: wait {} s", mode.slow_secs)
            }
            "msg_slowmode" => "Slow mode: wait a moment".to_owned(),
            "msg_subsonly" => "Subscribers-only chat".to_owned(),
            "msg_emoteonly" => "Emote-only chat: emotes only".to_owned(),
            "msg_duplicate" => "Same as your last message: change it a little".to_owned(),
            c if c.starts_with("msg_rejected") => "Held by AutoMod for the moderators".to_owned(),
            "msg_banned" => "You are banned from this chat".to_owned(),
            _ if !message.trim().is_empty() => message.trim().to_owned(),
            _ => "Twitch did not post it".to_owned(),
        },
    }
}

/// The compose line's status: sending, or why the last message did not go.
fn compose_status(state: &AppState) -> (String, bool) {
    let c = &state.compose;
    if c.sending.is_some() {
        return ("Sending…".to_owned(), false);
    }
    match &c.problem {
        Some(p) => (send_problem_text(p, &state.room_mode), true),
        None => (String::new(), false),
    }
}

/// Characters of a long message shown in the box: its end, where the typing happens. The counter
/// still counts all of it.
const COMPOSE_SHOWN: usize = 120;

/// The end of `text`, behind an ellipsis when it is longer than [`COMPOSE_SHOWN`].
fn text_tail(text: &str) -> String {
    let len = text.chars().count();
    if len <= COMPOSE_SHOWN {
        return text.to_owned();
    }
    let tail: String = text.chars().skip(len - COMPOSE_SHOWN).collect();
    format!("…{tail}")
}

/// The line under the chat while not composing.
fn compose_hint(state: &AppState) -> &'static str {
    if state.screen != Screen::Player || state.composing() {
        ""
    } else if !state.is_signed_in() {
        "Sign in to Twitch to chat"
    } else if !state.can_chat() {
        "Chat needs a new Twitch sign-in: press X"
    } else {
        "Press X to write in chat"
    }
}

/// The values the player menu shows, in `PlayerMenuRow` order.
fn player_menu_values(state: &AppState) -> Vec<String> {
    PlayerMenuRow::ALL
        .iter()
        .map(|row| -> String {
            match row {
                PlayerMenuRow::Quality if state.audio_only => "Audio only".into(),
                PlayerMenuRow::Quality => state.quality().label().into(),
                PlayerMenuRow::Hwdec => state.session.hwdec.label().into(),
                PlayerMenuRow::Latency => state.session.latency.label().into(),
                PlayerMenuRow::Zoom => state.session.zoom.label().into(),
                PlayerMenuRow::Stats if state.session.stats => "On".into(),
                PlayerMenuRow::Stats => "Off".into(),
                PlayerMenuRow::AudioOnly if state.audio_only => "On".into(),
                PlayerMenuRow::AudioOnly => "Off".into(),
                PlayerMenuRow::SleepTimer => match state.sleep {
                    None => "Off".into(),
                    Some(t) if t.left_min == 0 => format!("{} min · ending", t.minutes),
                    Some(t) => format!("{} min · {} left", t.minutes, t.left_min),
                },
                // Action rows: no value, A opens the card or the keyboard. Signed out, the chat
                // row says what it needs instead.
                PlayerMenuRow::Subscribe => String::new(),
                PlayerMenuRow::WriteChat if !state.is_signed_in() => {
                    "Sign in to Twitch to chat".into()
                }
                PlayerMenuRow::WriteChat => String::new(),
            }
        })
        .collect()
}

/// The sleep timer for the HUD: "Sleep in 42 min", "Sleeping soon" in the last minute, or empty
/// while no timer runs.
fn sleep_label(state: &AppState) -> String {
    match state.sleep {
        None => String::new(),
        Some(t) if t.left_min == 0 => "Sleeping soon".into(),
        Some(t) => format!("Sleep in {} min", t.left_min),
    }
}

/// A bit rate as people read it: `850 kb/s`, `6.2 Mb/s`.
pub fn format_bitrate(bits_per_second: u64) -> String {
    if bits_per_second < 1_000_000 {
        format!("{} kb/s", (bits_per_second + 500) / 1_000)
    } else {
        let tenths = (bits_per_second + 50_000) / 100_000;
        format!("{}.{} Mb/s", tenths / 10, tenths % 10)
    }
}

/// Frames per second from thousandths, with no more decimals than it needs: `60`, `59.94`.
pub fn format_fps(fps_milli: u32) -> String {
    let hundredths = (fps_milli + 5) / 10;
    let text = format!("{}.{:02}", hundredths / 100, hundredths % 100);
    text.trim_end_matches('0').trim_end_matches('.').to_owned()
}

/// What the stats box says, one fact per line. Empty while it is off or off the Player screen.
fn stats_text(state: &AppState) -> String {
    if !state.session.stats || state.screen != Screen::Player {
        return String::new();
    }
    let asked = if state.audio_only {
        "Audio only"
    } else {
        state.quality().label()
    };
    let quality = format!("Asked for: {asked}");
    let Some(s) = &state.stats else {
        return format!("Waiting for the player…\n{quality}");
    };
    const UNKNOWN: &str = "—";
    let rate = |r: Option<u64>| r.map_or_else(|| UNKNOWN.to_owned(), format_bitrate);
    let buffer = s.buffer_ms.map_or_else(
        || UNKNOWN.to_owned(),
        |ms| format!("{}.{} s", ms / 1000, (ms % 1000) / 100),
    );
    // No video is decoded, so its lines would only say "—".
    if state.audio_only {
        return [
            format!(
                "Audio: {} · {}",
                s.audio_codec.as_deref().unwrap_or(UNKNOWN),
                rate(s.audio_bitrate)
            ),
            format!("Buffer: {buffer}"),
            quality,
        ]
        .join("\n");
    }
    let size = match (s.width, s.height) {
        (Some(w), Some(h)) => format!("{w}×{h}"),
        _ => UNKNOWN.to_owned(),
    };
    let fps = s
        .fps_milli
        .map(|f| format!(" · {} fps", format_fps(f)))
        .unwrap_or_default();
    let decoder = match &s.hwdec {
        Some(hw) => format!("hardware ({hw})"),
        None => "software".to_owned(),
    };
    let count = |n: Option<u64>| n.map_or_else(|| UNKNOWN.to_owned(), |n| n.to_string());
    [
        format!("Video: {size}{fps}"),
        format!(
            "Codec: {} · {decoder}",
            s.codec.as_deref().unwrap_or(UNKNOWN)
        ),
        format!(
            "Bitrate: {} video · {} audio",
            rate(s.video_bitrate),
            rate(s.audio_bitrate)
        ),
        format!("Buffer: {buffer}"),
        format!(
            "Dropped frames: {} · decoder {}",
            count(s.dropped_frames),
            count(s.decoder_dropped_frames)
        ),
        quality,
    ]
    .join("\n")
}

/// Compact viewer count: `860`, `4.2K`, `48.1K`, `1.3M`. Rounds to the nearest tenth.
pub fn format_viewers(viewers: u64) -> String {
    if viewers < 1_000 {
        return viewers.to_string();
    }
    let tenths_k = (viewers + 50) / 100;
    if tenths_k < 10_000 {
        return format!("{}.{}K", tenths_k / 10, tenths_k % 10);
    }
    let tenths_m = (viewers + 50_000) / 100_000;
    format!("{}.{}M", tenths_m / 10, tenths_m % 10)
}

/// A count with thousands separators: `1,234`, `48,100`.
pub fn format_count(n: u64) -> String {
    let digits = n.to_string();
    let mut out = String::new();
    for (i, c) in digits.chars().enumerate() {
        if i > 0 && (digits.len() - i).is_multiple_of(3) {
            out.push(',');
        }
        out.push(c);
    }
    out
}

/// The raid card's second line. Empty when nobody came along.
fn raid_viewers(viewers: u64) -> String {
    match viewers {
        0 => String::new(),
        1 => "with 1 viewer".to_owned(),
        n => format!("with {} viewers", format_count(n)),
    }
}

/// How full the hype train bar is, in thousandths. Never past full: a level can be overshot
/// for a moment before Twitch moves on to the next.
fn hype_permille(t: &HypeTrain) -> i32 {
    let p = u64::from(t.progress) * 1000 / u64::from(t.goal.max(1));
    to_i32(p.min(1000) as usize)
}

fn row_title(kind: RowKind, grid: bool) -> &'static str {
    match kind {
        // Grid screens have a heading instead of row titles, and the hero speaks for itself.
        _ if grid => "",
        RowKind::Featured | RowKind::CategoryStreams => "",
        RowKind::FollowedLive => "Followed · Live",
        RowKind::FollowedOffline => "Offline",
        RowKind::TopLive => "Top Live",
        RowKind::Categories | RowKind::SearchCategories => "Categories",
        RowKind::SearchLive => "Live channels",
        RowKind::SearchOffline => "Offline channels",
    }
}

fn shape_of(kind: RowKind) -> Shape {
    match kind {
        RowKind::Featured => Shape::Hero,
        RowKind::Categories | RowKind::SearchCategories => Shape::Portrait,
        RowKind::FollowedLive
        | RowKind::FollowedOffline
        | RowKind::TopLive
        | RowKind::CategoryStreams
        | RowKind::SearchLive
        | RowKind::SearchOffline => Shape::Landscape,
    }
}

fn stream_card(s: &Stream) -> CardVm {
    CardVm {
        channel: s.channel.login.clone(),
        title: s.title.clone(),
        category: s.category.clone(),
        viewers: format_viewers(s.viewers),
        tint: s.tint,
        thumb_url: s.thumbnail.clone().unwrap_or_default(),
        ..CardVm::default()
    }
}

fn tile_card(tile: &Tile) -> CardVm {
    match tile {
        Tile::Stream(s) => stream_card(s),
        Tile::Category(c) => CardVm {
            channel: c.name.clone(),
            tint: c.tint,
            thumb_url: c.thumbnail.clone().unwrap_or_default(),
            ..CardVm::default()
        },
        Tile::Channel(c) => CardVm {
            channel: c.name.clone(),
            tint: c.tint,
            offline: true,
            ..CardVm::default()
        },
    }
}

fn skeleton() -> CardVm {
    CardVm {
        loading: true,
        ..CardVm::default()
    }
}

fn retry_card() -> CardVm {
    CardVm {
        retry: true,
        ..CardVm::default()
    }
}

/// Placeholder rows shown while a feed's first load is in flight.
fn skeleton_rows(id: &FeedId) -> Vec<RowVm> {
    let row = |shape, n| RowVm {
        title: String::new(),
        shape,
        cards: vec![skeleton(); n],
    };
    match id {
        FeedId::Home => vec![
            row(Shape::Hero, 1),
            row(Shape::Landscape, SKELETON_HOME_COLS),
        ],
        FeedId::Following | FeedId::Search(_) => vec![row(Shape::Landscape, SKELETON_HOME_COLS)],
        FeedId::Browse => vec![
            row(Shape::Portrait, grid_cols(RowKind::Categories)),
            row(Shape::Portrait, grid_cols(RowKind::Categories)),
        ],
        FeedId::Category(_) => vec![
            row(Shape::Landscape, grid_cols(RowKind::CategoryStreams)),
            row(Shape::Landscape, grid_cols(RowKind::CategoryStreams)),
        ],
    }
}

/// Adds `extra` cards after the last row of a grid, filling it up to `cols` before starting a
/// new row.
fn push_grid_tail(rows: &mut Vec<RowVm>, cols: usize, extra: Vec<CardVm>) {
    for card in extra {
        match rows.last_mut() {
            Some(last) if last.cards.len() < cols => last.cards.push(card),
            Some(last) => {
                let next = RowVm {
                    title: String::new(),
                    shape: last.shape,
                    cards: vec![card],
                };
                rows.push(next);
            }
            None => {}
        }
    }
}

/// The rows to draw for a feed: its tiles, skeleton cards for what is still loading and a retry
/// card where a page failed.
fn feed_rows(id: &FeedId, feed: &Feed) -> Vec<RowVm> {
    if feed.rows.is_empty() {
        return if feed.loading {
            skeleton_rows(id)
        } else {
            Vec::new()
        };
    }
    let grid = id.is_grid();
    let mut rows: Vec<RowVm> = feed
        .rows
        .iter()
        .map(|r: &Row| RowVm {
            title: row_title(r.kind, grid).to_owned(),
            shape: shape_of(r.kind),
            cards: r.tiles.iter().map(tile_card).collect(),
        })
        .collect();
    if grid {
        if let Some(last) = feed.rows.last() {
            let cols = grid_cols(last.kind);
            let len = last.tiles.len();
            match last.paging {
                Paging::Loading(_) => {
                    // Complete the last row, then one more row of skeletons.
                    let fill = (cols - len % cols) % cols;
                    push_grid_tail(&mut rows, cols, vec![skeleton(); fill + cols]);
                }
                Paging::Failed(_) => push_grid_tail(&mut rows, cols, vec![retry_card()]),
                Paging::End | Paging::More(_) => {}
            }
        }
    } else {
        for (vm, row) in rows.iter_mut().zip(&feed.rows) {
            match row.paging {
                Paging::Loading(_) => vm.cards.extend(vec![skeleton(); SKELETON_TAIL]),
                Paging::Failed(_) => vm.cards.push(retry_card()),
                Paging::End | Paging::More(_) => {}
            }
        }
    }
    rows
}

/// Lightens a colour that would be hard to read on the dark background, keeping its hue.
pub fn readable_name_colour(c: Rgb) -> Rgb {
    let brightness =
        (0.2126 * f32::from(c.r) + 0.7152 * f32::from(c.g) + 0.0722 * f32::from(c.b)) / 255.0;
    if brightness >= MIN_NAME_BRIGHTNESS {
        return c;
    }
    // Mix in white until the brightness reaches the floor.
    let t = (MIN_NAME_BRIGHTNESS - brightness) / (1.0 - brightness);
    let lift = |v: u8| {
        (f32::from(v) + (255.0 - f32::from(v)) * t)
            .round()
            .clamp(0.0, 255.0) as u8
    };
    Rgb::new(lift(c.r), lift(c.g), lift(c.b))
}

fn chat_line(m: &ChatMessage) -> ChatLineVm {
    let mut pieces = Vec::new();
    for span in &m.spans {
        match span {
            ChatSpan::Text(text) => pieces.extend(text.split_whitespace().map(|word| PieceVm {
                text: word.to_owned(),
                ..PieceVm::default()
            })),
            ChatSpan::Emote { name, url } => pieces.push(PieceVm {
                text: name.clone(),
                url: url.clone(),
                emote: true,
            }),
        }
    }
    ChatLineVm {
        user: m.user.clone(),
        tint: readable_name_colour(m.color.unwrap_or(DEFAULT_CHAT_NAME)),
        kind: match m.kind {
            ChatKind::Message => ChatLineKind::Message,
            ChatKind::Action => ChatLineKind::Action,
            ChatKind::Notice => ChatLineKind::Notice,
        },
        badges: m.badges.clone(),
        pieces,
    }
}

/// How a Settings switch shows its value.
fn on_off(on: bool) -> &'static str {
    if on {
        "On"
    } else {
        "Off"
    }
}

/// What the chat header says about the connection. Empty when everything is fine.
fn chat_status_text(state: &AppState) -> &'static str {
    match state.chat_status {
        ChatStatus::Connecting => "Connecting…",
        ChatStatus::Reconnecting => "Reconnecting…",
        ChatStatus::Live if state.chat.is_empty() => "No messages yet",
        ChatStatus::Live | ChatStatus::Idle => "",
    }
}

/// Rail index of a root screen. `None` for overlays (Player, Login).
fn rail_slot(screen: Screen) -> Option<i32> {
    match screen {
        Screen::Home => Some(0),
        Screen::Following => Some(1),
        // The category screen belongs to the Browse entry.
        Screen::Browse | Screen::Category => Some(2),
        Screen::Search => Some(3),
        Screen::Settings => Some(4),
        Screen::Player | Screen::Login => None,
    }
}

fn to_i32(v: usize) -> i32 {
    i32::try_from(v).unwrap_or(i32::MAX)
}

fn heading(state: &AppState, id: &FeedId) -> String {
    match id {
        FeedId::Home => "Live now".to_owned(),
        FeedId::Following => "Following".to_owned(),
        FeedId::Browse => "Categories".to_owned(),
        FeedId::Category(_) => state
            .category
            .as_ref()
            .map(|c| c.name.clone())
            .unwrap_or_default(),
        FeedId::Search(_) => "Search".to_owned(),
    }
}

/// What Search shows where the results go while there are none (see `ViewModel::search_status`).
fn search_status(state: &AppState) -> String {
    let search = &state.search;
    if !SearchState::is_searchable(&search.query) {
        return "Type at least two letters or digits to find channels and categories.".to_owned();
    }
    if let Some(error) = &search.feed.error {
        return format!("Could not search: {error}. Press Start to try again.");
    }
    match (search.feed.rows.is_empty(), search.feed.loading) {
        (false, _) => String::new(),
        (true, true) => "Searching…".to_owned(),
        (true, false) => format!("No channels or categories match “{}”.", search.asked),
    }
}

fn toast(t: &Toast) -> ToastVm {
    let s = &t.stream;
    let line = match (s.title.is_empty(), s.category.is_empty()) {
        (false, false) => format!("{} · {}", s.title, s.category),
        (false, true) => s.title.clone(),
        (true, _) => s.category.clone(),
    };
    ToastVm {
        id: t.id as i32,
        name: s.channel.login.clone(),
        line,
        tint: s.tint,
        thumb_url: s.thumbnail.clone().unwrap_or_default(),
    }
}

/// What A does on the focused key of the Search keyboard.
fn key_label(key: Key) -> &'static str {
    match key {
        Key::Char(_) => "Type",
        Key::Space => "Space",
        Key::Backspace => "Delete",
        Key::Clear => "Clear",
        Key::Done => "Results",
        // Chat keys: never on the Search board.
        Key::Shift | Key::Page(_) | Key::Emote(_) | Key::Send => "",
    }
}

/// What an empty feed says. Following is empty when nobody the user follows is live and the
/// account follows no one.
fn empty_text(id: &FeedId) -> &'static str {
    match id {
        FeedId::Following => "You are not following anyone yet",
        // Search says it in `search_status`.
        FeedId::Search(_) => "",
        _ => "Nothing is live here right now",
    }
}

/// The verification address as it is read out: no scheme, no query (the code is typed in
/// separately, and the QR code carries the full link).
fn display_url(uri: &str) -> String {
    let rest = uri.split_once("://").map_or(uri, |(_, r)| r);
    rest.split(['?', '#']).next().unwrap_or(rest).to_owned()
}

/// The QR code of `text` as dark/light modules, row by row, and its width. Shared by the sign-in
/// screen and the subscribe card. Empty if the text does not fit a QR code (it never does not,
/// for a link).
fn qr_modules(text: &str) -> (Vec<bool>, i32) {
    match qrcode::QrCode::new(text.as_bytes()) {
        Ok(code) => {
            let width = code.width();
            let dark = code
                .to_colors()
                .into_iter()
                .map(|c| c == qrcode::Color::Dark)
                .collect();
            (dark, to_i32(width))
        }
        Err(err) => {
            tracing::warn!(%err, "could not build the QR code");
            (Vec::new(), 0)
        }
    }
}

/// The line under the code on the sign-in screen.
fn login_status(state: &AppState) -> String {
    match &state.login_problem {
        None if state.login.is_some() => "Waiting for approval...".to_owned(),
        None => "Getting a code from Twitch...".to_owned(),
        Some(LoginProblem::Expired) => "That code expired.".to_owned(),
        Some(LoginProblem::Denied) => "Sign-in was declined on your phone.".to_owned(),
        Some(LoginProblem::Failed(message)) => format!("Could not sign in: {message}"),
    }
}

pub fn view_model(state: &AppState) -> ViewModel {
    let screen = match state.screen {
        Screen::Player => ScreenId::Player,
        Screen::Login => ScreenId::SignIn,
        Screen::Browse | Screen::Category => ScreenId::Grid,
        Screen::Settings => ScreenId::Settings,
        Screen::Search => ScreenId::Search,
        // Following draws like Home: horizontal rows.
        Screen::Home | Screen::Following => ScreenId::Home,
    };
    let on_search = state.screen == Screen::Search;
    let search_on_keys = on_search && !state.search.in_results && !state.rail.focused;
    // Overlays highlight the root screen underneath them.
    let rail_current = rail_slot(state.screen)
        .or_else(|| state.back_stack.iter().rev().find_map(|s| rail_slot(*s)))
        .unwrap_or(0);
    let (device_code, verification_url, qr, qr_width) = match &state.login {
        Some(code) => {
            let (qr, width) = qr_modules(&code.verification_uri);
            (
                code.user_code.clone(),
                display_url(&code.verification_uri),
                qr,
                width,
            )
        }
        None => (String::new(), String::new(), Vec::new(), 0),
    };
    let login_status = login_status(state);
    let on_player = state.screen == Screen::Player;
    // Built only while the card is open, so the diff flags a new code when it opens or the
    // channel changes, and never otherwise.
    let subscribe_page = state
        .now_playing
        .as_ref()
        .filter(|_| on_player && state.subscribe_card)
        .map(|p| subscribe_url(&p.channel.login));
    let (subscribe_url_text, subscribe_qr, subscribe_qr_width) = match &subscribe_page {
        Some(url) => {
            let (qr, width) = qr_modules(url);
            (display_url(url), qr, width)
        }
        None => (String::new(), Vec::new(), 0),
    };
    let subscribe_open = subscribe_page.is_some();
    let raid = state.raid.as_ref().filter(|_| on_player);
    let hype = state.hype_train.as_ref().filter(|_| on_player);
    let composing = state.composing();
    let (compose_status, compose_problem) = compose_status(state);
    // The keys are only built while they are on screen, so the diff stays quiet otherwise.
    let compose_keys = if composing && state.can_chat() {
        compose_keys(state)
    } else {
        Vec::new()
    };

    // The feed drawn is the current screen's, or the one under an overlay.
    let id = state.visible_feed_id();
    let feed = state.feed(&id);
    let (rows, focus, error, loading) = match feed {
        Some(f) => (feed_rows(&id, f), f.focus, f.error.clone(), f.loading),
        None => (Vec::new(), Default::default(), None, false),
    };
    // Separate rows keep their own scroll position (see `Feed::row_cols`).
    let row_cols: Vec<i32> = match feed {
        Some(f) if !id.is_grid() => (0..rows.len())
            .map(|r| {
                to_i32(if r == focus.row {
                    focus.col
                } else {
                    f.col_of(r)
                })
            })
            .collect(),
        _ => Vec::new(),
    };
    let failed_page =
        feed.is_some_and(|f| f.rows.iter().any(|r| matches!(r.paging, Paging::Failed(_))));
    let selects_category = feed
        .and_then(Feed::focused_tile)
        .is_some_and(|t| matches!(t, Tile::Category(_)));
    let (playback, playback_error) = match &state.playback {
        Playback::Idle | Playback::Loading => (PlaybackId::Loading, String::new()),
        Playback::Playing => (PlaybackId::Playing, String::new()),
        Playback::Offline => (PlaybackId::Offline, String::new()),
        Playback::Failed(message) => (PlaybackId::Failed, message.clone()),
    };
    // The player puts its recent log lines after a blank line (htpc-player's `RecentLog`).
    let (playback_error, playback_details) = match playback_error.split_once("\n\n") {
        Some((summary, details)) => (summary.to_owned(), details.trim().to_owned()),
        None => (playback_error, String::new()),
    };
    // On the Player screen A and X act on the stream, not on the cards underneath. On Settings,
    // A is the account action.
    let (select_label, can_retry) = if state.screen == Screen::Settings {
        let label = match state.settings_focus {
            SettingsRow::Account if state.is_signed_in() => "Sign out",
            SettingsRow::Account => "Sign in",
            SettingsRow::ChatLayout
            | SettingsRow::Quality
            | SettingsRow::LiveNotifications
            | SettingsRow::BackgroundPlay => "Change",
        };
        (label, false)
    } else if search_on_keys {
        // X is backspace on the keyboard, not retry.
        (key_label(state.search.keys.key()), false)
    } else if raid.is_some() {
        // The card takes only A and B.
        ("Go now", false)
    } else if subscribe_open {
        // Only B, in Game Mode.
        let label = if state.can_open_browser {
            "Open in browser"
        } else {
            ""
        };
        (label, false)
    } else if composing && !state.can_chat() {
        ("Sign in again", false)
    } else if composing {
        // On a quick message A sends it, on the keys it presses the focused one.
        let label = match state.compose.chip {
            Some(_) => "Send",
            None => compose_key_label(state.compose.keys.key()),
        };
        (label, false)
    } else if on_player && state.player_menu.is_some_and(PlayerMenuRow::is_action) {
        ("Open", false)
    } else if on_player && state.player_menu.is_some() {
        // The menu has the buttons: A changes the focused value.
        ("Change", false)
    } else if on_player {
        match playback {
            PlaybackId::Loading => ("", false),
            PlaybackId::Playing if state.paused => ("Resume", false),
            PlaybackId::Playing => ("Pause", false),
            PlaybackId::Offline | PlaybackId::Failed => ("Retry", true),
        }
    } else {
        (
            if selects_category { "Open" } else { "Watch" },
            error.is_some() || failed_page,
        )
    };

    ViewModel {
        screen,
        sel_row: to_i32(focus.row),
        sel_col: to_i32(focus.col),
        row_cols,
        rail_index: to_i32(state.rail.index.index()),
        rail_focused: state.rail.focused,
        rail_current,
        chat_open: state.chat_open,
        chat_overlay: state.settings.chat_layout == ChatLayout::Overlay,
        chat_status: chat_status_text(state).to_owned(),
        user_name: state
            .user
            .as_ref()
            .map(|u| u.login.clone())
            .unwrap_or_default(),
        device_code,
        verification_url,
        login_status,
        login_failed: state.login_problem.is_some(),
        qr,
        qr_width,
        heading: heading(state, &id),
        feed_empty: rows.is_empty() && error.is_none() && !loading,
        empty_text: empty_text(&id).to_owned(),
        can_retry,
        feed_error: error.unwrap_or_default(),
        select_label: select_label.to_owned(),
        can_back: state.screen == Screen::Category,
        settings_focus: to_i32(state.settings_focus.index()),
        chat_layout_label: state.settings.chat_layout.label().to_owned(),
        quality_label: state.settings.quality.label().to_owned(),
        live_notifications_label: on_off(state.settings.live_notifications).to_owned(),
        background_play_label: on_off(state.settings.background_play).to_owned(),
        toasts: state.toasts.iter().take(TOASTS_SHOWN).map(toast).collect(),
        toast_more: to_i32(state.toasts.len().saturating_sub(TOASTS_SHOWN)),
        playback,
        playback_error,
        playback_details,
        paused: state.paused,
        player_menu_open: on_player && state.player_menu.is_some(),
        player_menu_focus: to_i32(state.player_menu.unwrap_or_default().index()),
        player_menu_values: player_menu_values(state),
        audio_only: on_player && state.audio_only,
        stats_text: stats_text(state),
        hud_pulse: state.hud_pulse as i32,
        exit_prompt: state.exit_prompt,
        sleep_label: sleep_label(state),
        sleep_warning: state.sleep.is_some_and(|t| t.warning),
        subscribe_open,
        subscribe_url: subscribe_url_text,
        subscribe_qr,
        subscribe_qr_width,
        subscribe_can_open: subscribe_open && state.can_open_browser,
        raid_open: raid.is_some(),
        raid_title: raid
            .map(|r| format!("{} is raiding {}", r.from, r.to_name))
            .unwrap_or_default(),
        raid_viewers: raid.map(|r| raid_viewers(r.viewers)).unwrap_or_default(),
        raid_seconds: raid.map_or(0, |r| i32::from(r.seconds_left)),
        hype_open: hype.is_some(),
        hype_level: hype.map_or(0, |t| to_i32(t.level as usize)),
        hype_permille: hype.map_or(0, hype_permille),
        hype_points: hype
            .map(|t| {
                format!(
                    "{} / {}",
                    format_count(u64::from(t.progress)),
                    format_count(u64::from(t.goal))
                )
            })
            .unwrap_or_default(),
        hype_seconds: hype.map_or(0, |t| to_i32(t.seconds_left as usize)),
        search_query: state.search.query.clone(),
        search_key_focus: to_i32(state.search.keys.index()),
        search_on_keys,
        search_in_results: on_search && state.search.in_results,
        search_status: search_status(state),
        compose_open: composing,
        compose_needs_sign_in: composing && !state.can_chat(),
        compose_text: if on_player {
            text_tail(&state.compose.text)
        } else {
            String::new()
        },
        compose_count: format!("{}/{CHAT_MAX}", state.compose.text.chars().count()),
        compose_keys,
        compose_key_focus: match state.compose.chip {
            Some(_) => -1,
            None => to_i32(state.compose.keys.index()),
        },
        compose_shift: match state.compose.shift {
            Shift::Off => 0,
            Shift::Once => 1,
            Shift::Lock => 2,
        },
        compose_chips: state.quick_messages.clone(),
        compose_chip_focus: state.compose.chip.map_or(-1, to_i32),
        compose_status,
        compose_problem,
        room_mode: if on_player {
            room_mode_text(&state.room_mode)
        } else {
            String::new()
        },
        compose_hint: compose_hint(state).to_owned(),
        rows,
        chat: state
            .chat
            .iter()
            .skip(state.chat.len().saturating_sub(CHAT_LINES_SHOWN))
            .map(chat_line)
            .collect(),
        now_playing: state
            .now_playing
            .as_ref()
            .map(stream_card)
            .unwrap_or_default(),
    }
}

/// A decoded thumbnail on its way to the window: plain RGBA8 pixels, so it is `Send`.
#[derive(Clone, PartialEq, Eq)]
pub struct Thumbnail {
    pub width: u32,
    pub height: u32,
    pub rgba: Vec<u8>,
}

impl std::fmt::Debug for Thumbnail {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "Thumbnail({}x{})", self.width, self.height)
    }
}

/// Decoded images by URL. Slint images are not `Send`, so this lives on the UI thread.
#[derive(Default)]
struct ImageMemory {
    images: HashMap<String, Image>,
    /// Insertion order, oldest first.
    order: VecDeque<String>,
}

impl ImageMemory {
    fn get(&self, url: &str) -> Option<Image> {
        self.images.get(url).cloned()
    }

    fn insert(&mut self, url: &str, image: Image) {
        if self.images.insert(url.to_owned(), image).is_none() {
            self.order.push_back(url.to_owned());
        }
    }

    /// Drops the oldest images that `in_use` does not need until at most `cap` are left. A
    /// dropped image only matters if its card is rebuilt later, which then shows its tint.
    fn trim(&mut self, cap: usize, in_use: impl Fn(&str) -> bool) {
        let mut keep = VecDeque::new();
        while self.images.len() > cap {
            let Some(url) = self.order.pop_front() else {
                break;
            };
            if in_use(&url) {
                keep.push_back(url);
            } else {
                self.images.remove(&url);
            }
        }
        // Skipped (in-use) entries stay the oldest ones.
        keep.append(&mut self.order);
        self.order = keep;
    }
}

thread_local! {
    static IMAGES: RefCell<ImageMemory> = RefCell::new(ImageMemory::default());
}

/// A view-model plus which list models changed since the previous one.
///
/// Built on the store thread (it is `Send`) and applied on the UI thread.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Update {
    pub vm: ViewModel,
    pub rows_changed: bool,
    pub chat_changed: bool,
    /// The QR code changed, so its image is rebuilt.
    pub qr_changed: bool,
    /// The same for the subscribe card's QR code.
    pub subscribe_qr_changed: bool,
    pub toasts_changed: bool,
    /// The chat keyboard's keys changed (another page, shift), so its model is rebuilt.
    pub compose_keys_changed: bool,
    pub compose_chips_changed: bool,
}

impl Update {
    /// The first update replaces everything.
    pub fn full(vm: ViewModel) -> Self {
        Self {
            vm,
            rows_changed: true,
            chat_changed: true,
            qr_changed: true,
            subscribe_qr_changed: true,
            toasts_changed: true,
            compose_keys_changed: true,
            compose_chips_changed: true,
        }
    }

    /// Returns `None` if nothing visible changed.
    pub fn diff(prev: &ViewModel, next: ViewModel) -> Option<Self> {
        if *prev == next {
            return None;
        }
        Some(Self {
            rows_changed: prev.rows != next.rows,
            chat_changed: prev.chat != next.chat,
            qr_changed: prev.qr != next.qr,
            subscribe_qr_changed: prev.subscribe_qr != next.subscribe_qr,
            toasts_changed: prev.toasts != next.toasts,
            compose_keys_changed: prev.compose_keys != next.compose_keys,
            compose_chips_changed: prev.compose_chips != next.compose_chips,
            vm: next,
        })
    }

    /// Pushes the update into the window. Must run on the UI thread.
    pub fn apply(self, ui: &MainWindow) {
        let vm = self.vm;
        IMAGES.with_borrow(|images| {
            if self.rows_changed {
                sync_rows(ui, &vm.rows, images);
            }
            if self.toasts_changed {
                sync_toasts(ui, &vm.toasts, images);
            }
            ui.set_now_playing(to_stream_item(&vm.now_playing, images));
        });
        ui.set_toast_more(vm.toast_more);
        if self.chat_changed {
            sync_chat(ui, &vm.chat);
        }
        ui.set_screen(vm.screen as i32);
        ui.set_sel_row(vm.sel_row);
        ui.set_sel_col(vm.sel_col);
        ui.set_row_cols(ModelRc::new(VecModel::from(vm.row_cols)));
        ui.set_rail_index(vm.rail_index);
        ui.set_rail_focused(vm.rail_focused);
        ui.set_rail_current(vm.rail_current);
        ui.set_chat_open(vm.chat_open);
        ui.set_chat_overlay(vm.chat_overlay);
        ui.set_chat_status(vm.chat_status.into());
        ui.set_user_name(vm.user_name.into());
        ui.set_device_code(vm.device_code.into());
        ui.set_verification_url(vm.verification_url.into());
        ui.set_login_status(vm.login_status.into());
        ui.set_login_failed(vm.login_failed);
        if self.qr_changed {
            ui.set_qr(qr_image(&vm.qr, vm.qr_width));
        }
        ui.set_heading(vm.heading.into());
        ui.set_feed_error(vm.feed_error.into());
        ui.set_feed_empty(vm.feed_empty);
        ui.set_empty_text(vm.empty_text.into());
        ui.set_select_label(vm.select_label.into());
        ui.set_can_retry(vm.can_retry);
        ui.set_can_back(vm.can_back);
        ui.set_settings_focus(vm.settings_focus);
        ui.set_chat_layout_label(vm.chat_layout_label.into());
        ui.set_quality_label(vm.quality_label.into());
        ui.set_live_notifications_label(vm.live_notifications_label.into());
        ui.set_background_play_label(vm.background_play_label.into());
        ui.set_playback(vm.playback as i32);
        ui.set_playback_error(vm.playback_error.into());
        ui.set_playback_details(vm.playback_details.into());
        ui.set_paused(vm.paused);
        ui.set_player_menu_open(vm.player_menu_open);
        ui.set_player_menu_focus(vm.player_menu_focus);
        let values: Vec<slint::SharedString> = vm
            .player_menu_values
            .iter()
            .map(|v| v.as_str().into())
            .collect();
        ui.set_player_menu_values(ModelRc::new(VecModel::from(values)));
        ui.set_audio_only(vm.audio_only);
        ui.set_stats_text(vm.stats_text.into());
        ui.set_hud_pulse(vm.hud_pulse);
        ui.set_exit_prompt(vm.exit_prompt);
        ui.set_sleep_label(vm.sleep_label.into());
        ui.set_sleep_warning(vm.sleep_warning);
        if self.subscribe_qr_changed {
            ui.set_subscribe_qr(qr_image(&vm.subscribe_qr, vm.subscribe_qr_width));
        }
        ui.set_subscribe_open(vm.subscribe_open);
        ui.set_subscribe_url(vm.subscribe_url.into());
        ui.set_subscribe_can_open(vm.subscribe_can_open);
        ui.set_raid_open(vm.raid_open);
        ui.set_raid_title(vm.raid_title.into());
        ui.set_raid_viewers(vm.raid_viewers.into());
        ui.set_raid_seconds(vm.raid_seconds);
        ui.set_hype_open(vm.hype_open);
        ui.set_hype_level(vm.hype_level);
        ui.set_hype_permille(vm.hype_permille);
        ui.set_hype_points(vm.hype_points.into());
        ui.set_hype_seconds(vm.hype_seconds);
        // The keys never change, so they are handed over once (recreating them would restart
        // their focus animations on every update).
        if ui.get_search_keys().row_count() == 0 {
            ui.set_search_keys(ModelRc::new(VecModel::from(search_keys())));
        }
        ui.set_search_query(vm.search_query.into());
        ui.set_search_key_focus(vm.search_key_focus);
        ui.set_search_on_keys(vm.search_on_keys);
        ui.set_search_in_results(vm.search_in_results);
        ui.set_search_status(vm.search_status.into());
        if self.compose_keys_changed {
            let keys: Vec<SearchKey> = vm.compose_keys.iter().map(to_key).collect();
            ui.set_compose_keys(ModelRc::new(VecModel::from(keys)));
        }
        if self.compose_chips_changed {
            let chips: Vec<slint::SharedString> =
                vm.compose_chips.iter().map(|c| c.as_str().into()).collect();
            ui.set_compose_chips(ModelRc::new(VecModel::from(chips)));
        }
        ui.set_compose_open(vm.compose_open);
        ui.set_compose_needs_sign_in(vm.compose_needs_sign_in);
        ui.set_compose_text(vm.compose_text.into());
        ui.set_compose_count(vm.compose_count.into());
        ui.set_compose_key_focus(vm.compose_key_focus);
        ui.set_compose_shift(vm.compose_shift);
        ui.set_compose_chip_focus(vm.compose_chip_focus);
        ui.set_compose_status(vm.compose_status.into());
        ui.set_compose_problem(vm.compose_problem);
        ui.set_room_mode(vm.room_mode.into());
        ui.set_compose_hint(vm.compose_hint.into());
    }
}

fn to_key(k: &KeyVm) -> SearchKey {
    SearchKey {
        label: k.label.as_str().into(),
        row: k.row,
        col: k.col,
        span: k.span,
    }
}

/// The on-screen keyboard for `search.slint`, in `htpc_core::keyboard::layout()` order.
fn search_keys() -> Vec<SearchKey> {
    htpc_core::keyboard::layout()
        .into_iter()
        .map(|k| SearchKey {
            label: k.key.label().into(),
            row: i32::from(k.row),
            col: i32::from(k.col),
            span: i32::from(k.span),
        })
        .collect()
}

/// Hands a decoded thumbnail to the window: remembers it and patches every card that shows
/// `url`. Must run on the UI thread.
pub fn apply_thumbnail(ui: &MainWindow, url: &str, thumb: &Thumbnail) {
    let (w, h) = (thumb.width, thumb.height);
    if w == 0 || h == 0 || thumb.rgba.len() != w as usize * h as usize * 4 {
        tracing::debug!(url, w, h, "ignoring a malformed thumbnail");
        return;
    }
    let mut buffer = SharedPixelBuffer::<Rgba8Pixel>::new(w, h);
    buffer.make_mut_bytes().copy_from_slice(&thumb.rgba);
    let image = Image::from_rgba8(buffer);

    let rows = ui.get_rows();
    IMAGES.with_borrow_mut(|images| {
        images.insert(url, image.clone());
        if images.images.len() > IMAGE_MEMORY_CAP {
            images.trim(IMAGE_MEMORY_CAP, |u| on_screen(&rows, u));
        }
    });
    for row in rows.iter() {
        let Some(model) = vec_model(&row.items) else {
            continue;
        };
        for (i, card) in row.items.iter().enumerate() {
            if card.thumb_url.as_str() == url {
                model.set_row_data(
                    i,
                    StreamItem {
                        thumbnail: image.clone(),
                        ..card
                    },
                );
            }
        }
    }
    // A toast's preview comes the same way as a card's.
    let toasts = ui.get_toasts();
    if let Some(model) = vec_model(&toasts) {
        for (i, toast) in toasts.iter().enumerate() {
            if toast.thumb_url.as_str() == url {
                model.set_row_data(
                    i,
                    ToastItem {
                        thumbnail: image.clone(),
                        ..toast
                    },
                );
            }
        }
    }
}

fn to_toast_item(t: &ToastVm, images: &ImageMemory) -> ToastItem {
    ToastItem {
        id: t.id,
        name: t.name.as_str().into(),
        line: t.line.as_str().into(),
        tint: to_color(t.tint),
        thumbnail: images.get(&t.thumb_url).unwrap_or_default(),
        thumb_url: t.thumb_url.as_str().into(),
    }
}

/// Brings the window's toasts in line with `want`, keeping each toast's element by id: a new
/// one is inserted where it goes, an expired one removed, and the others are left alone, so
/// their slide-in and timers carry on.
fn sync_toasts(ui: &MainWindow, want: &[ToastVm], images: &ImageMemory) {
    let current = ui.get_toasts();
    let Some(model) = vec_model(&current) else {
        let all: Vec<ToastItem> = want.iter().map(|t| to_toast_item(t, images)).collect();
        ui.set_toasts(ModelRc::new(VecModel::from(all)));
        return;
    };
    let mut i = 0;
    while i < model.row_count() {
        let id = model.row_data(i).map_or(-1, |t| t.id);
        if want.iter().any(|t| t.id == id) {
            i += 1;
        } else {
            model.remove(i);
        }
    }
    for (i, t) in want.iter().enumerate() {
        if model.row_data(i).map(|old| old.id) != Some(t.id) {
            model.insert(i, to_toast_item(t, images));
        }
    }
}

fn on_screen(rows: &ModelRc<RowData>, url: &str) -> bool {
    rows.iter()
        .any(|row| row.items.iter().any(|c| c.thumb_url.as_str() == url))
}

/// The `VecModel` behind a `ModelRc`, if that is what it is. Models made here are.
fn vec_model<T: Clone + 'static>(model: &ModelRc<T>) -> Option<&VecModel<T>> {
    model.as_any().downcast_ref::<VecModel<T>>()
}

fn to_color(c: Rgb) -> Color {
    Color::from_rgb_u8(c.r, c.g, c.b)
}

fn to_stream_item(c: &CardVm, images: &ImageMemory) -> StreamItem {
    StreamItem {
        channel: c.channel.as_str().into(),
        title: c.title.as_str().into(),
        category: c.category.as_str().into(),
        viewers: c.viewers.as_str().into(),
        tint: to_color(c.tint),
        // A card rebuilt after its image was already fetched shows it straight away.
        thumbnail: images.get(&c.thumb_url).unwrap_or_default(),
        thumb_url: c.thumb_url.as_str().into(),
        loading: c.loading,
        retry: c.retry,
        offline: c.offline,
    }
}

/// True if the card in the window already shows `new` (the image aside, which only ever
/// arrives later through [`apply_thumbnail`]).
fn same_card(old: &StreamItem, new: &CardVm) -> bool {
    old.channel.as_str() == new.channel
        && old.title.as_str() == new.title
        && old.category.as_str() == new.category
        && old.viewers.as_str() == new.viewers
        && old.thumb_url.as_str() == new.thumb_url
        && old.tint == to_color(new.tint)
        && old.loading == new.loading
        && old.retry == new.retry
        && old.offline == new.offline
}

/// Quiet zone around a QR code, in modules (the specification asks for four).
const QR_QUIET_ZONE: usize = 4;

/// The QR modules as a one-pixel-per-module image with its quiet zone. The window draws it with
/// nearest-neighbour scaling, so the modules stay sharp at any size.
fn qr_image(dark: &[bool], width: i32) -> Image {
    let side = usize::try_from(width).unwrap_or(0);
    if side == 0 || dark.len() != side * side {
        return Image::default();
    }
    let full = side + 2 * QR_QUIET_ZONE;
    let mut buffer = SharedPixelBuffer::<Rgba8Pixel>::new(full as u32, full as u32);
    let pixels = buffer.make_mut_slice();
    for p in pixels.iter_mut() {
        *p = Rgba8Pixel::new(255, 255, 255, 255);
    }
    for (i, is_dark) in dark.iter().enumerate() {
        if *is_dark {
            let (x, y) = (i % side + QR_QUIET_ZONE, i / side + QR_QUIET_ZONE);
            pixels[y * full + x] = Rgba8Pixel::new(0, 0, 0, 255);
        }
    }
    Image::from_rgba8(buffer)
}

fn to_row_data(r: &RowVm, images: &ImageMemory) -> RowData {
    let items: Vec<StreamItem> = r.cards.iter().map(|c| to_stream_item(c, images)).collect();
    RowData {
        title: r.title.as_str().into(),
        items: ModelRc::new(VecModel::from(items)),
        shape: r.shape as i32,
    }
}

fn to_piece(text: &str, url: &str, emote: bool, images: &ImageMemory) -> ChatPiece {
    ChatPiece {
        text: text.into(),
        // A line built after its images were fetched shows them straight away.
        image: images.get(url).unwrap_or_default(),
        url: url.into(),
        emote,
    }
}

fn to_chat_msg(m: &ChatLineVm, images: &ImageMemory) -> ChatMsg {
    let badges: Vec<ChatPiece> = m
        .badges
        .iter()
        .map(|url| to_piece("", url, false, images))
        .collect();
    let pieces: Vec<ChatPiece> = m
        .pieces
        .iter()
        .map(|p| to_piece(&p.text, &p.url, p.emote, images))
        .collect();
    ChatMsg {
        user: m.user.as_str().into(),
        tint: to_color(m.tint),
        kind: m.kind as i32,
        badges: ModelRc::new(VecModel::from(badges)),
        pieces: ModelRc::new(VecModel::from(pieces)),
    }
}

/// How to turn the lines `shown` into `want` by dropping some at the front and adding some at
/// the back, which is all a growing chat does: returns how many to drop and how many of `want`
/// are then already there. If the lines changed any other way (a ban removed some from the
/// middle), everything is dropped and rebuilt.
pub(crate) fn slide_plan<T: PartialEq>(shown: &[T], want: &[T]) -> (usize, usize) {
    for drop in 0..=shown.len() {
        let kept = &shown[drop..];
        if kept.len() <= want.len() && want[..kept.len()] == *kept {
            return (drop, kept.len());
        }
    }
    (shown.len(), 0)
}

/// What the window shows of the chat, kept to work out the next change.
#[derive(Default)]
struct ChatShown {
    lines: Vec<ChatLineVm>,
    images: ImageMemory,
}

thread_local! {
    static CHAT: RefCell<ChatShown> = RefCell::new(ChatShown::default());
}

/// Brings the window's chat lines in line with `want`. New lines are pushed and old ones removed
/// one by one, so a busy chat does not rebuild the lines that stay (and their images).
fn sync_chat(ui: &MainWindow, want: &[ChatLineVm]) {
    CHAT.with_borrow_mut(|chat| {
        let current = ui.get_chat();
        let model = vec_model(&current).filter(|m| m.row_count() == chat.lines.len());
        let Some(model) = model else {
            let all: Vec<ChatMsg> = want.iter().map(|l| to_chat_msg(l, &chat.images)).collect();
            ui.set_chat(ModelRc::new(VecModel::from(all)));
            chat.lines = want.to_vec();
            return;
        };
        let (drop, keep) = slide_plan(&chat.lines, want);
        for _ in 0..drop {
            model.remove(0);
        }
        for line in &want[keep..] {
            model.push(to_chat_msg(line, &chat.images));
        }
        chat.lines = want.to_vec();
    });
}

/// Hands a decoded chat image (emote or badge) to the window: remembers it and patches every
/// piece that shows `url`. Must run on the UI thread.
pub fn apply_chat_image(ui: &MainWindow, url: &str, thumb: &Thumbnail) {
    let (w, h) = (thumb.width, thumb.height);
    if w == 0 || h == 0 || thumb.rgba.len() != w as usize * h as usize * 4 {
        tracing::debug!(url, w, h, "ignoring a malformed chat image");
        return;
    }
    let mut buffer = SharedPixelBuffer::<Rgba8Pixel>::new(w, h);
    buffer.make_mut_bytes().copy_from_slice(&thumb.rgba);
    let image = Image::from_rgba8(buffer);

    let lines = ui.get_chat();
    CHAT.with_borrow_mut(|chat| {
        chat.images.insert(url, image.clone());
        chat.images.trim(CHAT_IMAGE_MEMORY_CAP, |u| {
            chat.lines
                .iter()
                .any(|l| l.badges.iter().any(|b| b == u) || l.pieces.iter().any(|p| p.url == u))
        });
    });
    let patch = |pieces: &ModelRc<ChatPiece>| {
        let Some(model) = vec_model(pieces) else {
            return;
        };
        for (i, piece) in pieces.iter().enumerate() {
            if piece.url.as_str() == url {
                model.set_row_data(
                    i,
                    ChatPiece {
                        image: image.clone(),
                        ..piece
                    },
                );
            }
        }
    };
    for line in lines.iter() {
        patch(&line.badges);
        patch(&line.pieces);
    }
}

/// Makes `model` hold exactly `want`, touching only the entries that differ: appended cards are
/// pushed, dropped ones removed, changed ones replaced. Untouched cards keep their state, so
/// loading a page or an image never restarts their animations.
fn sync_cards(model: &VecModel<StreamItem>, want: &[CardVm], images: &ImageMemory) {
    for (i, card) in want.iter().enumerate() {
        match model.row_data(i) {
            Some(old) if same_card(&old, card) => {}
            Some(_) => model.set_row_data(i, to_stream_item(card, images)),
            None => model.push(to_stream_item(card, images)),
        }
    }
    while model.row_count() > want.len() {
        model.remove(model.row_count() - 1);
    }
}

/// Brings the window's `rows` model in line with `want`, in place where it can.
fn sync_rows(ui: &MainWindow, want: &[RowVm], images: &ImageMemory) {
    let current = ui.get_rows();
    let Some(model) = vec_model(&current) else {
        let rows: Vec<RowData> = want.iter().map(|r| to_row_data(r, images)).collect();
        ui.set_rows(ModelRc::new(VecModel::from(rows)));
        return;
    };
    for (i, row) in want.iter().enumerate() {
        match model.row_data(i) {
            Some(old)
                if old.title.as_str() == row.title
                    && old.shape == row.shape as i32
                    && vec_model(&old.items).is_some() =>
            {
                if let Some(cards) = vec_model(&old.items) {
                    sync_cards(cards, &row.cards, images);
                }
            }
            Some(_) => model.set_row_data(i, to_row_data(row, images)),
            None => model.push(to_row_data(row, images)),
        }
    }
    while model.row_count() > want.len() {
        model.remove(model.row_count() - 1);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use htpc_core::{
        grid_rows, reduce, Category, Channel, ChannelRef, DeviceCode, Event, Intent, PageOutcome,
        RailEntry, RowId, User,
    };

    fn stream(channel: &str, viewers: u64, thumb: Option<&str>) -> Tile {
        Tile::Stream(Stream {
            channel: ChannelRef::twitch(channel),
            user_id: String::new(),
            title: format!("{channel} title"),
            category: "VALORANT".into(),
            viewers,
            tint: Rgb::from_hex(0x7a2e2e),
            thumbnail: thumb.map(Into::into),
        })
    }

    fn category(id: &str, name: &str) -> Tile {
        Tile::Category(Category {
            id: id.into(),
            name: name.into(),
            tint: Rgb::from_hex(0x6b5a2d),
            thumbnail: Some(format!("https://cdn/{id}.jpg")),
        })
    }

    fn streams(n: usize) -> Vec<Tile> {
        (0..n).map(|i| stream(&format!("s{i}"), 1, None)).collect()
    }

    fn home_rows() -> Vec<Row> {
        vec![
            Row::new(
                RowKind::Featured,
                vec![stream(
                    "grandmaster_k",
                    48_123,
                    Some("https://cdn/hero.jpg"),
                )],
            ),
            Row::new(RowKind::TopLive, vec![stream("tidewatch", 900, None)]),
            Row::new(RowKind::Categories, vec![category("26936", "Music")]),
        ]
    }

    fn with_rows(rows: Vec<Row>) -> AppState {
        let (s, _) = htpc_core::init();
        let feed = FeedId::Home;
        reduce(&s, Event::FeedLoaded { feed, rows }).0
    }

    fn loaded() -> AppState {
        with_rows(home_rows())
    }

    #[test]
    fn viewer_formatting() {
        assert_eq!(format_viewers(0), "0");
        assert_eq!(format_viewers(860), "860");
        assert_eq!(format_viewers(999), "999");
        assert_eq!(format_viewers(1_000), "1.0K");
        assert_eq!(format_viewers(1_949), "1.9K");
        assert_eq!(format_viewers(4_200), "4.2K");
        assert_eq!(format_viewers(48_123), "48.1K");
        assert_eq!(format_viewers(999_949), "999.9K");
        assert_eq!(format_viewers(999_950), "1.0M");
        assert_eq!(format_viewers(1_250_000), "1.3M");
    }

    #[test]
    fn bitrate_and_fps_formatting() {
        assert_eq!(format_bitrate(0), "0 kb/s");
        assert_eq!(format_bitrate(160_000), "160 kb/s");
        assert_eq!(format_bitrate(999_499), "999 kb/s");
        assert_eq!(format_bitrate(1_000_000), "1.0 Mb/s");
        assert_eq!(format_bitrate(6_240_000), "6.2 Mb/s");
        assert_eq!(format_fps(60_000), "60");
        assert_eq!(format_fps(59_940), "59.94");
        assert_eq!(format_fps(29_970), "29.97");
        assert_eq!(format_fps(25_500), "25.5");
        assert_eq!(format_fps(59_998), "60");
    }

    #[test]
    fn the_stats_box_is_only_on_the_player_while_on() {
        let (mut s, _) = reduce(&loaded(), Intent::Select);
        assert!(view_model(&s).stats_text.is_empty());
        s.session.stats = true;
        assert!(view_model(&s).stats_text.starts_with("Waiting"));
        let (s, _) = reduce(&s, Intent::Back);
        assert!(view_model(&s).stats_text.is_empty(), "not over Home");
    }

    #[test]
    fn home_has_a_hero_then_titled_rows() {
        let vm = view_model(&loaded());
        assert_eq!(vm.screen, ScreenId::Home);
        assert_eq!(vm.heading, "Live now");
        let shapes: Vec<_> = vm.rows.iter().map(|r| r.shape).collect();
        assert_eq!(shapes, [Shape::Hero, Shape::Landscape, Shape::Portrait]);
        let titles: Vec<_> = vm.rows.iter().map(|r| r.title.as_str()).collect();
        assert_eq!(titles, ["", "Top Live", "Categories"]);
        let hero = &vm.rows[0].cards[0];
        assert_eq!(hero.viewers, "48.1K");
        assert_eq!(hero.thumb_url, "https://cdn/hero.jpg");
        assert_eq!(hero.tint, Rgb::new(0x7a, 0x2e, 0x2e));
        // Category cards have empty title/category/viewers, which hides the live line.
        let cat = &vm.rows[2].cards[0];
        assert_eq!(cat.channel, "Music");
        assert_eq!((cat.title.as_str(), cat.viewers.as_str()), ("", ""));
        assert_eq!(cat.thumb_url, "https://cdn/26936.jpg");
        assert!(!vm.can_retry && !vm.feed_empty && vm.feed_error.is_empty());
    }

    #[test]
    fn a_loading_feed_shows_skeleton_cards() {
        let (s, _) = htpc_core::init();
        let vm = view_model(&s);
        assert_eq!(vm.rows.len(), 2);
        assert_eq!(vm.rows[0].shape, Shape::Hero);
        assert!(vm.rows.iter().all(|r| r.cards.iter().all(|c| c.loading)));
        assert!(!vm.feed_empty, "loading is not empty");
    }

    #[test]
    fn a_failed_feed_shows_the_error_and_a_retry_hint() {
        let (s, _) = htpc_core::init();
        let failed = Event::FeedFailed {
            feed: FeedId::Home,
            message: "offline".into(),
        };
        let vm = view_model(&reduce(&s, failed).0);
        assert!(vm.rows.is_empty(), "no skeletons once it failed");
        assert_eq!(vm.feed_error, "offline");
        assert!(vm.can_retry && !vm.feed_empty);
    }

    #[test]
    fn a_feed_with_nothing_live_is_empty_not_failed() {
        let vm = view_model(&with_rows(Vec::new()));
        assert!(vm.feed_empty && vm.feed_error.is_empty() && !vm.can_retry);
    }

    fn paged_row(n: usize) -> Row {
        Row::new(RowKind::TopLive, streams(n)).with_paging(Paging::More("c".into()))
    }

    fn press(mut s: AppState, intent: Intent, times: usize) -> AppState {
        for _ in 0..times {
            s = reduce(&s, intent).0;
        }
        s
    }

    #[test]
    fn a_loading_page_adds_skeletons_to_the_end_of_the_row() {
        // 8 tiles: the next page starts once focus reaches col 4 (col + 1 + 3 >= 8).
        let s = with_rows(vec![paged_row(8)]);
        let early = view_model(&press(s.clone(), Intent::Right, 3));
        assert_eq!(early.rows[0].cards.len(), 8, "not near the end yet");

        let vm = view_model(&press(s, Intent::Right, 4));
        let cards = &vm.rows[0].cards;
        assert_eq!(cards.len(), 8 + SKELETON_TAIL);
        assert!(cards[8..].iter().all(|c| c.loading));
        assert!(cards[..8].iter().all(|c| !c.loading));
    }

    #[test]
    fn a_failed_page_adds_a_retry_card_and_the_x_hint() {
        let s = press(with_rows(vec![paged_row(4)]), Intent::Right, 1);
        let failed = Event::MoreLoaded {
            feed: FeedId::Home,
            row: RowId::twitch(RowKind::TopLive),
            cursor: "c".into(),
            outcome: PageOutcome::Failed,
        };
        let vm = view_model(&reduce(&s, failed).0);
        let last = vm.rows[0].cards.last().unwrap();
        assert!(last.retry && !last.loading);
        assert!(vm.can_retry);
    }

    /// The Browse screen with `n` categories loaded.
    fn browse(n: usize, paging: Paging) -> AppState {
        let (s, _) = htpc_core::init();
        let (s, _) = reduce(&s, Intent::RailPick(RailEntry::Browse));
        let tiles: Vec<Tile> = (0..n).map(|i| category(&format!("g{i}"), "G")).collect();
        let rows = grid_rows(RowKind::Categories, tiles, paging);
        let feed = FeedId::Browse;
        reduce(&s, Event::FeedLoaded { feed, rows }).0
    }

    #[test]
    fn browse_is_a_portrait_grid_with_a_heading() {
        // Skeleton grid while the directory loads.
        let (s, _) = htpc_core::init();
        let (s, _) = reduce(&s, Intent::RailPick(RailEntry::Browse));
        let vm = view_model(&s);
        assert_eq!(vm.screen, ScreenId::Grid);
        assert_eq!(vm.heading, "Categories");
        assert!(vm.rows.iter().all(|r| r.shape == Shape::Portrait));
        assert!(vm.rows.iter().all(|r| r.cards.iter().all(|c| c.loading)));
        assert_eq!(vm.rail_current, 2);
        assert!(!vm.can_back, "Browse is a root screen");

        let vm = view_model(&browse(12, Paging::End));
        let lens: Vec<_> = vm.rows.iter().map(|r| r.cards.len()).collect();
        assert_eq!(lens, [5, 5, 2]);
        assert!(vm.rows.iter().all(|r| r.title.is_empty()));
        assert_eq!(vm.select_label, "Open");
    }

    #[test]
    fn a_loading_grid_page_fills_the_row_then_adds_a_row_of_skeletons() {
        let s = browse(12, Paging::More("n".into()));
        // 3 rows: the trigger is row + 1 >= 2, so one Down starts the page.
        let vm = view_model(&press(s, Intent::Down, 1));
        let lens: Vec<_> = vm.rows.iter().map(|r| r.cards.len()).collect();
        // The last row (2 of 5) is completed with 3 skeletons, then a full skeleton row follows.
        assert_eq!(lens, [5, 5, 5, 5]);
        assert!(vm.rows[2].cards[2..].iter().all(|c| c.loading));
        assert!(vm.rows[3].cards.iter().all(|c| c.loading));
    }

    #[test]
    fn the_category_screen_shows_its_streams_and_offers_back() {
        let (s, _) = reduce(&browse(1, Paging::End), Intent::Select);
        let vm = view_model(&s);
        assert_eq!(vm.screen, ScreenId::Grid);
        assert_eq!(vm.heading, "G");
        assert!(vm.can_back);
        assert_eq!(vm.rail_current, 2, "the category belongs to Browse");

        let rows = grid_rows(RowKind::CategoryStreams, streams(4), Paging::End);
        let feed = FeedId::Category("g0".into());
        let vm = view_model(&reduce(&s, Event::FeedLoaded { feed, rows }).0);
        let lens: Vec<_> = vm.rows.iter().map(|r| r.cards.len()).collect();
        assert_eq!(lens, [3, 1]);
        assert!(vm.rows.iter().all(|r| r.shape == Shape::Landscape));
        assert_eq!(vm.select_label, "Watch");
    }

    #[test]
    fn player_and_login_screens() {
        let (s, _) = reduce(&loaded(), Intent::Select);
        let vm = view_model(&s);
        assert_eq!(vm.screen, ScreenId::Player);
        assert_eq!(vm.now_playing.channel, "grandmaster_k");
        assert_eq!(vm.rail_current, 0, "overlay highlights Home underneath");
        // The cards under the player stay in the model.
        assert_eq!(vm.rows.len(), 3);

        let (s, _) = reduce(&loaded(), Intent::RailPick(RailEntry::Account));
        let code = DeviceCode {
            user_code: "KQRT-7ZMP".into(),
            verification_uri: "twitch.tv/activate".into(),
        };
        let (s, _) = reduce(&s, Event::DeviceCodeIssued(code));
        let vm = view_model(&s);
        assert_eq!(vm.screen, ScreenId::SignIn);
        assert_eq!(vm.device_code, "KQRT-7ZMP");
        assert_eq!(vm.verification_url, "twitch.tv/activate");
    }

    #[test]
    fn user_and_chat_colours() {
        let mut s = loaded();
        s.user = Some(User {
            login: "nightowl_dev".into(),
            can_chat: true,
        });
        let line = |user: &str, color| ChatMessage {
            color,
            ..ChatMessage::plain(user, "x")
        };
        s.chat = vec![line("a", None), line("b", Some(Rgb::new(250, 120, 40)))].into();
        let vm = view_model(&s);
        assert_eq!(vm.user_name, "nightowl_dev");
        assert_eq!(vm.chat[0].tint, DEFAULT_CHAT_NAME);
        assert_eq!(vm.chat[1].tint, Rgb::new(250, 120, 40));
    }

    // ---- chat --------------------------------------------------------------------------

    use htpc_core::{ChatBuffer, Moderation, CHAT_CAPACITY};

    #[test]
    fn dark_name_colours_are_lightened_and_bright_ones_left_alone() {
        let bright = Rgb::new(30, 144, 255);
        assert_eq!(readable_name_colour(bright), bright);
        let lime = DEFAULT_CHAT_NAME;
        assert_eq!(readable_name_colour(lime), lime);

        let brightness = |c: Rgb| {
            (0.2126 * f32::from(c.r) + 0.7152 * f32::from(c.g) + 0.0722 * f32::from(c.b)) / 255.0
        };
        for dark in [Rgb::new(0, 0, 0), Rgb::new(0, 0, 255), Rgb::new(40, 10, 60)] {
            let lifted = readable_name_colour(dark);
            assert!(
                brightness(lifted) >= MIN_NAME_BRIGHTNESS - 0.01,
                "{dark:?} -> {lifted:?}"
            );
            assert!(brightness(lifted) > brightness(dark));
        }
        // Blue stays blue-ish: the hue is kept.
        let blue = readable_name_colour(Rgb::new(0, 0, 255));
        assert!(blue.b > blue.r && blue.b > blue.g);
    }

    fn watching(lines: Vec<ChatMessage>) -> AppState {
        let (mut s, _) = reduce(&loaded(), Intent::Select);
        s.chat = ChatBuffer::from(lines);
        s
    }

    #[test]
    fn a_failure_shows_its_summary_and_the_player_log_apart() {
        let mut s = watching(Vec::new());
        s.playback = htpc_core::Playback::Failed(
            "the stream could not be opened (mpv error -13)\n\n[ffmpeg] error: a\n[cplayer] error: b"
                .into(),
        );
        let vm = view_model(&s);
        assert_eq!(
            vm.playback_error,
            "the stream could not be opened (mpv error -13)"
        );
        assert_eq!(vm.playback_details, "[ffmpeg] error: a\n[cplayer] error: b");

        // A message without log lines is all summary.
        s.playback = htpc_core::Playback::Failed("Streamlink is not installed".into());
        let vm = view_model(&s);
        assert_eq!(vm.playback_error, "Streamlink is not installed");
        assert!(vm.playback_details.is_empty());
    }

    #[test]
    fn a_line_becomes_badges_words_and_emotes() {
        let line = ChatMessage {
            badges: vec!["https://cdn/mod.png".into()],
            spans: vec![
                ChatSpan::Text("nice  play".into()),
                ChatSpan::Emote {
                    name: "Kappa".into(),
                    url: "https://cdn/kappa.png".into(),
                },
                ChatSpan::Text("gg".into()),
            ],
            kind: ChatKind::Action,
            ..ChatMessage::plain("Marie", "")
        };
        let vm = view_model(&watching(vec![line]));
        let l = &vm.chat[0];
        assert_eq!((l.user.as_str(), l.kind), ("Marie", ChatLineKind::Action));
        assert_eq!(l.badges, ["https://cdn/mod.png"]);
        let words: Vec<_> = l
            .pieces
            .iter()
            .map(|p| (p.text.as_str(), p.emote))
            .collect();
        assert_eq!(
            words,
            [
                ("nice", false),
                ("play", false),
                ("Kappa", true),
                ("gg", false)
            ]
        );
        assert_eq!(l.pieces[2].url, "https://cdn/kappa.png");
        assert!(l.pieces[0].url.is_empty());
    }

    #[test]
    fn notices_and_messages_are_told_apart() {
        let vm = view_model(&watching(vec![
            ChatMessage::notice("slow mode is on"),
            ChatMessage::plain("a", "hi"),
        ]));
        assert_eq!(vm.chat[0].kind, ChatLineKind::Notice);
        assert_eq!(vm.chat[1].kind, ChatLineKind::Message);
    }

    #[test]
    fn only_the_newest_lines_reach_the_window() {
        let lines = (0..CHAT_CAPACITY)
            .map(|i| ChatMessage::plain(&format!("u{i}"), "x"))
            .collect();
        let vm = view_model(&watching(lines));
        assert_eq!(vm.chat.len(), CHAT_LINES_SHOWN);
        assert_eq!(
            vm.chat.last().unwrap().user,
            format!("u{}", CHAT_CAPACITY - 1)
        );
        assert_eq!(
            vm.chat[0].user,
            format!("u{}", CHAT_CAPACITY - CHAT_LINES_SHOWN)
        );
    }

    #[test]
    fn the_status_line_follows_the_connection() {
        let status = |s: &AppState| view_model(s).chat_status;
        let (s, _) = reduce(&loaded(), Intent::Select);
        assert_eq!(status(&s), "Connecting…");

        let event = |status| Event::ChatStatus {
            channel: ChannelRef::twitch("grandmaster_k"),
            status,
        };
        let (live, _) = reduce(&s, event(ChatStatus::Live));
        assert_eq!(status(&live), "No messages yet");
        let (back, _) = reduce(&live, event(ChatStatus::Reconnecting));
        assert_eq!(status(&back), "Reconnecting…");

        let line = Event::ChatMessages {
            channel: ChannelRef::twitch("grandmaster_k"),
            messages: vec![ChatMessage::plain("a", "hi")],
        };
        let (talking, _) = reduce(&live, line);
        assert_eq!(status(&talking), "", "nothing to say once lines flow");
    }

    #[test]
    fn the_layout_setting_chooses_side_panel_or_overlay() {
        let mut s = watching(Vec::new());
        assert!(!view_model(&s).chat_overlay);
        s.settings.chat_layout = ChatLayout::Overlay;
        assert!(view_model(&s).chat_overlay);
    }

    #[test]
    fn a_ban_changes_the_chat_in_the_diff() {
        let mut a = ChatMessage::plain("Troll", "bad");
        a.id = "1".into();
        let s = watching(vec![a, ChatMessage::plain("friend", "hi")]);
        let before = view_model(&s);
        let ban = Event::ChatModerated {
            channel: ChannelRef::twitch("grandmaster_k"),
            action: Moderation::ClearUser("troll".into()),
        };
        let (s, _) = reduce(&s, ban);
        let u = Update::diff(&before, view_model(&s)).expect("chat changed");
        assert!(u.chat_changed && !u.rows_changed);
        assert_eq!(u.vm.chat.len(), 1);
    }

    #[test]
    fn a_growing_chat_slides_a_ban_rebuilds() {
        let letters = |s: &str| s.chars().collect::<Vec<_>>();
        // Appending: nothing dropped, all old lines kept.
        assert_eq!(slide_plan(&letters("ab"), &letters("abc")), (0, 2));
        // The window is full: old lines fall off the front.
        assert_eq!(slide_plan(&letters("abc"), &letters("bcd")), (1, 2));
        assert_eq!(slide_plan(&letters("abc"), &letters("cde")), (2, 1));
        // Unchanged.
        assert_eq!(slide_plan(&letters("abc"), &letters("abc")), (0, 3));
        // From nothing.
        assert_eq!(slide_plan(&[], &letters("ab")), (0, 0));
        // A line removed from the middle is not a slide: drop everything, add everything.
        assert_eq!(slide_plan(&letters("abcd"), &letters("acd")), (4, 0));
        // Everything replaced.
        assert_eq!(slide_plan(&letters("ab"), &letters("xy")), (2, 0));
        // Everything gone.
        assert_eq!(slide_plan(&letters("ab"), &[]), (2, 0));
    }

    #[test]
    fn diff_only_flags_changed_models() {
        let a = view_model(&loaded());
        assert_eq!(Update::diff(&a, a.clone()), None);

        let (s, _) = reduce(&loaded(), Intent::Down);
        let u = Update::diff(&a, view_model(&s)).expect("focus changed");
        assert!(!u.rows_changed, "focus moves must not touch the cards");
        assert!(!u.chat_changed);

        let u = Update::diff(&ViewModel::default(), a).expect("rows loaded");
        assert!(u.rows_changed);
        assert!(!u.chat_changed);
    }

    #[test]
    fn image_memory_evicts_the_oldest_unused_images_first() {
        let mut m = ImageMemory::default();
        for i in 0..IMAGE_MEMORY_CAP + 3 {
            m.insert(&format!("u{i}"), Image::default());
        }
        // u0 is still on screen, so u1..u3 go instead.
        m.trim(IMAGE_MEMORY_CAP, |u| u == "u0");
        assert_eq!(m.images.len(), IMAGE_MEMORY_CAP);
        assert!(m.get("u0").is_some());
        assert!(m.get("u1").is_none() && m.get("u3").is_none());
        assert!(m.get("u4").is_some());
    }

    // ---- sign-in, Settings, Following --------------------------------------------------

    const ACTIVATE: &str = "https://www.twitch.tv/activate?device-code=ABCDEFGH";

    fn on_login() -> AppState {
        reduce(&loaded(), Intent::RailPick(RailEntry::Account)).0
    }

    fn issue(s: &AppState, uri: &str) -> AppState {
        let code = DeviceCode {
            user_code: "ABCD-EFGH".into(),
            verification_uri: uri.into(),
        };
        reduce(s, Event::DeviceCodeIssued(code)).0
    }

    #[test]
    fn the_sign_in_screen_tells_the_stages_apart() {
        let vm = view_model(&on_login());
        assert_eq!(vm.login_status, "Getting a code from Twitch...");
        assert!(!vm.login_failed && vm.qr.is_empty());

        let s = issue(&on_login(), ACTIVATE);
        let vm = view_model(&s);
        assert_eq!(vm.login_status, "Waiting for approval...");
        assert_eq!(vm.device_code, "ABCD-EFGH");
        assert!(!vm.login_failed);

        let say = |problem| view_model(&reduce(&s, Event::LoginFailed(problem)).0);
        let vm = say(LoginProblem::Expired);
        assert_eq!(vm.login_status, "That code expired.");
        assert!(vm.login_failed && vm.device_code.is_empty() && vm.qr.is_empty());
        let denied = say(LoginProblem::Denied);
        assert_eq!(denied.login_status, "Sign-in was declined on your phone.");
        let failed = say(LoginProblem::Failed("no network".into()));
        assert_eq!(failed.login_status, "Could not sign in: no network");
    }

    #[test]
    fn the_address_is_shown_plain_and_the_qr_carries_the_link() {
        let vm = view_model(&issue(&on_login(), ACTIVATE));
        assert_eq!(vm.verification_url, "www.twitch.tv/activate");
        assert!(vm.qr_width >= 21, "a QR code is at least 21 modules wide");
        assert_eq!(vm.qr.len(), (vm.qr_width * vm.qr_width) as usize);
        assert!(vm.qr.iter().any(|d| *d) && vm.qr.iter().any(|d| !*d));
        // The three finder patterns have a dark corner module.
        let w = vm.qr_width as usize;
        assert!(vm.qr[0] && vm.qr[w - 1] && vm.qr[w * (w - 1)]);
    }

    #[test]
    fn display_url_drops_the_scheme_query_and_fragment() {
        assert_eq!(display_url("https://a.test/go?x=1#y"), "a.test/go");
        assert_eq!(display_url("twitch.tv/activate"), "twitch.tv/activate");
    }

    #[test]
    fn the_qr_image_has_a_quiet_zone() {
        let dark = [true, false, false, true];
        let size = qr_image(&dark, 2).size();
        assert_eq!((size.width, size.height), (2 + 8, 2 + 8));
        assert_eq!(
            qr_image(&dark, 3).size().width,
            0,
            "a bad size gives no image"
        );
        assert_eq!(qr_image(&[], 0).size().width, 0);
    }

    #[test]
    fn a_new_qr_is_flagged_in_the_diff() {
        let before = view_model(&on_login());
        let waiting = issue(&on_login(), ACTIVATE);
        let after = view_model(&waiting);
        let u = Update::diff(&before, after.clone()).expect("code arrived");
        assert!(u.qr_changed);
        // A on a waiting login changes nothing, so nothing is sent.
        let pressed = reduce(&waiting, Intent::Select).0;
        assert!(Update::diff(&after, view_model(&pressed)).is_none());
    }

    fn signed_in(s: &AppState) -> AppState {
        let user = User {
            login: "nightowl_dev".into(),
            can_chat: true,
        };
        reduce(s, Event::SignedIn(user)).0
    }

    #[test]
    fn settings_offers_the_account_action() {
        let (s, _) = reduce(&loaded(), Intent::RailPick(RailEntry::Settings));
        let vm = view_model(&s);
        assert_eq!(vm.screen, ScreenId::Settings);
        assert_eq!(vm.rail_current, 4, "after Search");
        assert_eq!((vm.select_label.as_str(), vm.can_retry), ("Sign in", false));

        let vm = view_model(&signed_in(&s));
        assert_eq!(vm.select_label, "Sign out");
        assert_eq!(vm.user_name, "nightowl_dev");
    }

    #[test]
    fn settings_shows_the_choices_and_labels_a_by_the_focused_row() {
        let (s, _) = reduce(&loaded(), Intent::RailPick(RailEntry::Settings));
        let vm = view_model(&s);
        assert_eq!(vm.settings_focus, 0);
        assert_eq!(vm.chat_layout_label, "Side panel");
        assert_eq!(vm.quality_label, "Best available");
        assert_eq!(vm.live_notifications_label, "On");
        assert_eq!(vm.background_play_label, "Off");

        let (s, _) = reduce(&s, Intent::Down);
        let (s, _) = reduce(&s, Intent::Select);
        let vm = view_model(&s);
        assert_eq!(vm.settings_focus, 1);
        assert_eq!(vm.chat_layout_label, "Over the video");
        assert!(vm.chat_overlay);
        assert_eq!(vm.select_label, "Change");
    }

    fn offline(name: &str) -> Tile {
        Tile::Channel(Channel {
            channel: ChannelRef::twitch(name),
            name: name.into(),
            tint: Rgb::new(9, 9, 9),
        })
    }

    #[test]
    fn following_shows_live_cards_then_dimmed_offline_ones() {
        let s = signed_in(&loaded());
        let (s, _) = reduce(&s, Intent::RailPick(RailEntry::Following));
        // While it loads for the first time: skeletons, under the heading.
        let vm = view_model(&s);
        assert_eq!(vm.screen, ScreenId::Home);
        assert_eq!(vm.heading, "Following");
        assert_eq!(vm.rail_current, 1);
        assert!(vm.rows.iter().all(|r| r.cards.iter().all(|c| c.loading)));

        let rows = vec![
            Row::new(RowKind::FollowedLive, vec![stream("live0", 1_500, None)]),
            Row::new(RowKind::FollowedOffline, vec![offline("a"), offline("b")]),
        ];
        let feed = FeedId::Following;
        let vm = view_model(&reduce(&s, Event::FeedLoaded { feed, rows }).0);
        assert_eq!(vm.rows[1].title, "Offline");
        assert!(!vm.rows[0].cards[0].offline);
        assert_eq!(vm.rows[0].cards[0].viewers, "1.5K");
        let all_dimmed = vm.rows[1]
            .cards
            .iter()
            .all(|c| c.offline && c.viewers.is_empty());
        assert!(all_dimmed);
        assert_eq!(vm.rows[1].cards[0].channel, "a");
        assert_eq!(vm.select_label, "Watch");
    }

    #[test]
    fn an_empty_following_tab_says_so() {
        let s = signed_in(&loaded());
        let (s, _) = reduce(&s, Intent::RailPick(RailEntry::Following));
        let feed = FeedId::Following;
        let rows = Vec::new();
        let vm = view_model(&reduce(&s, Event::FeedLoaded { feed, rows }).0);
        assert!(vm.feed_empty);
        assert_eq!(vm.empty_text, "You are not following anyone yet");
        // Home keeps its own wording.
        let home = view_model(&loaded());
        assert_eq!(home.empty_text, "Nothing is live here right now");
    }

    /// Watching, with the player menu open on the Subscribe row.
    fn on_subscribe_row(can_open_browser: bool) -> AppState {
        let mut s = loaded();
        s.can_open_browser = can_open_browser;
        let (s, _) = reduce(&s, Intent::Select);
        let s = reduce(&s, Event::PlaybackStarted).0;
        (0..7).fold(reduce(&s, Intent::Menu).0, |s, _| {
            reduce(&s, Intent::Down).0
        })
    }

    #[test]
    fn the_subscribe_row_is_an_action() {
        let s = on_subscribe_row(true);
        let vm = view_model(&s);
        assert_eq!(vm.player_menu_focus, 7);
        assert_eq!(vm.player_menu_values[7], "");
        assert_eq!(vm.select_label, "Open");
        assert!(!vm.subscribe_open && vm.subscribe_qr.is_empty());
    }

    #[test]
    fn the_subscribe_qr_carries_the_channels_page() {
        let s = reduce(&on_subscribe_row(true), Intent::Select).0;
        let channel = s.now_playing.as_ref().unwrap().channel.login.clone();
        let vm = view_model(&s);
        assert!(vm.subscribe_open && vm.subscribe_can_open);
        assert_eq!(vm.subscribe_url, format!("www.twitch.tv/subs/{channel}"));
        let (expected, width) = qr_modules(&format!("https://www.twitch.tv/subs/{channel}"));
        assert_eq!(vm.subscribe_qr, expected);
        assert_eq!(vm.subscribe_qr_width, width);
        assert!(width >= 21);
        assert_eq!(vm.select_label, "Open in browser");
    }

    #[test]
    fn game_mode_offers_no_browser() {
        let s = reduce(&on_subscribe_row(false), Intent::Select).0;
        let vm = view_model(&s);
        assert!(vm.subscribe_open);
        assert!(!vm.subscribe_can_open);
        assert_eq!(vm.select_label, "");
    }

    #[test]
    fn room_rules_read_short() {
        assert_eq!(room_mode_text(&RoomMode::default()), "");
        let all = RoomMode {
            slow_secs: 120,
            followers_min: Some(0),
            subs_only: true,
            emote_only: true,
        };
        assert_eq!(
            room_mode_text(&all),
            "Slow mode 120 s · Followers only · Subscribers only · Emote only"
        );
        let wait = |min| {
            room_mode_text(&RoomMode {
                followers_min: Some(min),
                ..RoomMode::default()
            })
        };
        assert_eq!(wait(10), "Followers only (10 min)");
        assert_eq!(wait(90), "Followers only (90 min)");
        assert_eq!(wait(180), "Followers only (3 h)");
        assert_eq!(wait(1440), "Followers only (1 day)");
        assert_eq!(wait(7 * 1440), "Followers only (7 days)");
    }

    #[test]
    fn drop_reasons_read_short_and_use_the_rooms_numbers() {
        let dropped = |code: &str, message: &str| SendProblem::Dropped {
            code: code.into(),
            message: message.into(),
        };
        let room = RoomMode {
            slow_secs: 30,
            followers_min: Some(10),
            ..RoomMode::default()
        };
        let text = |p: &SendProblem| send_problem_text(p, &room);
        assert_eq!(
            text(&dropped("msg_followersonly", "long")),
            "Followers-only chat (10 min)"
        );
        assert_eq!(
            text(&dropped("msg_followersonly_followed", "")),
            "Followers-only chat (10 min)"
        );
        assert_eq!(text(&dropped("msg_slowmode", "")), "Slow mode: wait 30 s");
        assert_eq!(
            send_problem_text(&dropped("msg_slowmode", ""), &RoomMode::default()),
            "Slow mode: wait a moment"
        );
        assert_eq!(text(&dropped("msg_subsonly", "")), "Subscribers-only chat");
        assert_eq!(
            text(&dropped("msg_rejected_mandatory", "")),
            "Held by AutoMod for the moderators"
        );
        assert_eq!(
            text(&dropped("msg_banned", "")),
            "You are banned from this chat"
        );
        // Unknown codes show Twitch's own sentence, or a plain one without it.
        assert_eq!(
            text(&dropped(
                "msg_timedout",
                " You are timed out for 9 more seconds. "
            )),
            "You are timed out for 9 more seconds."
        );
        assert_eq!(text(&dropped("", "")), "Twitch did not post it");
        assert_eq!(
            text(&SendProblem::TooFast),
            "Sending too fast: wait a few seconds"
        );
        assert_eq!(
            text(&SendProblem::Failed("network error".into())),
            "Could not send: network error"
        );
    }

    #[test]
    fn a_long_message_shows_its_end_and_counts_all_of_it() {
        let mut s = loaded();
        s.user = Some(User {
            login: "me".into(),
            can_chat: true,
        });
        let (mut s, _) = reduce(&s, Intent::Select);
        s.compose.text = format!("start {}", "x".repeat(200));
        let vm = view_model(&s);
        assert_eq!(vm.compose_count, "206/500");
        assert!(vm.compose_text.starts_with('…'));
        assert_eq!(vm.compose_text.chars().count(), COMPOSE_SHOWN + 1);
        assert!(!vm.compose_text.contains("start"));
    }

    #[test]
    fn the_keys_are_rebuilt_only_when_the_page_or_shift_changes() {
        let mut s = loaded();
        s.user = Some(User {
            login: "me".into(),
            can_chat: true,
        });
        let (s, _) = reduce(&s, Intent::Select);
        let (s, _) = reduce(&s, Event::PlaybackStarted);
        let (s, _) = reduce(&s, Intent::ToggleChat);
        let (open, _) = reduce(&s, Intent::Retry);
        let u = Update::diff(&view_model(&s), view_model(&open)).expect("opened");
        assert!(u.compose_keys_changed);
        // Moving and typing keep the keys.
        let (moved, _) = reduce(&open, Intent::Right);
        let (typed, _) = reduce(&moved, Intent::Select);
        let u = Update::diff(&view_model(&open), view_model(&typed)).expect("typed");
        assert!(!u.compose_keys_changed);
        assert_eq!(view_model(&typed).compose_text, "b");
        // A new page does not.
        let (paged, _) = reduce(&typed, Intent::PageRight);
        let u = Update::diff(&view_model(&typed), view_model(&paged)).expect("paged");
        assert!(u.compose_keys_changed);
        assert_eq!(view_model(&paged).compose_keys[0].label, "1");
    }

    #[test]
    fn the_subscribe_qr_is_rebuilt_only_when_it_changes() {
        let menu = on_subscribe_row(true);
        let open = reduce(&menu, Intent::Select).0;
        let u = Update::diff(&view_model(&menu), view_model(&open)).expect("the card opened");
        assert!(u.subscribe_qr_changed);
        // Pressing A again opens the browser, and the code stays.
        let again = reduce(&open, Intent::Select).0;
        if let Some(u) = Update::diff(&view_model(&open), view_model(&again)) {
            assert!(!u.subscribe_qr_changed);
        }
        let closed = reduce(&open, Intent::Back).0;
        let u = Update::diff(&view_model(&open), view_model(&closed)).expect("the card closed");
        assert!(u.subscribe_qr_changed);
        assert!(view_model(&closed).subscribe_qr.is_empty());
    }
}
