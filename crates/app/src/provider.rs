//! `ApiProvider`: runs `Command`s against any [`TwitchApi`] and maps the Helix types to domain
//! `Event`s. The real app uses `HelixClient` or the anonymous `GqlClient`. `--mock` uses `FakeApi`
//! with fixture data, so all three go through the same mapping and the same `Event`s.

use std::collections::HashSet;
use std::future::Future;
use std::sync::Mutex;
use std::time::{Instant, SystemTime, UNIX_EPOCH};

use htpc_core::{
    grid_rows, Category, Channel, Command, Event, FeedId, HypeTrain, PageOutcome, Paging, Rgb, Row,
    RowKind, SendProblem, Stream, Tile,
};
use htpc_storage::ImageCache;
use htpc_twitch_api::{
    ChannelResult, DropReason, FollowedChannel, Game, Page, PageRequest, StreamInfo, TwitchApi,
};
use htpc_ui::Thumbnail;
use tokio::sync::OnceCell;

use crate::chat_limit::SendLimit;
use crate::session::Viewer;

/// Cards fetched per page of a Home row.
pub const HOME_PAGE_SIZE: u8 = 20;
/// Cards fetched per page of a grid (Browse, Category). A multiple of `GRID_COLS`, and the
/// largest page the anonymous GQL API accepts.
pub const GRID_PAGE_SIZE: u8 = 30;

/// Helix's largest page, for the follow lists (they are read to the end).
const FOLLOW_PAGE_SIZE: u8 = 100;
/// Pages read per follow list: 1000 channels. A bound, so a broken cursor cannot loop for ever.
const FOLLOW_MAX_PAGES: usize = 10;

/// Stream previews are drawn at this size (the card slot is 360x240).
const PREVIEW_SIZE: (u32, u32) = (440, 248);
/// Box art is requested at Twitch's standard size.
const BOX_ART_SIZE: (u32, u32) = (285, 380);
/// A stream's preview image is replaced under the same URL every few minutes, so the cache key
/// carries a time bucket of this many seconds. Without it a cached preview would never refresh.
const PREVIEW_REFRESH_SECS: u64 = 600;

/// Runs side effects. Each command yields zero or more events for the reducer.
pub trait Provider: Send + Sync + 'static {
    fn execute(&self, cmd: Command) -> impl Future<Output = Vec<Event>> + Send;

    /// One decoded thumbnail, or `None` if it cannot be had (offline, no cache). Failures are
    /// logged here: a card simply keeps its tint.
    fn thumbnail(&self, url: &str) -> impl Future<Output = Option<Thumbnail>> + Send;

    /// Every live followed channel, for live notifications (`live_watch.rs`). `None` when it
    /// cannot be had (signed out, a failed request), which skips that poll.
    fn followed_live(&self) -> impl Future<Output = Option<Vec<Stream>>> + Send {
        async { None }
    }

    /// The hype train in `channel` (`hype_train.rs`): `Some(None)` when none runs, `None` when
    /// the request failed, which skips that poll.
    fn hype_train(&self, channel: &str) -> impl Future<Output = Option<Option<HypeTrain>>> + Send {
        let _ = channel;
        async { None }
    }
}

pub struct ApiProvider<A> {
    api: A,
    /// Helix user id of the token's owner, looked up once. `Some(None)` means the token is not a
    /// user token (no Followed row).
    viewer: OnceCell<Option<String>>,
    /// The signed-in account. When set it replaces `viewer`: sign-in and sign-out
    /// change it while the app runs, so it is read on every request.
    account: Option<Viewer>,
    /// Thumbnail source. `None` (mock mode, or the cache folder is unusable) means tints only.
    images: Option<ImageCache>,
    /// Keeps chat messages within Twitch's rate limit.
    send_limit: Mutex<SendLimit>,
}

impl<A: TwitchApi> ApiProvider<A> {
    pub fn new(api: A) -> Self {
        Self {
            api,
            viewer: OnceCell::new(),
            account: None,
            images: None,
            send_limit: Mutex::new(SendLimit::default()),
        }
    }

    /// Follows the session's account instead of looking the token's owner up once.
    pub fn with_account(mut self, account: Viewer) -> Self {
        self.account = Some(account);
        self
    }

    /// Fetches thumbnails through `cache`.
    pub fn with_images(mut self, cache: Option<ImageCache>) -> Self {
        self.images = cache;
        self
    }

    /// The token owner's user id. Lookup errors are not cached, so the next load retries.
    async fn viewer_id(&self) -> Option<String> {
        if let Some(account) = &self.account {
            return account.lock().unwrap_or_else(|e| e.into_inner()).clone();
        }
        let lookup = self
            .viewer
            .get_or_try_init(|| async {
                match self.api.user(None).await {
                    Ok(user) => Ok(user.map(|u| u.id)),
                    // No token, or an app token (Helix answers 400 to `/users` without a
                    // login): browse without the Followed row.
                    Err(
                        htpc_twitch_api::Error::NotSignedIn | htpc_twitch_api::Error::Api { .. },
                    ) => Ok(None),
                    Err(err) => Err(err),
                }
            })
            .await;
        match lookup {
            Ok(id) => id.clone(),
            Err(err) => {
                tracing::warn!(%err, "could not look up the signed-in user");
                None
            }
        }
    }

    async fn load_feed(&self, feed: FeedId) -> Event {
        let result = match &feed {
            FeedId::Home => self.load_home().await,
            FeedId::Following => self.load_following().await,
            FeedId::Browse => self.load_browse().await,
            FeedId::Category(game) => self.load_category(game).await,
            FeedId::Search(text) => self.load_search(text).await,
        };
        match result {
            Ok(rows) => Event::FeedLoaded { feed, rows },
            Err(message) => Event::FeedFailed { feed, message },
        }
    }

    /// The home rows, fetched concurrently: the hero (the most-watched stream), Followed Live,
    /// Top Live and Categories. Failed and empty rows are skipped. If every row fails, the error
    /// text is returned instead.
    async fn load_home(&self) -> Result<Vec<Row>, String> {
        let page = || PageRequest::first(HOME_PAGE_SIZE);
        let bucket = refresh_bucket();
        let followed = async {
            match self.viewer_id().await {
                Some(id) => Some(self.api.followed_streams(&id, page()).await),
                None => None,
            }
        };
        let (followed, top, games) = tokio::join!(
            followed,
            self.api.top_streams(page()),
            self.api.top_categories(page()),
        );

        let mut errors = Vec::new();
        let mut note = |what: &str, err: &htpc_twitch_api::Error| {
            tracing::warn!(what, %err, "failed to load home row");
            errors.push(err.to_string());
        };

        let mut hero = None;
        let mut top_row = None;
        match top {
            Ok(page) => {
                let paging = paging_of(&page);
                let mut tiles = stream_tiles(&page, bucket);
                if !tiles.is_empty() {
                    // The hero is the #1 stream. It is left out of Top Live so it shows once.
                    hero = Some(Row::new(RowKind::Featured, vec![tiles.remove(0)]));
                }
                if !tiles.is_empty() {
                    top_row = Some(Row::new(RowKind::TopLive, tiles).with_paging(paging));
                }
            }
            Err(err) => note("top streams", &err),
        }
        let mut followed_row = None;
        match followed {
            Some(Ok(page)) => {
                let tiles = stream_tiles(&page, bucket);
                if !tiles.is_empty() {
                    followed_row =
                        Some(Row::new(RowKind::FollowedLive, tiles).with_paging(paging_of(&page)));
                }
            }
            Some(Err(err)) => note("followed streams", &err),
            None => {}
        }
        let mut categories_row = None;
        match games {
            Ok(page) => {
                let tiles = category_tiles(&page);
                if !tiles.is_empty() {
                    categories_row =
                        Some(Row::new(RowKind::Categories, tiles).with_paging(paging_of(&page)));
                }
            }
            Err(err) => note("categories", &err),
        }

        let rows: Vec<Row> = [hero, followed_row, top_row, categories_row]
            .into_iter()
            .flatten()
            .collect();
        if rows.is_empty() && !errors.is_empty() {
            Err(errors.join("; "))
        } else {
            Ok(rows)
        }
    }

    /// The Following tab: the live followed channels first, then every other followed channel as
    /// a dimmed card. Both lists are paged through to the end (a follow list is a few hundred
    /// names at most, and the tab is only useful complete). Live channels stay out of the
    /// offline row, so nobody shows twice.
    async fn load_following(&self) -> Result<Vec<Row>, String> {
        let Some(id) = self.viewer_id().await else {
            return Err("Sign in to Twitch to see the channels you follow".to_owned());
        };
        let bucket = refresh_bucket();
        let (live, all) = tokio::join!(self.all_followed_streams(&id), self.all_followed(&id));
        let live = live.map_err(|err| {
            tracing::warn!(%err, "failed to load the followed streams");
            err.to_string()
        })?;
        // A failed channel list still leaves the live ones, which is what matters most.
        let all = all.unwrap_or_else(|err| {
            tracing::warn!(%err, "failed to load the followed channels");
            Vec::new()
        });

        let live_logins: std::collections::HashSet<String> = live
            .iter()
            .map(|s| s.user_login.to_ascii_lowercase())
            .collect();
        let live_tiles: Vec<Tile> = live.iter().map(|s| stream(s, bucket)).collect();
        let offline_tiles: Vec<Tile> = all
            .iter()
            .filter(|c| !live_logins.contains(&c.broadcaster_login.to_ascii_lowercase()))
            .map(channel)
            .collect();
        let mut rows = Vec::new();
        if !live_tiles.is_empty() {
            rows.push(Row::new(RowKind::FollowedLive, live_tiles));
        }
        if !offline_tiles.is_empty() {
            rows.push(Row::new(RowKind::FollowedOffline, offline_tiles));
        }
        Ok(rows)
    }

    /// Every page of the live followed streams.
    async fn all_followed_streams(&self, id: &str) -> htpc_twitch_api::Result<Vec<StreamInfo>> {
        let mut out = Vec::new();
        let mut req = PageRequest::first(FOLLOW_PAGE_SIZE);
        for _ in 0..FOLLOW_MAX_PAGES {
            let page = self.api.followed_streams(id, req).await?;
            out.extend(page.items);
            match page.cursor {
                Some(cursor) => req = PageRequest::after(FOLLOW_PAGE_SIZE, cursor),
                None => break,
            }
        }
        Ok(out)
    }

    /// Every page of the followed channels, live or not.
    async fn all_followed(&self, id: &str) -> htpc_twitch_api::Result<Vec<FollowedChannel>> {
        let mut out = Vec::new();
        let mut req = PageRequest::first(FOLLOW_PAGE_SIZE);
        for _ in 0..FOLLOW_MAX_PAGES {
            let page = self.api.followed_channels(id, req).await?;
            out.extend(page.items);
            match page.cursor {
                Some(cursor) => req = PageRequest::after(FOLLOW_PAGE_SIZE, cursor),
                None => break,
            }
        }
        Ok(out)
    }

    /// The category directory: a grid of categories.
    async fn load_browse(&self) -> Result<Vec<Row>, String> {
        let page = self
            .api
            .top_categories(PageRequest::first(GRID_PAGE_SIZE))
            .await
            .map_err(|err| {
                tracing::warn!(%err, "failed to load the category directory");
                err.to_string()
            })?;
        let paging = paging_of(&page);
        Ok(grid_rows(
            RowKind::Categories,
            category_tiles(&page),
            paging,
        ))
    }

    /// The live streams of one category: a grid of streams.
    async fn load_category(&self, game_id: &str) -> Result<Vec<Row>, String> {
        let page = self
            .api
            .streams_by_category(game_id, PageRequest::first(GRID_PAGE_SIZE))
            .await
            .map_err(|err| {
                tracing::warn!(game_id, %err, "failed to load the category's streams");
                err.to_string()
            })?;
        let paging = paging_of(&page);
        Ok(grid_rows(
            RowKind::CategoryStreams,
            stream_tiles(&page, refresh_bucket()),
            paging,
        ))
    }

    /// The search results for `text`, fetched concurrently: live channels (looked up again for
    /// their viewer counts and previews, so they look like every other stream card), categories,
    /// and the channels that are not live. Failed and empty rows are skipped. If every request
    /// fails, the error text is returned instead.
    async fn load_search(&self, text: &str) -> Result<Vec<Row>, String> {
        let page = || PageRequest::first(HOME_PAGE_SIZE);
        let bucket = refresh_bucket();
        let (live, all, games) = tokio::join!(
            async {
                let found = self.api.search_channels(text, true, page()).await?;
                let tiles = self.live_tiles(&found.items, bucket).await;
                Ok::<_, htpc_twitch_api::Error>((tiles, paging_of(&found)))
            },
            self.api.search_channels(text, false, page()),
            self.api.search_categories(text, page()),
        );

        let mut errors = Vec::new();
        let mut note = |what: &str, err: &htpc_twitch_api::Error| {
            tracing::warn!(what, %err, "search failed");
            errors.push(err.to_string());
        };
        let mut rows = Vec::new();
        let mut live_logins = HashSet::new();
        match live {
            Ok((tiles, paging)) => {
                live_logins.extend(tiles.iter().map(|t| t.key().to_ascii_lowercase()));
                if !tiles.is_empty() {
                    rows.push(Row::new(RowKind::SearchLive, tiles).with_paging(paging));
                }
            }
            Err(err) => note("live channels", &err),
        }
        match games {
            Ok(found) => {
                let tiles = category_tiles(&found);
                if !tiles.is_empty() {
                    rows.push(
                        Row::new(RowKind::SearchCategories, tiles).with_paging(paging_of(&found)),
                    );
                }
            }
            Err(err) => note("categories", &err),
        }
        match all {
            Ok(found) => {
                let tiles: Vec<Tile> = offline_tiles(&found.items)
                    .into_iter()
                    .filter(|t| !live_logins.contains(&t.key().to_ascii_lowercase()))
                    .collect();
                if !tiles.is_empty() {
                    rows.push(
                        Row::new(RowKind::SearchOffline, tiles).with_paging(paging_of(&found)),
                    );
                }
            }
            Err(err) => note("channels", &err),
        }
        if rows.is_empty() && errors.len() == 3 {
            Err(errors.join("; "))
        } else {
            Ok(rows)
        }
    }

    /// Stream cards for live search results, in the search's order. Each is looked up in
    /// `/streams` for its viewer count and preview. One that is not found there (it just went
    /// offline, or the lookup failed) still gets a card from what the search said.
    async fn live_tiles(&self, found: &[ChannelResult], bucket: u64) -> Vec<Tile> {
        let live: Vec<&ChannelResult> = found.iter().filter(|c| c.is_live).collect();
        if live.is_empty() {
            return Vec::new();
        }
        let logins: Vec<String> = live.iter().map(|c| c.broadcaster_login.clone()).collect();
        let streams = self
            .api
            .streams_by_login(&logins)
            .await
            .unwrap_or_else(|err| {
                tracing::warn!(%err, "could not look up the live search results");
                Vec::new()
            });
        live.iter()
            .map(|c| {
                streams
                    .iter()
                    .find(|s| s.user_login.eq_ignore_ascii_case(&c.broadcaster_login))
                    .map_or_else(|| channel_stream(c), |s| stream(s, bucket))
            })
            .collect()
    }

    /// What Twitch knows of one channel: its live stream, or for a channel that is not live just
    /// its user id. `None` (logged) if neither can be had.
    async fn stream_info(&self, channel: &str) -> Option<Event> {
        match self.api.streams_by_login(&[channel.to_owned()]).await {
            Ok(found) => {
                if let Some(s) = found
                    .iter()
                    .find(|s| s.user_login.eq_ignore_ascii_case(channel))
                {
                    return Some(Event::StreamInfo(stream_of(s, refresh_bucket())));
                }
            }
            Err(err) => {
                tracing::debug!(%err, channel, "could not look up the stream");
                return None;
            }
        }
        match self.api.user(Some(channel)).await {
            Ok(Some(user)) => Some(Event::StreamInfo(Stream {
                channel: user.login.clone(),
                user_id: user.id,
                title: String::new(),
                category: String::new(),
                viewers: 0,
                tint: tint_for(&user.login),
                thumbnail: None,
            })),
            Ok(None) => None,
            Err(err) => {
                tracing::debug!(%err, channel, "could not look up the channel");
                None
            }
        }
    }

    /// Writes `text` in `channel`'s chat as the signed-in user, within the app's own rate limit.
    async fn send_chat(
        &self,
        channel: &str,
        broadcaster_id: &str,
        text: &str,
    ) -> Result<(), SendProblem> {
        let allowed = self
            .send_limit
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .try_take(Instant::now());
        if !allowed {
            tracing::info!("chat message held back: the rate limit is reached");
            return Err(SendProblem::TooFast);
        }
        let Some(sender) = self.viewer_id().await else {
            return Err(SendProblem::Failed("Not signed in".to_owned()));
        };
        // Cards made from a login only carry no user id. Signed in, `FetchStreamInfo` usually
        // has filled it by now; if not, it is looked up here.
        let broadcaster = if broadcaster_id.is_empty() {
            match self.api.user(Some(channel)).await {
                Ok(Some(user)) => user.id,
                Ok(None) => return Err(SendProblem::Failed("Channel not found".to_owned())),
                Err(err) => return Err(SendProblem::Failed(err.to_string())),
            }
        } else {
            broadcaster_id.to_owned()
        };
        match self.api.send_chat(&broadcaster, &sender, text).await {
            Ok(outcome) if outcome.sent => Ok(()),
            Ok(outcome) => {
                let reason = outcome.drop_reason.unwrap_or(DropReason {
                    code: String::new(),
                    message: String::new(),
                });
                tracing::info!(code = %reason.code, message = %reason.message, "chat message dropped");
                Err(SendProblem::Dropped {
                    code: reason.code,
                    message: reason.message,
                })
            }
            Err(err) => {
                tracing::warn!(%err, channel, "could not send the chat message");
                Err(SendProblem::Failed(err.to_string()))
            }
        }
    }

    /// One more page of a row. `IntegrityRequired` is the anonymous GQL API's answer to every
    /// page after the first, so it ends the row instead of failing it.
    async fn load_more(&self, feed: FeedId, kind: RowKind, cursor: String) -> Event {
        let size = if feed.is_grid() {
            GRID_PAGE_SIZE
        } else {
            HOME_PAGE_SIZE
        };
        let req = PageRequest::after(size, cursor.clone());
        let bucket = refresh_bucket();
        let fetched: htpc_twitch_api::Result<(Vec<Tile>, Option<String>)> = match (&feed, kind) {
            (_, RowKind::TopLive) => self
                .api
                .top_streams(req)
                .await
                .map(|p| (stream_tiles(&p, bucket), p.cursor)),
            (_, RowKind::FollowedLive) => match self.viewer_id().await {
                Some(id) => self
                    .api
                    .followed_streams(&id, req)
                    .await
                    .map(|p| (stream_tiles(&p, bucket), p.cursor)),
                None => Ok((Vec::new(), None)),
            },
            (_, RowKind::Categories) => self
                .api
                .top_categories(req)
                .await
                .map(|p| (category_tiles(&p), p.cursor)),
            (FeedId::Category(game), RowKind::CategoryStreams) => self
                .api
                .streams_by_category(game, req)
                .await
                .map(|p| (stream_tiles(&p, bucket), p.cursor)),
            (FeedId::Search(text), RowKind::SearchLive) => {
                match self.api.search_channels(text, true, req).await {
                    Ok(p) => Ok((self.live_tiles(&p.items, bucket).await, p.cursor)),
                    Err(err) => Err(err),
                }
            }
            (FeedId::Search(text), RowKind::SearchCategories) => self
                .api
                .search_categories(text, req)
                .await
                .map(|p| (category_tiles(&p), p.cursor)),
            // The reducer drops a channel already shown (it went live meanwhile, or repeats).
            (FeedId::Search(text), RowKind::SearchOffline) => self
                .api
                .search_channels(text, false, req)
                .await
                .map(|p| (offline_tiles(&p.items), p.cursor)),
            // The hero never pages, and Following is loaded whole (see `load_following`).
            _ => Ok((Vec::new(), None)),
        };
        let outcome = match fetched {
            Ok((tiles, next)) => PageOutcome::Page { tiles, next },
            Err(htpc_twitch_api::Error::IntegrityRequired) => {
                tracing::debug!(?feed, ?kind, "anonymous API has no further pages");
                PageOutcome::End
            }
            Err(err) => {
                tracing::warn!(?feed, ?kind, %err, "failed to load the next page");
                PageOutcome::Failed
            }
        };
        Event::MoreLoaded {
            feed,
            kind,
            cursor,
            outcome,
        }
    }
}

impl<A: TwitchApi> Provider for ApiProvider<A> {
    async fn execute(&self, cmd: Command) -> Vec<Event> {
        match cmd {
            Command::LoadFeed { feed } => vec![self.load_feed(feed).await],
            Command::LoadMore { feed, kind, cursor } => {
                vec![self.load_more(feed, kind, cursor).await]
            }
            // Waiting for typing to pause is `search.rs`'s job. This runs the search itself.
            Command::Search { id, query } => {
                if !htpc_core::SearchState::is_searchable(&query) {
                    return Vec::new();
                }
                match self.load_search(&query).await {
                    Ok(rows) => vec![Event::SearchResults { id, rows }],
                    Err(message) => vec![Event::SearchFailed { id, message }],
                }
            }
            Command::FetchStreamInfo { channel } => {
                self.stream_info(&channel).await.into_iter().collect()
            }
            Command::SendChat {
                id,
                channel,
                broadcaster_id,
                text,
            } => vec![
                match self.send_chat(&channel, &broadcaster_id, &text).await {
                    Ok(()) => Event::ChatSent { id },
                    Err(problem) => Event::ChatDropped { id, problem },
                },
            ],
            // Playback commands run in `playback.rs`, chat in `chat.rs`, the login ones in
            // `session.rs`, the sleep timer in `sleep.rs`, raids in `raids.rs`, hype trains in
            // `hype_train.rs`, and thumbnails and chat images are delivered through
            // `Provider::thumbnail`, so none of them produces events here. The store runs `Quit`.
            Command::WatchHypeTrain { .. }
            | Command::StopHypeTrain
            | Command::WatchRaids { .. }
            | Command::StopRaids
            | Command::DisconnectRaids
            | Command::StartRaidCountdown { .. }
            | Command::Play { .. }
            | Command::SetPaused(_)
            | Command::StopPlayback
            | Command::SetPlayerOption(_)
            | Command::SetStats(_)
            | Command::FetchThumbnails { .. }
            | Command::FetchChatImages { .. }
            | Command::JoinChat { .. }
            | Command::LeaveChat
            | Command::StartLogin
            | Command::CancelLogin
            | Command::SignOut
            | Command::SaveSettings(_)
            | Command::StartSleepTimer { .. }
            | Command::CancelSleepTimer
            | Command::Suspend
            | Command::OpenUrl(_)
            | Command::StartLiveWatch
            | Command::StopLiveWatch
            | Command::ExpireToast { .. }
            | Command::ScheduleOfflineRecheck { .. }
            | Command::Quit => Vec::new(),
        }
    }

    async fn followed_live(&self) -> Option<Vec<Stream>> {
        let id = self.viewer_id().await?;
        match self.all_followed_streams(&id).await {
            Ok(live) => {
                let bucket = refresh_bucket();
                Some(live.iter().map(|s| stream_of(s, bucket)).collect())
            }
            Err(err) => {
                tracing::debug!(%err, "could not check who went live");
                None
            }
        }
    }

    async fn hype_train(&self, channel: &str) -> Option<Option<HypeTrain>> {
        match self.api.hype_train(channel).await {
            Ok(train) => Some(train.map(|t| HypeTrain {
                level: t.level,
                progress: t.progress,
                goal: t.goal,
                seconds_left: t.seconds_left,
            })),
            Err(err) => {
                tracing::debug!(channel, %err, "could not check for a hype train");
                None
            }
        }
    }

    async fn thumbnail(&self, url: &str) -> Option<Thumbnail> {
        let cache = self.images.as_ref()?;
        match cache.get(url).await {
            Ok(img) => Some(Thumbnail {
                width: img.width,
                height: img.height,
                rgba: img.rgba,
            }),
            Err(err) => {
                tracing::debug!(url, %err, "thumbnail unavailable, keeping the tint");
                None
            }
        }
    }
}

/// The current refresh bucket, see [`PREVIEW_REFRESH_SECS`].
fn refresh_bucket() -> u64 {
    let secs = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| d.as_secs());
    secs / PREVIEW_REFRESH_SECS
}

/// Fills in a Helix thumbnail template (`...-{width}x{height}.jpg`). GQL URLs are already sized
/// and pass through. An empty URL means the source has no image.
fn sized_url(template: &str, (w, h): (u32, u32)) -> Option<String> {
    let url = template.trim();
    (!url.is_empty()).then(|| {
        url.replace("{width}", &w.to_string())
            .replace("{height}", &h.to_string())
    })
}

/// How a row continues, from the page cursor. Whether a later page can really be fetched is
/// decided when it is asked for (anonymous GQL ends the row then).
fn paging_of<T>(page: &Page<T>) -> Paging {
    page.cursor.clone().map_or(Paging::End, Paging::More)
}

fn stream_tiles(page: &Page<StreamInfo>, bucket: u64) -> Vec<Tile> {
    page.items.iter().map(|s| stream(s, bucket)).collect()
}

fn category_tiles(page: &Page<Game>) -> Vec<Tile> {
    page.items.iter().map(category).collect()
}

fn stream(s: &StreamInfo, bucket: u64) -> Tile {
    Tile::Stream(stream_of(s, bucket))
}

fn stream_of(s: &StreamInfo, bucket: u64) -> Stream {
    Stream {
        channel: s.user_login.clone(),
        user_id: s.user_id.clone(),
        title: s.title.clone(),
        category: s.game_name.clone(),
        viewers: s.viewer_count,
        tint: tint_for(&s.user_login),
        // The bucket makes the cache fetch a fresh preview every `PREVIEW_REFRESH_SECS`.
        thumbnail: sized_url(&s.thumbnail_url, PREVIEW_SIZE).map(|u| format!("{u}?t={bucket}")),
    }
}

/// A live search result that `/streams` did not know: a card from what the search said, without
/// viewers or a preview.
fn channel_stream(c: &ChannelResult) -> Tile {
    Tile::Stream(Stream {
        channel: c.broadcaster_login.clone(),
        user_id: c.id.clone(),
        title: c.title.clone(),
        category: c.game_name.clone(),
        viewers: 0,
        tint: tint_for(&c.broadcaster_login),
        thumbnail: None,
    })
}

/// The search results that are not live, as dimmed channel cards.
fn offline_tiles(found: &[ChannelResult]) -> Vec<Tile> {
    found
        .iter()
        .filter(|c| !c.is_live)
        .map(|c| {
            Tile::Channel(Channel {
                channel: c.broadcaster_login.clone(),
                name: c.display_name.clone(),
                tint: tint_for(&c.broadcaster_login),
            })
        })
        .collect()
}

/// A followed channel that is not live.
fn channel(c: &FollowedChannel) -> Tile {
    Tile::Channel(Channel {
        channel: c.broadcaster_login.clone(),
        name: c.broadcaster_name.clone(),
        tint: tint_for(&c.broadcaster_login),
    })
}

fn category(g: &Game) -> Tile {
    Tile::Category(Category {
        id: g.id.clone(),
        name: g.name.clone(),
        tint: tint_for(&g.name),
        thumbnail: sized_url(&g.box_art_url, BOX_ART_SIZE),
    })
}

/// Muted placeholder tints in the scaffold's palette, shown until a thumbnail has loaded.
const PALETTE: [u32; 10] = [
    0x3a5a40, 0x6b2d3c, 0x2d4a6b, 0x5a4a2d, 0x4a2d6b, 0x7a2e2e, 0x2e5a7a, 0x4f6b2d, 0x6b4f2d,
    0x2d6b5e,
];

/// A stable tint per name (FNV-1a), so a card keeps its colour across reloads.
pub fn tint_for(key: &str) -> Rgb {
    let hash = key.bytes().fold(0x811c_9dc5_u32, |h, b| {
        (h ^ u32::from(b)).wrapping_mul(0x0100_0193)
    });
    Rgb::from_hex(PALETTE[hash as usize % PALETTE.len()])
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::mock::{fake_api, MOCK_USER_ID};
    use htpc_twitch_api::{Credentials, FakeApi, HelixClient, RetryPolicy};
    use std::sync::Arc;
    use std::time::Duration;
    use wiremock::matchers::{body_string_contains, path, query_param};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    fn rows_of(event: Event) -> Vec<Row> {
        match event {
            Event::FeedLoaded { rows, .. } => rows,
            other => panic!("expected rows, got {other:?}"),
        }
    }

    fn kinds(rows: &[Row]) -> Vec<RowKind> {
        rows.iter().map(|r| r.kind).collect()
    }

    fn channel(tile: &Tile) -> &str {
        match tile {
            Tile::Stream(s) => &s.channel,
            Tile::Category(c) => &c.name,
            Tile::Channel(c) => &c.channel,
        }
    }

    /// `fake_api()` with `n` streams in Top Live.
    fn api_with_top(n: usize) -> FakeApi {
        let mut api = fake_api();
        api.top = (0..n)
            .map(|i| {
                let mut s = api.top[0].clone();
                s.user_login = format!("s{i}");
                s
            })
            .collect();
        api
    }

    #[tokio::test]
    async fn mock_home_has_a_hero_then_the_scaffold_rows() {
        let provider = ApiProvider::new(fake_api());
        let rows = rows_of(provider.load_feed(FeedId::Home).await);
        assert_eq!(
            kinds(&rows),
            [
                RowKind::Featured,
                RowKind::FollowedLive,
                RowKind::TopLive,
                RowKind::Categories
            ]
        );
        // The hero is the #1 stream and is not repeated in Top Live.
        assert_eq!(rows[0].tiles.len(), 1);
        assert_eq!(channel(&rows[0].tiles[0]), "grandmaster_k");
        assert_eq!(rows[2].tiles.len(), 4);
        assert!(rows[2].tiles.iter().all(|t| channel(t) != "grandmaster_k"));
        assert_eq!(rows[1].tiles.len(), 5);
        let Tile::Stream(first) = &rows[1].tiles[0] else {
            panic!("stream expected")
        };
        assert_eq!(
            (first.channel.as_str(), first.viewers),
            ("nightowl_dev", 4_200)
        );
        let Tile::Category(cat) = &rows[3].tiles[0] else {
            panic!("category expected")
        };
        assert_eq!(cat.name, "Just Chatting");
        assert!(!cat.id.is_empty());
        // Everything fits in one page, so no row pages.
        assert!(rows.iter().all(|r| r.paging == Paging::End));
    }

    #[tokio::test]
    async fn signed_out_skips_the_followed_row() {
        let api = FakeApi {
            me: None,
            ..fake_api()
        };
        let rows = rows_of(ApiProvider::new(api).load_feed(FeedId::Home).await);
        assert_eq!(
            kinds(&rows),
            [RowKind::Featured, RowKind::TopLive, RowKind::Categories]
        );
    }

    #[tokio::test]
    async fn rows_with_more_pages_carry_their_cursor() {
        let provider = ApiProvider::new(api_with_top(30));
        let rows = rows_of(provider.load_feed(FeedId::Home).await);
        let top = rows.iter().find(|r| r.kind == RowKind::TopLive).unwrap();
        // 20 per page, minus the hero.
        assert_eq!(top.tiles.len(), HOME_PAGE_SIZE as usize - 1);
        assert_eq!(top.paging, Paging::More("20".into()));
    }

    #[tokio::test]
    async fn load_more_returns_the_next_page_and_its_cursor() {
        let provider = ApiProvider::new(api_with_top(50));
        let event = provider
            .load_more(FeedId::Home, RowKind::TopLive, "20".into())
            .await;
        let Event::MoreLoaded {
            outcome: PageOutcome::Page { tiles, next },
            cursor,
            ..
        } = event
        else {
            panic!("a page was expected");
        };
        assert_eq!(cursor, "20");
        assert_eq!(tiles.len(), 20);
        assert_eq!(channel(&tiles[0]), "s20");
        assert_eq!(next.as_deref(), Some("40"));
    }

    #[tokio::test]
    async fn a_failed_page_is_reported_as_failed() {
        // FakeApi rejects an unknown cursor like Helix's 400.
        let provider = ApiProvider::new(fake_api());
        let event = provider
            .load_more(FeedId::Browse, RowKind::Categories, "bogus".into())
            .await;
        assert!(matches!(
            event,
            Event::MoreLoaded {
                outcome: PageOutcome::Failed,
                ..
            }
        ));
    }

    #[tokio::test]
    async fn browse_and_category_feeds_are_grids() {
        let mut api = fake_api();
        api.top[0].game_id = "g1".into();
        api.top[1].game_id = "g1".into();
        let provider = ApiProvider::new(api);

        let rows = rows_of(provider.load_feed(FeedId::Browse).await);
        // 5 games fill one portrait grid row.
        assert_eq!(kinds(&rows), [RowKind::Categories]);

        let rows = rows_of(provider.load_feed(FeedId::Category("g1".into())).await);
        assert_eq!(kinds(&rows), [RowKind::CategoryStreams]);
        assert_eq!(rows[0].tiles.len(), 2);
    }

    #[tokio::test]
    async fn an_unknown_category_loads_as_empty() {
        let provider = ApiProvider::new(fake_api());
        let rows = rows_of(provider.load_feed(FeedId::Category("nope".into())).await);
        assert!(rows.is_empty());
    }

    #[tokio::test]
    async fn chat_and_image_commands_are_not_the_providers_business() {
        let provider = ApiProvider::new(fake_api());
        let join = Command::JoinChat {
            channel: "x".into(),
        };
        assert!(provider.execute(join).await.is_empty());
        let images = Command::FetchChatImages {
            urls: vec!["https://cdn/x.png".into()],
        };
        assert!(provider.execute(images).await.is_empty());
    }

    #[test]
    fn tints_are_stable_and_from_the_palette() {
        assert_eq!(tint_for("grandmaster_k"), tint_for("grandmaster_k"));
        let all: Vec<_> = PALETTE.iter().map(|&h| Rgb::from_hex(h)).collect();
        assert!(all.contains(&tint_for("anything")));
    }

    #[test]
    fn thumbnail_urls_are_sized_and_cache_busted() {
        assert_eq!(
            sized_url("https://cdn/x-{width}x{height}.jpg", (440, 248)).as_deref(),
            Some("https://cdn/x-440x248.jpg")
        );
        // GQL urls are already sized and pass through.
        assert_eq!(
            sized_url("https://cdn/x-440x248.jpg", (440, 248)).as_deref(),
            Some("https://cdn/x-440x248.jpg")
        );
        assert_eq!(sized_url("  ", (1, 1)), None);

        let info = StreamInfo {
            thumbnail_url: "https://cdn/live-{width}x{height}.jpg".into(),
            ..fake_api().top[0].clone()
        };
        let Tile::Stream(s) = stream(&info, 7) else {
            panic!("stream expected")
        };
        assert_eq!(
            s.thumbnail.as_deref(),
            Some("https://cdn/live-440x248.jpg?t=7")
        );
        // Box art has no cache buster: it rarely changes.
        let game = Game {
            id: "1".into(),
            name: "G".into(),
            box_art_url: "https://cdn/art-{width}x{height}.jpg".into(),
        };
        let Tile::Category(c) = category(&game) else {
            panic!("category expected")
        };
        assert_eq!(c.thumbnail.as_deref(), Some("https://cdn/art-285x380.jpg"));
    }

    #[tokio::test]
    async fn without_an_image_cache_there_are_no_thumbnails() {
        let provider = ApiProvider::new(fake_api());
        assert!(provider.thumbnail("https://cdn/x.png").await.is_none());
    }

    fn policy() -> RetryPolicy {
        RetryPolicy {
            max_retries: 0,
            base_backoff: Duration::from_millis(1),
            max_wait: Duration::from_millis(1),
        }
    }

    fn helix(server: &MockServer) -> HelixClient {
        let creds = Credentials {
            client_id: "cid".into(),
            access_token: "tok".into(),
        };
        HelixClient::with_base_url(server.uri(), Some(creds), policy()).unwrap()
    }

    fn gql(server: &MockServer) -> htpc_twitch_api::GqlClient {
        htpc_twitch_api::GqlClient::with_endpoint(server.uri(), "web", policy()).unwrap()
    }

    fn json(v: serde_json::Value) -> ResponseTemplate {
        ResponseTemplate::new(200).set_body_json(v)
    }

    fn gql_stream(login: &str) -> serde_json::Value {
        serde_json::json!({ "cursor": "c", "node": {
            "title": "t", "viewersCount": 7,
            "broadcaster": { "id": "1", "login": login, "displayName": login },
            "game": { "id": "509658", "name": "Just Chatting" },
            "previewImageURL": format!("https://cdn/{login}-440x248.jpg")
        }})
    }

    #[tokio::test]
    async fn helix_home_maps_real_responses_and_thumbnails() {
        let server = MockServer::start().await;
        Mock::given(path("/users"))
            .respond_with(json(serde_json::json!({ "data": [
                { "id": MOCK_USER_ID, "login": "me", "display_name": "Me" }
            ]})))
            .expect(1)
            .mount(&server)
            .await;
        Mock::given(path("/streams/followed"))
            .and(query_param("user_id", MOCK_USER_ID))
            .respond_with(json(serde_json::json!({ "data": [], "pagination": {} })))
            .mount(&server)
            .await;
        Mock::given(path("/streams"))
            .respond_with(json(serde_json::json!({ "data": [
                { "user_id": "1", "user_login": "octave", "user_name": "Octave",
                  "game_id": "30921", "game_name": "Rocket League",
                  "title": "Road to Champion", "viewer_count": 14000,
                  "thumbnail_url": "https://cdn/octave-{width}x{height}.jpg" },
                { "user_id": "2", "user_login": "second", "user_name": "Second",
                  "viewer_count": 9000 }
            ], "pagination": { "cursor": "abc" } })))
            .mount(&server)
            .await;
        Mock::given(path("/games/top"))
            .respond_with(ResponseTemplate::new(503))
            .mount(&server)
            .await;

        let provider = ApiProvider::new(helix(&server));
        let rows = rows_of(provider.load_feed(FeedId::Home).await);
        // The empty Followed row and the failed Categories row are skipped.
        assert_eq!(kinds(&rows), [RowKind::Featured, RowKind::TopLive]);
        let Tile::Stream(hero) = &rows[0].tiles[0] else {
            panic!("stream expected")
        };
        assert_eq!((hero.channel.as_str(), hero.viewers), ("octave", 14_000));
        assert_eq!(hero.category, "Rocket League");
        assert!(hero
            .thumbnail
            .as_deref()
            .is_some_and(|u| u.starts_with("https://cdn/octave-440x248.jpg?t=")));
        assert_eq!(rows[1].paging, Paging::More("abc".into()));
        // A stream without a thumbnail keeps its tint only.
        let Tile::Stream(second) = &rows[1].tiles[0] else {
            panic!("stream expected")
        };
        assert_eq!(second.thumbnail, None);
        // The viewer lookup is cached: `/users` is hit once across loads (`expect(1)`).
        provider.load_feed(FeedId::Home).await;
    }

    #[tokio::test]
    async fn anonymous_gql_home_has_no_followed_row() {
        let server = MockServer::start().await;
        Mock::given(body_string_contains("streams(first"))
            .respond_with(json(serde_json::json!({ "data": { "streams": {
                "edges": [gql_stream("sable"), gql_stream("next")],
                "pageInfo": { "hasNextPage": false }
            }}})))
            .mount(&server)
            .await;
        Mock::given(body_string_contains("games(first"))
            .respond_with(json(serde_json::json!({ "data": { "games": {
                "edges": [{ "cursor": "g", "node": { "id": "509658", "name": "Just Chatting",
                    "boxArtURL": "https://cdn/jc-285x380.jpg" } }],
                "pageInfo": { "hasNextPage": true }
            }}})))
            .mount(&server)
            .await;

        let provider = ApiProvider::new(gql(&server));
        let rows = rows_of(provider.load_feed(FeedId::Home).await);
        assert_eq!(
            kinds(&rows),
            [RowKind::Featured, RowKind::TopLive, RowKind::Categories]
        );
        let Tile::Category(c) = &rows[2].tiles[0] else {
            panic!("category expected")
        };
        assert_eq!(
            (c.id.as_str(), c.name.as_str()),
            ("509658", "Just Chatting")
        );
        assert_eq!(c.thumbnail.as_deref(), Some("https://cdn/jc-285x380.jpg"));
        // GQL says another page exists, so the row carries a cursor. Asking for it is what fails.
        assert_eq!(rows[2].paging, Paging::More("g".into()));
        // Only the two row queries were sent: no viewer lookup, no follows.
        assert_eq!(server.received_requests().await.unwrap().len(), 2);
    }

    #[tokio::test]
    async fn anonymous_gql_later_pages_end_the_row_instead_of_failing_it() {
        let server = MockServer::start().await;
        // What gql.twitch.tv answers to any `after` cursor.
        Mock::given(body_string_contains("after"))
            .respond_with(json(serde_json::json!({
                "errors": [{ "message": "failed integrity check",
                            "extensions": { "code": "IntegrityCheckFailed" } }],
                "data": { "streams": null }
            })))
            .mount(&server)
            .await;

        let provider = ApiProvider::new(gql(&server));
        let event = provider
            .load_more(FeedId::Home, RowKind::TopLive, "c".into())
            .await;
        assert!(matches!(
            event,
            Event::MoreLoaded {
                outcome: PageOutcome::End,
                ..
            }
        ));
    }

    #[tokio::test]
    async fn helix_total_failure_is_an_event() {
        let server = MockServer::start().await;
        Mock::given(wiremock::matchers::any())
            .respond_with(ResponseTemplate::new(401).set_body_json(serde_json::json!({
                "error": "Unauthorized", "status": 401, "message": "Invalid OAuth token"
            })))
            .mount(&server)
            .await;
        let cmd = Command::LoadFeed { feed: FeedId::Home };
        let events = ApiProvider::new(helix(&server)).execute(cmd).await;
        match &events[..] {
            [Event::FeedFailed {
                feed: FeedId::Home,
                message,
            }] => assert!(message.contains("Invalid OAuth token")),
            other => panic!("expected FeedFailed, got {other:?}"),
        }
    }

    // ---- the Following feed and the session's account ----------------------------------

    fn signed_in_viewer() -> Viewer {
        Viewer::new(std::sync::Mutex::new(Some(MOCK_USER_ID.to_owned())))
    }

    fn follows(logins: &[&str]) -> Vec<htpc_twitch_api::FollowedChannel> {
        logins
            .iter()
            .map(|l| htpc_twitch_api::FollowedChannel {
                broadcaster_id: format!("id-{l}"),
                broadcaster_login: (*l).into(),
                broadcaster_name: l.to_uppercase(),
            })
            .collect()
    }

    #[tokio::test]
    async fn following_lists_the_live_channels_then_the_offline_ones() {
        let provider = ApiProvider::new(fake_api()).with_account(signed_in_viewer());
        let rows = rows_of(provider.load_feed(FeedId::Following).await);
        assert_eq!(
            kinds(&rows),
            [RowKind::FollowedLive, RowKind::FollowedOffline]
        );
        let live: Vec<_> = rows[0].tiles.iter().map(channel_of).collect();
        let want = [
            "nightowl_dev",
            "pixelpilgrim",
            "marrow",
            "lofi_lena",
            "quietcoast",
        ];
        assert_eq!(live, want);
        // The three who are not live, once each, under their display name.
        let off: Vec<_> = rows[1].tiles.iter().map(channel_of).collect();
        assert_eq!(off, ["sleepy_fox", "driftwood", "kettle_kai"]);
        let Tile::Channel(c) = &rows[1].tiles[0] else {
            panic!("an offline channel tile expected")
        };
        assert_eq!(c.name, "sleepy_fox");
        assert_eq!(c.tint, tint_for("sleepy_fox"));
        assert!(rows.iter().all(|r| r.paging == Paging::End));
    }

    fn channel_of(tile: &Tile) -> &str {
        match tile {
            Tile::Stream(s) => &s.channel,
            Tile::Channel(c) => &c.channel,
            Tile::Category(c) => &c.id,
        }
    }

    #[tokio::test]
    async fn a_live_channel_is_not_repeated_among_the_offline_ones() {
        let mut api = fake_api();
        // Case differs between the two endpoints, as it can in practice.
        api.follows = follows(&["NightOwl_Dev", "sleepy_fox"]);
        let provider = ApiProvider::new(api).with_account(signed_in_viewer());
        let rows = rows_of(provider.load_feed(FeedId::Following).await);
        let off: Vec<_> = rows[1].tiles.iter().map(channel_of).collect();
        assert_eq!(off, ["sleepy_fox"]);
    }

    #[tokio::test]
    async fn following_with_nobody_live_shows_only_the_offline_row() {
        let mut api = fake_api();
        api.followed.clear();
        let provider = ApiProvider::new(api).with_account(signed_in_viewer());
        let rows = rows_of(provider.load_feed(FeedId::Following).await);
        assert_eq!(kinds(&rows), [RowKind::FollowedOffline]);
    }

    #[tokio::test]
    async fn following_reads_every_page_of_a_long_list() {
        let mut api = fake_api();
        api.followed.clear();
        let names: Vec<String> = (0..250).map(|i| format!("chan{i}")).collect();
        let names: Vec<&str> = names.iter().map(String::as_str).collect();
        api.follows = follows(&names);
        let provider = ApiProvider::new(api).with_account(signed_in_viewer());
        let rows = rows_of(provider.load_feed(FeedId::Following).await);
        assert_eq!(rows[0].tiles.len(), 250, "three pages of 100, 100 and 50");
    }

    #[tokio::test]
    async fn following_while_signed_out_is_an_error_not_an_empty_tab() {
        let provider = ApiProvider::new(fake_api()).with_account(Viewer::default());
        match provider.load_feed(FeedId::Following).await {
            Event::FeedFailed {
                feed: FeedId::Following,
                message,
            } => assert!(message.contains("Sign in")),
            other => panic!("expected FeedFailed, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn following_fails_with_the_reason_when_the_live_streams_cannot_be_read() {
        let mut api = fake_api();
        // No token owner: both follow endpoints answer "not signed in".
        api.me = None;
        let provider = ApiProvider::new(api).with_account(signed_in_viewer());
        match provider.load_feed(FeedId::Following).await {
            Event::FeedFailed {
                feed: FeedId::Following,
                message,
            } => assert!(message.contains("not signed in"), "{message}"),
            other => panic!("expected FeedFailed, got {other:?}"),
        }
    }

    // ---- search ------------------------------------------------------------------------

    async fn search(provider: &ApiProvider<impl TwitchApi>, text: &str) -> Vec<Event> {
        let cmd = Command::Search {
            id: 7,
            query: text.into(),
        };
        provider.execute(cmd).await
    }

    fn search_rows(events: Vec<Event>) -> Vec<Row> {
        match &events[..] {
            [Event::SearchResults { id: 7, rows }] => rows.clone(),
            other => panic!("expected search results, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn mock_search_splits_live_categories_and_offline() {
        let mut api = fake_api();
        api.follows = follows(&["marrow", "marrowfan_tv"]);
        let provider = ApiProvider::new(api);
        let rows = search_rows(search(&provider, "MARROW").await);
        assert_eq!(kinds(&rows), [RowKind::SearchLive, RowKind::SearchOffline]);
        // The live one is a full stream card (from `/streams`), and is not repeated as offline.
        let Tile::Stream(live) = &rows[0].tiles[0] else {
            panic!("a stream card expected")
        };
        assert_eq!((live.channel.as_str(), live.viewers), ("marrow", 1_900));
        let off: Vec<_> = rows[1].tiles.iter().map(channel_of).collect();
        assert_eq!(off, ["marrowfan_tv"]);
        assert!(matches!(rows[1].tiles[0], Tile::Channel(_)));

        let rows = search_rows(search(&provider, "music").await);
        assert_eq!(kinds(&rows), [RowKind::SearchCategories]);
        assert!(search_rows(search(&provider, "zzzz").await).is_empty());
    }

    #[tokio::test]
    async fn a_short_search_asks_nothing() {
        let provider = ApiProvider::new(fake_api());
        assert!(search(&provider, " a ").await.is_empty());
    }

    #[tokio::test]
    async fn signed_out_search_fails_with_the_reason() {
        let server = MockServer::start().await;
        let provider = ApiProvider::new(gql(&server));
        match &search(&provider, "abc").await[..] {
            [Event::SearchFailed { id: 7, message }] => {
                assert!(message.contains("not signed in"), "{message}")
            }
            other => panic!("expected a failure, got {other:?}"),
        }
        assert!(server.received_requests().await.unwrap().is_empty());
    }

    #[tokio::test]
    async fn helix_search_enriches_live_results_and_keeps_the_ones_it_cannot_find() {
        let server = MockServer::start().await;
        let channel = |login: &str, live: bool| {
            serde_json::json!({ "broadcaster_login": login, "display_name": login.to_uppercase(),
                "id": format!("id-{login}"), "is_live": live, "game_name": "Chess",
                "title": format!("{login} title"), "thumbnail_url": "" })
        };
        Mock::given(path("/search/channels"))
            .and(query_param("live_only", "true"))
            .respond_with(json(serde_json::json!({
                "data": [channel("found", true), channel("gone", true)],
                "pagination": { "cursor": "next" }
            })))
            .mount(&server)
            .await;
        Mock::given(path("/search/channels"))
            .and(query_param("live_only", "false"))
            .respond_with(json(serde_json::json!({
                "data": [channel("found", true), channel("sleeper", false)],
                "pagination": {}
            })))
            .mount(&server)
            .await;
        Mock::given(path("/search/categories"))
            .respond_with(ResponseTemplate::new(503))
            .mount(&server)
            .await;
        Mock::given(path("/streams"))
            .and(query_param("user_login", "found"))
            .respond_with(json(serde_json::json!({ "data": [
                { "user_id": "1", "user_login": "found", "user_name": "Found",
                  "game_name": "Chess", "title": "live now", "viewer_count": 321,
                  "thumbnail_url": "https://cdn/found-{width}x{height}.jpg" }
            ]})))
            .mount(&server)
            .await;

        let provider = ApiProvider::new(helix(&server));
        let rows = search_rows(search(&provider, "fo").await);
        // The failed categories row is left out.
        assert_eq!(kinds(&rows), [RowKind::SearchLive, RowKind::SearchOffline]);
        assert_eq!(rows[0].paging, Paging::More("next".into()));
        let cards: Vec<_> = rows[0]
            .tiles
            .iter()
            .map(|t| match t {
                Tile::Stream(s) => (s.channel.as_str(), s.viewers, s.thumbnail.is_some()),
                other => panic!("stream expected, got {other:?}"),
            })
            .collect();
        assert_eq!(cards, [("found", 321, true), ("gone", 0, false)]);
        assert_eq!(channel_of(&rows[1].tiles[0]), "sleeper");

        // The next page of the live row goes to the same text, with the cursor.
        let event = provider
            .load_more(
                FeedId::Search("fo".into()),
                RowKind::SearchLive,
                "next".into(),
            )
            .await;
        let Event::MoreLoaded {
            outcome: PageOutcome::Page { tiles, .. },
            ..
        } = event
        else {
            panic!("a page was expected, got {event:?}");
        };
        assert_eq!(tiles.len(), 2);
    }

    #[tokio::test]
    async fn followed_live_lists_every_live_followed_channel_while_signed_in() {
        let provider = ApiProvider::new(fake_api()).with_account(signed_in_viewer());
        let live = provider.followed_live().await.expect("signed in");
        let names: Vec<_> = live.iter().map(|s| s.channel.as_str()).collect();
        assert_eq!(names[0], "nightowl_dev");
        assert_eq!(live.len(), 5);

        let signed_out = ApiProvider::new(fake_api()).with_account(Viewer::default());
        assert_eq!(signed_out.followed_live().await, None);
    }

    #[tokio::test]
    async fn the_account_decides_the_followed_row_on_every_load() {
        let viewer = Viewer::default();
        let provider = ApiProvider::new(fake_api()).with_account(Arc::clone(&viewer));
        let signed_out = rows_of(provider.load_feed(FeedId::Home).await);
        assert!(!kinds(&signed_out).contains(&RowKind::FollowedLive));

        // The session signs in: the very next load has the row, with no new provider.
        *viewer.lock().unwrap() = Some(MOCK_USER_ID.to_owned());
        let signed_in = rows_of(provider.load_feed(FeedId::Home).await);
        assert!(kinds(&signed_in).contains(&RowKind::FollowedLive));

        // And it is gone again after the sign-out.
        *viewer.lock().unwrap() = None;
        let again = rows_of(provider.load_feed(FeedId::Home).await);
        assert!(!kinds(&again).contains(&RowKind::FollowedLive));
    }

    fn fetch_info(channel: &str) -> Command {
        Command::FetchStreamInfo {
            channel: channel.into(),
        }
    }

    #[tokio::test]
    async fn stream_info_has_the_live_stream_and_its_user_id() {
        let mut api = FakeApi::new();
        let mut live = api_with_top(1).top.remove(0);
        live.user_login = "next_up".into();
        live.user_id = "200".into();
        live.title = "Just got raided".into();
        live.game_name = "Art".into();
        api.top = vec![live];
        let events = ApiProvider::new(api).execute(fetch_info("Next_Up")).await;
        let [Event::StreamInfo(s)] = events.as_slice() else {
            panic!("one StreamInfo expected, got {events:?}");
        };
        assert_eq!((s.channel.as_str(), s.user_id.as_str()), ("next_up", "200"));
        assert_eq!(
            (s.title.as_str(), s.category.as_str()),
            ("Just got raided", "Art")
        );
    }

    #[tokio::test]
    async fn stream_info_of_an_offline_channel_still_has_its_user_id() {
        let mut api = FakeApi::new();
        api.users = vec![htpc_twitch_api::UserInfo {
            id: "555".into(),
            login: "sleepy".into(),
            display_name: "Sleepy".into(),
            profile_image_url: String::new(),
        }];
        let provider = ApiProvider::new(api);
        let events = provider.execute(fetch_info("sleepy")).await;
        let [Event::StreamInfo(s)] = events.as_slice() else {
            panic!("one StreamInfo expected, got {events:?}");
        };
        assert_eq!(s.user_id, "555");
        assert!(s.title.is_empty() && s.viewers == 0);
        // Nobody by that name: nothing to report.
        assert!(provider.execute(fetch_info("nobody")).await.is_empty());
    }

    fn send(id: u32, broadcaster_id: &str, text: &str) -> Command {
        Command::SendChat {
            id,
            channel: "sable".into(),
            broadcaster_id: broadcaster_id.into(),
            text: text.into(),
        }
    }

    #[tokio::test]
    async fn send_chat_posts_as_the_signed_in_user() {
        let api = fake_api();
        let sent = std::sync::Arc::clone(&api.chat_sent);
        let provider = ApiProvider::new(api);
        let events = provider.execute(send(7, "id-sable", "gg")).await;
        assert_eq!(events, [Event::ChatSent { id: 7 }]);
        assert_eq!(
            *sent.lock().unwrap(),
            [("id-sable".to_owned(), "gg".to_owned())]
        );
    }

    #[tokio::test]
    async fn a_missing_broadcaster_id_is_looked_up_first() {
        let mut api = fake_api();
        api.users = vec![htpc_twitch_api::UserInfo {
            id: "777".into(),
            login: "sable".into(),
            display_name: "Sable".into(),
            profile_image_url: String::new(),
        }];
        let sent = std::sync::Arc::clone(&api.chat_sent);
        let provider = ApiProvider::new(api);
        let events = provider.execute(send(1, "", "hi")).await;
        assert_eq!(events, [Event::ChatSent { id: 1 }]);
        assert_eq!(sent.lock().unwrap()[0].0, "777");
    }

    #[tokio::test]
    async fn a_dropped_message_carries_twitchs_reason() {
        let mut api = fake_api();
        api.chat_drop = Some(htpc_twitch_api::DropReason {
            code: "msg_followersonly".into(),
            message: "This room is in followers-only mode.".into(),
        });
        let provider = ApiProvider::new(api);
        let events = provider.execute(send(3, "id-sable", "hello")).await;
        assert_eq!(
            events,
            [Event::ChatDropped {
                id: 3,
                problem: SendProblem::Dropped {
                    code: "msg_followersonly".into(),
                    message: "This room is in followers-only mode.".into(),
                },
            }]
        );
    }

    #[tokio::test]
    async fn the_twenty_first_message_in_thirty_seconds_is_held_back() {
        let api = fake_api();
        let sent = std::sync::Arc::clone(&api.chat_sent);
        let provider = ApiProvider::new(api);
        for id in 0..20 {
            let events = provider.execute(send(id, "id-sable", "LUL")).await;
            assert_eq!(events, [Event::ChatSent { id }]);
        }
        let events = provider.execute(send(20, "id-sable", "LUL")).await;
        assert_eq!(
            events,
            [Event::ChatDropped {
                id: 20,
                problem: SendProblem::TooFast,
            }]
        );
        assert_eq!(sent.lock().unwrap().len(), 20, "the 21st never left");
    }

    #[tokio::test]
    async fn sending_signed_out_fails_without_a_request() {
        let api = FakeApi {
            me: None,
            ..fake_api()
        };
        let sent = std::sync::Arc::clone(&api.chat_sent);
        let provider = ApiProvider::new(api);
        let events = provider.execute(send(1, "id-sable", "hi")).await;
        assert!(
            matches!(
                events.as_slice(),
                [Event::ChatDropped {
                    id: 1,
                    problem: SendProblem::Failed(_)
                }]
            ),
            "{events:?}"
        );
        assert!(sent.lock().unwrap().is_empty());
    }
}
