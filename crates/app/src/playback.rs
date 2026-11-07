//! Playback commands: resolve a channel with the `StreamResolver`, then hand the playlist to the
//! `Player`. Player events come back as core `Event`s.

use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use std::time::Duration;

use htpc_core::{
    ChannelRef, Command, Event, Hwdec, Latency, PlayerOption, PlayerStats, StreamKind, Zoom,
};
use htpc_player::{LoadOptions, Player, PlayerEvent};
use htpc_resolver::{ResolveError, StreamResolver};

/// How long the offline screen waits before looking for the stream again
/// (`Command::ScheduleOfflineRecheck`). The player's end screen says this number too.
pub const OFFLINE_RECHECK: Duration = Duration::from_secs(10);

/// Runs `Play`, `SetPaused`, `StopPlayback`, `SetPlayerOption` and `SetStats`. Returns the events
/// to send back, if any.
pub struct Playback<R> {
    resolver: R,
    player: Arc<dyn Player>,
    /// Bumped by every `Play` and `StopPlayback`. A resolve that finishes after a newer one
    /// started, or after the stream was left, is dropped: it must not load a stream nobody is
    /// watching, nor answer for the newer one.
    generation: AtomicU64,
}

impl<R: StreamResolver> Playback<R> {
    pub fn new(resolver: R, player: Arc<dyn Player>) -> Self {
        Self {
            resolver,
            player,
            generation: AtomicU64::new(0),
        }
    }

    /// True for the commands this module runs.
    pub fn handles(cmd: &Command) -> bool {
        matches!(
            cmd,
            Command::Play { .. }
                | Command::SetPaused(_)
                | Command::StopPlayback
                | Command::SetPlayerOption(_)
                | Command::SetStats(_)
        )
    }

    pub async fn execute(&self, cmd: Command) -> Vec<Event> {
        match cmd {
            Command::Play {
                channel,
                stream,
                latency,
            } => self.play(channel, stream, latency).await,
            Command::SetPaused(paused) => {
                self.player.set_paused(paused);
                Vec::new()
            }
            Command::StopPlayback => {
                self.generation.fetch_add(1, Ordering::SeqCst);
                self.player.stop();
                Vec::new()
            }
            Command::SetPlayerOption(option) => {
                self.player.set_option(player_option(option));
                Vec::new()
            }
            Command::SetStats(on) => {
                self.player.set_stats(on);
                Vec::new()
            }
            _ => Vec::new(),
        }
    }

    async fn play(&self, channel: ChannelRef, stream: StreamKind, latency: Latency) -> Vec<Event> {
        let rendition = rendition_name(stream);
        let generation = self.generation.fetch_add(1, Ordering::SeqCst) + 1;
        let resolved = self.resolver.resolve(&channel.login, rendition).await;
        if self.generation.load(Ordering::SeqCst) != generation {
            tracing::debug!(channel = %channel.login, "a newer play or stop replaced this one");
            return Vec::new();
        }
        match resolved {
            Ok(url) => {
                tracing::info!(channel = %channel.login, rendition, "playlist resolved, starting the player");
                let latency = match latency {
                    Latency::Low => htpc_player::Latency::Low,
                    Latency::Normal => htpc_player::Latency::Normal,
                };
                // Before the load, as mpv reads it when the file opens. Without an audio
                // rendition Streamlink falls back to video, which is then still not decoded.
                let video = !matches!(stream, StreamKind::AudioOnly);
                self.player
                    .set_option(htpc_player::PlayerOption::Video(video));
                self.player.load(&url.0, LoadOptions { latency });
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

/// The name the resolver is asked for. Twitch's sound-only rendition is `audio_only`, which the
/// Streamlink resolver turns into `audio_only,best`.
fn rendition_name(stream: StreamKind) -> &'static str {
    match stream {
        StreamKind::Video(quality) => quality.as_str(),
        StreamKind::AudioOnly => "audio_only",
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
/// state (the reducer already toggled it), so it has no event.
pub fn player_event(event: PlayerEvent) -> Option<Event> {
    match event {
        PlayerEvent::Playing => Some(Event::PlaybackStarted),
        PlayerEvent::Ended => Some(Event::PlaybackEnded),
        PlayerEvent::Failed(message) => Some(Event::PlaybackError(message)),
        PlayerEvent::Paused(_) => None,
        PlayerEvent::Stats(stats) => Some(Event::PlayerStats(core_stats(stats))),
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
    use htpc_core::Quality;
    use htpc_player::{FakePlayer, PlayerCall};
    use htpc_resolver::FakeResolver;

    fn play(channel: &str) -> Command {
        Command::Play {
            channel: ChannelRef::twitch(channel),
            stream: StreamKind::Video(Quality::P720),
            latency: Latency::Low,
        }
    }

    const VIDEO_ON: PlayerCall = PlayerCall::SetOption(htpc_player::PlayerOption::Video(true));

    fn playback(resolver: FakeResolver) -> (Playback<FakeResolver>, FakePlayer) {
        let player = FakePlayer::new();
        (Playback::new(resolver, Arc::new(player.clone())), player)
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
        assert_eq!(rendition_name(StreamKind::Video(Quality::P720)), "720p");
        assert_eq!(rendition_name(StreamKind::Video(Quality::Best)), "best");
    }

    #[tokio::test]
    async fn the_latency_goes_with_the_load() {
        let (pb, player) = playback(FakeResolver::playing("u"));
        let cmd = Command::Play {
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
            Command::SetStats(true),
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
            _: &str,
            _: &str,
        ) -> Result<htpc_resolver::PlaylistUrl, ResolveError> {
            tokio::time::sleep(Duration::from_secs(1)).await;
            Ok(htpc_resolver::PlaylistUrl("u".into()))
        }
    }

    #[tokio::test(start_paused = true)]
    async fn a_resolve_that_finishes_after_stop_or_a_newer_play_is_dropped() {
        let player = FakePlayer::new();
        let pb = Arc::new(Playback::new(Slow, Arc::new(player.clone())));
        // Stop while resolving: nothing is loaded.
        let first = tokio::spawn({
            let pb = Arc::clone(&pb);
            async move { pb.execute(play("chan")).await }
        });
        tokio::task::yield_now().await;
        pb.execute(Command::StopPlayback).await;
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
        assert!(pb.execute(Command::SetPaused(true)).await.is_empty());
        assert!(pb.execute(Command::StopPlayback).await.is_empty());
        assert_eq!(
            player.calls(),
            [PlayerCall::SetPaused(true), PlayerCall::Stop]
        );
    }

    #[test]
    fn player_events_map_to_core_events() {
        assert_eq!(
            player_event(PlayerEvent::Playing),
            Some(Event::PlaybackStarted)
        );
        assert_eq!(player_event(PlayerEvent::Ended), Some(Event::PlaybackEnded));
        assert_eq!(
            player_event(PlayerEvent::Failed("x".into())),
            Some(Event::PlaybackError("x".into()))
        );
        assert_eq!(player_event(PlayerEvent::Paused(true)), None);
        let stats = htpc_player::PlayerStats {
            width: Some(1280),
            hwdec: Some("vaapi".into()),
            ..Default::default()
        };
        assert_eq!(
            player_event(PlayerEvent::Stats(stats)),
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
        assert!(Playback::<FakeResolver>::handles(&Command::StopPlayback));
        assert!(!Playback::<FakeResolver>::handles(&Command::LeaveChat));
    }
}
