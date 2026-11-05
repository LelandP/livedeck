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

/// A Twitch user (`GET /users`).
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub struct UserInfo {
    pub id: String,
    pub login: String,
    pub display_name: String,
    #[serde(default)]
    pub profile_image_url: String,
}
