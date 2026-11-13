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

/// Which videos of a channel: its past broadcasts or its highlights. (Uploads are left out: few
/// channels have any, see vod-plan.md section 8.)
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum VideoType {
    /// A past broadcast (Twitch's `ARCHIVE`). The newest one is still `recording` while live.
    Archive,
    Highlight,
}

/// A part of a video Twitch muted (copyrighted music), in seconds from its start.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct MutedSegment {
    pub offset: u32,
    pub duration: u32,
}

/// A video of a channel, as the website's GQL API lists it to any viewer.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct VideoInfo {
    /// Digits only: `https://www.twitch.tv/videos/<id>`.
    pub id: String,
    pub kind: VideoType,
    pub title: String,
    pub length_seconds: u32,
    /// RFC 3339, as Twitch writes it (`2026-11-10T18:28:22Z`).
    pub created_at: String,
    pub view_count: u64,
    pub game_name: String,
    /// Already sized (440x248). Empty when Twitch has none.
    pub thumbnail_url: String,
    /// Still being recorded: the broadcast is live.
    pub recording: bool,
    /// Only the channel's subscribers can watch it (`resourceRestriction` is set).
    pub sub_only: bool,
    pub muted: Vec<MutedSegment>,
    pub owner_login: String,
    pub owner_name: String,
}

/// A clip of a channel.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ClipInfo {
    /// Letters, digits, `_` and `-`: `https://clips.twitch.tv/<slug>`.
    pub slug: String,
    pub title: String,
    pub duration_seconds: u32,
    pub view_count: u64,
    /// RFC 3339.
    pub created_at: String,
    /// Already sized by Twitch. Empty when there is none.
    pub thumbnail_url: String,
    /// Who made the clip. Empty when Twitch does not say.
    pub curator_name: String,
    pub game_name: String,
}

/// Everything a channel page shows, in one request (`TwitchApi::channel_videos`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ChannelVideos {
    pub user: UserInfo,
    /// The live stream, if the channel is live now.
    pub live: Option<StreamInfo>,
    /// RFC 3339 start of the last broadcast, live or not. `None` if it never streamed.
    pub last_broadcast_at: Option<String>,
    /// Past broadcasts, newest first (the first page only: later pages need an integrity token).
    pub archives: Vec<VideoInfo>,
    pub highlights: Vec<VideoInfo>,
    /// The most watched clips of the last 7 days.
    pub clips_week: Vec<ClipInfo>,
    /// The most watched clips ever.
    pub clips_all_time: Vec<ClipInfo>,
}

/// One chapter of a video: where the streamer changed category.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct VideoChapter {
    pub position_ms: u64,
    pub duration_ms: u64,
    /// The category (game) name.
    pub name: String,
}

/// What the player needs about one video beyond its card (`TwitchApi::video_details`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct VideoDetails {
    pub id: String,
    pub length_seconds: u32,
    /// In order. A video without chapter markers gets one chapter, the whole video, named after
    /// its category (empty if it has none).
    pub chapters: Vec<VideoChapter>,
    pub muted: Vec<MutedSegment>,
    /// The storyboard description (`…-info.json`) for seek previews. Empty when there is none.
    pub seek_previews_url: String,
}

/// The past broadcast being recorded from a live stream (`TwitchApi::live_archive`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LiveArchive {
    pub video_id: String,
    /// RFC 3339 start of the live stream. A point in the stream maps to this many seconds into
    /// the video (the recording starts a few seconds after it).
    pub stream_started_at: String,
}

/// One piece of a replayed chat line: plain text, or an emote.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReplayFragment {
    pub text: String,
    /// Twitch's emote id when the piece is an emote (`text` is its name). The app turns it into
    /// an image address on Twitch's emote CDN.
    pub emote_id: Option<String>,
}

/// One line of a video's chat replay (`TwitchApi::chat_replay`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReplayComment {
    pub id: String,
    /// Whole seconds from the start of the video.
    pub offset_seconds: u32,
    /// Empty when the account is gone.
    pub login: String,
    pub name: String,
    /// `#RRGGBB`, or empty when the chatter never picked one.
    pub color: String,
    pub fragments: Vec<ReplayFragment>,
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
