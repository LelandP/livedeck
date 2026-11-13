//! `HybridApi`: the signed-in Helix API once there are credentials, the anonymous web GQL API
//! before that.
//!
//! Signing in or out is `set_credentials`. The next call already goes to the other API, with no
//! need to rebuild anything that holds the client. Clones share the same clients, so the session
//! can switch the credentials while the provider keeps using its copy.

use std::sync::Arc;

use crate::{
    ChannelResult, ChannelVideos, Credentials, FollowedChannel, Game, GqlClient, HelixClient,
    HypeTrain, LiveArchive, Page, PageRequest, ReplayComment, Result, SendOutcome, StreamInfo,
    TwitchApi, UserInfo, VideoDetails,
};

#[derive(Debug)]
struct Parts {
    gql: GqlClient,
    helix: HelixClient,
}

#[derive(Debug, Clone)]
pub struct HybridApi {
    parts: Arc<Parts>,
}

impl HybridApi {
    /// `helix` may already hold credentials (a token from the environment).
    pub fn new(gql: GqlClient, helix: HelixClient) -> Self {
        Self {
            parts: Arc::new(Parts { gql, helix }),
        }
    }

    /// Switches to Helix with these credentials, or back to anonymous browsing for `None`.
    pub fn set_credentials(&self, credentials: Option<Credentials>) {
        self.parts.helix.set_credentials(credentials);
    }

    /// True while requests go to Helix.
    pub fn is_signed_in(&self) -> bool {
        self.parts.helix.has_credentials()
    }
}

impl TwitchApi for HybridApi {
    async fn top_streams(&self, page: PageRequest) -> Result<Page<StreamInfo>> {
        if self.is_signed_in() {
            self.parts.helix.top_streams(page).await
        } else {
            self.parts.gql.top_streams(page).await
        }
    }

    async fn followed_streams(&self, user_id: &str, page: PageRequest) -> Result<Page<StreamInfo>> {
        if self.is_signed_in() {
            self.parts.helix.followed_streams(user_id, page).await
        } else {
            self.parts.gql.followed_streams(user_id, page).await
        }
    }

    async fn followed_channels(
        &self,
        user_id: &str,
        page: PageRequest,
    ) -> Result<Page<FollowedChannel>> {
        if self.is_signed_in() {
            self.parts.helix.followed_channels(user_id, page).await
        } else {
            self.parts.gql.followed_channels(user_id, page).await
        }
    }

    async fn top_categories(&self, page: PageRequest) -> Result<Page<Game>> {
        if self.is_signed_in() {
            self.parts.helix.top_categories(page).await
        } else {
            self.parts.gql.top_categories(page).await
        }
    }

    async fn streams_by_category(
        &self,
        game_id: &str,
        page: PageRequest,
    ) -> Result<Page<StreamInfo>> {
        if self.is_signed_in() {
            self.parts.helix.streams_by_category(game_id, page).await
        } else {
            self.parts.gql.streams_by_category(game_id, page).await
        }
    }

    async fn user(&self, login: Option<&str>) -> Result<Option<UserInfo>> {
        if self.is_signed_in() {
            self.parts.helix.user(login).await
        } else {
            self.parts.gql.user(login).await
        }
    }

    async fn search_channels(
        &self,
        query: &str,
        live_only: bool,
        page: PageRequest,
    ) -> Result<Page<ChannelResult>> {
        if self.is_signed_in() {
            self.parts
                .helix
                .search_channels(query, live_only, page)
                .await
        } else {
            self.parts.gql.search_channels(query, live_only, page).await
        }
    }

    async fn search_categories(&self, query: &str, page: PageRequest) -> Result<Page<Game>> {
        if self.is_signed_in() {
            self.parts.helix.search_categories(query, page).await
        } else {
            self.parts.gql.search_categories(query, page).await
        }
    }

    async fn streams_by_login(&self, logins: &[String]) -> Result<Vec<StreamInfo>> {
        if self.is_signed_in() {
            self.parts.helix.streams_by_login(logins).await
        } else {
            self.parts.gql.streams_by_login(logins).await
        }
    }

    async fn send_chat(
        &self,
        broadcaster_id: &str,
        sender_id: &str,
        text: &str,
    ) -> Result<SendOutcome> {
        if self.is_signed_in() {
            self.parts
                .helix
                .send_chat(broadcaster_id, sender_id, text)
                .await
        } else {
            self.parts
                .gql
                .send_chat(broadcaster_id, sender_id, text)
                .await
        }
    }

    /// Always GQL: Helix shows hype trains to the broadcaster only.
    async fn hype_train(&self, login: &str) -> Result<Option<HypeTrain>> {
        self.parts.gql.hype_train(login).await
    }

    /// Always GQL: Helix has no chapters or mute info, and would need a request per row.
    async fn channel_videos(&self, login: &str) -> Result<Option<ChannelVideos>> {
        self.parts.gql.channel_videos(login).await
    }

    /// Always GQL, like [`HybridApi::channel_videos`].
    async fn video_details(&self, id: &str) -> Result<Option<VideoDetails>> {
        self.parts.gql.video_details(id).await
    }

    /// Always GQL, like [`HybridApi::channel_videos`].
    async fn live_archive(&self, login: &str) -> Result<Option<LiveArchive>> {
        self.parts.gql.live_archive(login).await
    }

    /// Always GQL: Helix has no chat replay.
    async fn chat_replay(&self, id: &str, offset_s: u32) -> Result<Option<Vec<ReplayComment>>> {
        self.parts.gql.chat_replay(id, offset_s).await
    }
}
