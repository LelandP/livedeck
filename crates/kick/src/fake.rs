//! `FakeKick`: canned Kick data for tests and `--mock`.

use std::sync::{Arc, Mutex};

use crate::model::{ChannelInfo, LiveStream, LivestreamPage, PusherApp, SearchHit};
use crate::{Error, KickApi, Result};

#[derive(Clone, Default)]
pub struct FakeKick {
    inner: Arc<Mutex<Inner>>,
}

#[derive(Default)]
struct Inner {
    /// Top Live, in pages.
    pages: Vec<Vec<LiveStream>>,
    /// Channels that exist but are not live.
    offline: Vec<(String, String)>,
    /// Every call fails with this.
    failing: Option<Error>,
    calls: usize,
}

impl FakeKick {
    pub fn new() -> Self {
        Self::default()
    }

    /// `n` live streams, `per_page` to a page, named `kick_<i>`, most viewers first.
    pub fn with_top(self, n: usize, per_page: usize) -> Self {
        let streams: Vec<_> = (0..n)
            .map(|i| live(&format!("kick_{i}"), (n - i) as u64 * 100))
            .collect();
        self.with_pages(streams.chunks(per_page.max(1)).map(<[_]>::to_vec).collect())
    }

    pub fn with_pages(self, pages: Vec<Vec<LiveStream>>) -> Self {
        self.lock().pages = pages;
        self
    }

    /// A channel that is known but not live.
    pub fn with_offline(self, slug: &str, name: &str) -> Self {
        self.lock().offline.push((slug.to_owned(), name.to_owned()));
        self
    }

    pub fn failing(self, error: Error) -> Self {
        self.lock().failing = Some(error);
        self
    }

    /// How many requests were made. "Show Kick" off must mean none.
    pub fn calls(&self) -> usize {
        self.lock().calls
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, Inner> {
        self.inner.lock().unwrap_or_else(|e| e.into_inner())
    }

    fn start(&self) -> Result<std::sync::MutexGuard<'_, Inner>> {
        let mut inner = self.lock();
        inner.calls += 1;
        match &inner.failing {
            Some(err) => Err(err.clone()),
            None => Ok(inner),
        }
    }
}

/// A live stream with made-up details.
pub fn live(slug: &str, viewers: u64) -> LiveStream {
    LiveStream {
        slug: slug.to_owned(),
        name: slug.to_owned(),
        channel_id: 1000 + viewers,
        title: format!("{slug} live on Kick"),
        category: "Just Chatting".to_owned(),
        viewers,
        thumbnail: None,
    }
}

impl KickApi for FakeKick {
    async fn top_livestreams(&self, page: u32) -> Result<LivestreamPage> {
        let inner = self.start()?;
        let idx = page.max(1) as usize - 1;
        let streams = inner.pages.get(idx).cloned().unwrap_or_default();
        let next_page = (idx + 1 < inner.pages.len()).then_some(page.max(1) + 1);
        Ok(LivestreamPage { streams, next_page })
    }

    async fn channel(&self, slug: &str) -> Result<Option<ChannelInfo>> {
        let inner = self.start()?;
        if let Some(s) = inner.pages.iter().flatten().find(|s| s.slug == slug) {
            return Ok(Some(ChannelInfo {
                id: s.channel_id,
                slug: s.slug.clone(),
                name: s.name.clone(),
                chatroom_id: s.channel_id,
                live: Some(s.clone()),
            }));
        }
        Ok(inner
            .offline
            .iter()
            .find(|(s, _)| s == slug)
            .map(|(slug, name)| ChannelInfo {
                id: 1,
                slug: slug.clone(),
                name: name.clone(),
                chatroom_id: 1,
                live: None,
            }))
    }

    async fn search_channels(&self, query: &str) -> Result<Vec<SearchHit>> {
        let inner = self.start()?;
        let q = query.to_ascii_lowercase();
        let live = inner
            .pages
            .iter()
            .flatten()
            .map(|s| (&s.slug, &s.name, true));
        let offline = inner.offline.iter().map(|(s, n)| (s, n, false));
        Ok(live
            .chain(offline)
            .filter(|(slug, _, _)| slug.contains(&q))
            .map(|(slug, name, is_live)| SearchHit {
                slug: slug.clone(),
                name: name.clone(),
                is_live,
            })
            .collect())
    }

    async fn chat_app(&self, _channel_id: u64) -> Result<PusherApp> {
        drop(self.start()?);
        Ok(crate::web::fallback_pusher_app())
    }
}
