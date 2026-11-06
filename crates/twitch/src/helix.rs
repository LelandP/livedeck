//! `HelixClient`: the real [`TwitchApi`] over HTTPS.

use std::sync::RwLock;

use reqwest::{Client, StatusCode};
use serde::de::DeserializeOwned;
use serde::Deserialize;

use crate::http::{build_client, error_message, is_retryable, map_status};
use crate::{
    ChannelResult, Error, FollowedChannel, Game, Page, PageRequest, RateLimiter, Result,
    RetryPolicy, SendOutcome, StreamInfo, TwitchApi, UserInfo, MAX_LOGINS, MAX_PAGE_SIZE,
};

pub const DEFAULT_BASE_URL: &str = "https://api.twitch.tv/helix";

/// Public client ID plus an access token. Helix needs both on every request.
#[derive(Clone, PartialEq, Eq)]
pub struct Credentials {
    pub client_id: String,
    /// User or app access token, without the `Bearer ` prefix.
    pub access_token: String,
}

impl std::fmt::Debug for Credentials {
    // Never log the token.
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Credentials")
            .field("client_id", &self.client_id)
            .field("access_token", &"<redacted>")
            .finish()
    }
}

/// Helix list envelope: `{ "data": [...], "pagination": { "cursor": "..." } }`.
#[derive(Deserialize)]
struct Envelope<T> {
    data: Vec<T>,
    #[serde(default)]
    pagination: Pagination,
}

#[derive(Deserialize, Default)]
struct Pagination {
    cursor: Option<String>,
}

#[derive(Debug)]
pub struct HelixClient {
    http: Client,
    base_url: String,
    credentials: RwLock<Option<Credentials>>,
    limiter: RateLimiter,
}

impl HelixClient {
    /// A client for the real Helix API.
    pub fn new(credentials: Option<Credentials>) -> Result<Self> {
        Self::with_base_url(DEFAULT_BASE_URL, credentials, RetryPolicy::default())
    }

    /// A client for any Helix-compatible base URL (tests point this at `wiremock`).
    pub fn with_base_url(
        base_url: impl Into<String>,
        credentials: Option<Credentials>,
        policy: RetryPolicy,
    ) -> Result<Self> {
        let base_url = base_url.into().trim_end_matches('/').to_owned();
        let http: Client = build_client(&base_url)?;
        Ok(Self {
            http,
            base_url,
            credentials: RwLock::new(credentials),
            limiter: RateLimiter::new(policy),
        })
    }

    /// Replaces the credentials, e.g. after login or a token refresh.
    pub fn set_credentials(&self, credentials: Option<Credentials>) {
        *self.credentials.write().unwrap_or_else(|e| e.into_inner()) = credentials;
    }

    pub fn has_credentials(&self) -> bool {
        self.credentials().is_some()
    }

    pub fn rate_limiter(&self) -> &RateLimiter {
        &self.limiter
    }

    fn credentials(&self) -> Option<Credentials> {
        self.credentials
            .read()
            .unwrap_or_else(|e| e.into_inner())
            .clone()
    }

    /// GET `path` with `query`, retrying 429 and 5xx with backoff. Returns the decoded envelope.
    async fn get<T: DeserializeOwned>(
        &self,
        path: &str,
        query: &[(&str, String)],
    ) -> Result<Envelope<T>> {
        let creds = self.credentials().ok_or(Error::NotSignedIn)?;
        let url = format!("{}{}", self.base_url, path);
        let policy = self.limiter.policy();
        let mut attempt = 0;
        loop {
            let wait = self.limiter.wait_before_request();
            if !wait.is_zero() {
                tracing::debug!(?wait, "Helix bucket empty, waiting for reset");
                tokio::time::sleep(wait).await;
            }

            let resp = self
                .http
                .get(&url)
                .query(query)
                .header("Client-Id", &creds.client_id)
                .bearer_auth(&creds.access_token)
                .send()
                .await
                .map_err(Error::Network)?;
            self.limiter.observe(resp.headers());
            let status = resp.status();

            if status.is_success() {
                let body = resp.bytes().await.map_err(Error::Network)?;
                return serde_json::from_slice(&body).map_err(|e| Error::Decode(e.to_string()));
            }

            let message = error_message(resp).await;
            if is_retryable(status) && attempt < policy.max_retries {
                let wait = self
                    .limiter
                    .backoff(attempt, status == StatusCode::TOO_MANY_REQUESTS);
                tracing::warn!(%status, attempt, ?wait, %path, "Helix request failed, retrying");
                tokio::time::sleep(wait).await;
                attempt += 1;
                continue;
            }
            return Err(map_status(status, message));
        }
    }

    /// POST `body` as JSON to `path` and decode the envelope. Never retried: a request that
    /// timed out or hit a server error may still have done its work (a chat message posted
    /// twice is worse than one that failed). Only the bucket wait applies.
    async fn post<T: DeserializeOwned>(
        &self,
        path: &str,
        body: &serde_json::Value,
    ) -> Result<Envelope<T>> {
        let creds = self.credentials().ok_or(Error::NotSignedIn)?;
        let url = format!("{}{}", self.base_url, path);
        let wait = self.limiter.wait_before_request();
        if !wait.is_zero() {
            tracing::debug!(?wait, "Helix bucket empty, waiting for reset");
            tokio::time::sleep(wait).await;
        }
        let resp = self
            .http
            .post(&url)
            .header("Client-Id", &creds.client_id)
            .bearer_auth(&creds.access_token)
            .json(body)
            .send()
            .await
            .map_err(Error::Network)?;
        self.limiter.observe(resp.headers());
        let status = resp.status();
        if status.is_success() {
            let body = resp.bytes().await.map_err(Error::Network)?;
            return serde_json::from_slice(&body).map_err(|e| Error::Decode(e.to_string()));
        }
        let message = error_message(resp).await;
        Err(map_status(status, message))
    }

    async fn get_page<T: DeserializeOwned>(
        &self,
        path: &str,
        mut query: Vec<(&str, String)>,
        page: &PageRequest,
    ) -> Result<Page<T>> {
        query.push(("first", page.first.to_string()));
        if let Some(after) = &page.after {
            query.push(("after", after.clone()));
        }
        let env = self.get::<T>(path, &query).await?;
        Ok(Page {
            items: env.data,
            // Helix sends `{}` or an empty cursor on the last page.
            cursor: env.pagination.cursor.filter(|c| !c.is_empty()),
        })
    }
}

impl TwitchApi for HelixClient {
    async fn top_streams(&self, page: PageRequest) -> Result<Page<StreamInfo>> {
        let query = vec![("type", "live".to_owned())];
        self.get_page("/streams", query, &page).await
    }

    async fn followed_streams(&self, user_id: &str, page: PageRequest) -> Result<Page<StreamInfo>> {
        let query = vec![("user_id", user_id.to_owned())];
        self.get_page("/streams/followed", query, &page).await
    }

    async fn followed_channels(
        &self,
        user_id: &str,
        page: PageRequest,
    ) -> Result<Page<FollowedChannel>> {
        let query = vec![("user_id", user_id.to_owned())];
        self.get_page("/channels/followed", query, &page).await
    }

    async fn top_categories(&self, page: PageRequest) -> Result<Page<Game>> {
        self.get_page("/games/top", Vec::new(), &page).await
    }

    async fn streams_by_category(
        &self,
        game_id: &str,
        page: PageRequest,
    ) -> Result<Page<StreamInfo>> {
        let query = vec![("type", "live".to_owned()), ("game_id", game_id.to_owned())];
        self.get_page("/streams", query, &page).await
    }

    async fn user(&self, login: Option<&str>) -> Result<Option<UserInfo>> {
        let query: Vec<_> = login.map(|l| ("login", l.to_owned())).into_iter().collect();
        let env = self.get::<UserInfo>("/users", &query).await?;
        Ok(env.data.into_iter().next())
    }

    async fn search_channels(
        &self,
        query: &str,
        live_only: bool,
        page: PageRequest,
    ) -> Result<Page<ChannelResult>> {
        let params = vec![
            ("query", query.to_owned()),
            ("live_only", live_only.to_string()),
        ];
        self.get_page("/search/channels", params, &page).await
    }

    async fn search_categories(&self, query: &str, page: PageRequest) -> Result<Page<Game>> {
        let params = vec![("query", query.to_owned())];
        let mut found: Page<Game> = self.get_page("/search/categories", params, &page).await?;
        // Unlike `/games/top`, search hands out box art at a fixed small size.
        for game in &mut found.items {
            game.box_art_url = box_art_template(&game.box_art_url);
        }
        Ok(found)
    }

    async fn streams_by_login(&self, logins: &[String]) -> Result<Vec<StreamInfo>> {
        if logins.is_empty() {
            return Ok(Vec::new());
        }
        let mut query: Vec<(&str, String)> = logins
            .iter()
            .take(MAX_LOGINS)
            .map(|l| ("user_login", l.clone()))
            .collect();
        query.push(("first", MAX_PAGE_SIZE.to_string()));
        Ok(self.get::<StreamInfo>("/streams", &query).await?.data)
    }

    async fn send_chat(
        &self,
        broadcaster_id: &str,
        sender_id: &str,
        text: &str,
    ) -> Result<SendOutcome> {
        let body = serde_json::json!({
            "broadcaster_id": broadcaster_id,
            "sender_id": sender_id,
            "message": text,
        });
        let env = self.post::<SendOutcome>("/chat/messages", &body).await?;
        env.data
            .into_iter()
            .next()
            .ok_or_else(|| Error::Decode("no answer for the chat message".to_owned()))
    }
}

/// Turns a sized box-art URL (`.../33214-52x72.jpg`) back into a `{width}x{height}` template, so
/// search results can be sized like the directory's. Anything else passes through.
fn box_art_template(url: &str) -> String {
    let Some((stem, ext)) = url.rsplit_once('.') else {
        return url.to_owned();
    };
    let Some((base, size)) = stem.rsplit_once('-') else {
        return url.to_owned();
    };
    let sized = size.split_once('x').is_some_and(|(w, h)| {
        !w.is_empty()
            && !h.is_empty()
            && w.bytes().all(|b| b.is_ascii_digit())
            && h.bytes().all(|b| b.is_ascii_digit())
    });
    if sized && !ext.contains('/') {
        format!("{base}-{{width}}x{{height}}.{ext}")
    } else {
        url.to_owned()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn search_box_art_becomes_a_template() {
        assert_eq!(
            box_art_template("https://cdn/ttv-boxart/33214-52x72.jpg"),
            "https://cdn/ttv-boxart/33214-{width}x{height}.jpg"
        );
        // Already a template, or no size in it: unchanged.
        let t = "https://cdn/ttv-boxart/1-{width}x{height}.jpg";
        assert_eq!(box_art_template(t), t);
        assert_eq!(
            box_art_template("https://cdn/art.jpg"),
            "https://cdn/art.jpg"
        );
        assert_eq!(box_art_template(""), "");
    }

    #[test]
    fn credentials_debug_hides_the_token() {
        let c = Credentials {
            client_id: "id".into(),
            access_token: "secret".into(),
        };
        assert!(!format!("{c:?}").contains("secret"));
    }
}
