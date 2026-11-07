//! What the app gets from Kick, already flattened. The wire shapes (`wire`) stay private, so a
//! change on Kick's side is mapped here and nowhere else.

use serde::Deserialize;

/// A live stream.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LiveStream {
    /// The channel's slug: its address (`kick.com/<slug>`) and what Streamlink is given.
    pub slug: String,
    /// The name as the streamer writes it.
    pub name: String,
    /// Kick's channel id (the realtime endpoints take it).
    pub channel_id: u64,
    pub title: String,
    /// The first category, empty if none.
    pub category: String,
    pub viewers: u64,
    /// Preview image URL, if Kick gave one.
    pub thumbnail: Option<String>,
}

/// One page of the top live streams.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LivestreamPage {
    pub streams: Vec<LiveStream>,
    /// The page to ask for next, `None` on the last one.
    pub next_page: Option<u32>,
}

/// A channel, live or not.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ChannelInfo {
    pub id: u64,
    pub slug: String,
    pub name: String,
    /// The chat room's id (`chatrooms.<id>.v2` on Pusher).
    pub chatroom_id: u64,
    /// Its stream, while it is live.
    pub live: Option<LiveStream>,
}

/// A channel found by name.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SearchHit {
    pub slug: String,
    pub name: String,
    pub is_live: bool,
}

/// The Pusher application chat is read from.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PusherApp {
    pub key: String,
    pub cluster: String,
}

/// The JSON as Kick sends it. Only the fields used are named; the rest is ignored.
pub(crate) mod wire {
    use super::Deserialize;

    #[derive(Deserialize)]
    pub struct LivestreamsPage {
        #[serde(default)]
        pub data: Vec<ListedStream>,
        #[serde(default)]
        pub current_page: Option<u32>,
        #[serde(default)]
        pub next_page_url: Option<String>,
    }

    #[derive(Deserialize)]
    pub struct ListedStream {
        #[serde(default)]
        pub session_title: String,
        #[serde(default)]
        pub viewer_count: u64,
        #[serde(default)]
        pub thumbnail: Option<Thumbnail>,
        #[serde(default)]
        pub categories: Vec<Category>,
        pub channel: ChannelBrief,
    }

    #[derive(Deserialize)]
    pub struct Thumbnail {
        /// The list answer names it `src`, a channel's livestream `url`.
        #[serde(alias = "url")]
        pub src: Option<String>,
    }

    #[derive(Deserialize)]
    pub struct Category {
        #[serde(default)]
        pub name: String,
    }

    #[derive(Deserialize)]
    pub struct ChannelBrief {
        pub id: u64,
        pub slug: String,
        #[serde(default)]
        pub user: Option<User>,
    }

    #[derive(Deserialize)]
    pub struct User {
        #[serde(default)]
        pub username: String,
    }

    #[derive(Deserialize)]
    pub struct Channel {
        pub id: u64,
        pub slug: String,
        #[serde(default)]
        pub user: Option<User>,
        pub chatroom: Chatroom,
        #[serde(default)]
        pub livestream: Option<ChannelStream>,
    }

    #[derive(Deserialize)]
    pub struct Chatroom {
        pub id: u64,
    }

    #[derive(Deserialize)]
    pub struct ChannelStream {
        #[serde(default)]
        pub session_title: String,
        #[serde(default)]
        pub is_live: bool,
        #[serde(default)]
        pub viewer_count: u64,
        #[serde(default)]
        pub thumbnail: Option<Thumbnail>,
        #[serde(default)]
        pub categories: Vec<Category>,
    }

    #[derive(Deserialize)]
    pub struct Search {
        #[serde(default)]
        pub channels: Vec<SearchChannel>,
    }

    #[derive(Deserialize)]
    pub struct SearchChannel {
        pub slug: String,
        #[serde(default)]
        pub user: Option<User>,
        #[serde(default, rename = "isLive", alias = "is_live")]
        pub is_live: bool,
    }

    #[derive(Deserialize)]
    pub struct Realtime {
        pub data: RealtimeData,
    }

    #[derive(Deserialize)]
    pub struct RealtimeData {
        #[serde(default)]
        pub connections: Vec<Connection>,
    }

    #[derive(Deserialize)]
    pub struct Connection {
        pub provider: String,
        #[serde(default)]
        pub credentials: Credentials,
    }

    #[derive(Deserialize, Default)]
    pub struct Credentials {
        #[serde(default)]
        pub app_key: Option<String>,
        #[serde(default)]
        pub cluster: Option<String>,
    }
}

fn name_of(user: Option<wire::User>, slug: &str) -> String {
    user.map(|u| u.username)
        .filter(|n| !n.is_empty())
        .unwrap_or_else(|| slug.to_owned())
}

fn first_category(categories: &[wire::Category]) -> String {
    categories
        .first()
        .map(|c| c.name.clone())
        .unwrap_or_default()
}

impl From<wire::ListedStream> for LiveStream {
    fn from(s: wire::ListedStream) -> Self {
        let category = first_category(&s.categories);
        Self {
            name: name_of(s.channel.user, &s.channel.slug),
            slug: s.channel.slug,
            channel_id: s.channel.id,
            title: s.session_title,
            category,
            viewers: s.viewer_count,
            thumbnail: s.thumbnail.and_then(|t| t.src),
        }
    }
}

impl From<wire::Channel> for ChannelInfo {
    fn from(c: wire::Channel) -> Self {
        let name = name_of(c.user, &c.slug);
        let live = c.livestream.filter(|l| l.is_live).map(|l| LiveStream {
            slug: c.slug.clone(),
            name: name.clone(),
            channel_id: c.id,
            category: first_category(&l.categories),
            title: l.session_title,
            viewers: l.viewer_count,
            thumbnail: l.thumbnail.and_then(|t| t.src),
        });
        Self {
            id: c.id,
            slug: c.slug,
            name,
            chatroom_id: c.chatroom.id,
            live,
        }
    }
}

impl From<wire::SearchChannel> for SearchHit {
    fn from(c: wire::SearchChannel) -> Self {
        Self {
            name: name_of(c.user, &c.slug),
            slug: c.slug,
            is_live: c.is_live,
        }
    }
}
