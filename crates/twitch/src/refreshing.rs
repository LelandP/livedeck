//! `RefreshingApi`: retries a request once after the token was refreshed.
//!
//! Helix answers 401 when the access token has expired. The wrapper asks its [`Refresher`] for a
//! new token and repeats the request once. Refresh tokens are single use and a Home load sends
//! three requests at once, so the wrapper passes the credentials *generation* it started with:
//! the refresher renews only if nobody did already, and everybody else just retries.

use std::future::Future;
use std::sync::Arc;

use crate::{
    ChannelResult, ChannelVideos, Error, FollowedChannel, Game, HypeTrain, LiveArchive, Page,
    PageRequest, Result, SendOutcome, StreamInfo, TwitchApi, UserInfo, VideoDetails,
};

/// Renews the credentials of the inner API. The app's session implements it.
pub trait Refresher: Send + Sync + 'static {
    /// Changes every time the credentials change (sign-in, refresh, sign-out).
    fn generation(&self) -> u64;

    /// Renews the credentials if they still are generation `seen`. Returns true if the request
    /// is worth repeating (new credentials are in place), false if there is nothing to retry
    /// with (no refresh token, refresh refused, network down).
    fn refresh(&self, seen: u64) -> impl Future<Output = bool> + Send;
}

#[derive(Debug)]
pub struct RefreshingApi<A, R> {
    inner: A,
    refresher: Arc<R>,
}

impl<A, R> RefreshingApi<A, R> {
    pub fn new(inner: A, refresher: Arc<R>) -> Self {
        Self { inner, refresher }
    }
}

impl<A: TwitchApi, R: Refresher> RefreshingApi<A, R> {
    /// Runs `op`, and once more if it failed with 401 and the credentials could be renewed.
    async fn with_refresh<T, Fut>(&self, op: impl Fn() -> Fut) -> Result<T>
    where
        Fut: Future<Output = Result<T>>,
    {
        let seen = self.refresher.generation();
        match op().await {
            Err(Error::Unauthorized(why)) => {
                if self.refresher.refresh(seen).await {
                    tracing::debug!(%why, "token refreshed, repeating the request");
                    op().await
                } else {
                    Err(Error::Unauthorized(why))
                }
            }
            other => other,
        }
    }
}

impl<A: TwitchApi, R: Refresher> TwitchApi for RefreshingApi<A, R> {
    async fn top_streams(&self, page: PageRequest) -> Result<Page<StreamInfo>> {
        self.with_refresh(|| self.inner.top_streams(page.clone()))
            .await
    }

    async fn followed_streams(&self, user_id: &str, page: PageRequest) -> Result<Page<StreamInfo>> {
        self.with_refresh(|| self.inner.followed_streams(user_id, page.clone()))
            .await
    }

    async fn followed_channels(
        &self,
        user_id: &str,
        page: PageRequest,
    ) -> Result<Page<FollowedChannel>> {
        self.with_refresh(|| self.inner.followed_channels(user_id, page.clone()))
            .await
    }

    async fn top_categories(&self, page: PageRequest) -> Result<Page<Game>> {
        self.with_refresh(|| self.inner.top_categories(page.clone()))
            .await
    }

    async fn streams_by_category(
        &self,
        game_id: &str,
        page: PageRequest,
    ) -> Result<Page<StreamInfo>> {
        self.with_refresh(|| self.inner.streams_by_category(game_id, page.clone()))
            .await
    }

    async fn user(&self, login: Option<&str>) -> Result<Option<UserInfo>> {
        self.with_refresh(|| self.inner.user(login)).await
    }

    async fn search_channels(
        &self,
        query: &str,
        live_only: bool,
        page: PageRequest,
    ) -> Result<Page<ChannelResult>> {
        self.with_refresh(|| self.inner.search_channels(query, live_only, page.clone()))
            .await
    }

    async fn search_categories(&self, query: &str, page: PageRequest) -> Result<Page<Game>> {
        self.with_refresh(|| self.inner.search_categories(query, page.clone()))
            .await
    }

    async fn streams_by_login(&self, logins: &[String]) -> Result<Vec<StreamInfo>> {
        self.with_refresh(|| self.inner.streams_by_login(logins))
            .await
    }

    /// A 401 means Twitch did not take the message, so repeating it cannot post it twice.
    async fn send_chat(
        &self,
        broadcaster_id: &str,
        sender_id: &str,
        text: &str,
    ) -> Result<SendOutcome> {
        self.with_refresh(|| self.inner.send_chat(broadcaster_id, sender_id, text))
            .await
    }

    async fn hype_train(&self, login: &str) -> Result<Option<HypeTrain>> {
        self.with_refresh(|| self.inner.hype_train(login)).await
    }

    async fn channel_videos(&self, login: &str) -> Result<Option<ChannelVideos>> {
        self.with_refresh(|| self.inner.channel_videos(login)).await
    }

    async fn video_details(&self, id: &str) -> Result<Option<VideoDetails>> {
        self.with_refresh(|| self.inner.video_details(id)).await
    }

    async fn live_archive(&self, login: &str) -> Result<Option<LiveArchive>> {
        self.with_refresh(|| self.inner.live_archive(login)).await
    }
}
