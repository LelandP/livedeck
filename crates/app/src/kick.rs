//! Kick as a source of rows: maps `htpc-kick-api` to domain tiles for the provider, which puts
//! them next to Twitch's (`provider.rs`). Read-only and signed out: Top Live and channel search.

use std::future::Future;
use std::pin::Pin;

use htpc_core::{Channel, ChannelRef, Stream, Tile};
use htpc_kick_api::{KickApi, LiveStream, SearchHit};

use crate::provider::tint_for;

pub type BoxFuture<'a, T> = Pin<Box<dyn Future<Output = T> + Send + 'a>>;

/// One page of tiles and the page after it, or why not.
pub type TopLive = Result<(Vec<Tile>, Option<u32>), String>;
/// Live tiles, then the others, or why not.
pub type Found = Result<(Vec<Tile>, Vec<Tile>), String>;

/// Live search results looked up for their viewer counts and previews (one request each).
/// Further live results still get a card, from what the search said.
const SEARCH_LOOKUPS: usize = 6;

/// What the provider needs from Kick, as domain tiles. Object-safe, so the provider holds any
/// `KickApi` (the real client, or `FakeKick` in `--mock` and tests) without a type parameter.
pub trait KickFeed: Send + Sync + 'static {
    /// One page of Top Live (from 1), and the page after it if there is one.
    fn top_live(&self, page: u32) -> BoxFuture<'_, TopLive>;

    /// Channels matching `query`: the live ones, then the others.
    fn search<'a>(&'a self, query: &'a str) -> BoxFuture<'a, Found>;

    /// The channel `slug` (a favorite): its live stream, or a dimmed card when it is not live.
    /// A channel Kick does not know, or a failed lookup (logged), is a dimmed card too.
    fn channel<'a>(&'a self, slug: &'a str) -> BoxFuture<'a, Tile>;
}

pub struct KickSource<K> {
    api: K,
}

impl<K: KickApi> KickSource<K> {
    pub fn new(api: K) -> Self {
        Self { api }
    }

    async fn live_tile(&self, hit: &SearchHit) -> Tile {
        match self.api.channel(&hit.slug).await {
            Ok(Some(info)) if info.live.is_some() => stream_tile(info.live.as_ref().unwrap()),
            Ok(_) => hit_tile(hit),
            Err(err) => {
                tracing::debug!(slug = %hit.slug, %err, "could not look up a live Kick result");
                hit_tile(hit)
            }
        }
    }
}

impl<K: KickApi> KickFeed for KickSource<K> {
    fn top_live(&self, page: u32) -> BoxFuture<'_, TopLive> {
        Box::pin(async move {
            let page = self
                .api
                .top_livestreams(page)
                .await
                .map_err(|e| e.to_string())?;
            Ok((
                page.streams.iter().map(stream_tile).collect(),
                page.next_page,
            ))
        })
    }

    fn search<'a>(&'a self, query: &'a str) -> BoxFuture<'a, Found> {
        Box::pin(async move {
            let hits = self
                .api
                .search_channels(query)
                .await
                .map_err(|e| e.to_string())?;
            let mut live = Vec::new();
            for (i, hit) in hits.iter().filter(|h| h.is_live).enumerate() {
                live.push(if i < SEARCH_LOOKUPS {
                    self.live_tile(hit).await
                } else {
                    hit_tile(hit)
                });
            }
            let offline = hits
                .iter()
                .filter(|h| !h.is_live)
                .map(|h| offline_tile(&h.slug, &h.name))
                .collect();
            Ok((live, offline))
        })
    }

    fn channel<'a>(&'a self, slug: &'a str) -> BoxFuture<'a, Tile> {
        Box::pin(async move {
            match self.api.channel(slug).await {
                Ok(Some(info)) => match &info.live {
                    Some(live) => stream_tile(live),
                    None => offline_tile(&info.slug, &info.name),
                },
                Ok(None) => offline_tile(slug, slug),
                Err(err) => {
                    tracing::debug!(slug, %err, "could not look up a Kick favorite");
                    offline_tile(slug, slug)
                }
            }
        })
    }
}

/// A Kick channel that is not live: a dimmed card.
fn offline_tile(slug: &str, name: &str) -> Tile {
    Tile::Channel(Channel {
        channel: ChannelRef::kick(slug),
        name: name.to_owned(),
        tint: tint_for(slug),
    })
}

fn stream_tile(s: &LiveStream) -> Tile {
    Tile::Stream(Stream {
        channel: ChannelRef::kick(&s.slug),
        user_id: s.channel_id.to_string(),
        title: s.title.clone(),
        category: s.category.clone(),
        viewers: s.viewers,
        tint: tint_for(&s.slug),
        thumbnail: s.thumbnail.clone(),
    })
}

/// A live search result that could not be looked up: a card without viewers or a preview.
fn hit_tile(hit: &SearchHit) -> Tile {
    Tile::Stream(Stream {
        channel: ChannelRef::kick(&hit.slug),
        user_id: String::new(),
        title: String::new(),
        category: String::new(),
        viewers: 0,
        tint: tint_for(&hit.slug),
        thumbnail: None,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use htpc_core::Platform;
    use htpc_kick_api::{fake_live, FakeKick};

    #[tokio::test]
    async fn top_live_is_kick_streams_with_the_next_page() {
        let source = KickSource::new(FakeKick::new().with_top(5, 3));
        let (tiles, next) = source.top_live(1).await.unwrap();
        assert_eq!(tiles.len(), 3);
        assert_eq!(next, Some(2));
        let Tile::Stream(s) = &tiles[0] else {
            panic!("a stream")
        };
        assert_eq!(s.channel, ChannelRef::kick("kick_0"));
        assert_eq!(s.viewers, 500);
        assert_eq!(source.top_live(2).await.unwrap().1, None);
    }

    #[tokio::test]
    async fn search_splits_live_and_offline_kick_channels() {
        let kick = FakeKick::new()
            .with_pages(vec![vec![fake_live("foxy", 42)]])
            .with_offline("foxhole", "FoxHole");
        let (live, offline) = KickSource::new(kick).search("fox").await.unwrap();
        assert_eq!(live.len(), 1);
        let Tile::Stream(s) = &live[0] else {
            panic!("a stream")
        };
        assert_eq!((s.channel.platform, s.viewers), (Platform::Kick, 42));
        assert_eq!(offline.len(), 1);
        assert_eq!(offline[0].key(), (Platform::Kick, "foxhole"));
    }

    #[tokio::test]
    async fn a_favorite_is_its_live_stream_or_a_dimmed_card() {
        let kick = FakeKick::new()
            .with_pages(vec![vec![fake_live("foxy", 42)]])
            .with_offline("foxhole", "FoxHole");
        let source = KickSource::new(kick);
        let Tile::Stream(s) = source.channel("foxy").await else {
            panic!("live")
        };
        assert_eq!(s.viewers, 42);
        let Tile::Channel(c) = source.channel("foxhole").await else {
            panic!("offline")
        };
        assert_eq!(
            (c.channel, c.name.as_str()),
            (ChannelRef::kick("foxhole"), "FoxHole")
        );
        assert!(matches!(source.channel("nobody").await, Tile::Channel(_)));
    }

    #[tokio::test]
    async fn a_failing_kick_is_an_error_not_a_panic() {
        let kick = FakeKick::new().failing(htpc_kick_api::Error::Blocked);
        let source = KickSource::new(kick);
        assert!(source.top_live(1).await.is_err());
        assert!(source.search("x").await.is_err());
    }
}
