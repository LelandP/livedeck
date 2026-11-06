//! Twitch Helix client behind the mockable [`TwitchApi`] trait.
//!
//! - [`HelixClient`]: the real client (`reqwest` + `serde`). The base URL is injectable for tests,
//!   it follows Helix cursor pagination and backs off on 429 / 5xx using the `Ratelimit-*` headers.
//! - [`GqlClient`]: anonymous browsing for logged-out users over Twitch's internal, unofficial GQL
//!   API. First pages only.
//! - [`FakeApi`]: canned in-memory data, for tests and `--mock`.
//!
//! This crate does not depend on `htpc-core`. The app maps the Helix types to domain types.

mod auth;
mod error;
mod fake;
mod fake_auth;
mod gql;
mod helix;
mod http;
mod hybrid;
mod model;
mod ratelimit;
mod refreshing;

use std::future::Future;

pub use auth::{
    AuthApi, DeviceGrant, HttpAuth, Identity, PollOutcome, TokenGrant, DEFAULT_AUTH_URL,
    DEVICE_GRANT_TYPE, SCOPE_READ_FOLLOWS,
};
pub use error::{Error, Result};
pub use fake::FakeApi;
pub use fake_auth::{FakeAuth, FakeAuthCall};
pub use gql::{GqlClient, GQL_MAX_PAGE_SIZE, GQL_URL, WEB_CLIENT_ID};
pub use helix::{Credentials, HelixClient, DEFAULT_BASE_URL};
pub use hybrid::HybridApi;
pub use model::{
    ChannelResult, FollowedChannel, Game, Page, PageRequest, StreamInfo, UserInfo, MAX_PAGE_SIZE,
};
pub use ratelimit::{Bucket, RateLimiter, RetryPolicy};
pub use refreshing::{Refresher, RefreshingApi};

/// The Helix endpoints the app uses. Every method fetches one page.
pub trait TwitchApi: Send + Sync + 'static {
    /// `GET /streams`: live streams, most viewers first.
    fn top_streams(
        &self,
        page: PageRequest,
    ) -> impl Future<Output = Result<Page<StreamInfo>>> + Send;

    /// `GET /streams/followed`: live streams followed by `user_id`. Needs a user token with
    /// `user:read:follows`.
    fn followed_streams(
        &self,
        user_id: &str,
        page: PageRequest,
    ) -> impl Future<Output = Result<Page<StreamInfo>>> + Send;

    /// `GET /channels/followed`: every channel `user_id` follows, live or not. Needs a user token
    /// with `user:read:follows`. The Following tab dims the offline ones.
    fn followed_channels(
        &self,
        user_id: &str,
        page: PageRequest,
    ) -> impl Future<Output = Result<Page<FollowedChannel>>> + Send;

    /// `GET /games/top`: categories, most viewers first.
    fn top_categories(&self, page: PageRequest) -> impl Future<Output = Result<Page<Game>>> + Send;

    /// `GET /streams?game_id=`: live streams in one category.
    fn streams_by_category(
        &self,
        game_id: &str,
        page: PageRequest,
    ) -> impl Future<Output = Result<Page<StreamInfo>>> + Send;

    /// `GET /users`: the user with this login, or the token's owner when `login` is `None`.
    /// `Ok(None)` if no such user exists.
    fn user(&self, login: Option<&str>) -> impl Future<Output = Result<Option<UserInfo>>> + Send;

    /// `GET /search/channels`: channels whose name matches `query`, best match first. With
    /// `live_only`, only the ones live right now. Needs a token (any kind): the anonymous client
    /// answers [`Error::NotSignedIn`].
    fn search_channels(
        &self,
        query: &str,
        live_only: bool,
        page: PageRequest,
    ) -> impl Future<Output = Result<Page<ChannelResult>>> + Send;

    /// `GET /search/categories`: categories whose name matches `query`, best match first. Needs
    /// a token, like [`TwitchApi::search_channels`].
    fn search_categories(
        &self,
        query: &str,
        page: PageRequest,
    ) -> impl Future<Output = Result<Page<Game>>> + Send;

    /// `GET /streams?user_login=`: the live streams of these channels (at most 100 logins), in
    /// no particular order. A channel that is not live is simply missing.
    fn streams_by_login(
        &self,
        logins: &[String],
    ) -> impl Future<Output = Result<Vec<StreamInfo>>> + Send;
}

/// Most logins one `streams_by_login` call takes (Helix's limit).
pub const MAX_LOGINS: usize = 100;
