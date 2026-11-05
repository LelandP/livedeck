//! Playback commands: resolve a channel with the `StreamResolver`, then hand the playlist to the
//! `Player`. Player events come back as core `Event`s.

use std::sync::Arc;

use htpc_core::{Command, Event, Quality};
use htpc_player::{Player, PlayerEvent};
use htpc_resolver::{ResolveError, StreamResolver};

/// Runs `Play`, `SetPaused` and `StopPlayback`. Returns the events to send back, if any.
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
            Command::Play { .. } | Command::SetPaused(_) | Command::StopPlayback
        )
    }

    pub async fn execute(&self, cmd: Command) -> Vec<Event> {
        match cmd {
            Command::Play { channel, quality } => self.play(channel, quality).await,
            Command::SetPaused(paused) => {
                self.player.set_paused(paused);
                Vec::new()
            }
            Command::StopPlayback => {
                self.player.stop();
                Vec::new()
            }
            _ => Vec::new(),
        }
    }

    async fn play(&self, channel: String, quality: Quality) -> Vec<Event> {
        match self.resolver.resolve(&channel, quality.as_str()).await {
            Ok(url) => {
                tracing::info!(%channel, "playlist resolved, starting the player");
                self.player.load(&url.0);
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

/// Maps a player report to the event the reducer understands. `Paused` changes nothing in the
/// state (the reducer already toggled it), so it has no event.
pub fn player_event(event: PlayerEvent) -> Option<Event> {
    match event {
        PlayerEvent::Playing => Some(Event::PlaybackStarted),
        PlayerEvent::Ended => Some(Event::PlaybackEnded),
        PlayerEvent::Failed(message) => Some(Event::PlaybackError(message)),
        PlayerEvent::Paused(_) => None,
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
            [PlayerCall::Load("https://x/a.m3u8".into())]
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
    }

    #[test]
    fn handles_only_playback_commands() {
        assert!(Playback::<FakeResolver>::handles(&play("c")));
        assert!(Playback::<FakeResolver>::handles(&Command::StopPlayback));
        assert!(!Playback::<FakeResolver>::handles(&Command::LeaveChat));
    }
}
