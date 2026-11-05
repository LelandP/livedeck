//! HTTP helpers shared by the Helix and GQL clients.

use std::time::Duration;

use reqwest::{Client, StatusCode};
use serde::Deserialize;

use crate::{Error, Result};

/// Per-request timeout. Browsing must never hang the store.
pub(crate) const REQUEST_TIMEOUT: Duration = Duration::from_secs(10);

/// Builds the `reqwest` client for `base_url`.
///
/// `HTTP(S)_PROXY` is honoured for the real APIs, but a proxy cannot reach a local test server
/// (and corporate proxies often lack a `NO_PROXY` entry for loopback), so loopback skips it.
pub(crate) fn build_client(base_url: &str) -> Result<Client> {
    let mut builder = Client::builder()
        .timeout(REQUEST_TIMEOUT)
        .user_agent(concat!("livedeck/", env!("CARGO_PKG_VERSION")));
    if is_loopback(base_url) {
        builder = builder.no_proxy();
    }
    builder.build().map_err(Error::Network)
}

/// True for `http(s)://localhost`, `127.x.x.x` and `[::1]` base URLs.
pub(crate) fn is_loopback(base_url: &str) -> bool {
    let rest = base_url.split_once("://").map_or(base_url, |(_, r)| r);
    let authority = rest.split('/').next().unwrap_or_default();
    let host = match authority.strip_prefix('[') {
        Some(v6) => v6.split(']').next().unwrap_or_default(),
        None => authority.split(':').next().unwrap_or_default(),
    };
    host.eq_ignore_ascii_case("localhost")
        || host
            .parse::<std::net::IpAddr>()
            .is_ok_and(|ip| ip.is_loopback())
}

/// Twitch error body: `{ "error": "Unauthorized", "status": 401, "message": "Invalid OAuth token" }`.
#[derive(Deserialize)]
struct ErrorBody {
    #[serde(default)]
    error: String,
    #[serde(default)]
    message: String,
}

/// Reads the error message of a failed response, falling back to the raw body.
pub(crate) async fn error_message(resp: reqwest::Response) -> String {
    let body = resp.text().await.unwrap_or_default();
    match serde_json::from_str::<ErrorBody>(&body) {
        Ok(e) if !e.message.is_empty() => e.message,
        Ok(e) if !e.error.is_empty() => e.error,
        _ => body.chars().take(200).collect(),
    }
}

/// True for statuses worth retrying with backoff.
pub(crate) fn is_retryable(status: StatusCode) -> bool {
    status == StatusCode::TOO_MANY_REQUESTS || status.is_server_error()
}

/// Maps a final (non-retried) failure status to an [`Error`].
pub(crate) fn map_status(status: StatusCode, message: String) -> Error {
    match status {
        StatusCode::UNAUTHORIZED => Error::Unauthorized(message),
        StatusCode::TOO_MANY_REQUESTS => Error::RateLimited,
        s if s.is_server_error() => Error::Server {
            status: s.as_u16(),
            message,
        },
        s => Error::Api {
            status: s.as_u16(),
            message,
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn loopback_detection() {
        assert!(is_loopback("http://127.0.0.1:5000"));
        assert!(is_loopback("http://localhost/helix"));
        assert!(is_loopback("http://[::1]:8080"));
        assert!(!is_loopback("https://api.twitch.tv/helix"));
        assert!(!is_loopback("https://127.example.com"));
    }
}
