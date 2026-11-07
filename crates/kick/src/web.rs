//! `KickWebClient`: the website's own (unofficial) API. No token, a browser-like User-Agent, and
//! nothing that tries to get around Kick's bot protection: a 403 is reported as
//! [`Error::Blocked`], and the app shows Kick rows as failed.

use std::time::{Duration, SystemTime, UNIX_EPOCH};

use reqwest::StatusCode;
use serde::de::DeserializeOwned;

use crate::model::{wire, ChannelInfo, LivestreamPage, PusherApp, SearchHit};
use crate::{Error, KickApi, Result, PUSHER_CLUSTER, PUSHER_KEY};

/// Where the website's API lives.
pub const DEFAULT_SITE_URL: &str = "https://kick.com";
/// Where the website asks which realtime service to read chat from.
pub const DEFAULT_REALTIME_URL: &str = "https://web.kick.com/api";

/// Streams per page of Top Live, the same as the website asks for.
pub const PAGE_SIZE: u32 = 24;

/// Says what is asking. Kick's bot protection refuses this client posing as a browser.
const USER_AGENT: &str = concat!("LiveDeck/", env!("CARGO_PKG_VERSION"));
const TIMEOUT: Duration = Duration::from_secs(15);

#[derive(Clone)]
pub struct KickWebClient {
    http: reqwest::Client,
    site: String,
    realtime: String,
    /// Sent with the chat-connection request, as the website sends its own client id.
    client_id: String,
}

impl KickWebClient {
    pub fn new() -> Result<Self> {
        Self::with_base_urls(DEFAULT_SITE_URL, DEFAULT_REALTIME_URL)
    }

    /// Talks to other servers (tests).
    pub fn with_base_urls(site: impl Into<String>, realtime: impl Into<String>) -> Result<Self> {
        let http = reqwest::Client::builder()
            .user_agent(USER_AGENT)
            .timeout(TIMEOUT)
            .build()
            .map_err(|e| Error::Network(e.to_string()))?;
        let nanos = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map_or(0, |d| d.as_nanos());
        Ok(Self {
            http,
            site: site.into().trim_end_matches('/').to_owned(),
            realtime: realtime.into().trim_end_matches('/').to_owned(),
            client_id: format!("livedeck-{nanos:x}"),
        })
    }

    /// GETs `url` and reads its JSON. `Ok(None)` for a 404.
    async fn get<T: DeserializeOwned>(&self, url: &str) -> Result<Option<T>> {
        let response = self
            .http
            .get(url)
            .header("Accept", "application/json")
            .send()
            .await
            .map_err(|e| Error::Network(e.to_string()))?;
        read(response).await
    }
}

async fn read<T: DeserializeOwned>(response: reqwest::Response) -> Result<Option<T>> {
    match response.status() {
        StatusCode::NOT_FOUND => return Ok(None),
        StatusCode::FORBIDDEN => return Err(Error::Blocked),
        StatusCode::TOO_MANY_REQUESTS => return Err(Error::RateLimited),
        s if !s.is_success() => return Err(Error::Status(s.as_u16())),
        _ => {}
    }
    let bytes = response
        .bytes()
        .await
        .map_err(|e| Error::Network(e.to_string()))?;
    serde_json::from_slice(&bytes)
        .map(Some)
        .map_err(|e| Error::Decode(e.to_string()))
}

/// Kick channel slugs: letters, digits, `_` and `-`. Anything else is not looked up.
fn valid_slug(slug: &str) -> bool {
    !slug.is_empty()
        && slug.len() <= 50
        && slug
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-')
}

impl KickApi for KickWebClient {
    async fn top_livestreams(&self, page: u32) -> Result<LivestreamPage> {
        let page = page.max(1);
        // `/en` is only the page's language: the list holds every language, like Twitch's.
        let url = format!(
            "{}/stream/livestreams/en?page={page}&limit={PAGE_SIZE}&sort=desc",
            self.site
        );
        let answer: wire::LivestreamsPage = self.get(&url).await?.ok_or(Error::Status(404))?;
        let current = answer.current_page.unwrap_or(page);
        let streams: Vec<_> = answer.data.into_iter().map(Into::into).collect();
        let next_page =
            (answer.next_page_url.is_some() && !streams.is_empty()).then(|| current + 1);
        Ok(LivestreamPage { streams, next_page })
    }

    async fn channel(&self, slug: &str) -> Result<Option<ChannelInfo>> {
        if !valid_slug(slug) {
            return Ok(None);
        }
        let url = format!("{}/api/v2/channels/{slug}", self.site);
        Ok(self.get::<wire::Channel>(&url).await?.map(Into::into))
    }

    async fn search_channels(&self, query: &str) -> Result<Vec<SearchHit>> {
        let url = reqwest::Url::parse_with_params(
            &format!("{}/api/search", self.site),
            &[("searched_word", query)],
        )
        .map_err(|e| Error::Decode(e.to_string()))?;
        let found: Option<wire::Search> = self.get(url.as_str()).await?;
        Ok(found
            .map(|s| s.channels.into_iter().map(Into::into).collect())
            .unwrap_or_default())
    }

    async fn chat_app(&self, channel_id: u64) -> Result<PusherApp> {
        let url = format!(
            "{}/v1/realtime/channels/{channel_id}/chat/connection",
            self.realtime
        );
        // Pusher only: the website prefers Centrifugo, which LiveDeck does not speak.
        let body = serde_json::json!({
            "client": { "id": self.client_id, "type": "web" },
            "capabilities": { "accepted_providers": [{ "provider": "pusher" }] },
        });
        let response = self
            .http
            .post(&url)
            .header("Accept", "application/json")
            .json(&body)
            .send()
            .await
            .map_err(|e| Error::Network(e.to_string()))?;
        let answer: wire::Realtime = read(response).await?.ok_or(Error::Status(404))?;
        answer
            .data
            .connections
            .into_iter()
            .filter(|c| c.provider == "pusher")
            .find_map(|c| Some((c.credentials.app_key?, c.credentials.cluster?)))
            .map(|(key, cluster)| PusherApp { key, cluster })
            .ok_or_else(|| Error::Decode("no Pusher connection offered".into()))
    }
}

/// The Pusher app to use when Kick's answer cannot be had.
pub fn fallback_pusher_app() -> PusherApp {
    PusherApp {
        key: PUSHER_KEY.to_owned(),
        cluster: PUSHER_CLUSTER.to_owned(),
    }
}
