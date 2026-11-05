//! Error type for every `TwitchApi` call.

/// Why a Twitch API call failed.
#[derive(Debug, thiserror::Error)]
pub enum Error {
    /// No access token is configured, or the endpoint needs a user token we do not have.
    /// No request was sent.
    #[error("not signed in")]
    NotSignedIn,
    /// Helix answered 401: the token is invalid or expired. The app refreshes on this.
    #[error("unauthorized: {0}")]
    Unauthorized(String),
    /// The OAuth server refused a login step or a refresh token (expired, used or revoked).
    /// The user has to sign in again.
    #[error("sign-in was rejected: {0}")]
    AuthRejected(String),
    /// Helix kept answering 429 after every retry.
    #[error("rate limited by Twitch, retries exhausted")]
    RateLimited,
    /// Helix kept answering 5xx after every retry.
    #[error("Twitch server error {status}: {message}")]
    Server { status: u16, message: String },
    /// Any other non-success status (400, 403, 404, ...). Not retried.
    #[error("Twitch API error {status}: {message}")]
    Api { status: u16, message: String },
    /// The request did not complete (DNS, connect, TLS, timeout).
    #[error("network error: {0}")]
    Network(#[source] reqwest::Error),
    /// The anonymous GQL API refused the request with an integrity challenge (it does so for
    /// `after` cursors, i.e. any page after the first). Not retried. Needs sign-in (Helix).
    #[error("Twitch requires an integrity token for this request (sign in to load more)")]
    IntegrityRequired,
    /// The GQL API answered 200 with `errors` and no usable data.
    #[error("Twitch GQL error: {0}")]
    Graphql(String),
    /// The response body was not the JSON we expected.
    #[error("unexpected response: {0}")]
    Decode(String),
}

pub type Result<T, E = Error> = std::result::Result<T, E>;
