//! Playback commands: resolve a channel with the `StreamResolver`, then hand the playlist to the
//! slot's player in the `PlayerPool`. Player events come back as core `Event`s carrying the slot.
//! Slot 0 (`MAIN_SLOT`) is the single player; multiview's tiles use the others too.

use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use std::time::Duration;

use htpc_core::{
    ChannelRef, Command, Event, Health, Hwdec, Latency, Platform, PlayerOption, PlayerStats,
    StreamKind, Zoom,
};
use htpc_player::{LoadOptions, PlayerEvent, PlayerPool, Slot, SLOTS};
use htpc_resolver::{ResolveError, Site, StreamResolver};

/// How long the offline screen waits before looking for the stream again
/// (`Command::ScheduleOfflineRecheck`). The player's end screen says this number too.
pub const OFFLINE_RECHECK: Duration = Duration::from_secs(10);

/// How long multiview's layout must stay put before tiles reload at their new quality
/// (`Command::SettleQuality`, plan.md section 3).
pub const QUALITY_SETTLE: Duration = Duration::from_secs(2);

/// Streamlink resolves running at once. Starting multiview with nine streams would otherwise ask
/// Twitch nine times together, and risk its rate limits (plan.md, section 7). The reducer sends
/// the stream with sound first, so it starts first.
pub const RESOLVES_AT_ONCE: usize = 3;

/// Runs `Play`, `SetPaused`, `SetMuted`, `StopPlayback`, `SetPlayerOption` and `SetStats`. Returns
/// the events to send back, if any.
pub struct Playback<R> {
    resolver: R,
    pool: Arc<PlayerPool>,
    /// A permit per running resolve, at most [`RESOLVES_AT_ONCE`]. Waiters go in the order they
    /// asked.
    resolves: tokio::sync::Semaphore,
    /// One per slot, bumped by every `Play` and `StopPlayback` of that slot. A resolve that
    /// finishes after a newer one started in its slot, or after the slot was stopped, is dropped:
    /// it must not load a stream nobody is watching, nor answer for the newer one. Per slot, so a
    /// late resolve can never load into another tile either.
    generations: [AtomicU64; SLOTS],
    /// The channel each slot last loaded, until it is stopped. Loading it again (another quality,
    /// latency or sound only, or back to live) while it plays goes through the slot's spare player,
    /// so the picture never stops (`PlayerPool::spare`).
    loaded: std::sync::Mutex<[Option<ChannelRef>; SLOTS]>,
}

impl<R: StreamResolver> Playback<R> {
    pub fn new(resolver: R, pool: Arc<PlayerPool>) -> Self {
        Self {
            resolver,
            pool,
            resolves: tokio::sync::Semaphore::new(RESOLVES_AT_ONCE),
            generations: std::array::from_fn(|_| AtomicU64::new(0)),
            loaded: std::sync::Mutex::default(),
        }
    }

    /// Notes that `slot` loads `channel` now (or nothing). Returns what it loaded before.
    fn set_loaded(&self, slot: Slot, channel: Option<ChannelRef>) -> Option<ChannelRef> {
        let mut loaded = self.loaded.lock().expect("loaded lock");
        std::mem::replace(&mut loaded[usize::from(slot)], channel)
    }

    fn generation(&self, slot: Slot) -> &AtomicU64 {
        &self.generations[usize::from(slot)]
    }

    /// True for the commands this module runs.
    pub fn handles(cmd: &Command) -> bool {
        matches!(
            cmd,
            Command::Play { .. }
                | Command::SetPaused { .. }
                | Command::SetMuted { .. }
                | Command::StopPlayback { .. }
                | Command::SetPlayerOption(_)
                | Command::SetStats { .. }
        )
    }

    /// Runs `cmd` on the player of the slot it names. Player options are shared by every slot.
    pub async fn execute(&self, cmd: Command) -> Vec<Event> {
        match cmd {
            Command::Play {
                slot,
                channel,
                stream,
                latency,
            } if usize::from(slot) < SLOTS => self.play(slot, channel, stream, latency).await,
            Command::SetPaused { slot, paused } => {
                if let Some(player) = self.existing(slot) {
                    player.set_paused(paused);
                }
                Vec::new()
            }
            // Made if needed: a muted tile is muted before its first load.
            Command::SetMuted { slot, muted } if usize::from(slot) < SLOTS => {
                self.pool.set_muted(slot, muted);
                Vec::new()
            }
            Command::StopPlayback { slot } if usize::from(slot) < SLOTS => {
                self.generation(slot).fetch_add(1, Ordering::SeqCst);
                self.set_loaded(slot, None);
                self.pool.stop(slot);
                Vec::new()
            }
            Command::SetPlayerOption(option) => {
                self.pool.set_shared_option(player_option(option));
                Vec::new()
            }
            Command::SetStats { slot, on } if usize::from(slot) < SLOTS => {
                self.pool.set_stats(slot, on);
                Vec::new()
            }
            other => {
                tracing::warn!(?other, "not a playback command, or no such slot");
                Vec::new()
            }
        }
    }

    fn existing(&self, slot: Slot) -> Option<Arc<dyn htpc_player::Player>> {
        if usize::from(slot) < SLOTS {
            self.pool.existing(slot)
        } else {
            None
        }
    }

    async fn play(
        &self,
        slot: Slot,
        channel: ChannelRef,
        stream: StreamKind,
        latency: Latency,
    ) -> Vec<Event> {
        let rendition = rendition_name(stream);
        let generation = self.generation(slot).fetch_add(1, Ordering::SeqCst) + 1;
        // Made (or kept from idle) before resolving, so it is not dropped meanwhile.
        let player = self.pool.player(slot);
        let stale = || self.generation(slot).load(Ordering::SeqCst) != generation;
        let resolved = {
            // The semaphore is never closed, so this only fails if it were.
            let Ok(_permit) = self.resolves.acquire().await else {
                return Vec::new();
            };
            // Replaced or stopped while waiting its turn: no need to ask at all.
            if stale() {
                tracing::debug!(slot, channel = %channel.login, "a newer play or stop replaced this one");
                return Vec::new();
            }
            self.resolver
                .resolve(site(channel.platform), &channel.login, &rendition)
                .await
        };
        if stale() {
            tracing::debug!(slot, channel = %channel.login, "a newer play or stop replaced this one");
            return Vec::new();
        }
        match resolved {
            Ok(url) => {
                tracing::info!(slot, channel = %channel.login, %rendition, "playlist resolved, starting the player");
                let latency = match latency {
                    Latency::Low => htpc_player::Latency::Low,
                    Latency::Normal => htpc_player::Latency::Normal,
                };
                // The stream on screen again, at another quality: the old one plays until the
                // new one's first frame. Anything else replaces it at once.
                let before = self.set_loaded(slot, Some(channel.clone()));
                let again = before.is_some_and(|b| b.is_same(&channel));
                let spare = (again && self.pool.is_playing(slot))
                    .then(|| self.pool.spare(slot))
                    .flatten();
                let player = match spare {
                    Some(spare) => {
                        tracing::debug!(slot, "loading the new quality behind the old one");
                        spare
                    }
                    None => {
                        drop(player);
                        self.pool.fresh(slot)
                    }
                };
                // Before the load, as mpv reads it when the file opens. Without an audio
                // rendition Streamlink falls back to video, which is then still not decoded.
                let video = !matches!(stream, StreamKind::AudioOnly);
                player.set_option(htpc_player::PlayerOption::Video(video));
                player.load(&url.0, LoadOptions { latency });
                Vec::new()
            }
            Err(ResolveError::Offline) => vec![Event::StreamOffline { channel }],
            Err(err) => {
                tracing::warn!(channel = %channel.login, %err, "could not resolve the stream");
                vec![Event::PlaybackFailed {
                    channel,
                    message: user_message(&err),
                }]
            }
        }
    }
}

/// The resolver's name for a platform.
fn site(platform: Platform) -> Site {
    match platform {
        Platform::Twitch => Site::Twitch,
        Platform::Kick => Site::Kick,
    }
}

/// The rendition (or Streamlink fallback list) the resolver is asked for. A height is a list of its
/// Twitch and Kick names and the heights below it (`Quality::streamlink_list`), as a bare `720p`
/// matches no Twitch rendition. Twitch's sound-only rendition is `audio_only`, which the
/// Streamlink resolver turns into `audio_only,best`.
fn rendition_name(stream: StreamKind) -> String {
    match stream {
        StreamKind::Video(quality) => quality.streamlink_list(),
        StreamKind::AudioOnly => "audio_only".to_owned(),
        StreamKind::Tile { rendition, rate } => rendition.streamlink_list(rate),
    }
}

/// What the end screen says. Short, because it is read from the couch.
fn user_message(err: &ResolveError) -> String {
    match err {
        ResolveError::NotInstalled => {
            "Streamlink was not found. It should be packaged with the app: reinstall it, or install Streamlink and put it on PATH.".into()
        }
        ResolveError::TimedOut => "Twitch took too long to answer.".into(),
        ResolveError::NotFound => "That channel could not be found.".into(),
        ResolveError::Offline => "The channel is offline.".into(),
        ResolveError::Failed(msg) => msg.clone(),
    }
}

/// The player crate's form of a core player option.
fn player_option(option: PlayerOption) -> htpc_player::PlayerOption {
    match option {
        PlayerOption::Hwdec(hwdec) => htpc_player::PlayerOption::Hwdec(hwdec == Hwdec::Auto),
        PlayerOption::Zoom(Zoom::Fit) => htpc_player::PlayerOption::Zoom(htpc_player::Zoom::Fit),
        PlayerOption::Zoom(Zoom::Fill) => htpc_player::PlayerOption::Zoom(htpc_player::Zoom::Fill),
    }
}

/// Maps a player report to the event the reducer understands. `Paused` changes nothing in the
/// state (the reducer already toggled it), so it has no event. `at_ms` is when the report came,
/// on the app's own clock ([`Clock`]); health reports carry it.
pub fn player_event(slot: Slot, event: PlayerEvent, at_ms: u64) -> Option<Event> {
    match event {
        PlayerEvent::Playing => Some(Event::PlaybackStarted { slot }),
        PlayerEvent::Ended => Some(Event::PlaybackEnded { slot }),
        PlayerEvent::Failed(message) => Some(Event::PlaybackError { slot, message }),
        PlayerEvent::Paused(_) => None,
        PlayerEvent::Stats(stats) => Some(Event::PlayerStats(core_stats(stats))),
        PlayerEvent::Health(h) => Some(Event::PlayerHealth {
            slot,
            at_ms,
            health: Health {
                stalled: h.stalled,
                buffer_ms: h.buffer_ms,
            },
        }),
    }
}

/// Milliseconds since the app started, never going back: the time on `Event::PlayerHealth`.
#[derive(Debug, Clone, Copy)]
pub struct Clock(std::time::Instant);

impl Clock {
    pub fn start() -> Self {
        Self(std::time::Instant::now())
    }

    pub fn ms(&self) -> u64 {
        u64::try_from(self.0.elapsed().as_millis()).unwrap_or(u64::MAX)
    }
}

fn core_stats(s: htpc_player::PlayerStats) -> PlayerStats {
    PlayerStats {
        width: s.width,
        height: s.height,
        fps_milli: s.fps_milli,
        codec: s.codec,
        hwdec: s.hwdec,
        video_bitrate: s.video_bitrate,
        audio_bitrate: s.audio_bitrate,
        audio_codec: s.audio_codec,
        buffer_ms: s.buffer_ms,
        dropped_frames: s.dropped_frames,
        decoder_dropped_frames: s.decoder_dropped_frames,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use htpc_core::{FrameRate, Quality, Rendition};
    use htpc_player::{FakePool, PlayerCall, MAIN_SLOT};
    use htpc_resolver::FakeResolver;

    fn play(channel: &str) -> Command {
        play_on(MAIN_SLOT, channel)
    }

    fn play_on(slot: Slot, channel: &str) -> Command {
        Command::Play {
            slot,
            channel: ChannelRef::twitch(channel),
            stream: StreamKind::Video(Quality::P720),
            latency: Latency::Low,
        }
    }

    const VIDEO_ON: PlayerCall = PlayerCall::SetOption(htpc_player::PlayerOption::Video(true));

    /// The single player of a fake pool.
    struct MainPlayer(FakePool);

    impl MainPlayer {
        fn calls(&self) -> Vec<PlayerCall> {
            self.0.calls(MAIN_SLOT)
        }
    }

    /// A pool whose single player already exists, as the app makes it at startup.
    fn pool() -> (Arc<PlayerPool>, FakePool) {
        let (pool, fakes) = PlayerPool::fake();
        pool.player(MAIN_SLOT);
        (pool, fakes)
    }

    fn playback<Res: StreamResolver>(resolver: Res) -> (Playback<Res>, MainPlayer) {
        let (pool, fakes) = pool();
        (Playback::new(resolver, pool), MainPlayer(fakes))
    }

    #[tokio::test]
    async fn a_resolved_playlist_is_loaded_into_the_player() {
        let (pb, player) = playback(FakeResolver::playing("https://x/a.m3u8"));
        assert!(pb.execute(play("chan")).await.is_empty());
        assert_eq!(
            player.calls(),
            [
                VIDEO_ON,
                PlayerCall::Load("https://x/a.m3u8".into(), LoadOptions::default())
            ]
        );
    }

    #[tokio::test]
    async fn audio_only_turns_video_off_before_the_load() {
        let (pb, player) = playback(FakeResolver::playing("u"));
        let cmd = Command::Play {
            slot: MAIN_SLOT,
            channel: ChannelRef::twitch("chan"),
            stream: StreamKind::AudioOnly,
            latency: Latency::Low,
        };
        assert!(pb.execute(cmd).await.is_empty());
        assert_eq!(
            player.calls(),
            [
                PlayerCall::SetOption(htpc_player::PlayerOption::Video(false)),
                PlayerCall::Load("u".into(), LoadOptions::default())
            ]
        );
    }

    #[test]
    fn audio_only_resolves_the_audio_rendition() {
        assert_eq!(rendition_name(StreamKind::AudioOnly), "audio_only");
        assert!(rendition_name(StreamKind::Video(Quality::P720)).starts_with("720p60,720p,720p30,"));
        assert_eq!(rendition_name(StreamKind::Video(Quality::Best)), "best");
        assert_eq!(rendition_name(StreamKind::Video(Quality::Worst)), "worst");
        let tile = StreamKind::Tile {
            rendition: Rendition::P360,
            rate: FrameRate::Light,
        };
        assert_eq!(
            rendition_name(tile),
            "360p30,360p,360p60,160p30,160p,160p60,worst"
        );
    }

    #[tokio::test]
    async fn the_latency_goes_with_the_load() {
        let (pb, player) = playback(FakeResolver::playing("u"));
        let cmd = Command::Play {
            slot: MAIN_SLOT,
            channel: ChannelRef::twitch("chan"),
            stream: StreamKind::Video(Quality::Best),
            latency: Latency::Normal,
        };
        pb.execute(cmd).await;
        assert_eq!(
            player.calls(),
            [
                VIDEO_ON,
                PlayerCall::Load(
                    "u".into(),
                    LoadOptions {
                        latency: htpc_player::Latency::Normal
                    }
                )
            ]
        );
    }

    #[tokio::test]
    async fn options_and_stats_reach_the_player() {
        let (pb, player) = playback(FakeResolver::playing("u"));
        for cmd in [
            Command::SetPlayerOption(PlayerOption::Hwdec(Hwdec::Off)),
            Command::SetPlayerOption(PlayerOption::Hwdec(Hwdec::Auto)),
            Command::SetPlayerOption(PlayerOption::Zoom(Zoom::Fill)),
            Command::SetStats {
                slot: MAIN_SLOT,
                on: true,
            },
        ] {
            assert!(Playback::<FakeResolver>::handles(&cmd));
            assert!(pb.execute(cmd).await.is_empty());
        }
        assert_eq!(
            player.calls(),
            [
                PlayerCall::SetOption(htpc_player::PlayerOption::Hwdec(false)),
                PlayerCall::SetOption(htpc_player::PlayerOption::Hwdec(true)),
                PlayerCall::SetOption(htpc_player::PlayerOption::Zoom(htpc_player::Zoom::Fill)),
                PlayerCall::SetStats(true),
            ]
        );
    }

    #[tokio::test]
    async fn offline_is_reported_and_nothing_is_loaded() {
        let (pb, player) = playback(FakeResolver::failing(ResolveError::Offline));
        let events = pb.execute(play("chan")).await;
        assert_eq!(
            events,
            [Event::StreamOffline {
                channel: ChannelRef::twitch("chan")
            }]
        );
        assert!(player.calls().is_empty());
    }

    /// Answers with a playlist after a second.
    struct Slow;

    impl StreamResolver for Slow {
        async fn resolve(
            &self,
            _: Site,
            _: &str,
            _: &str,
        ) -> Result<htpc_resolver::PlaylistUrl, ResolveError> {
            tokio::time::sleep(Duration::from_secs(1)).await;
            Ok(htpc_resolver::PlaylistUrl("u".into()))
        }
    }

    #[tokio::test(start_paused = true)]
    async fn a_resolve_that_finishes_after_stop_or_a_newer_play_is_dropped() {
        let (pb, player) = playback(Slow);
        let pb = Arc::new(pb);
        // Stop while resolving: nothing is loaded.
        let first = tokio::spawn({
            let pb = Arc::clone(&pb);
            async move { pb.execute(play("chan")).await }
        });
        tokio::task::yield_now().await;
        pb.execute(Command::StopPlayback { slot: MAIN_SLOT }).await;
        assert!(first.await.unwrap().is_empty());
        assert_eq!(player.calls(), [PlayerCall::Stop]);

        // Two plays at once: only the newer one loads.
        let older = tokio::spawn({
            let pb = Arc::clone(&pb);
            async move { pb.execute(play("chan")).await }
        });
        tokio::task::yield_now().await;
        pb.execute(play("chan")).await;
        older.await.unwrap();
        let loads = player
            .calls()
            .into_iter()
            .filter(|c| matches!(c, PlayerCall::Load(..)))
            .count();
        assert_eq!(loads, 1);
    }

    #[tokio::test(start_paused = true)]
    async fn each_slot_has_its_own_generation() {
        let (pool, fakes) = pool();
        let pb = Arc::new(Playback::new(Slow, pool));
        // Slot 0 resolves while slot 3 starts and stops: slot 0 still loads, and only there.
        let main = tokio::spawn({
            let pb = Arc::clone(&pb);
            async move { pb.execute(play("main")).await }
        });
        tokio::task::yield_now().await;
        let tile = tokio::spawn({
            let pb = Arc::clone(&pb);
            async move { pb.execute(play_on(3, "tile")).await }
        });
        tokio::task::yield_now().await;
        pb.execute(Command::StopPlayback { slot: 3 }).await;
        main.await.unwrap();
        tile.await.unwrap();

        assert_eq!(
            fakes.calls(MAIN_SLOT),
            [
                VIDEO_ON,
                PlayerCall::Load("u".into(), LoadOptions::default())
            ]
        );
        assert_eq!(fakes.calls(3), [PlayerCall::Stop]);
    }

    /// Answers after a second, and counts how many resolves run at once.
    #[derive(Default)]
    struct Counting {
        running: std::sync::atomic::AtomicUsize,
        most: std::sync::atomic::AtomicUsize,
        asked: std::sync::Mutex<Vec<String>>,
    }

    /// A Counting shared with the test.
    struct Counted(Arc<Counting>);

    impl StreamResolver for Counted {
        async fn resolve(
            &self,
            _: Site,
            channel: &str,
            _: &str,
        ) -> Result<htpc_resolver::PlaylistUrl, ResolveError> {
            let me = &self.0;
            me.asked.lock().unwrap().push(channel.to_owned());
            let now = me.running.fetch_add(1, Ordering::SeqCst) + 1;
            me.most.fetch_max(now, Ordering::SeqCst);
            tokio::time::sleep(Duration::from_secs(1)).await;
            me.running.fetch_sub(1, Ordering::SeqCst);
            Ok(htpc_resolver::PlaylistUrl("u".into()))
        }
    }

    #[tokio::test(start_paused = true)]
    async fn nine_streams_are_resolved_a_few_at_a_time_in_order() {
        let counting = Arc::new(Counting::default());
        let (pool, fakes) = pool();
        let pb = Arc::new(Playback::new(Counted(Arc::clone(&counting)), pool));
        let mut plays = Vec::new();
        for slot in 0..9u8 {
            let pb = Arc::clone(&pb);
            plays.push(tokio::spawn(async move {
                pb.execute(play_on(slot, &format!("c{slot}"))).await
            }));
            // Sent one after the other, as the store spawns them.
            tokio::task::yield_now().await;
        }
        for play in plays {
            play.await.unwrap();
        }
        assert_eq!(counting.most.load(Ordering::SeqCst), RESOLVES_AT_ONCE);
        let asked = counting.asked.lock().unwrap().clone();
        let want: Vec<String> = (0..9).map(|i| format!("c{i}")).collect();
        assert_eq!(asked, want, "first asked, first resolved");
        for slot in 0..9u8 {
            assert!(
                fakes
                    .calls(slot)
                    .iter()
                    .any(|c| matches!(c, PlayerCall::Load(..))),
                "slot {slot} loaded"
            );
        }
    }

    #[tokio::test(start_paused = true)]
    async fn a_play_stopped_while_waiting_its_turn_is_never_resolved() {
        let counting = Arc::new(Counting::default());
        let (pool, _) = pool();
        let pb = Arc::new(Playback::new(Counted(Arc::clone(&counting)), pool));
        let mut plays = Vec::new();
        for slot in 0..4u8 {
            let pb = Arc::clone(&pb);
            plays.push(tokio::spawn(async move {
                pb.execute(play_on(slot, &format!("c{slot}"))).await
            }));
            tokio::task::yield_now().await;
        }
        // The fourth waits for a permit; stop it meanwhile.
        pb.execute(Command::StopPlayback { slot: 3 }).await;
        for play in plays {
            play.await.unwrap();
        }
        assert_eq!(*counting.asked.lock().unwrap(), ["c0", "c1", "c2"]);
    }

    #[tokio::test]
    async fn options_reach_players_made_later() {
        let (pool, fakes) = pool();
        let pb = Playback::new(FakeResolver::playing("u"), Arc::clone(&pool));
        pb.execute(Command::SetPlayerOption(PlayerOption::Hwdec(Hwdec::Off)))
            .await;
        pb.execute(play_on(2, "tile")).await;
        assert_eq!(
            fakes.calls(2),
            [
                PlayerCall::SetOption(htpc_player::PlayerOption::Hwdec(false)),
                VIDEO_ON,
                PlayerCall::Load("u".into(), LoadOptions::default())
            ]
        );
    }

    #[tokio::test]
    async fn other_failures_become_a_message() {
        let (pb, _) = playback(FakeResolver::failing(ResolveError::NotInstalled));
        let events = pb.execute(play("chan")).await;
        let [Event::PlaybackFailed { channel, message }] = &events[..] else {
            panic!("expected one PlaybackFailed, got {events:?}");
        };
        assert_eq!(channel.login, "chan");
        assert!(message.contains("Streamlink was not found"));
    }

    #[tokio::test]
    async fn pause_and_stop_reach_the_player() {
        let (pb, player) = playback(FakeResolver::playing("u"));
        let paused = Command::SetPaused {
            slot: MAIN_SLOT,
            paused: true,
        };
        assert!(pb.execute(paused).await.is_empty());
        assert!(pb
            .execute(Command::StopPlayback { slot: MAIN_SLOT })
            .await
            .is_empty());
        assert_eq!(
            player.calls(),
            [PlayerCall::SetPaused(true), PlayerCall::Stop]
        );
    }

    #[tokio::test]
    async fn a_tile_is_muted_on_its_own_slot_before_it_loads() {
        let (pool, fakes) = pool();
        let pb = Playback::new(FakeResolver::playing("u"), pool);
        pb.execute(Command::SetMuted {
            slot: 4,
            muted: true,
        })
        .await;
        let tile = Command::Play {
            slot: 4,
            channel: ChannelRef::twitch("tile"),
            stream: StreamKind::Tile {
                rendition: Rendition::P480,
                rate: FrameRate::Light,
            },
            latency: Latency::Low,
        };
        assert!(pb.execute(tile).await.is_empty());
        assert_eq!(
            fakes.calls(4),
            [
                PlayerCall::SetOption(htpc_player::PlayerOption::Muted(true)),
                VIDEO_ON,
                PlayerCall::Load("u".into(), LoadOptions::default())
            ]
        );
        assert!(
            fakes.calls(MAIN_SLOT).is_empty(),
            "the single player is untouched"
        );
    }

    #[tokio::test]
    async fn a_slot_out_of_range_is_ignored() {
        let (pb, _) = playback(FakeResolver::playing("u"));
        assert!(pb.execute(play_on(9, "x")).await.is_empty());
        assert!(pb
            .execute(Command::StopPlayback { slot: 200 })
            .await
            .is_empty());
    }

    #[test]
    fn player_events_map_to_core_events() {
        let main = |e| player_event(MAIN_SLOT, e, 0);
        assert_eq!(
            main(PlayerEvent::Playing),
            Some(Event::PlaybackStarted { slot: MAIN_SLOT })
        );
        assert_eq!(
            main(PlayerEvent::Ended),
            Some(Event::PlaybackEnded { slot: MAIN_SLOT })
        );
        assert_eq!(
            main(PlayerEvent::Failed("x".into())),
            Some(Event::PlaybackError {
                slot: MAIN_SLOT,
                message: "x".into()
            })
        );
        assert_eq!(main(PlayerEvent::Paused(true)), None);
        // Every slot reports: multiview's tiles too.
        assert_eq!(
            player_event(5, PlayerEvent::Ended, 0),
            Some(Event::PlaybackEnded { slot: 5 })
        );
        // Health carries the time it came.
        let health = htpc_player::PlayerHealth {
            stalled: true,
            buffer_ms: 120,
        };
        assert_eq!(
            player_event(3, PlayerEvent::Health(health), 4_200),
            Some(Event::PlayerHealth {
                slot: 3,
                at_ms: 4_200,
                health: Health {
                    stalled: true,
                    buffer_ms: 120
                }
            })
        );
        let stats = htpc_player::PlayerStats {
            width: Some(1280),
            hwdec: Some("vaapi".into()),
            ..Default::default()
        };
        assert_eq!(
            main(PlayerEvent::Stats(stats)),
            Some(Event::PlayerStats(PlayerStats {
                width: Some(1280),
                hwdec: Some("vaapi".into()),
                ..Default::default()
            }))
        );
    }

    #[test]
    fn handles_only_playback_commands() {
        assert!(Playback::<FakeResolver>::handles(&play("c")));
        assert!(Playback::<FakeResolver>::handles(&Command::StopPlayback {
            slot: 3
        }));
        assert!(Playback::<FakeResolver>::handles(&Command::SetMuted {
            slot: 3,
            muted: true
        }));
        assert!(!Playback::<FakeResolver>::handles(&Command::LeaveChat));
    }
}
