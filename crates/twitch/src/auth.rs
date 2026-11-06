//! OAuth Device Code Grant for a public client (no client secret), behind the mockable
//! [`AuthApi`] trait.
//!
//! - [`HttpAuth`]: the real client for `id.twitch.tv/oauth2` (device code, token, refresh,
//!   validate). Each method sends exactly one request.
//! - [`crate::FakeAuth`]: a scripted in-memory implementation for tests and `--mock`.
//!
//! The polling loop, token storage and refresh policy live in the app (`session.rs`). This module
//! only speaks the protocol.

use std::fmt;
use std::future::Future;
use std::time::Duration;

use reqwest::{Client, Response, StatusCode};
use serde::de::DeserializeOwned;
use serde::Deserialize;

use crate::http::{build_client, error_message, map_status};
use crate::{Error, Result};

pub const DEFAULT_AUTH_URL: &str = "https://id.twitch.tv/oauth2";
pub const DEVICE_GRANT_TYPE: &str = "urn:ietf:params:oauth:grant-type:device_code";
/// The scope behind both `/streams/followed` and `/channels/followed`.
pub const SCOPE_READ_FOLLOWS: &str = "user:read:follows";
/// The scope behind `POST /chat/messages` (writing in chat).
pub const SCOPE_WRITE_CHAT: &str = "user:write:chat";
/// Every scope the app asks for when the user signs in.
pub const SCOPES: &[&str] = &[SCOPE_READ_FOLLOWS, SCOPE_WRITE_CHAT];
/// Twitch's documented polling interval, used if the response has none.
const DEFAULT_INTERVAL_SECS: u64 = 5;

/// What `POST /device` returns: the code to show and what to poll with.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DeviceGrant {
    /// Secret half. Only ever sent back to the token endpoint.
    pub device_code: String,
    /// The code the user types in on their phone.
    pub user_code: String,
    /// Where to enter it. Twitch includes the code in the URL, so it also works as a QR target.
    pub verification_uri: String,
    /// How long the code stays valid.
    pub expires_in: Duration,
    /// Minimum time between polls.
    pub interval: Duration,
}

/// Tokens from the token endpoint. The secrets never appear in `Debug`.
#[derive(Clone, PartialEq, Eq)]
pub struct TokenGrant {
    pub access_token: String,
    pub refresh_token: Option<String>,
    /// Lifetime of the access token, if Twitch reported one.
    pub expires_in: Option<Duration>,
    pub scopes: Vec<String>,
}

impl fmt::Debug for TokenGrant {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("TokenGrant")
            .field("access_token", &"<redacted>")
            .field(
                "refresh_token",
                &self.refresh_token.as_ref().map(|_| "<redacted>"),
            )
            .field("expires_in", &self.expires_in)
            .field("scopes", &self.scopes)
            .finish()
    }
}

/// Who a token belongs to (`GET /validate`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Identity {
    pub user_id: String,
    pub login: String,
    pub scopes: Vec<String>,
}

/// The answer to one poll of the token endpoint.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PollOutcome {
    /// The user has not approved yet. Poll again after the interval.
    Pending,
    /// Poll less often (the standard OAuth `slow_down` answer).
    SlowDown,
    /// The code ran out, or was already used.
    Expired,
    /// The user refused.
    Denied,
    Granted(TokenGrant),
}

/// The OAuth endpoints the app uses. Every method sends one request.
pub trait AuthApi: Send + Sync + 'static {
    /// `POST /device`: starts a login.
    fn request_device_code(
        &self,
        scopes: &[&str],
    ) -> impl Future<Output = Result<DeviceGrant>> + Send;

    /// `POST /token` with the device-code grant type: one poll.
    fn poll_token(
        &self,
        device_code: &str,
        scopes: &[&str],
    ) -> impl Future<Output = Result<PollOutcome>> + Send;

    /// `POST /token` with the refresh grant. Refresh tokens are single use: the answer carries
    /// the next one. [`Error::AuthRejected`] means the refresh token is dead (sign in again).
    fn refresh(&self, refresh_token: &str) -> impl Future<Output = Result<TokenGrant>> + Send;

    /// `GET /validate`: whose token this is. [`Error::Unauthorized`] if it is invalid or expired.
    fn validate(&self, access_token: &str) -> impl Future<Output = Result<Identity>> + Send;

    /// `POST /revoke`: invalidates an access token (sign-out). Revoking a token that is already
    /// dead is not an error.
    fn revoke(&self, access_token: &str) -> impl Future<Output = Result<()>> + Send;
}

#[derive(Deserialize)]
struct DeviceBody {
    device_code: String,
    user_code: String,
    verification_uri: String,
    expires_in: u64,
    #[serde(default)]
    interval: Option<u64>,
}

/// `scope` is an array on Twitch, but OAuth allows a space-separated string, so take both.
#[derive(Deserialize)]
#[serde(untagged)]
enum ScopeList {
    Many(Vec<String>),
    One(String),
}

impl ScopeList {
    fn into_vec(self) -> Vec<String> {
        match self {
            ScopeList::Many(v) => v,
            ScopeList::One(s) => s.split_whitespace().map(str::to_owned).collect(),
        }
    }
}

#[derive(Deserialize)]
struct TokenBody {
    access_token: String,
    #[serde(default)]
    refresh_token: Option<String>,
    #[serde(default)]
    expires_in: Option<u64>,
    #[serde(default)]
    scope: Option<ScopeList>,
}

impl From<TokenBody> for TokenGrant {
    fn from(b: TokenBody) -> Self {
        Self {
            access_token: b.access_token,
            // An empty string means "no refresh token".
            refresh_token: b.refresh_token.filter(|t| !t.is_empty()),
            expires_in: b.expires_in.map(Duration::from_secs),
            scopes: b.scope.map(ScopeList::into_vec).unwrap_or_default(),
        }
    }
}

#[derive(Deserialize)]
struct ValidateBody {
    user_id: String,
    login: String,
    #[serde(default)]
    scopes: Vec<String>,
}

/// The real client for `id.twitch.tv/oauth2`.
#[derive(Debug)]
pub struct HttpAuth {
    http: Client,
    base_url: String,
    client_id: String,
}

impl HttpAuth {
    /// A client for the real Twitch OAuth server. `client_id` is the app's public client ID.
    pub fn new(client_id: impl Into<String>) -> Result<Self> {
        Self::with_base_url(DEFAULT_AUTH_URL, client_id)
    }

    /// Any OAuth-compatible base URL (tests point this at `wiremock`).
    pub fn with_base_url(
        base_url: impl Into<String>,
        client_id: impl Into<String>,
    ) -> Result<Self> {
        let base_url = base_url.into().trim_end_matches('/').to_owned();
        Ok(Self {
            http: build_client(&base_url)?,
            base_url,
            client_id: client_id.into(),
        })
    }

    async fn post(&self, path: &str, form: &[(&str, &str)]) -> Result<Response> {
        self.http
            .post(format!("{}{path}", self.base_url))
            .form(form)
            .send()
            .await
            .map_err(Error::Network)
    }
}

async fn decode<T: DeserializeOwned>(resp: Response) -> Result<T> {
    let body = resp.bytes().await.map_err(Error::Network)?;
    serde_json::from_slice(&body).map_err(|e| Error::Decode(e.to_string()))
}

/// Maps the 400 answers of a token poll. Twitch documents `authorization_pending` and
/// `invalid device code`. The standard OAuth names `slow_down`, `access_denied` and
/// `expired_token` are accepted too, so a server that uses them behaves.
fn classify_poll(message: &str) -> Option<PollOutcome> {
    let m = message.to_ascii_lowercase();
    if m.contains("authorization_pending") {
        Some(PollOutcome::Pending)
    } else if m.contains("slow_down") {
        Some(PollOutcome::SlowDown)
    } else if m.contains("access_denied") {
        Some(PollOutcome::Denied)
    } else if m.contains("invalid device code") || m.contains("expired") {
        Some(PollOutcome::Expired)
    } else {
        None
    }
}

impl AuthApi for HttpAuth {
    async fn request_device_code(&self, scopes: &[&str]) -> Result<DeviceGrant> {
        let scope = scopes.join(" ");
        let resp = self
            .post(
                "/device",
                &[
                    ("client_id", self.client_id.as_str()),
                    ("scopes", scope.as_str()),
                ],
            )
            .await?;
        let status = resp.status();
        if !status.is_success() {
            return Err(map_status(status, error_message(resp).await));
        }
        let body: DeviceBody = decode(resp).await?;
        Ok(DeviceGrant {
            device_code: body.device_code,
            user_code: body.user_code,
            verification_uri: body.verification_uri,
            expires_in: Duration::from_secs(body.expires_in),
            interval: Duration::from_secs(body.interval.unwrap_or(DEFAULT_INTERVAL_SECS)),
        })
    }

    async fn poll_token(&self, device_code: &str, scopes: &[&str]) -> Result<PollOutcome> {
        let scope = scopes.join(" ");
        let resp = self
            .post(
                "/token",
                &[
                    ("client_id", self.client_id.as_str()),
                    ("scopes", scope.as_str()),
                    ("device_code", device_code),
                    ("grant_type", DEVICE_GRANT_TYPE),
                ],
            )
            .await?;
        let status = resp.status();
        if status.is_success() {
            return Ok(PollOutcome::Granted(
                decode::<TokenBody>(resp).await?.into(),
            ));
        }
        let message = error_message(resp).await;
        if status == StatusCode::BAD_REQUEST {
            if let Some(outcome) = classify_poll(&message) {
                return Ok(outcome);
            }
        }
        Err(map_status(status, message))
    }

    async fn refresh(&self, refresh_token: &str) -> Result<TokenGrant> {
        let resp = self
            .post(
                "/token",
                &[
                    ("client_id", self.client_id.as_str()),
                    ("grant_type", "refresh_token"),
                    ("refresh_token", refresh_token),
                ],
            )
            .await?;
        let status = resp.status();
        if status.is_success() {
            return Ok(decode::<TokenBody>(resp).await?.into());
        }
        let message = error_message(resp).await;
        match status {
            // Twitch answers 400 "Invalid refresh token" for a used or revoked one.
            StatusCode::BAD_REQUEST | StatusCode::UNAUTHORIZED => Err(Error::AuthRejected(message)),
            _ => Err(map_status(status, message)),
        }
    }

    async fn validate(&self, access_token: &str) -> Result<Identity> {
        let resp = self
            .http
            .get(format!("{}/validate", self.base_url))
            .header("Authorization", format!("OAuth {access_token}"))
            .send()
            .await
            .map_err(Error::Network)?;
        let status = resp.status();
        if !status.is_success() {
            return Err(map_status(status, error_message(resp).await));
        }
        let body: ValidateBody = decode(resp).await?;
        Ok(Identity {
            user_id: body.user_id,
            login: body.login,
            scopes: body.scopes,
        })
    }

    async fn revoke(&self, access_token: &str) -> Result<()> {
        let resp = self
            .post(
                "/revoke",
                &[
                    ("client_id", self.client_id.as_str()),
                    ("token", access_token),
                ],
            )
            .await?;
        let status = resp.status();
        // Twitch answers 400 "Invalid token" for one that is already gone, which is the goal.
        if status.is_success() || status == StatusCode::BAD_REQUEST {
            return Ok(());
        }
        Err(map_status(status, error_message(resp).await))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn poll_messages_map_to_outcomes() {
        assert_eq!(
            classify_poll("authorization_pending"),
            Some(PollOutcome::Pending)
        );
        assert_eq!(classify_poll("slow_down"), Some(PollOutcome::SlowDown));
        assert_eq!(classify_poll("access_denied"), Some(PollOutcome::Denied));
        assert_eq!(
            classify_poll("invalid device code"),
            Some(PollOutcome::Expired)
        );
        assert_eq!(classify_poll("expired_token"), Some(PollOutcome::Expired));
        assert_eq!(classify_poll("invalid client"), None);
    }

    #[test]
    fn token_grant_debug_hides_the_secrets() {
        let grant = TokenGrant {
            access_token: "secret-a".into(),
            refresh_token: Some("secret-r".into()),
            expires_in: None,
            scopes: Vec::new(),
        };
        let text = format!("{grant:?}");
        assert!(!text.contains("secret-a") && !text.contains("secret-r"));
    }

    #[test]
    fn scopes_accept_an_array_or_a_string() {
        let many: TokenBody =
            serde_json::from_str(r#"{"access_token":"a","scope":["x","y"]}"#).unwrap();
        assert_eq!(TokenGrant::from(many).scopes, ["x", "y"]);
        let one: TokenBody = serde_json::from_str(r#"{"access_token":"a","scope":"x y"}"#).unwrap();
        assert_eq!(TokenGrant::from(one).scopes, ["x", "y"]);
        let none: TokenBody = serde_json::from_str(r#"{"access_token":"a"}"#).unwrap();
        assert!(TokenGrant::from(none).scopes.is_empty());
    }
}
