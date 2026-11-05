//! `HelixClient`: the real [`TwitchApi`] over HTTPS.

use std::sync::RwLock;

use reqwest::{Client, StatusCode};
use serde::de::DeserializeOwned;
use serde::Deserialize;

use crate::http::{build_client, error_message, is_retryable, map_status};
use crate::{
    Error, FollowedChannel, Game, Page, PageRequest, RateLimiter, Result, RetryPolicy, StreamInfo,
    TwitchApi, UserInfo,
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
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn credentials_debug_hides_the_token() {
        let c = Credentials {
            client_id: "id".into(),
            access_token: "secret".into(),
        };
        assert!(!format!("{c:?}").contains("secret"));
    }
}
