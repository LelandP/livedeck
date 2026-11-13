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
    grid_cols, subscribe_url, AppState, ChannelPage, ChannelTab, ChatKind, ChatLayout, ChatMessage,
    ChatSpan, ChatStatus, Feed, FeedId, HypeTrain, Key, LoginProblem, Paging, Platform, Playback,
    PlayerMenuRow, RailEntry, Rgb, RoomMode, Row, RowKind, Screen, SearchState, SendProblem,
    SettingsRow, Shift, Stream, Tile, Toast, ToastKind, CHAT_MAX, RESUME_MOSAIC, TOASTS_SHOWN,
};
use slint::{
    Color, ComponentHandle, Image, Model, ModelRc, Rgba8Pixel, SharedPixelBuffer, VecModel,
};

use crate::{
    ChatMsg, ChatPiece, MainWindow, MenuItem, MosaicCell, MvLayoutChip, MvStripItem, MvTile,
    PopupItem, RowData, SearchKey, SpanItem, StreamItem, ToastItem, VideoHud,
};

mod multiview;
mod video;

pub use multiview::{layout_name, LayoutChipVm, MultiviewVm, MvTileVm, PopupRowVm, StripVm};
pub use video::{seek_chip, speed_label, video_vm, SpanVm, VideoVm};

/// Players whose frames the window keeps (`tile-videos`): one per multiview stream.
const VIDEO_SLOTS: usize = htpc_core::layout::MAX_STREAMS;

/// A thumbnail of the strip over multiview's full screen, in logical px (`Theme.strip-thumb-w/h`).
const STRIP_THUMB: (f32, f32) = (176.0, 99.0);

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
    /// Multiview's grid. Its full screen is `Player`.
    Multiview = 6,
    /// A Twitch channel's page: header, Videos / Clips tabs and rows of cards.
    Channel = 7,
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
    /// A Kick channel: drawn with a "Kick" chip. Twitch cards have none.
    pub kick: bool,
    /// Picked for multiview: drawn with a tick badge.
    pub picked: bool,
    /// Home's *Resume multiview* card: `mosaic` instead of the thumbnail, `title` says how many.
    pub resume: bool,
    /// The resume card's 2×2 mosaic: up to four of its streams.
    pub mosaic: Vec<MosaicVm>,
    /// A video or clip card's second line: "Nov 10 · Just Chatting". Empty on other cards.
    pub meta: String,
    /// A video or clip's length, as a chip at the top right: "4:12:08". Empty on other cards.
    pub length: String,
    /// A video that is in *Continue watching*: how far in, in thousandths. 0 draws no bar.
    pub progress: i32,
    /// Twitch muted part of the video's sound: a "Some audio muted" chip.
    pub muted_audio: bool,
    /// A subscriber-only video: drawn with a lock.
    pub locked: bool,
}

/// One stream of the *Resume multiview* card's mosaic.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct MosaicVm {
    pub tint: Rgb,
    /// Cache key of the stream preview. Empty without one.
    pub thumb_url: String,
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

/// A "<channel> is live" popup, or a note ("2 offline, skipped").
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct ToastVm {
    /// The toast's id: the window keeps each popup's element (and its animations) by it.
    pub id: i32,
    /// The channel, or a note's whole text.
    pub name: String,
    /// A note: `name` alone, without "is live", a preview or a second line.
    pub note: bool,
    /// The stream title and category, as far as known.
    pub line: String,
    pub tint: Rgb,
    /// Cache key of the stream preview. Empty without one.
    pub thumb_url: String,
}

/// Everything `MainWindow` displays.
#[derive(Debug, Clone, PartialEq, Default)]
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
    /// The chat panel's header. Empty for live chat (the panel says "CHAT"), "CHAT REPLAY" for a
    /// video's: read only, with no compose box.
    pub chat_title: String,
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
    /// What A does on the focused card: "Watch", "Open", or "Resume" (multiview).
    pub select_label: String,
    /// Something failed and X would try again.
    pub can_retry: bool,
    /// B has somewhere to go on this feed screen (only the Category screen).
    pub can_back: bool,
    /// A stream or channel card has focus on a browse screen: View opens its popup.
    pub can_view: bool,
    /// What the hints bar calls that: "Multiview" on a live stream, "Favorites" on an offline
    /// channel. Empty when `can_view` is off.
    pub view_label: String,
    /// The tray above the hints bar on browse screens: "Multiview · 3 picked". Empty while
    /// nothing is picked.
    pub tray_text: String,
    /// A Twitch stream or channel card has focus on a browse screen: Y opens its channel page.
    pub can_channel: bool,
    /// The channel page's header: the name, the profile picture's cache key (empty without one),
    /// the first letter to show instead, whether it is live, the line under the name ("Live
    /// now", "Offline · last live 3 hours ago") and the tab (0 Videos, 1 Clips).
    pub channel_name: String,
    pub channel_avatar: String,
    pub channel_initial: String,
    pub channel_live: bool,
    pub channel_status: String,
    pub channel_tab: i32,
    /// On the channel page, the focused video is in *Continue watching*: X starts it over.
    pub can_start_over: bool,
    /// On Home, the focused card is a video of *Continue watching*: X takes it off the row.
    pub can_remove: bool,
    /// Focused row of the Settings screen (`SettingsRow` order: account, chat layout, quality).
    pub settings_focus: i32,
    pub chat_layout_label: String,
    pub quality_label: String,
    /// "On" or "Off".
    pub live_notifications_label: String,
    /// "On" or "Off".
    pub background_play_label: String,
    /// "On" or "Off".
    pub show_kick_label: String,
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
    /// Its focused row, as an index into `player_menu_rows`.
    pub player_menu_focus: i32,
    /// The menu's rows for what is on screen, top to bottom: a live stream's
    /// (`PlayerMenuRow::ALL`), a video's or a clip's (vod-plan.md section 2).
    pub player_menu_rows: Vec<MenuRowVm>,
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
    /// The player slot of the stream with sound: its frames are the Player screen's `video`.
    pub video_slot: i32,
    /// Multiview's grid, picker, strip and the View popup.
    pub mv: MultiviewVm,
    /// The video player's HUD, seek bar and end card, while a video is on the Player screen.
    pub video: VideoVm,
}

/// One row of the player menu: what it changes, its value, what it does (shown under the rows
/// while it has focus), whether A runs it (an action row, no arrows), and whether it cannot be
/// changed now (Quality while audio only is on).
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct MenuRowVm {
    pub title: String,
    pub value: String,
    pub help: String,
    pub action: bool,
    pub locked: bool,
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
    if state.screen != Screen::Player || state.composing() || state.video.is_some() {
        ""
    } else if !state.on_screen_features().chat_send {
        // Kick: chat is read-only in LiveDeck.
        ""
    } else if !state.is_signed_in() {
        "Sign in to Twitch to chat"
    } else if !state.can_chat() {
        "Chat needs a new Twitch sign-in: press X"
    } else {
        "Press X to write in chat"
    }
}

/// The value a player-menu row shows.
fn player_menu_value(state: &AppState, row: PlayerMenuRow) -> String {
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
        PlayerMenuRow::Speed => state
            .video
            .as_ref()
            .map(|v| speed_label(v.speed))
            .unwrap_or_default(),
        PlayerMenuRow::Chapters => state
            .video
            .as_ref()
            .map(video::chapters_value)
            .unwrap_or_default(),
        // Action rows: no value, A opens the card or the keyboard. Signed out, the chat
        // row says what it needs instead.
        // Twitch's own features, on a stream of another platform.
        PlayerMenuRow::Subscribe if !state.on_screen_features().subscribe => "Twitch only".into(),
        PlayerMenuRow::WriteChat if !state.on_screen_features().chat_send => "Twitch only".into(),
        PlayerMenuRow::Subscribe | PlayerMenuRow::StartOver => String::new(),
        PlayerMenuRow::WriteChat if !state.is_signed_in() => "Sign in to Twitch to chat".into(),
        PlayerMenuRow::WriteChat => String::new(),
        PlayerMenuRow::ChannelVideos if state.multiview.is_some() => "Not in multiview".into(),
        PlayerMenuRow::ChannelVideos if !state.on_screen_features().videos => "Twitch only".into(),
        PlayerMenuRow::ChannelVideos => String::new(),
    }
}

/// What a player-menu row is called. The subscribe row names the channel.
fn player_menu_title(state: &AppState, row: PlayerMenuRow) -> String {
    match row {
        PlayerMenuRow::Quality => "Quality".into(),
        PlayerMenuRow::Hwdec => "Hardware decoding".into(),
        PlayerMenuRow::Latency => "Latency".into(),
        PlayerMenuRow::Zoom => "Zoom".into(),
        PlayerMenuRow::Stats => "Stats".into(),
        PlayerMenuRow::AudioOnly => "Audio only".into(),
        PlayerMenuRow::SleepTimer => "Sleep timer".into(),
        PlayerMenuRow::Subscribe => format!(
            "Subscribe to {}",
            state
                .now_playing
                .as_ref()
                .map(|p| p.channel.login.as_str())
                .unwrap_or_default()
        ),
        PlayerMenuRow::WriteChat => "Write in chat".into(),
        PlayerMenuRow::ChannelVideos => "Channel videos".into(),
        PlayerMenuRow::Speed => "Speed".into(),
        PlayerMenuRow::Chapters => "Chapters".into(),
        PlayerMenuRow::StartOver => "Start over".into(),
    }
}

/// What a player-menu row does, shown under the rows while it has focus. A video's rows say
/// "video" where a stream's say "stream".
fn player_menu_help(state: &AppState, row: PlayerMenuRow) -> &'static str {
    let video = state.video.is_some();
    match row {
        PlayerMenuRow::Quality if state.audio_only => "Locked while Audio only is on.",
        PlayerMenuRow::Quality if video => {
            "Picture quality. Lower uses less data and battery. Reloads the video where it is."
        }
        PlayerMenuRow::Quality => {
            "Picture quality. Lower uses less data and battery. Reloads the stream."
        }
        PlayerMenuRow::Hwdec => {
            "Decode on the graphics chip. Turn off if the picture stutters or shows glitches."
        }
        PlayerMenuRow::Latency => "Low stays close to live. Normal buffers more, for fewer stalls on weak Wi-Fi. Reloads the stream.",
        PlayerMenuRow::Zoom => {
            "Fit shows the whole picture with black bars. Fill crops it to fill the screen."
        }
        PlayerMenuRow::Stats => "A box with resolution, codec, bitrate, buffer and dropped frames.",
        PlayerMenuRow::AudioOnly if video => {
            "Sound only, on a dark screen. Saves data and battery. For this video only."
        }
        PlayerMenuRow::AudioOnly => {
            "Sound only, on a dark screen. Saves data and battery. For this stream only."
        }
        PlayerMenuRow::SleepTimer if video => "Stops the video and puts the device to sleep after a while. Asks a minute before.",
        PlayerMenuRow::SleepTimer => "Stops the stream and puts the device to sleep after a while. Asks a minute before.",
        PlayerMenuRow::Subscribe => {
            "Shows a QR code of the channel's subscribe page, to finish on your phone."
        }
        PlayerMenuRow::WriteChat => "Write in this channel's chat with the on-screen keyboard or quick messages. X does it too while chat is shown.",
        PlayerMenuRow::ChannelVideos if video => {
            "Back to this channel's past broadcasts, highlights and clips. Stops the video."
        }
        PlayerMenuRow::ChannelVideos => {
            "Past broadcasts, highlights and clips of this channel. Stops the stream."
        }
        PlayerMenuRow::Speed => "Faster or slower, with the pitch kept. For this video only.",
        PlayerMenuRow::Chapters => "Left and Right pick a chapter, A jumps there. With the menu closed, LB and RB jump to the previous and next one.",
        PlayerMenuRow::StartOver => "Plays the video again from 0:00.",
    }
}

/// The player menu's rows for what is on screen, top to bottom.
fn player_menu_rows(state: &AppState) -> Vec<MenuRowVm> {
    state
        .player_menu_rows()
        .iter()
        .map(|row| MenuRowVm {
            title: player_menu_title(state, *row),
            value: player_menu_value(state, *row),
            help: player_menu_help(state, *row).to_owned(),
            action: row.is_action(),
            locked: *row == PlayerMenuRow::Quality && state.audio_only,
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

/// A video's length or position: `0:28`, `12:44`, `4:12:08`.
pub fn format_duration(seconds: u32) -> String {
    let (h, m, s) = (seconds / 3600, seconds % 3600 / 60, seconds % 60);
    if h > 0 {
        format!("{h}:{m:02}:{s:02}")
    } else {
        format!("{m}:{s:02}")
    }
}

/// A date as a card shows it: `Nov 10`, or `Nov 10, 2025` when it is not this year. `now` is the
/// page's own clock (`ChannelInfo::loaded_at`). Empty for 0, which means unknown.
pub fn format_date(unix: u64, now: u64) -> String {
    const MONTHS: [&str; 12] = [
        "Jan", "Feb", "Mar", "Apr", "May", "Jun", "Jul", "Aug", "Sep", "Oct", "Nov", "Dec",
    ];
    if unix == 0 {
        return String::new();
    }
    let (year, month, day) = htpc_core::date_from_unix(unix);
    let name = MONTHS[(month as usize).clamp(1, 12) - 1];
    if now == 0 || htpc_core::date_from_unix(now).0 == year {
        format!("{name} {day}")
    } else {
        format!("{name} {day}, {year}")
    }
}

/// How long before `now` something was, in the largest whole unit: `just now`, `12 min ago`,
/// `3 hours ago`, `1 day ago`, `2 months ago`, `1 year ago`.
pub fn format_ago(then: u64, now: u64) -> String {
    let plural = |n: u64, unit: &str| {
        if n == 1 {
            format!("1 {unit} ago")
        } else {
            format!("{n} {unit}s ago")
        }
    };
    let secs = now.saturating_sub(then);
    match secs {
        0..=59 => "just now".to_owned(),
        60..=3_599 => format!("{} min ago", secs / 60),
        3_600..=86_399 => plural(secs / 3_600, "hour"),
        86_400..=2_591_999 => plural(secs / 86_400, "day"),
        2_592_000..=31_535_999 => plural(secs / 2_592_000, "month"),
        _ => plural(secs / 31_536_000, "year"),
    }
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

fn row_title(kind: RowKind, platform: Platform, feed: &FeedId) -> &'static str {
    match kind {
        // Grid screens have a heading instead of row titles, and the hero speaks for itself.
        _ if feed.is_grid() => "",
        RowKind::Featured | RowKind::CategoryStreams => "",
        RowKind::FollowedLive => "Followed · Live",
        RowKind::FollowedOffline | RowKind::FavoritesOffline => "Offline",
        // The Favorites screen's heading already says whose they are.
        RowKind::FavoritesLive if *feed == FeedId::Favorites => "Live",
        RowKind::FavoritesLive => "Favorites · Live",
        RowKind::TopLive if platform == Platform::Kick => "Top Live on Kick",
        RowKind::TopLive => "Top Live",
        RowKind::Categories | RowKind::SearchCategories => "Categories",
        RowKind::SearchLive => "Live channels",
        RowKind::SearchOffline => "Offline channels",
        // Only drawn when it holds the resume card alone (Home has no other row of streams).
        RowKind::Resume => "Multiview",
        // A channel page's rows (vod-plan.md section 2). The page itself is phase 3.
        RowKind::LiveNow => "Live now",
        RowKind::PastBroadcasts => "Past broadcasts",
        RowKind::Highlights => "Highlights",
        RowKind::ClipsWeek => "Top clips · last 7 days",
        RowKind::ClipsAllTime => "Top clips · all time",
        RowKind::ContinueWatching => "Continue watching",
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
        | RowKind::SearchOffline
        | RowKind::FavoritesLive
        | RowKind::FavoritesOffline
        | RowKind::Resume
        | RowKind::LiveNow
        | RowKind::PastBroadcasts
        | RowKind::Highlights
        | RowKind::ClipsWeek
        | RowKind::ClipsAllTime
        | RowKind::ContinueWatching => Shape::Landscape,
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
        kick: s.channel.platform == Platform::Kick,
        ..CardVm::default()
    }
}

fn tile_card(state: &AppState, tile: &Tile) -> CardVm {
    match tile {
        Tile::Stream(s) => CardVm {
            picked: state.is_picked(&s.channel),
            ..stream_card(s)
        },
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
            kick: c.channel.platform == Platform::Kick,
            ..CardVm::default()
        },
        Tile::Resume(card) => CardVm {
            channel: "Resume multiview".to_owned(),
            title: format!("{} streams", card.streams.len()),
            resume: true,
            mosaic: card
                .streams
                .iter()
                .take(RESUME_MOSAIC)
                .map(|s| MosaicVm {
                    tint: s.tint,
                    thumb_url: s.thumbnail.clone().unwrap_or_default(),
                })
                .collect(),
            ..CardVm::default()
        },
        // The channel page's cards (vod-plan.md section 2): the title as the big line, then the
        // date and the category, the length as a chip, and a bar for a video left part-way.
        Tile::Video(v) => CardVm {
            channel: v.title.clone(),
            meta: meta_line(&[format_date(v.created_at, now_of(state)), v.category.clone()]),
            length: format_duration(v.length_s),
            progress: state
                .progress_of(&v.id)
                .map_or(0, |p| to_i32(p.permille() as usize)),
            muted_audio: !v.muted.is_empty(),
            locked: v.sub_only,
            tint: v.tint,
            thumb_url: v.thumbnail.clone().unwrap_or_default(),
            ..CardVm::default()
        },
        Tile::Clip(c) => CardVm {
            channel: c.title.clone(),
            // Who clipped it shows with the card focused, in the second line of text.
            title: if c.clipped_by.is_empty() {
                String::new()
            } else {
                format!("Clipped by {}", c.clipped_by)
            },
            meta: meta_line(&[
                format!("{} views", format_viewers(c.views)),
                format_date(c.created_at, now_of(state)),
            ]),
            length: format_duration(c.length_s),
            tint: c.tint,
            thumb_url: c.thumbnail.clone().unwrap_or_default(),
            ..CardVm::default()
        },
    }
}

/// The clock for dates: the channel page's, or on Home the newest saved place's (0 when there is
/// none, which leaves the year out).
fn now_of(state: &AppState) -> u64 {
    state
        .channel_page
        .as_ref()
        .map(|p| p.now)
        .filter(|now| *now > 0)
        .or_else(|| state.continue_watching.iter().map(|p| p.saved_at).max())
        .unwrap_or(0)
}

/// The parts that are not empty, joined: "Nov 10 · Just Chatting".
fn meta_line(parts: &[String]) -> String {
    let parts: Vec<&str> = parts
        .iter()
        .map(String::as_str)
        .filter(|p| !p.is_empty())
        .collect();
    parts.join(" · ")
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
        FeedId::Following | FeedId::Favorites | FeedId::Search(_) => {
            vec![row(Shape::Landscape, SKELETON_HOME_COLS)]
        }
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
/// card where a page failed. Picked streams wear a tick.
fn feed_rows(state: &AppState, id: &FeedId, feed: &Feed) -> Vec<RowVm> {
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
            title: row_title(r.kind, r.platform, id).to_owned(),
            shape: shape_of(r.kind),
            cards: r.tiles.iter().map(|t| tile_card(state, t)).collect(),
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

/// The channel page's rows for one tab: its tiles, or skeleton cards while it loads. The rows
/// have no further pages.
fn channel_rows(state: &AppState, feed: &Feed) -> Vec<RowVm> {
    if feed.rows.is_empty() {
        return if feed.loading {
            vec![RowVm {
                title: String::new(),
                shape: Shape::Landscape,
                cards: vec![skeleton(); SKELETON_HOME_COLS],
            }]
        } else {
            Vec::new()
        };
    }
    feed.rows
        .iter()
        .map(|r| RowVm {
            title: row_title(r.kind, r.platform, &FeedId::Home).to_owned(),
            shape: shape_of(r.kind),
            cards: r.tiles.iter().map(|t| tile_card(state, t)).collect(),
        })
        .collect()
}

/// The line under the channel's name: "Live now", or "Offline · last live 3 hours ago". Empty
/// until the page has said which.
fn channel_status(page: &ChannelPage) -> String {
    if page.live.is_some() {
        return "Live now".to_owned();
    }
    match page.last_live_at {
        Some(at) if page.now > 0 => format!("Offline · last live {}", format_ago(at, page.now)),
        _ if page.now == 0 => String::new(),
        _ => "Offline".to_owned(),
    }
}

/// What an empty channel tab says.
fn channel_empty_text(tab: ChannelTab) -> &'static str {
    match tab {
        ChannelTab::Videos => "No past broadcasts or highlights yet",
        ChannelTab::Clips => "No clips yet",
    }
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
    // A video's replay has no connection: it says when it is fetching, or that nothing was said.
    if let Some(v) = &state.video {
        return match (state.chat.is_empty(), v.replay.loading()) {
            (true, true) => "Loading…",
            (true, false) => "No messages yet",
            (false, _) => "",
        };
    }
    match state.chat_status {
        ChatStatus::Connecting => "Connecting…",
        ChatStatus::Reconnecting => "Reconnecting…",
        ChatStatus::Live if state.chat.is_empty() => "No messages yet",
        ChatStatus::Live | ChatStatus::Idle => "",
    }
}

/// Rail index of a root screen (`RailEntry` order). `None` for overlays (Player, Login).
fn rail_slot(screen: Screen) -> Option<i32> {
    let entry = match screen {
        Screen::Home => RailEntry::Home,
        Screen::Following => RailEntry::Following,
        Screen::Favorites => RailEntry::Favorites,
        // The category screen belongs to the Browse entry.
        Screen::Browse | Screen::Category => RailEntry::Browse,
        Screen::Search => RailEntry::Search,
        Screen::Settings => RailEntry::Settings,
        // The channel page sits above the screen it was opened from, which stays highlighted.
        Screen::Player | Screen::Login | Screen::Multiview | Screen::Channel => return None,
    };
    Some(to_i32(entry.index()))
}

fn to_i32(v: usize) -> i32 {
    i32::try_from(v).unwrap_or(i32::MAX)
}

fn heading(state: &AppState, id: &FeedId) -> String {
    match id {
        FeedId::Home => "Live now".to_owned(),
        FeedId::Following => "Following".to_owned(),
        FeedId::Favorites => "Favorites".to_owned(),
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
    let note = |name: String| ToastVm {
        id: t.id as i32,
        name,
        note: true,
        ..ToastVm::default()
    };
    let s = match &t.kind {
        ToastKind::Live(s) => s,
        ToastKind::OfflineSkipped(n) => return note(format!("{n} offline, skipped")),
        ToastKind::SlowNetwork { muted: true } => {
            return note("Slow network: lowered quality of muted streams".to_owned())
        }
        ToastKind::SlowNetwork { muted: false } => {
            return note("Slow network: lowered the quality".to_owned())
        }
        ToastKind::NoChatReplay => return note("No chat replay for this video".to_owned()),
        ToastKind::Resumed { pos_s } => {
            return note(format!(
                "Resumed at {} · Start over in the menu",
                format_duration(*pos_s)
            ))
        }
    };
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
        note: false,
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
        FeedId::Favorites => {
            "No favorites yet. Press View on a channel's card, or while watching, to star it"
        }
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
        Screen::Multiview => ScreenId::Multiview,
        Screen::Login => ScreenId::SignIn,
        Screen::Browse | Screen::Category => ScreenId::Grid,
        Screen::Settings => ScreenId::Settings,
        Screen::Search => ScreenId::Search,
        Screen::Channel => ScreenId::Channel,
        // Following and Favorites draw like Home: horizontal rows.
        Screen::Home | Screen::Following | Screen::Favorites => ScreenId::Home,
    };
    let on_search = state.screen == Screen::Search;
    let search_on_keys = on_search && !state.search.in_results && !state.rail.focused;
    // The screens of cards, where streams are picked for multiview.
    let browsing = matches!(
        state.screen,
        Screen::Home
            | Screen::Following
            | Screen::Favorites
            | Screen::Browse
            | Screen::Category
            | Screen::Search
    );
    // What View does on the focused card: multiview on a live stream, favorites on an offline
    // channel, nothing on a category.
    let view_label = match state.focused_tile() {
        _ if !browsing || state.rail.focused || search_on_keys => "",
        Some(Tile::Stream(_)) => "Multiview",
        Some(Tile::Channel(_)) => "Favorites",
        Some(Tile::Category(_) | Tile::Resume(_) | Tile::Video(_) | Tile::Clip(_)) | None => "",
    };
    // Y opens the channel page of a Twitch stream or channel card (Kick has none).
    let can_channel = browsing
        && !state.rail.focused
        && !search_on_keys
        && match state.focused_tile() {
            Some(Tile::Stream(s)) => s.channel.features().videos,
            Some(Tile::Channel(c)) => c.channel.features().videos,
            _ => false,
        };
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
    let on_channel = state.screen == Screen::Channel;
    let subscribe_page = state
        .subscribe_channel()
        .filter(|_| (on_player || on_channel) && state.subscribe_card)
        .map(|c| subscribe_url(&c.login));
    let (subscribe_url_text, subscribe_qr, subscribe_qr_width) = match &subscribe_page {
        Some(url) => {
            let (qr, width) = qr_modules(url);
            (display_url(url), qr, width)
        }
        None => (String::new(), Vec::new(), 0),
    };
    let subscribe_open = subscribe_page.is_some();
    // The raid card shows over multiview's grid too: the raid is the stream with sound's.
    let raid = state
        .raid
        .as_ref()
        .filter(|_| on_player || state.screen == Screen::Multiview);
    let mv = multiview::multiview_vm(state);
    let video = video_vm(state);
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
    // The channel page's tab on screen is not a `FeedId`.
    let channel_page = state.channel_page.as_ref().filter(|_| on_channel);
    let feed = match channel_page {
        Some(page) => Some(page.feed()),
        None => state.feed(&id),
    };
    let (rows, focus, error, loading) = match feed {
        Some(f) if channel_page.is_some() => {
            (channel_rows(state, f), f.focus, f.error.clone(), f.loading)
        }
        Some(f) => (
            feed_rows(state, &id, f),
            f.focus,
            f.error.clone(),
            f.loading,
        ),
        None => (Vec::new(), Default::default(), None, false),
    };
    // Separate rows keep their own scroll position (see `Feed::row_cols`).
    let separate_rows = channel_page.is_some() || !id.is_grid();
    let row_cols: Vec<i32> = match feed {
        Some(f) if separate_rows => (0..rows.len())
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
    let focused_card = feed.and_then(Feed::focused_tile);
    let selects_category = focused_card.is_some_and(|t| matches!(t, Tile::Category(_)));
    let selects_resume = focused_card.is_some_and(|t| matches!(t, Tile::Resume(_)));
    // A on an offline Twitch channel opens its channel page instead of the end screen.
    let selects_channel_page =
        focused_card.is_some_and(|t| matches!(t, Tile::Channel(c) if c.channel.features().videos));
    // What A does on the channel page's focused card.
    let channel_select: Option<String> = channel_page.map(|_| match focused_card {
        Some(Tile::Stream(_) | Tile::Clip(_)) => "Watch".to_owned(),
        // The subscribe card (a QR code), as the player menu's Subscribe row.
        Some(Tile::Video(v)) if v.sub_only => "Subscribe".to_owned(),
        Some(Tile::Video(v)) => match state.progress_of(&v.id) {
            Some(p) => format!("Resume at {}", format_duration(p.pos_s)),
            None => "Watch".to_owned(),
        },
        _ => String::new(),
    });
    // Home's *Continue watching* row: A resumes the focused video, X takes it off the row.
    let home_video = match focused_card {
        Some(Tile::Video(v)) if state.screen == Screen::Home => Some(v),
        _ => None,
    };
    let home_select: Option<String> = home_video.map(|v| match state.progress_of(&v.id) {
        Some(p) => format!("Resume at {}", format_duration(p.pos_s)),
        None => "Watch".to_owned(),
    });
    let can_remove = home_video.is_some() && !state.rail.focused;
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
    let mv_select = multiview::select_label(state, &mv);
    let (select_label, can_retry) = if let (Some(select), None) = (mv_select, raid) {
        // The popup over any screen, and the grid and its picker.
        select
    } else if state.screen == Screen::Settings {
        let label = match state.settings_focus {
            SettingsRow::Account if state.is_signed_in() => "Sign out",
            SettingsRow::Account => "Sign in",
            SettingsRow::ChatLayout
            | SettingsRow::Quality
            | SettingsRow::LiveNotifications
            | SettingsRow::BackgroundPlay
            | SettingsRow::ShowKick => "Change",
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
    } else if on_player && state.player_menu == Some(PlayerMenuRow::StartOver) {
        ("Start over", false)
    } else if on_player && state.player_menu == Some(PlayerMenuRow::Chapters) {
        ("Jump there", false)
    } else if on_player && state.player_menu.is_some_and(PlayerMenuRow::is_action) {
        ("Open", false)
    } else if on_player && state.player_menu.is_some() {
        // The menu has the buttons: A changes the focused value.
        ("Change", false)
    } else if video.end_card {
        ("Select", false)
    } else if video.scrubbing {
        // A jumps to the playhead at once.
        ("Jump here", false)
    } else if on_player {
        match playback {
            PlaybackId::Loading => ("", false),
            // In multiview, play jumps back to live.
            PlaybackId::Playing if state.paused && mv.active => ("Play live", false),
            PlaybackId::Playing if state.paused => ("Resume", false),
            PlaybackId::Playing => ("Pause", false),
            PlaybackId::Offline | PlaybackId::Failed => ("Retry", true),
        }
    } else if let Some(label) = &channel_select {
        (label.as_str(), error.is_some())
    } else if let Some(label) = &home_select {
        (label.as_str(), error.is_some() || failed_page)
    } else {
        (
            if selects_category {
                "Open"
            } else if selects_resume {
                "Resume"
            } else if selects_channel_page {
                "Channel videos"
            } else {
                "Watch"
            },
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
        chat_title: if state.video.is_some() {
            "CHAT REPLAY".to_owned()
        } else {
            String::new()
        },
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
        heading: match channel_page {
            Some(page) => page.name.clone(),
            None => heading(state, &id),
        },
        feed_empty: rows.is_empty() && error.is_none() && !loading,
        empty_text: match channel_page {
            Some(page) => channel_empty_text(page.tab).to_owned(),
            None => empty_text(&id).to_owned(),
        },
        can_retry,
        feed_error: error.unwrap_or_default(),
        select_label: select_label.to_owned(),
        can_back: state.screen == Screen::Category,
        can_view: !view_label.is_empty(),
        view_label: view_label.to_owned(),
        tray_text: if browsing && !state.picked.is_empty() {
            format!("Multiview · {} picked", state.picked.len())
        } else {
            String::new()
        },
        can_channel,
        channel_name: channel_page.map(|p| p.name.clone()).unwrap_or_default(),
        channel_avatar: channel_page
            .and_then(|p| p.avatar.clone())
            .unwrap_or_default(),
        channel_initial: channel_page
            .and_then(|p| p.name.chars().next())
            .map(|c| c.to_uppercase().to_string())
            .unwrap_or_default(),
        channel_live: channel_page.is_some_and(|p| p.live.is_some()),
        channel_status: channel_page.map(channel_status).unwrap_or_default(),
        channel_tab: channel_page.map_or(0, |p| p.tab as i32),
        can_start_over: channel_page.is_some() && state.focused_progress().is_some(),
        can_remove,
        settings_focus: to_i32(state.settings_focus.index()),
        chat_layout_label: state.settings.chat_layout.label().to_owned(),
        quality_label: state.settings.quality.label().to_owned(),
        live_notifications_label: on_off(state.settings.live_notifications).to_owned(),
        background_play_label: on_off(state.settings.background_play).to_owned(),
        show_kick_label: on_off(state.settings.show_kick).to_owned(),
        toasts: state.toasts.iter().take(TOASTS_SHOWN).map(toast).collect(),
        toast_more: to_i32(state.toasts.len().saturating_sub(TOASTS_SHOWN)),
        playback,
        playback_error,
        playback_details,
        paused: state.paused,
        player_menu_open: on_player && state.player_menu.is_some(),
        player_menu_focus: to_i32(
            state
                .player_menu
                .and_then(|row| state.player_menu_rows().iter().position(|r| *r == row))
                .unwrap_or(0),
        ),
        player_menu_rows: player_menu_rows(state),
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
        // A video shows as its channel and title (the loading tint, the audio-only watermark).
        now_playing: match (&state.now_playing, &state.video) {
            (Some(stream), _) => stream_card(stream),
            (None, Some(v)) => video::now_playing_card(v),
            (None, None) => CardVm::default(),
        },
        video_slot: i32::from(state.sound_slot()),
        mv,
        video,
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
#[derive(Debug, Clone, PartialEq)]
pub struct Update {
    pub vm: ViewModel,
    pub rows_changed: bool,
    /// Multiview's lists: the picker's rows (and which are watched), the layout chips, the strip
    /// and the popup's rows. The tiles are synced in place on every update.
    pub picker_changed: bool,
    pub mv_lists_changed: bool,
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
            picker_changed: true,
            mv_lists_changed: true,
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
        let (a, b) = (&prev.mv, &next.mv);
        Some(Self {
            rows_changed: prev.rows != next.rows,
            picker_changed: a.picker_rows != b.picker_rows
                || a.picker_watching != b.picker_watching
                || a.picker_starred != b.picker_starred,
            mv_lists_changed: a.layouts != b.layouts
                || a.strip != b.strip
                || a.popup_rows != b.popup_rows,
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
            if self.picker_changed {
                let items: Vec<StreamItem> = vm
                    .mv
                    .picker_rows
                    .iter()
                    .map(|c| to_stream_item(c, images))
                    .collect();
                ui.set_picker_items(ModelRc::new(VecModel::from(items)));
                ui.set_picker_watching(ModelRc::new(VecModel::from(vm.mv.picker_watching.clone())));
                ui.set_picker_starred(ModelRc::new(VecModel::from(vm.mv.picker_starred.clone())));
            }
            ui.set_now_playing(to_stream_item(&vm.now_playing, images));
            // The profile picture, like a card's image: shown at once if it was fetched before.
            ui.set_channel_avatar(images.get(&vm.channel_avatar).unwrap_or_default());
        });
        apply_multiview(ui, &vm.mv, self.mv_lists_changed);
        ui.set_video_slot(vm.video_slot);
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
        ui.set_chat_title(vm.chat_title.into());
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
        ui.set_can_view(vm.can_view);
        ui.set_view_label(vm.view_label.into());
        ui.set_tray_text(vm.tray_text.into());
        ui.set_can_channel(vm.can_channel);
        ui.set_channel_name(vm.channel_name.into());
        // Rust patches the image in when it arrives, by this key (`apply_thumbnail`).
        ui.set_channel_avatar_url(vm.channel_avatar.into());
        ui.set_channel_initial(vm.channel_initial.into());
        ui.set_channel_live(vm.channel_live);
        ui.set_channel_status(vm.channel_status.into());
        ui.set_channel_tab(vm.channel_tab);
        ui.set_can_start_over(vm.can_start_over);
        ui.set_can_remove(vm.can_remove);
        ui.set_settings_focus(vm.settings_focus);
        ui.set_chat_layout_label(vm.chat_layout_label.into());
        ui.set_quality_label(vm.quality_label.into());
        ui.set_live_notifications_label(vm.live_notifications_label.into());
        ui.set_background_play_label(vm.background_play_label.into());
        ui.set_show_kick_label(vm.show_kick_label.into());
        ui.set_playback(vm.playback as i32);
        ui.set_playback_error(vm.playback_error.into());
        ui.set_playback_details(vm.playback_details.into());
        ui.set_paused(vm.paused);
        ui.set_player_menu_open(vm.player_menu_open);
        ui.set_player_menu_focus(vm.player_menu_focus);
        let items: Vec<MenuItem> = vm
            .player_menu_rows
            .iter()
            .map(|r| MenuItem {
                title: r.title.as_str().into(),
                value: r.value.as_str().into(),
                help: r.help.as_str().into(),
                action: r.action,
                locked: r.locked,
            })
            .collect();
        ui.set_player_menu_items(ModelRc::new(VecModel::from(items)));
        ui.set_video_hud(to_video_hud(&vm.video));
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

fn to_video_hud(v: &VideoVm) -> VideoHud {
    let muted: Vec<SpanItem> = v
        .muted
        .iter()
        .map(|m| SpanItem {
            start: m.start,
            width: m.width,
        })
        .collect();
    VideoHud {
        on: v.on,
        kind: v.kind.as_str().into(),
        channel: v.channel.as_str().into(),
        title: v.title.as_str().into(),
        meta: v.meta.as_str().into(),
        pos: v.pos.as_str().into(),
        length: v.length.as_str().into(),
        head: v.head,
        chapter: v.chapter.as_str().into(),
        ticks: ModelRc::new(VecModel::from(v.ticks.clone())),
        muted: ModelRc::new(VecModel::from(muted)),
        in_muted: v.in_muted,
        chip: v.chip.as_str().into(),
        chip_side: v.chip_side,
        seeking: v.seeking,
        scrubbing: v.scrubbing,
        speed: v.speed.as_str().into(),
        chapters: v.chapters,
        replay: v.replay,
        end_card: v.end_card,
        end_focus: v.end_focus,
    }
}

fn to_mv_tile(t: &MvTileVm) -> MvTile {
    MvTile {
        x: t.x,
        y: t.y,
        w: t.w,
        h: t.h,
        empty: t.empty,
        slot: t.slot,
        channel: t.channel.as_str().into(),
        line: t.line.as_str().into(),
        kick: t.kick,
        tint: to_color(t.tint),
        quality: t.quality.as_str().into(),
        lowered: t.lowered,
        sound: t.sound,
        focused: t.focused,
        state: t.state as i32,
        action: t.action.as_str().into(),
    }
}

/// Brings the window's tiles in line with `want` in place: a tile whose row is only changed
/// keeps its element, so its move to a new place animates.
fn sync_mv_tiles(ui: &MainWindow, want: &[MvTileVm]) {
    let current = ui.get_mv_tiles();
    let Some(model) = vec_model(&current) else {
        let all: Vec<MvTile> = want.iter().map(to_mv_tile).collect();
        ui.set_mv_tiles(ModelRc::new(VecModel::from(all)));
        return;
    };
    for (i, tile) in want.iter().enumerate() {
        let new = to_mv_tile(tile);
        match model.row_data(i) {
            Some(old) if old == new => {}
            Some(_) => model.set_row_data(i, new),
            None => model.push(new),
        }
    }
    while model.row_count() > want.len() {
        model.remove(model.row_count() - 1);
    }
}

/// Multiview's properties. `lists_changed`: the layout chips, the strip or the popup's rows
/// differ from the last update.
fn apply_multiview(ui: &MainWindow, mv: &MultiviewVm, lists_changed: bool) {
    sync_mv_tiles(ui, &mv.tiles);
    if lists_changed {
        let chips: Vec<MvLayoutChip> = mv
            .layouts
            .iter()
            .map(|c| MvLayoutChip {
                label: c.label.as_str().into(),
                on: c.on,
                fits: c.fits,
            })
            .collect();
        ui.set_mv_layouts(ModelRc::new(VecModel::from(chips)));
        let strip: Vec<MvStripItem> = mv
            .strip
            .iter()
            .map(|s| MvStripItem {
                slot: s.slot,
                channel: s.channel.as_str().into(),
                kick: s.kick,
                on: s.on,
            })
            .collect();
        ui.set_mv_strip(ModelRc::new(VecModel::from(strip)));
        let rows: Vec<PopupItem> = mv
            .popup_rows
            .iter()
            .map(|r| PopupItem {
                label: r.label.as_str().into(),
                value: r.value.as_str().into(),
            })
            .collect();
        ui.set_popup_rows(ModelRc::new(VecModel::from(rows)));
    }
    ui.set_mv_active(mv.active);
    ui.set_mv_count_text(mv.count_text.as_str().into());
    ui.set_mv_count(mv.count);
    ui.set_mv_position(mv.position.as_str().into());
    ui.set_picker_open(mv.picker_open);
    ui.set_picker_tab(mv.picker_tab);
    ui.set_picker_focus(mv.picker_focus);
    ui.set_picker_empty(mv.picker_empty.as_str().into());
    ui.set_picker_star_label(mv.picker_star_label.as_str().into());
    ui.set_popup_open(mv.popup_open);
    ui.set_popup_title(mv.popup_title.as_str().into());
    ui.set_popup_sub(mv.popup_sub.as_str().into());
    ui.set_popup_focus(mv.popup_focus);
    ui.set_popup_on_layout(mv.popup_on_layout);
}

/// Hands a player's new frame to the window: into `tile-videos` for multiview's tiles and strip,
/// and into `video` too when it is the stream with sound. Must run on the UI thread.
pub fn show_video(ui: &MainWindow, slot: u8, image: Image) {
    let index = usize::from(slot);
    if index >= VIDEO_SLOTS {
        return;
    }
    let has_room = vec_model(&ui.get_tile_videos()).is_some_and(|m| m.row_count() == VIDEO_SLOTS);
    if !has_room {
        let blank = vec![Image::default(); VIDEO_SLOTS];
        ui.set_tile_videos(ModelRc::new(VecModel::from(blank)));
    }
    if i32::from(slot) == ui.get_video_slot() {
        ui.set_video(image.clone());
    }
    let videos = ui.get_tile_videos();
    if let Some(model) = vec_model(&videos) {
        model.set_row_data(index, image);
    }
}

/// The size in physical pixels a player's video is drawn at now, or (0, 0) while it is not shown
/// (it then keeps playing into a tiny target, see `htpc_player::VideoOutputs`). Must run on the UI
/// thread.
///
/// - The Player screen (the single player, or multiview's full screen): the stream with sound
///   fills the video area, and while the strip is up the others are its thumbnails.
/// - Multiview's grid: each stream is its tile, scaled as the window draws it.
pub fn video_size(ui: &MainWindow, slot: u8) -> (u32, u32) {
    let scale = ui.window().scale_factor();
    let px = |len: f32| (len * scale).round().max(0.0) as u32;
    let slot = i32::from(slot);
    let screen = ui.get_screen();
    if screen == ScreenId::Player as i32 {
        if slot == ui.get_video_slot() {
            return (px(ui.get_video_w()), px(ui.get_video_h()));
        }
        if ui.get_mv_strip_visible() && ui.get_mv_strip().iter().any(|s| s.slot == slot) {
            return (px(STRIP_THUMB.0), px(STRIP_THUMB.1));
        }
    } else if screen == ScreenId::Multiview as i32 {
        let k = ui.get_mv_k();
        if let Some(tile) = ui
            .get_mv_tiles()
            .iter()
            .find(|t| !t.empty && t.slot == slot)
        {
            return (px(tile.w * k), px(tile.h * k));
        }
    }
    (0, 0)
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
            // The resume card's mosaic is patched in its own model.
            if let Some(mosaic) = vec_model(&card.mosaic) {
                for (j, cell) in card.mosaic.iter().enumerate() {
                    if cell.thumb_url.as_str() == url {
                        mosaic.set_row_data(
                            j,
                            MosaicCell {
                                thumbnail: image.clone(),
                                ..cell
                            },
                        );
                    }
                }
            }
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
    // So does the picker's.
    let picker = ui.get_picker_items();
    if let Some(model) = vec_model(&picker) {
        for (i, item) in picker.iter().enumerate() {
            if item.thumb_url.as_str() == url {
                model.set_row_data(
                    i,
                    StreamItem {
                        thumbnail: image.clone(),
                        ..item
                    },
                );
            }
        }
    }
    // The channel page's profile picture.
    if ui.get_channel_avatar_url().as_str() == url {
        ui.set_channel_avatar(image.clone());
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
        note: t.note,
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
        .any(|row| row.items.iter().any(|c| card_urls(&c).any(|u| u == url)))
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
        kick: c.kick,
        picked: c.picked,
        resume: c.resume,
        meta: c.meta.as_str().into(),
        length: c.length.as_str().into(),
        progress: c.progress,
        muted_audio: c.muted_audio,
        locked: c.locked,
        mosaic: ModelRc::new(VecModel::from(
            c.mosaic
                .iter()
                .map(|m| MosaicCell {
                    tint: to_color(m.tint),
                    thumbnail: images.get(&m.thumb_url).unwrap_or_default(),
                    thumb_url: m.thumb_url.as_str().into(),
                })
                .collect::<Vec<_>>(),
        )),
    }
}

/// The URLs of the images a card in the window shows: its thumbnail, or its mosaic's.
fn card_urls(card: &StreamItem) -> impl Iterator<Item = slint::SharedString> + '_ {
    std::iter::once(card.thumb_url.clone()).chain(card.mosaic.iter().map(|m| m.thumb_url))
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
        && old.kick == new.kick
        && old.picked == new.picked
        && old.resume == new.resume
        && old.meta.as_str() == new.meta
        && old.length.as_str() == new.length
        && old.progress == new.progress
        && old.muted_audio == new.muted_audio
        && old.locked == new.locked
        && old.mosaic.row_count() == new.mosaic.len()
        && old
            .mosaic
            .iter()
            .zip(&new.mosaic)
            .all(|(o, n)| o.thumb_url.as_str() == n.thumb_url && o.tint == to_color(n.tint))
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
        assert_eq!(vm.rail_current, 3);
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
        assert_eq!(vm.rail_current, 3, "the category belongs to Browse");

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
        assert_eq!(vm.rail_current, 5, "after Search");
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
    fn the_kick_row_has_its_own_title_and_its_cards_a_chip() {
        let (s, _) = htpc_core::init();
        let kick_stream = Tile::Stream(Stream {
            channel: ChannelRef::kick("awa-xo"),
            user_id: String::new(),
            title: "t".into(),
            category: "c".into(),
            viewers: 1,
            tint: Rgb::default(),
            thumbnail: None,
        });
        let twitch = Row::new(RowKind::TopLive, vec![stream("ender", 5, None)]);
        let kick = Row::new(RowKind::TopLive, vec![kick_stream]).on(Platform::Kick);
        let s = reduce(
            &s,
            Event::FeedLoaded {
                feed: FeedId::Home,
                rows: vec![twitch, kick],
            },
        )
        .0;
        let vm = view_model(&s);
        assert_eq!(vm.rows[0].title, "Top Live");
        assert!(!vm.rows[0].cards[0].kick);
        assert_eq!(vm.rows[1].title, "Top Live on Kick");
        assert!(vm.rows[1].cards[0].kick);
        assert_eq!(vm.rows[1].cards[0].channel, "awa-xo");
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
        let s = reduce(&s, Event::PlaybackStarted { slot: 0 }).0;
        (0..7).fold(reduce(&s, Intent::Menu).0, |s, _| {
            reduce(&s, Intent::Down).0
        })
    }

    #[test]
    fn the_subscribe_row_is_an_action() {
        let s = on_subscribe_row(true);
        let vm = view_model(&s);
        assert_eq!(vm.player_menu_focus, 7);
        assert_eq!(vm.player_menu_rows[7].value, "");
        assert!(vm.player_menu_rows[7].action);
        assert_eq!(
            vm.player_menu_rows[7].title,
            format!(
                "Subscribe to {}",
                s.now_playing.as_ref().unwrap().channel.login
            )
        );
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
        let (s, _) = reduce(&s, Event::PlaybackStarted { slot: 0 });
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
