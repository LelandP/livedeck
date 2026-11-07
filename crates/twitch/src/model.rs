//! Helix data types. They mirror the Helix JSON (only the fields we use), so they are
//! deserialised directly. The app maps them to `htpc-core` domain types.

use serde::Deserialize;

/// Largest page Helix allows (`first` is 1..=100).
pub const MAX_PAGE_SIZE: u8 = 100;

/// Which page to fetch.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PageRequest {
    /// Items per page, clamped to 1..=100.
    pub first: u8,
    /// Cursor from the previous [`Page`], or `None` for the first page.
    pub after: Option<String>,
}

impl PageRequest {
    pub fn first(first: u8) -> Self {
        Self {
            first: first.clamp(1, MAX_PAGE_SIZE),
            after: None,
        }
    }

    /// The page after `cursor`, with the same size.
    pub fn after(first: u8, cursor: impl Into<String>) -> Self {
        Self {
            after: Some(cursor.into()),
            ..Self::first(first)
        }
    }
}

impl Default for PageRequest {
    fn default() -> Self {
        Self::first(20)
    }
}

/// One page of results.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Page<T> {
    pub items: Vec<T>,
    /// Pass to [`PageRequest::after`] for the next page. `None` on the last page.
    pub cursor: Option<String>,
}

/// A live stream (`GET /streams`, `GET /streams/followed`).
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub struct StreamInfo {
    pub user_id: String,
    pub user_login: String,
    pub user_name: String,
    #[serde(default)]
    pub game_id: String,
    #[serde(default)]
    pub game_name: String,
    #[serde(default)]
    pub title: String,
    #[serde(default)]
    pub viewer_count: u64,
    /// Template with `{width}` and `{height}` placeholders.
    #[serde(default)]
    pub thumbnail_url: String,
}

/// A game or category (`GET /games/top`).
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub struct Game {
    pub id: String,
    pub name: String,
    /// Template with `{width}` and `{height}` placeholders.
    #[serde(default)]
    pub box_art_url: String,
}

/// A channel the user follows (`GET /channels/followed`), live or not.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub struct FollowedChannel {
    pub broadcaster_id: String,
    pub broadcaster_login: String,
    pub broadcaster_name: String,
}

/// A channel found by name (`GET /search/channels`), live or not. It has no viewer count or
/// stream preview: the app looks the live ones up with [`crate::TwitchApi::streams_by_login`].
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub struct ChannelResult {
    pub id: String,
    pub broadcaster_login: String,
    pub display_name: String,
    #[serde(default)]
    pub game_name: String,
    #[serde(default)]
    pub is_live: bool,
    /// The stream title, or the last one while offline.
    #[serde(default)]
    pub title: String,
    /// The profile image (a full URL, not a template).
    #[serde(default)]
    pub thumbnail_url: String,
}

/// The answer to `POST /chat/messages`: whether Twitch posted the message, and if not, why.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub struct SendOutcome {
    /// Empty when the message was dropped.
    #[serde(default)]
    pub message_id: String,
    #[serde(rename = "is_sent")]
    pub sent: bool,
    /// Why Twitch dropped the message (followers-only, slow mode, AutoMod, banned, ...).
    #[serde(default)]
    pub drop_reason: Option<DropReason>,
}

/// Why Twitch did not post a chat message.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub struct DropReason {
    /// A machine-readable code, the same as the IRC NOTICE `msg-id` (`msg_slowmode`,
    /// `msg_followersonly`, ...).
    #[serde(default)]
    pub code: String,
    /// Twitch's own sentence for the user.
    #[serde(default)]
    pub message: String,
}

/// A hype train running in a channel, as the website's GQL API shows it to any viewer (Helix only
/// shows it to the broadcaster). Points are Twitch's hype points (bits, subs, gift subs).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HypeTrain {
    /// Twitch's id of this train. It stays the same across its levels.
    pub id: String,
    /// The level being worked on, from 1.
    pub level: u32,
    /// Points gathered toward the next level, counted from the start of this one.
    pub progress: u32,
    /// Points this level needs. At least 1.
    pub goal: u32,
    /// Seconds before the train ends, unless the next level is reached first (each level up
    /// restarts the clock).
    pub seconds_left: u32,
    /// The channel's highest level ever.
    pub all_time_high: u32,
}

/// A Twitch user (`GET /users`).
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub struct UserInfo {
    pub id: String,
    pub login: String,
    pub display_name: String,
    #[serde(default)]
    pub profile_image_url: String,
}
