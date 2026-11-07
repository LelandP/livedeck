//! `GqlClient`: an anonymous [`TwitchApi`] over Twitch's internal GraphQL endpoint.
//!
//! This is the API the twitch.tv website uses. It needs only the website's public `Client-Id`,
//! so logged-out users can browse. It is **undocumented and unofficial**: it can change
//! without notice and using it is a Terms of Service grey area, like Streamlink. Signed-in users
//! use the official `HelixClient` instead.
//!
//! Verified behaviour (2026-11-04, live probe):
//! - first pages of `streams`, `games` and `game(id).streams` work anonymously, `first` is 1..=30;
//! - any `after` cursor fails with `IntegrityCheckFailed` (needs a browser integrity token), so
//!   only the first page is available ([`Error::IntegrityRequired`]);
//! - unknown users and games come back as `null`;
//! - no rate-limit headers are sent, so 429 / 5xx use plain exponential backoff.

use reqwest::Client;
use serde::de::DeserializeOwned;
use serde::Deserialize;
use serde_json::{json, Value};

use crate::http::{build_client, error_message, is_retryable, map_status};
use crate::{
    ChannelResult, Error, FollowedChannel, Game, HypeTrain, Page, PageRequest, RateLimiter, Result,
    RetryPolicy, StreamInfo, TwitchApi, UserInfo,
};

pub const GQL_URL: &str = "https://gql.twitch.tv/gql";
/// The twitch.tv website's public client ID. Not a secret (it ships in every page load).
/// Override with `with_endpoint` if Twitch rotates it.
pub const WEB_CLIENT_ID: &str = "kimne78kx3ncx6brgo4mv6wki5h1ko";
/// Largest page the GQL API accepts.
pub const GQL_MAX_PAGE_SIZE: u8 = 30;

const STREAM_FIELDS: &str = "cursor node { title viewersCount \
    broadcaster { id login displayName } game { id name } \
    previewImageURL(width: 440, height: 248) }";

/// `query($first: Int!) { streams(first: $first) { ... } }` and friends, built once per call.
fn streams_query() -> String {
    format!("query($first: Int!) {{ streams(first: $first) {{ edges {{ {STREAM_FIELDS} }} pageInfo {{ hasNextPage }} }} }}")
}

fn game_streams_query() -> String {
    format!("query($id: ID!, $first: Int!) {{ game(id: $id) {{ streams(first: $first) {{ edges {{ {STREAM_FIELDS} }} pageInfo {{ hasNextPage }} }} }} }}")
}

const GAMES_QUERY: &str = "query($first: Int!) { games(first: $first) { edges { cursor node { \
    id name boxArtURL(width: 285, height: 380) } } pageInfo { hasNextPage } } }";

const USER_QUERY: &str = "query($login: String!) { user(login: $login) { \
    id login displayName profileImageURL(width: 300) } }";

/// The website's hype train banner data. Verified 2026-11-06 on a live train: it answers
/// anonymously, `execution` is `null` when no train runs (also straight after one ends), and
/// level and progress change from one 10 s poll to the next. (`hypeTrain.config` needs a sign-in,
/// so it is not asked for.)
const HYPE_TRAIN_QUERY: &str = "query($login: String!) { user(login: $login) { channel { \
    hypeTrain { execution { id isActive endedAt \
    progress { level { value } goal progression remainingSeconds } \
    allTimeHigh { level { value } } } } } } }";

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct HypeTrainExecution {
    id: String,
    #[serde(default)]
    is_active: bool,
    #[serde(default)]
    ended_at: Option<String>,
    progress: HypeTrainProgress,
    #[serde(default)]
    all_time_high: Option<HypeTrainRecord>,
}

/// `progression` and `goal` count within the current level; the website also sends `total`
/// (every point since the start), which the bar does not need.
#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct HypeTrainProgress {
    level: HypeTrainLevel,
    #[serde(default)]
    goal: u32,
    #[serde(default)]
    progression: u32,
    #[serde(default)]
    remaining_seconds: u32,
}

#[derive(Deserialize)]
struct HypeTrainLevel {
    #[serde(default)]
    value: u32,
}

#[derive(Deserialize)]
struct HypeTrainRecord {
    level: HypeTrainLevel,
}

impl HypeTrainExecution {
    /// `None` for a train that has ended but is still listed.
    fn into_train(self) -> Option<HypeTrain> {
        if !self.is_active || self.ended_at.is_some() {
            return None;
        }
        let p = self.progress;
        Some(HypeTrain {
            id: self.id,
            level: p.level.value.max(1),
            progress: p.progression,
            goal: p.goal.max(1),
            seconds_left: p.remaining_seconds,
            all_time_high: self.all_time_high.map_or(0, |r| r.level.value),
        })
    }
}

/// The hype train in a GQL answer to [`HYPE_TRAIN_QUERY`].
fn hype_train_of(data: &Value) -> Result<Option<HypeTrain>> {
    let exec: Option<HypeTrainExecution> =
        at(data, &["user", "channel", "hypeTrain", "execution"])?;
    Ok(exec.and_then(HypeTrainExecution::into_train))
}

/// `{ "data": ..., "errors": [...] }`.
#[derive(Deserialize)]
struct Response {
    data: Option<Value>,
    #[serde(default)]
    errors: Vec<GqlError>,
}

#[derive(Deserialize)]
struct GqlError {
    #[serde(default)]
    message: String,
    #[serde(default)]
    extensions: Option<GqlErrorExt>,
}

#[derive(Deserialize)]
struct GqlErrorExt {
    #[serde(default)]
    code: String,
}

#[derive(Deserialize)]
#[serde(bound = "N: Deserialize<'de>")]
struct Connection<N> {
    #[serde(default)]
    edges: Vec<Edge<N>>,
    #[serde(rename = "pageInfo", default)]
    page_info: PageInfo,
}

#[derive(Deserialize)]
struct Edge<N> {
    #[serde(default)]
    cursor: Option<String>,
    node: N,
}

#[derive(Deserialize, Default)]
struct PageInfo {
    #[serde(rename = "hasNextPage", default)]
    has_next_page: bool,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct StreamNode {
    #[serde(default)]
    title: Option<String>,
    #[serde(default)]
    viewers_count: u64,
    /// `null` for streams whose broadcaster is hidden or banned. Those are skipped.
    broadcaster: Option<UserNode>,
    game: Option<GameNode>,
    #[serde(rename = "previewImageURL", default)]
    preview_image_url: Option<String>,
}

#[derive(Deserialize)]
struct GameNode {
    id: String,
    name: String,
    #[serde(rename = "boxArtURL", default)]
    box_art_url: Option<String>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct UserNode {
    id: String,
    login: String,
    #[serde(default)]
    display_name: Option<String>,
    #[serde(rename = "profileImageURL", default)]
    profile_image_url: Option<String>,
}

impl StreamNode {
    fn into_info(self) -> Option<StreamInfo> {
        let b = self.broadcaster?;
        let (game_id, game_name) = self.game.map(|g| (g.id, g.name)).unwrap_or_default();
        Some(StreamInfo {
            user_name: b.display_name.unwrap_or_else(|| b.login.clone()),
            user_id: b.id,
            user_login: b.login,
            game_id,
            game_name,
            title: self.title.unwrap_or_default(),
            viewer_count: self.viewers_count,
            thumbnail_url: self.preview_image_url.unwrap_or_default(),
        })
    }
}

impl From<GameNode> for Game {
    fn from(g: GameNode) -> Self {
        Game {
            id: g.id,
            name: g.name,
            box_art_url: g.box_art_url.unwrap_or_default(),
        }
    }
}

/// Turns a connection into a [`Page`]. The next cursor is the last edge's cursor, but only while
/// `hasNextPage` is true. Fetching it anonymously currently fails with `IntegrityRequired`.
fn into_page<N, T>(conn: Connection<N>, map: impl FnMut(N) -> Option<T>) -> Page<T> {
    let cursor = conn
        .page_info
        .has_next_page
        .then(|| conn.edges.last().and_then(|e| e.cursor.clone()))
        .flatten()
        .filter(|c| !c.is_empty());
    Page {
        items: conn
            .edges
            .into_iter()
            .map(|e| e.node)
            .filter_map(map)
            .collect(),
        cursor,
    }
}

/// Reads `data.<path...>` as `T`. A `null` anywhere on the path gives `Ok(None)`.
fn at<T: DeserializeOwned>(data: &Value, path: &[&str]) -> Result<Option<T>> {
    let mut v = data;
    for key in path {
        match v.get(key) {
            Some(Value::Null) | None => return Ok(None),
            Some(next) => v = next,
        }
    }
    T::deserialize(v)
        .map(Some)
        .map_err(|e| Error::Decode(format!("{}: {e}", path.join("."))))
}

fn empty<T>() -> Page<T> {
    Page {
        items: Vec::new(),
        cursor: None,
    }
}

/// Adds `$after` to a first-page query when the request has a cursor.
fn with_after(query: &str, page: &PageRequest, vars: &mut Value) -> String {
    match &page.after {
        None => query.to_owned(),
        Some(after) => {
            vars["after"] = json!(after);
            query
                .replacen("$first: Int!", "$first: Int!, $after: Cursor", 1)
                .replacen("(first: $first)", "(first: $first, after: $after)", 1)
        }
    }
}

/// Maps GQL `errors`. Errors with usable `data` are only logged (GQL can return partial data).
fn check(resp: Response) -> Result<Value> {
    if let Some(e) = resp.errors.first() {
        let code = e.extensions.as_ref().map_or("", |x| x.code.as_str());
        if code == "IntegrityCheckFailed" || e.message.contains("integrity") {
            return Err(Error::IntegrityRequired);
        }
        let has_data = resp
            .data
            .as_ref()
            .and_then(Value::as_object)
            .is_some_and(|o| o.values().any(|v| !v.is_null()));
        if !has_data {
            return Err(Error::Graphql(e.message.clone()));
        }
        tracing::debug!(message = %e.message, "GQL returned partial data");
    }
    resp.data
        .ok_or_else(|| Error::Decode("missing data".into()))
}

impl From<UserNode> for UserInfo {
    fn from(u: UserNode) -> Self {
        UserInfo {
            display_name: u.display_name.unwrap_or_else(|| u.login.clone()),
            id: u.id,
            login: u.login,
            profile_image_url: u.profile_image_url.unwrap_or_default(),
        }
    }
}

#[derive(Debug)]
pub struct GqlClient {
    http: Client,
    url: String,
    client_id: String,
    limiter: RateLimiter,
}

impl GqlClient {
    /// The anonymous client for gql.twitch.tv.
    pub fn new() -> Result<Self> {
        Self::with_endpoint(GQL_URL, WEB_CLIENT_ID, RetryPolicy::default())
    }

    /// Any GQL endpoint and client ID (tests point this at `wiremock`).
    pub fn with_endpoint(
        url: impl Into<String>,
        client_id: impl Into<String>,
        policy: RetryPolicy,
    ) -> Result<Self> {
        let url = url.into();
        Ok(Self {
            http: build_client(&url)?,
            url,
            client_id: client_id.into(),
            limiter: RateLimiter::new(policy),
        })
    }

    /// POSTs one query and returns `data`, retrying 429 / 5xx with backoff.
    async fn query(&self, query: &str, variables: Value) -> Result<Value> {
        let body = json!({ "query": query, "variables": variables });
        let policy = self.limiter.policy();
        let mut attempt = 0;
        loop {
            let resp = self
                .http
                .post(&self.url)
                .header("Client-Id", &self.client_id)
                .json(&body)
                .send()
                .await
                .map_err(Error::Network)?;
            let status = resp.status();
            if status.is_success() {
                let bytes = resp.bytes().await.map_err(Error::Network)?;
                let parsed: Response =
                    serde_json::from_slice(&bytes).map_err(|e| Error::Decode(e.to_string()))?;
                return check(parsed);
            }
            let message = error_message(resp).await;
            if is_retryable(status) && attempt < policy.max_retries {
                let wait = self.limiter.backoff(attempt, false);
                tracing::warn!(%status, attempt, ?wait, "GQL request failed, retrying");
                tokio::time::sleep(wait).await;
                attempt += 1;
                continue;
            }
            return Err(map_status(status, message));
        }
    }

    /// One page of a stream connection found at `path` in `data`.
    async fn streams(
        &self,
        query: &str,
        mut vars: Value,
        path: &[&str],
        page: &PageRequest,
    ) -> Result<Page<StreamInfo>> {
        vars["first"] = json!(page.first.min(GQL_MAX_PAGE_SIZE));
        let query = with_after(query, page, &mut vars);
        let data = self.query(&query, vars).await?;
        let conn: Option<Connection<StreamNode>> = at(&data, path)?;
        Ok(conn.map_or_else(empty, |c| into_page(c, StreamNode::into_info)))
    }
}

impl TwitchApi for GqlClient {
    async fn top_streams(&self, page: PageRequest) -> Result<Page<StreamInfo>> {
        self.streams(&streams_query(), json!({}), &["streams"], &page)
            .await
    }

    /// Followed channels need a signed-in user, which this anonymous client never has.
    async fn followed_streams(
        &self,
        _user_id: &str,
        _page: PageRequest,
    ) -> Result<Page<StreamInfo>> {
        Err(Error::NotSignedIn)
    }

    /// Follows need a signed-in user, which this anonymous client never has.
    async fn followed_channels(
        &self,
        _user_id: &str,
        _page: PageRequest,
    ) -> Result<Page<FollowedChannel>> {
        Err(Error::NotSignedIn)
    }

    async fn top_categories(&self, page: PageRequest) -> Result<Page<Game>> {
        let mut vars = json!({ "first": page.first.min(GQL_MAX_PAGE_SIZE) });
        let query = with_after(GAMES_QUERY, &page, &mut vars);
        let data = self.query(&query, vars).await?;
        let conn: Option<Connection<GameNode>> = at(&data, &["games"])?;
        Ok(conn.map_or_else(empty, |c| into_page(c, |g| Some(Game::from(g)))))
    }

    async fn streams_by_category(
        &self,
        game_id: &str,
        page: PageRequest,
    ) -> Result<Page<StreamInfo>> {
        let vars = json!({ "id": game_id });
        self.streams(&game_streams_query(), vars, &["game", "streams"], &page)
            .await
    }

    async fn user(&self, login: Option<&str>) -> Result<Option<UserInfo>> {
        // "The token's owner" does not exist for an anonymous client.
        let login = login.ok_or(Error::NotSignedIn)?;
        let data = self.query(USER_QUERY, json!({ "login": login })).await?;
        Ok(at::<UserNode>(&data, &["user"])?.map(UserInfo::from))
    }

    /// Search needs a sign-in in this app (Helix's `/search`), so the website's unofficial
    /// `searchFor` query is never used.
    async fn search_channels(
        &self,
        _query: &str,
        _live_only: bool,
        _page: PageRequest,
    ) -> Result<Page<ChannelResult>> {
        Err(Error::NotSignedIn)
    }

    /// See [`GqlClient::search_channels`].
    async fn search_categories(&self, _query: &str, _page: PageRequest) -> Result<Page<Game>> {
        Err(Error::NotSignedIn)
    }

    async fn hype_train(&self, login: &str) -> Result<Option<HypeTrain>> {
        let data = self
            .query(HYPE_TRAIN_QUERY, json!({ "login": login }))
            .await?;
        hype_train_of(&data)
    }

    /// Only search uses it, and search needs a sign-in.
    async fn streams_by_login(&self, _logins: &[String]) -> Result<Vec<StreamInfo>> {
        Err(Error::NotSignedIn)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn after_cursor_is_spliced_into_the_query() {
        let mut vars = json!({ "first": 2 });
        let q = with_after(&streams_query(), &PageRequest::after(2, "abc"), &mut vars);
        assert!(q.contains("$first: Int!, $after: Cursor"));
        assert!(q.contains("streams(first: $first, after: $after)"));
        assert_eq!(vars["after"], "abc");

        let mut vars = json!({});
        let q = with_after(GAMES_QUERY, &PageRequest::first(2), &mut vars);
        assert!(!q.contains("after"));
        assert!(vars.get("after").is_none());
    }

    #[test]
    fn null_paths_are_none() {
        let data = json!({ "game": null });
        let got: Option<Connection<StreamNode>> = at(&data, &["game", "streams"]).unwrap();
        assert!(got.is_none());
    }
}
