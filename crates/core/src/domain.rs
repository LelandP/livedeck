//! Domain types. These are UI-agnostic: no Slint types, no pre-formatted strings.

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

/// A live stream.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Stream {
    /// Channel login, used to join chat and to resolve the playlist.
    pub channel: String,
    /// The broadcaster's Twitch user id, which EventSub conditions (raids) use. Empty when the
    /// source did not say (a card made from a login only); `Command::FetchStreamInfo` fills it.
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
    /// Channel login, same as `Stream::channel`. Selecting the card tries to play it.
    pub channel: String,
    /// The name as the streamer writes it.
    pub name: String,
    pub tint: Rgb,
}

/// One card in a row.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Tile {
    Stream(Stream),
    Category(Category),
    /// A followed channel that is offline.
    Channel(Channel),
}

impl Tile {
    /// The image to show on the card, if the source gave one.
    pub fn thumbnail(&self) -> Option<&str> {
        match self {
            Tile::Stream(s) => s.thumbnail.as_deref(),
            Tile::Category(c) => c.thumbnail.as_deref(),
            Tile::Channel(_) => None,
        }
    }

    /// Identity within a row: the channel login or the game id. Used to drop duplicates when a
    /// later page repeats an item (viewer counts shift between requests). Streams and channels
    /// share the login space, so a channel that went live is not shown twice on Following.
    pub fn key(&self) -> &str {
        match self {
            Tile::Stream(s) => &s.channel,
            Tile::Category(c) => &c.id,
            Tile::Channel(c) => &c.channel,
        }
    }
}

/// What a row shows. The view-model maps this to a (later translatable) title.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
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

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Row {
    pub kind: RowKind,
    pub tiles: Vec<Tile>,
    pub paging: Paging,
}

impl Row {
    /// A row with no further pages.
    pub fn new(kind: RowKind, tiles: Vec<Tile>) -> Self {
        Self {
            kind,
            tiles,
            paging: Paging::End,
        }
    }

    pub fn with_paging(mut self, paging: Paging) -> Self {
        self.paging = paging;
        self
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
    let mut rows: Vec<Row> = tiles
        .chunks(grid_cols(kind))
        .map(|chunk| Row::new(kind, chunk.to_vec()))
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
}

impl FeedId {
    /// Grid feeds scroll vertically through rows of [`grid_cols`] and page on their last row.
    pub fn is_grid(&self) -> bool {
        !matches!(self, FeedId::Home | FeedId::Following | FeedId::Search(_))
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
