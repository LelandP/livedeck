//! Domain types. These are UI-agnostic: no Slint types, no pre-formatted strings.

use crate::video::{Clip, Video};

/// An sRGB colour. The placeholder tint shown until a card's thumbnail has loaded.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Rgb {
    pub r: u8,
    pub g: u8,
    pub b: u8,
}

impl Rgb {
    pub const fn new(r: u8, g: u8, b: u8) -> Self {
        Self { r, g, b }
    }

    /// Builds a colour from `0xRRGGBB`.
    pub const fn from_hex(rgb: u32) -> Self {
        Self::new((rgb >> 16) as u8, (rgb >> 8) as u8, rgb as u8)
    }
}

/// Where a channel lives. Every channel, stream and row belongs to one.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Default)]
pub enum Platform {
    #[default]
    Twitch,
    Kick,
}

/// What a platform offers beyond watching and reading chat. One table, so the reducer and the
/// view-model ask here instead of checking the platform in many places.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Features {
    /// Follow a raid from the channel on screen (Twitch EventSub).
    pub raids: bool,
    /// The hype train bar (Twitch's GQL API).
    pub hype_train: bool,
    /// The subscribe card (a Twitch page).
    pub subscribe: bool,
    /// Write in chat. Kick needs a Kick sign-in, which LiveDeck does not have.
    pub chat_send: bool,
    /// A channel page with past broadcasts, highlights and clips (vod-plan.md). Twitch's GQL API.
    pub videos: bool,
}

impl Platform {
    pub fn features(self) -> Features {
        match self {
            Platform::Twitch => Features {
                raids: true,
                hype_train: true,
                subscribe: true,
                chat_send: true,
                videos: true,
            },
            Platform::Kick => Features {
                raids: false,
                hype_train: false,
                subscribe: false,
                chat_send: false,
                videos: false,
            },
        }
    }
}

/// One channel: the platform and its login there (a Kick channel's slug). The same login on two
/// platforms is two channels.
#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord, Default)]
pub struct ChannelRef {
    pub platform: Platform,
    pub login: String,
}

impl ChannelRef {
    pub fn new(platform: Platform, login: impl Into<String>) -> Self {
        Self {
            platform,
            login: login.into(),
        }
    }

    pub fn twitch(login: impl Into<String>) -> Self {
        Self::new(Platform::Twitch, login)
    }

    pub fn kick(slug: impl Into<String>) -> Self {
        Self::new(Platform::Kick, slug)
    }

    /// The same channel, ignoring the case of the login (sources disagree on it).
    pub fn is_same(&self, other: &ChannelRef) -> bool {
        self.platform == other.platform && self.login.eq_ignore_ascii_case(&other.login)
    }

    pub fn is_twitch(&self) -> bool {
        self.platform == Platform::Twitch
    }

    pub fn features(&self) -> Features {
        self.platform.features()
    }
}

/// A live stream.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Stream {
    /// The channel, used to join chat and to resolve the playlist.
    pub channel: ChannelRef,
    /// The broadcaster's id on its platform (Twitch: the user id, which EventSub conditions use).
    /// Empty when the source did not say (a card made from a login only);
    /// `Command::FetchStreamInfo` fills it.
    pub user_id: String,
    pub title: String,
    pub category: String,
    /// Raw viewer count. Formatting ("4.2K") is done by the view-model.
    pub viewers: u64,
    pub tint: Rgb,
    /// Preview image URL, already sized for a card. `None` keeps the tint placeholder.
    pub thumbnail: Option<String>,
}

/// A game or category.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Category {
    /// Twitch game id, used to list the category's streams (the Category screen).
    pub id: String,
    pub name: String,
    pub tint: Rgb,
    /// Box art URL, already sized for a card. `None` keeps the tint placeholder.
    pub thumbnail: Option<String>,
}

/// A followed channel that is not live (the dimmed cards of the Following tab).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Channel {
    /// Same as `Stream::channel`. Selecting the card tries to play it.
    pub channel: ChannelRef,
    /// The name as the streamer writes it.
    pub name: String,
    pub tint: Rgb,
}

/// Home's *Resume multiview* card: the streams of the last multiview session that are live, in
/// their saved order. Shown with at least two (`RESUME_MIN_LIVE`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ResumeCard {
    pub streams: Vec<Stream>,
}

/// The fewest live streams the *Resume multiview* card is shown with: multiview needs two.
pub const RESUME_MIN_LIVE: usize = 2;

/// The `Tile::key` of the *Resume multiview* card. No login or game id looks like it.
const RESUME_KEY: &str = "\0resume";

/// One card in a row.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Tile {
    Stream(Stream),
    Category(Category),
    /// A followed channel that is offline.
    Channel(Channel),
    /// *Resume multiview*, at the start of Home's first row of cards.
    Resume(ResumeCard),
    /// A past broadcast or highlight (a channel page's rows).
    Video(Video),
    /// A clip (a channel page's Clips tab).
    Clip(Clip),
}

impl Tile {
    /// The image to show on the card, if the source gave one. The *Resume multiview* card shows
    /// several: see [`Tile::thumbnails`].
    pub fn thumbnail(&self) -> Option<&str> {
        match self {
            Tile::Stream(s) => s.thumbnail.as_deref(),
            Tile::Category(c) => c.thumbnail.as_deref(),
            Tile::Video(v) => v.thumbnail.as_deref(),
            Tile::Clip(c) => c.thumbnail.as_deref(),
            Tile::Channel(_) | Tile::Resume(_) => None,
        }
    }

    /// Every image the card shows: its thumbnail, or the *Resume multiview* card's mosaic.
    pub fn thumbnails(&self) -> Vec<&str> {
        match self {
            Tile::Resume(card) => card
                .streams
                .iter()
                .take(RESUME_MOSAIC)
                .filter_map(|s| s.thumbnail.as_deref())
                .collect(),
            other => other.thumbnail().into_iter().collect(),
        }
    }

    /// The channel of a stream or channel card. `None` for a category, the resume card, and
    /// video and clip cards: those are about the video, so multiview and favorites leave them out.
    pub fn channel(&self) -> Option<&ChannelRef> {
        match self {
            Tile::Stream(s) => Some(&s.channel),
            Tile::Channel(c) => Some(&c.channel),
            Tile::Category(_) | Tile::Resume(_) | Tile::Video(_) | Tile::Clip(_) => None,
        }
    }

    /// Identity within a row: the platform and the channel login or game id. Used to drop
    /// duplicates when a later page repeats an item (viewer counts shift between requests).
    /// Streams and channels share the login space, so a channel that went live is not shown twice
    /// on Following. Categories are Twitch's.
    pub fn key(&self) -> (Platform, &str) {
        match self {
            Tile::Stream(s) => (s.channel.platform, &s.channel.login),
            Tile::Category(c) => (Platform::Twitch, &c.id),
            Tile::Channel(c) => (c.channel.platform, &c.channel.login),
            Tile::Resume(_) => (Platform::Twitch, RESUME_KEY),
            // A channel page row holds only videos or only clips, so the id is enough.
            Tile::Video(v) => (v.channel.platform, &v.id),
            Tile::Clip(c) => (c.channel.platform, &c.slug),
        }
    }
}

/// Streams shown in the *Resume multiview* card's 2×2 mosaic.
pub const RESUME_MOSAIC: usize = 4;

/// What a row shows. The view-model maps this to a (later translatable) title.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum RowKind {
    /// The hero: the single most-watched live stream, drawn as a wide card.
    Featured,
    FollowedLive,
    /// Followed channels that are not live, dimmed, after the live ones (the Following tab).
    FollowedOffline,
    TopLive,
    Categories,
    /// The live streams of one category (the Category screen).
    CategoryStreams,
    /// Search: live channels whose name matches.
    SearchLive,
    /// Search: categories whose name matches.
    SearchCategories,
    /// Search: channels whose name matches that are not live, dimmed.
    SearchOffline,
    /// Starred channels that are live (the Favorites screen, and a Home row).
    FavoritesLive,
    /// Starred channels that are not live, dimmed, after the live ones (the Favorites screen).
    FavoritesOffline,
    /// Home: the last multiview session's channels, live (streams) or not (channels), in their
    /// saved order. The provider sends it with Home's rows; the reducer turns it into the
    /// *Resume multiview* card at the start of the first row, and it is never drawn as a row
    /// itself, except to hold that card when Home has no other row of streams.
    Resume,
    /// A channel page: its live stream, one card, only while live (vod-plan.md section 2).
    LiveNow,
    /// A channel page: past broadcasts, newest first.
    PastBroadcasts,
    /// A channel page: highlights, newest first.
    Highlights,
    /// A channel page's Clips tab: the most watched clips of the last 7 days.
    ClipsWeek,
    /// A channel page's Clips tab: the most watched clips ever.
    ClipsAllTime,
}

impl RowKind {
    /// Rows of a channel page's Clips tab. The others belong to its Videos tab.
    pub fn is_clips(self) -> bool {
        matches!(self, RowKind::ClipsWeek | RowKind::ClipsAllTime)
    }
}

/// How a row continues past the tiles it already has.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub enum Paging {
    /// No more pages, or the source cannot give any (anonymous GQL).
    #[default]
    End,
    /// Another page exists. The cursor goes into the next request.
    More(String),
    /// The page after this cursor is being fetched.
    Loading(String),
    /// Fetching the page after this cursor failed. Retried on demand, never automatically, so a
    /// dead connection cannot cause a request storm.
    Failed(String),
}

/// Which row of a feed a page belongs to: its kind, and the platform its cursor is for. Home can
/// have a Top Live row per platform, each paging on its own.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct RowId {
    pub kind: RowKind,
    pub platform: Platform,
}

impl RowId {
    pub fn twitch(kind: RowKind) -> Self {
        Self {
            kind,
            platform: Platform::Twitch,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Row {
    pub kind: RowKind,
    /// The platform the row's tiles come from and its cursor is for. A row that mixes platforms
    /// (search results) names the one that pages it.
    pub platform: Platform,
    pub tiles: Vec<Tile>,
    pub paging: Paging,
}

impl Row {
    /// A Twitch row with no further pages.
    pub fn new(kind: RowKind, tiles: Vec<Tile>) -> Self {
        Self {
            kind,
            platform: Platform::Twitch,
            tiles,
            paging: Paging::End,
        }
    }

    pub fn with_paging(mut self, paging: Paging) -> Self {
        self.paging = paging;
        self
    }

    pub fn on(mut self, platform: Platform) -> Self {
        self.platform = platform;
        self
    }

    pub fn id(&self) -> RowId {
        RowId {
            kind: self.kind,
            platform: self.platform,
        }
    }
}

/// Tiles per grid row. Landscape stream cards (360 px slots) fit three across the 1280 px
/// baseline next to the rail, portrait category cards (220 px slots) fit five, so grids scroll
/// only vertically.
pub fn grid_cols(kind: RowKind) -> usize {
    match kind {
        RowKind::Categories => 5,
        _ => 3,
    }
}

/// Splits `tiles` into grid rows (see [`grid_cols`]). The last row carries `paging`, because on a
/// grid "the end of the list" is the end of the last row.
pub fn grid_rows(kind: RowKind, tiles: Vec<Tile>, paging: Paging) -> Vec<Row> {
    grid_rows_on(Platform::Twitch, kind, tiles, paging)
}

/// [`grid_rows`] for a grid of one platform.
pub fn grid_rows_on(
    platform: Platform,
    kind: RowKind,
    tiles: Vec<Tile>,
    paging: Paging,
) -> Vec<Row> {
    let mut rows: Vec<Row> = tiles
        .chunks(grid_cols(kind))
        .map(|chunk| Row::new(kind, chunk.to_vec()).on(platform))
        .collect();
    if let Some(last) = rows.last_mut() {
        last.paging = paging;
    }
    rows
}

/// Which list of rows an event or command belongs to.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum FeedId {
    /// Horizontal rows: hero, followed, top live, categories.
    Home,
    /// Followed channels: live ones first, then the offline ones (horizontal rows). Signed in only.
    Following,
    /// The category directory reached from the rail (a grid).
    Browse,
    /// The streams of the category with this Twitch game id (a grid).
    Category(String),
    /// The results for this search text (horizontal rows: live, categories, offline).
    Search(String),
    /// LiveDeck's own starred channels, Kick and Twitch: live ones first, then the offline ones
    /// (horizontal rows). Signed in only.
    Favorites,
}

impl FeedId {
    /// Grid feeds scroll vertically through rows of [`grid_cols`] and page on their last row.
    pub fn is_grid(&self) -> bool {
        !matches!(
            self,
            FeedId::Home | FeedId::Following | FeedId::Search(_) | FeedId::Favorites
        )
    }
}

/// One piece of a chat line, in order.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ChatSpan {
    Text(String),
    /// An emote: the text it stands for (also its alt text) and where its image is.
    Emote {
        name: String,
        url: String,
    },
}

/// How a chat line is drawn.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum ChatKind {
    /// A chatter's message: `name: text`.
    #[default]
    Message,
    /// A `/me` message: the whole line in the chatter's colour, no colon.
    Action,
    /// A line from Twitch itself (slow mode, banned words, ...), without a chatter.
    Notice,
}

#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct ChatMessage {
    /// Twitch's id of the message, which a moderator's delete refers to. Empty for notices.
    pub id: String,
    /// Login of the chatter, which a timeout or ban refers to. Empty for notices.
    pub login: String,
    /// The name as the chatter writes it.
    pub user: String,
    /// Name colour chosen by the user, if any.
    pub color: Option<Rgb>,
    /// Image URLs of the chatter's badges, in display order. A badge Twitch has no image for is
    /// left out.
    pub badges: Vec<String>,
    pub spans: Vec<ChatSpan>,
    pub kind: ChatKind,
}

impl ChatMessage {
    /// A plain message of `text` from `user`, with no id, badges or emotes.
    pub fn plain(user: &str, text: &str) -> Self {
        Self {
            login: user.to_ascii_lowercase(),
            user: user.to_owned(),
            spans: vec![ChatSpan::Text(text.to_owned())],
            ..Self::default()
        }
    }

    /// A line from Twitch itself.
    pub fn notice(text: &str) -> Self {
        Self {
            spans: vec![ChatSpan::Text(text.to_owned())],
            kind: ChatKind::Notice,
            ..Self::default()
        }
    }

    /// The text of the line with emotes written as their names.
    pub fn text(&self) -> String {
        let mut out = String::new();
        for span in &self.spans {
            if !out.is_empty() {
                out.push(' ');
            }
            match span {
                ChatSpan::Text(t) => out.push_str(t),
                ChatSpan::Emote { name, .. } => out.push_str(name),
            }
        }
        out
    }

    /// The URLs of every image the line needs: badges, then emotes.
    pub fn image_urls(&self) -> impl Iterator<Item = &str> {
        let emotes = self.spans.iter().filter_map(|s| match s {
            ChatSpan::Emote { url, .. } => Some(url.as_str()),
            ChatSpan::Text(_) => None,
        });
        self.badges.iter().map(String::as_str).chain(emotes)
    }
}

/// What a moderator did, as far as the lines on screen are concerned.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Moderation {
    /// The whole chat was cleared.
    ClearAll,
    /// A user was timed out or banned: their lines go (the login of the user).
    ClearUser(String),
    /// One line was deleted (the Twitch message id).
    Delete(String),
}

/// How the connection to the watched channel's chat is doing.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum ChatStatus {
    /// No chat is open.
    #[default]
    Idle,
    /// Connecting for the first time.
    Connecting,
    Live,
    /// The connection dropped and is being set up again.
    Reconnecting,
}

/// The signed-in Twitch user.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct User {
    pub login: String,
    /// The token may write in chat (`user:write:chat`). Tokens from before chat sending lack it,
    /// and the user has to sign in again for it.
    pub can_chat: bool,
}

/// A hype train running in the channel on screen, as of the last poll.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct HypeTrain {
    /// The level being worked on, from 1.
    pub level: u32,
    /// Points gathered toward the next level, counted from the start of this one.
    pub progress: u32,
    /// Points this level needs. At least 1.
    pub goal: u32,
    /// Seconds before it ends at the time of the poll, unless the next level comes first. The
    /// window counts down from it between polls.
    pub seconds_left: u32,
}

/// Who may write in the chat on screen, as the chat server last said (`ROOMSTATE`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct RoomMode {
    /// Seconds a chatter waits between messages. 0 is off.
    pub slow_secs: u32,
    /// Followers only, for those who have followed at least this many minutes (0: any follower).
    /// `None` is off.
    pub followers_min: Option<u32>,
    pub subs_only: bool,
    pub emote_only: bool,
}

/// A `ROOMSTATE` from the chat server. Joining sends every setting, later ones only what
/// changed: `None` leaves a setting as it was.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct RoomModeChange {
    pub slow_secs: Option<u32>,
    /// `Some(None)` turns followers-only off.
    pub followers_min: Option<Option<u32>>,
    pub subs_only: Option<bool>,
    pub emote_only: Option<bool>,
}

impl RoomMode {
    pub fn apply(&mut self, change: RoomModeChange) {
        if let Some(v) = change.slow_secs {
            self.slow_secs = v;
        }
        if let Some(v) = change.followers_min {
            self.followers_min = v;
        }
        if let Some(v) = change.subs_only {
            self.subs_only = v;
        }
        if let Some(v) = change.emote_only {
            self.emote_only = v;
        }
    }
}

/// Why a chat message was not posted.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SendProblem {
    /// Twitch refused it: its code (`msg_slowmode`, `msg_followersonly`, ...) and its sentence.
    Dropped { code: String, message: String },
    /// The app's own limit: too many messages in a short time.
    TooFast,
    /// The request failed (no network, Twitch error), with what went wrong.
    Failed(String),
}

/// Why a device-code login ended without signing in. Shown on the sign-in screen.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LoginProblem {
    /// The code ran out before it was approved.
    Expired,
    /// The user refused it on their phone.
    Denied,
    /// Anything else (no network, Twitch refused the client ID, ...), with a message to show.
    Failed(String),
}

/// What the player reports about the stream it plays, about once a second while the stats box
/// is on. Every value is `None` when the player does not know it (yet). Whole numbers in small
/// units keep the state `Eq`.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct PlayerStats {
    /// Decoded picture size in pixels.
    pub width: Option<u32>,
    pub height: Option<u32>,
    /// Frames per second, times 1000 (`59_940` is 59.94 fps).
    pub fps_milli: Option<u32>,
    /// Video codec, as the player names it ("h264").
    pub codec: Option<String>,
    /// The hardware decoder in use, `None` when decoding on the CPU.
    pub hwdec: Option<String>,
    /// Bits per second.
    pub video_bitrate: Option<u64>,
    pub audio_bitrate: Option<u64>,
    /// Audio codec, as the player names it ("aac"). Shown instead of the video lines in audio
    /// only mode.
    pub audio_codec: Option<String>,
    /// How much is buffered ahead, in milliseconds.
    pub buffer_ms: Option<u32>,
    /// Frames dropped on the way to the screen, and by the decoder, since the stream started.
    pub dropped_frames: Option<u64>,
    pub decoder_dropped_frames: Option<u64>,
}

/// Device Code Grant details shown on the sign-in screen.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DeviceCode {
    pub user_code: String,
    pub verification_uri: String,
}
