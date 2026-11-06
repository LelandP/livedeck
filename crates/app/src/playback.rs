//! Playback commands: resolve a channel with the `StreamResolver`, then hand the playlist to the
//! `Player`. Player events come back as core `Event`s.

use std::sync::Arc;

use htpc_core::{Command, Event, Hwdec, Latency, PlayerOption, PlayerStats, Quality, Zoom};
use htpc_player::{LoadOptions, Player, PlayerEvent};
use htpc_resolver::{ResolveError, StreamResolver};

/// Runs `Play`, `SetPaused`, `StopPlayback`, `SetPlayerOption` and `SetStats`. Returns the events
/// to send back, if any.
pub struct Playback<R> {
    resolver: R,
    player: Arc<dyn Player>,
}

impl<R: StreamResolver> Playback<R> {
    pub fn new(resolver: R, player: Arc<dyn Player>) -> Self {
        Self { resolver, player }
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
                quality,
                latency,
            } => self.play(channel, quality, latency).await,
            Command::SetPaused(paused) => {
                self.player.set_paused(paused);
                Vec::new()
            }
            Command::StopPlayback => {
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

    async fn play(&self, channel: String, quality: Quality, latency: Latency) -> Vec<Event> {
        match self.resolver.resolve(&channel, quality.as_str()).await {
            Ok(url) => {
                tracing::info!(%channel, "playlist resolved, starting the player");
                let latency = match latency {
                    Latency::Low => htpc_player::Latency::Low,
                    Latency::Normal => htpc_player::Latency::Normal,
                };
                self.player.load(&url.0, LoadOptions { latency });
                Vec::new()
            }
            Err(ResolveError::Offline) => vec![Event::StreamOffline { channel }],
            Err(err) => {
                tracing::warn!(%channel, %err, "could not resolve the stream");
                vec![Event::PlaybackFailed {
                    channel,
                    message: user_message(&err),
                }]
            }
        }
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
        buffer_ms: s.buffer_ms,
        dropped_frames: s.dropped_frames,
        decoder_dropped_frames: s.decoder_dropped_frames,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use htpc_player::{FakePlayer, PlayerCall};
    use htpc_resolver::FakeResolver;

    fn play(channel: &str) -> Command {
        Command::Play {
            channel: channel.into(),
            quality: Quality::P720,
            latency: Latency::Low,
        }
    }

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
            [PlayerCall::Load(
                "https://x/a.m3u8".into(),
                LoadOptions::default()
            )]
        );
    }

    #[tokio::test]
    async fn the_latency_goes_with_the_load() {
        let (pb, player) = playback(FakeResolver::playing("u"));
        let cmd = Command::Play {
            channel: "chan".into(),
            quality: Quality::Best,
            latency: Latency::Normal,
        };
        pb.execute(cmd).await;
        assert_eq!(
            player.calls(),
            [PlayerCall::Load(
                "u".into(),
                LoadOptions {
                    latency: htpc_player::Latency::Normal
                }
            )]
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
                channel: "chan".into()
            }]
        );
        assert!(player.calls().is_empty());
    }

    #[tokio::test]
    async fn other_failures_become_a_message() {
        let (pb, _) = playback(FakeResolver::failing(ResolveError::NotInstalled));
        let events = pb.execute(play("chan")).await;
        let [Event::PlaybackFailed { channel, message }] = &events[..] else {
            panic!("expected one PlaybackFailed, got {events:?}");
        };
        assert_eq!(channel, "chan");
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
