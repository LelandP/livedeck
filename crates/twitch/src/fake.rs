//! `FakeApi`: an in-memory [`TwitchApi`] for tests and `--mock`.
//!
//! It pages through its lists like Helix does: the cursor is the index of the next item as a
//! string, and the last page has no cursor.

use std::sync::{Arc, Mutex};

use crate::{
    ChannelResult, ChannelVideos, ClipInfo, DropReason, Error, FollowedChannel, Game, HypeTrain,
    LiveArchive, Page, PageRequest, Result, SendOutcome, StreamInfo, TwitchApi, UserInfo,
    VideoChapter, VideoDetails, VideoInfo, VideoType,
};

#[derive(Debug, Clone, Default)]
pub struct FakeApi {
    pub top: Vec<StreamInfo>,
    /// The live streams of followed channels (`followed_streams`).
    pub followed: Vec<StreamInfo>,
    /// Every followed channel, live or not (`followed_channels`).
    pub follows: Vec<FollowedChannel>,
    pub games: Vec<Game>,
    pub users: Vec<UserInfo>,
    /// The token owner returned by `user(None)`. `None` behaves as signed out.
    pub me: Option<UserInfo>,
    /// Every chat message posted, as `(broadcaster_id, text)`. Shared between clones.
    pub chat_sent: Arc<Mutex<Vec<(String, String)>>>,
    /// Drop every chat message with this reason instead of posting it.
    pub chat_drop: Option<DropReason>,
    /// The hype trains running, by channel login.
    pub hype_trains: Vec<(String, HypeTrain)>,
    /// Every channel's videos, newest first. `owner_login` says whose. A `recording` archive is
    /// the live stream's.
    pub videos: Vec<VideoInfo>,
    /// The top clips of the last 7 days, by channel login.
    pub clips_week: Vec<(String, ClipInfo)>,
    /// The top clips ever, by channel login.
    pub clips_all_time: Vec<(String, ClipInfo)>,
    /// Chapter markers, by video id. A video without any gets one chapter, as GQL's does.
    pub chapters: Vec<(String, Vec<VideoChapter>)>,
}

impl FakeApi {
    pub fn new() -> Self {
        Self::default()
    }
}

/// Cuts one page out of `items`. Unknown cursors are an error, like Helix's 400.
fn page_of<T: Clone>(items: &[T], req: &PageRequest) -> Result<Page<T>> {
    let start = match &req.after {
        None => 0,
        Some(c) => c
            .parse::<usize>()
            .ok()
            .filter(|&i| i <= items.len())
            .ok_or_else(|| Error::Api {
                status: 400,
                message: format!("invalid cursor {c:?}"),
            })?,
    };
    let end = (start + usize::from(req.first)).min(items.len());
    Ok(Page {
        items: items[start..end].to_vec(),
        cursor: (end < items.len()).then(|| end.to_string()),
    })
}

impl TwitchApi for FakeApi {
    async fn top_streams(&self, page: PageRequest) -> Result<Page<StreamInfo>> {
        page_of(&self.top, &page)
    }

    async fn followed_streams(&self, user_id: &str, page: PageRequest) -> Result<Page<StreamInfo>> {
        match &self.me {
            Some(me) if me.id == user_id => page_of(&self.followed, &page),
            Some(_) => Err(Error::Api {
                status: 400,
                message: "user_id must match the token owner".into(),
            }),
            None => Err(Error::NotSignedIn),
        }
    }

    async fn followed_channels(
        &self,
        user_id: &str,
        page: PageRequest,
    ) -> Result<Page<FollowedChannel>> {
        match &self.me {
            Some(me) if me.id == user_id => page_of(&self.follows, &page),
            Some(_) => Err(Error::Api {
                status: 400,
                message: "user_id must match the token owner".into(),
            }),
            None => Err(Error::NotSignedIn),
        }
    }

    async fn top_categories(&self, page: PageRequest) -> Result<Page<Game>> {
        page_of(&self.games, &page)
    }

    async fn streams_by_category(
        &self,
        game_id: &str,
        page: PageRequest,
    ) -> Result<Page<StreamInfo>> {
        let all: Vec<_> = self
            .top
            .iter()
            .chain(&self.followed)
            .filter(|s| s.game_id == game_id)
            .cloned()
            .collect();
        page_of(&all, &page)
    }

    async fn user(&self, login: Option<&str>) -> Result<Option<UserInfo>> {
        match login {
            None => self.me.clone().map(Some).ok_or(Error::NotSignedIn),
            Some(login) => Ok(self
                .users
                .iter()
                .chain(&self.me)
                .find(|u| u.login.eq_ignore_ascii_case(login))
                .cloned()),
        }
    }

    /// Matches the live streams (`top`, `followed`) and the followed channels by name. Case,
    /// spaces and punctuation are ignored, as Twitch does.
    async fn search_channels(
        &self,
        query: &str,
        live_only: bool,
        page: PageRequest,
    ) -> Result<Page<ChannelResult>> {
        let mut seen = std::collections::HashSet::new();
        let live = self
            .live()
            .filter(|s| matches(query, &s.user_login, &s.user_name));
        let mut found: Vec<ChannelResult> = live
            .map(|s| ChannelResult {
                id: s.user_id.clone(),
                broadcaster_login: s.user_login.clone(),
                display_name: s.user_name.clone(),
                game_name: s.game_name.clone(),
                is_live: true,
                title: s.title.clone(),
                thumbnail_url: String::new(),
            })
            .filter(|c| seen.insert(c.broadcaster_login.to_ascii_lowercase()))
            .collect();
        if !live_only {
            let offline = self
                .follows
                .iter()
                .filter(|c| matches(query, &c.broadcaster_login, &c.broadcaster_name))
                .map(|c| ChannelResult {
                    id: c.broadcaster_id.clone(),
                    broadcaster_login: c.broadcaster_login.clone(),
                    display_name: c.broadcaster_name.clone(),
                    game_name: String::new(),
                    is_live: false,
                    title: String::new(),
                    thumbnail_url: String::new(),
                })
                .filter(|c| seen.insert(c.broadcaster_login.to_ascii_lowercase()));
            found.extend(offline.collect::<Vec<_>>());
        }
        page_of(&found, &page)
    }

    async fn search_categories(&self, query: &str, page: PageRequest) -> Result<Page<Game>> {
        let found: Vec<Game> = self
            .games
            .iter()
            .filter(|g| matches(query, &g.name, &g.name))
            .cloned()
            .collect();
        page_of(&found, &page)
    }

    async fn streams_by_login(&self, logins: &[String]) -> Result<Vec<StreamInfo>> {
        Ok(self
            .live()
            .filter(|s| logins.iter().any(|l| l.eq_ignore_ascii_case(&s.user_login)))
            .cloned()
            .collect())
    }

    /// Posts as the token owner only, like Helix. Nothing echoes back into a chat.
    async fn send_chat(
        &self,
        broadcaster_id: &str,
        sender_id: &str,
        text: &str,
    ) -> Result<SendOutcome> {
        match &self.me {
            Some(me) if me.id == sender_id => {}
            Some(_) => {
                return Err(Error::Api {
                    status: 403,
                    message: "sender_id must match the token owner".into(),
                })
            }
            None => return Err(Error::NotSignedIn),
        }
        if let Some(reason) = &self.chat_drop {
            return Ok(SendOutcome {
                message_id: String::new(),
                sent: false,
                drop_reason: Some(reason.clone()),
            });
        }
        let mut sent = self.chat_sent.lock().unwrap_or_else(|e| e.into_inner());
        sent.push((broadcaster_id.to_owned(), text.to_owned()));
        Ok(SendOutcome {
            message_id: format!("fake-{}", sent.len()),
            sent: true,
            drop_reason: None,
        })
    }

    async fn hype_train(&self, login: &str) -> Result<Option<HypeTrain>> {
        Ok(self
            .hype_trains
            .iter()
            .find(|(l, _)| l.eq_ignore_ascii_case(login))
            .map(|(_, t)| t.clone()))
    }

    /// A channel is known if it is a user, live, or has videos. Every list is cut to 30, like
    /// GQL's first page.
    async fn channel_videos(&self, login: &str) -> Result<Option<ChannelVideos>> {
        let live = self
            .live()
            .find(|s| s.user_login.eq_ignore_ascii_case(login))
            .cloned();
        let videos: Vec<&VideoInfo> = self
            .videos
            .iter()
            .filter(|v| v.owner_login.eq_ignore_ascii_case(login))
            .collect();
        let user = self
            .users
            .iter()
            .chain(&self.me)
            .find(|u| u.login.eq_ignore_ascii_case(login))
            .cloned()
            .or_else(|| {
                live.as_ref().map(|s| UserInfo {
                    id: s.user_id.clone(),
                    login: s.user_login.clone(),
                    display_name: s.user_name.clone(),
                    profile_image_url: String::new(),
                })
            })
            .or_else(|| {
                videos.first().map(|v| UserInfo {
                    id: format!("id-{}", v.owner_login),
                    login: v.owner_login.clone(),
                    display_name: v.owner_name.clone(),
                    profile_image_url: String::new(),
                })
            });
        let Some(user) = user else {
            return Ok(None);
        };
        let of_kind = |kind| -> Vec<VideoInfo> {
            videos
                .iter()
                .filter(|v| v.kind == kind)
                .take(FAKE_PAGE)
                .map(|v| (*v).clone())
                .collect()
        };
        let clips = |list: &[(String, ClipInfo)]| -> Vec<ClipInfo> {
            list.iter()
                .filter(|(l, _)| l.eq_ignore_ascii_case(login))
                .take(FAKE_PAGE)
                .map(|(_, c)| c.clone())
                .collect()
        };
        let archives = of_kind(VideoType::Archive);
        Ok(Some(ChannelVideos {
            last_broadcast_at: archives.first().map(|v| v.created_at.clone()),
            live,
            archives,
            highlights: of_kind(VideoType::Highlight),
            clips_week: clips(&self.clips_week),
            clips_all_time: clips(&self.clips_all_time),
            user,
        }))
    }

    async fn video_details(&self, id: &str) -> Result<Option<VideoDetails>> {
        let Some(video) = self.videos.iter().find(|v| v.id == id) else {
            return Ok(None);
        };
        let chapters = self
            .chapters
            .iter()
            .find(|(v, _)| v == id)
            .map(|(_, c)| c.clone())
            .filter(|c| !c.is_empty())
            .unwrap_or_else(|| {
                vec![VideoChapter {
                    position_ms: 0,
                    duration_ms: u64::from(video.length_seconds) * 1000,
                    name: video.game_name.clone(),
                }]
            });
        Ok(Some(VideoDetails {
            id: video.id.clone(),
            length_seconds: video.length_seconds,
            chapters,
            muted: video.muted.clone(),
            seek_previews_url: String::new(),
        }))
    }

    /// The channel's `recording` archive while it is live. The stream "started" when the video was
    /// created.
    async fn live_archive(&self, login: &str) -> Result<Option<LiveArchive>> {
        if !self
            .live()
            .any(|s| s.user_login.eq_ignore_ascii_case(login))
        {
            return Ok(None);
        }
        Ok(self
            .videos
            .iter()
            .find(|v| v.owner_login.eq_ignore_ascii_case(login) && v.kind == VideoType::Archive)
            .filter(|v| v.recording)
            .map(|v| LiveArchive {
                video_id: v.id.clone(),
                stream_started_at: v.created_at.clone(),
            }))
    }
}

/// Items per row of a fake channel page: GQL's first page.
const FAKE_PAGE: usize = 30;

impl FakeApi {
    /// Every live stream the fake knows, Top Live first.
    fn live(&self) -> impl Iterator<Item = &StreamInfo> {
        self.top.iter().chain(&self.followed)
    }
}

/// True if `query` is part of `login` or `name`, comparing letters and digits only.
fn matches(query: &str, login: &str, name: &str) -> bool {
    let fold = |s: &str| -> String {
        s.chars()
            .filter(|c| c.is_alphanumeric())
            .flat_map(char::to_lowercase)
            .collect()
    };
    let q = fold(query);
    !q.is_empty() && (fold(login).contains(&q) || fold(name).contains(&q))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn stream(login: &str, game_id: &str) -> StreamInfo {
        StreamInfo {
            user_id: format!("id-{login}"),
            user_login: login.into(),
            user_name: login.into(),
            game_id: game_id.into(),
            game_name: format!("game {game_id}"),
            title: String::new(),
            viewer_count: 1,
            thumbnail_url: String::new(),
        }
    }

    #[tokio::test]
    async fn pages_like_helix() {
        let api = FakeApi {
            top: (0..5).map(|i| stream(&format!("s{i}"), "1")).collect(),
            ..FakeApi::new()
        };
        let p1 = api.top_streams(PageRequest::first(2)).await.unwrap();
        assert_eq!(p1.items.len(), 2);
        assert_eq!(p1.cursor.as_deref(), Some("2"));
        let p3 = api.top_streams(PageRequest::after(2, "4")).await.unwrap();
        assert_eq!(p3.items[0].user_login, "s4");
        assert_eq!(p3.cursor, None);
        assert!(api.top_streams(PageRequest::after(2, "x")).await.is_err());
    }

    #[tokio::test]
    async fn followed_needs_the_signed_in_user() {
        let api = FakeApi::new();
        let err = api.followed_streams("1", PageRequest::default()).await;
        assert!(matches!(err, Err(Error::NotSignedIn)));
        assert!(matches!(api.user(None).await, Err(Error::NotSignedIn)));
        assert_eq!(api.user(Some("nobody")).await.unwrap(), None);
    }

    #[tokio::test]
    async fn filters_by_category() {
        let api = FakeApi {
            top: vec![stream("a", "1"), stream("b", "2"), stream("c", "1")],
            ..FakeApi::new()
        };
        let page = api
            .streams_by_category("1", PageRequest::default())
            .await
            .unwrap();
        let logins: Vec<_> = page.items.iter().map(|s| s.user_login.as_str()).collect();
        assert_eq!(logins, ["a", "c"]);
    }

    #[tokio::test]
    async fn search_matches_names_loosely_and_splits_live_from_offline() {
        let api = FakeApi {
            top: vec![stream("counter_strike_fan", "1"), stream("other", "1")],
            follows: vec![
                FollowedChannel {
                    broadcaster_id: "9".into(),
                    broadcaster_login: "strikeout".into(),
                    broadcaster_name: "StrikeOut".into(),
                },
                // Live too: listed once, as live.
                FollowedChannel {
                    broadcaster_id: "1".into(),
                    broadcaster_login: "counter_strike_fan".into(),
                    broadcaster_name: "counter_strike_fan".into(),
                },
            ],
            games: vec![Game {
                id: "32399".into(),
                name: "Counter-Strike".into(),
                box_art_url: String::new(),
            }],
            ..FakeApi::new()
        };
        let all = api
            .search_channels("STRIKE", false, PageRequest::default())
            .await
            .unwrap();
        let found: Vec<_> = all
            .items
            .iter()
            .map(|c| (c.broadcaster_login.as_str(), c.is_live))
            .collect();
        assert_eq!(found, [("counter_strike_fan", true), ("strikeout", false)]);
        let live = api
            .search_channels("strike", true, PageRequest::default())
            .await
            .unwrap();
        assert_eq!(live.items.len(), 1);

        let games = api
            .search_categories("counter strike", PageRequest::default())
            .await
            .unwrap();
        assert_eq!(games.items[0].id, "32399");

        let streams = api
            .streams_by_login(&["Counter_Strike_Fan".into(), "nobody".into()])
            .await
            .unwrap();
        assert_eq!(streams.len(), 1);
    }
}
