//! Players for tests and for builds without libmpv.

use std::sync::{Arc, Mutex};

use crate::{Player, PlayerEvent};

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PlayerCall {
    Load(String),
    SetPaused(bool),
    Stop,
}

type Listener = Box<dyn Fn(PlayerEvent) + Send + Sync>;

/// Records every call. Tests send events with [`FakePlayer::emit`].
#[derive(Clone, Default)]
pub struct FakePlayer {
    calls: Arc<Mutex<Vec<PlayerCall>>>,
    listener: Arc<Mutex<Option<Listener>>>,
}

impl FakePlayer {
    pub fn new() -> Self {
        Self::default()
    }

    /// Where [`FakePlayer::emit`] delivers events (the app's store channel).
    pub fn on_event(&self, listener: impl Fn(PlayerEvent) + Send + Sync + 'static) {
        *self.listener.lock().expect("listener lock") = Some(Box::new(listener));
    }

    /// Pretends the player reported `event`.
    pub fn emit(&self, event: PlayerEvent) {
        if let Some(listener) = self.listener.lock().expect("listener lock").as_ref() {
            listener(event);
        }
    }

    pub fn calls(&self) -> Vec<PlayerCall> {
        self.calls.lock().expect("calls lock").clone()
    }

    fn record(&self, call: PlayerCall) {
        self.calls.lock().expect("calls lock").push(call);
    }
}

impl Player for FakePlayer {
    fn load(&self, url: &str) {
        self.record(PlayerCall::Load(url.to_owned()));
    }

    fn set_paused(&self, paused: bool) {
        self.record(PlayerCall::SetPaused(paused));
    }

    fn stop(&self) {
        self.record(PlayerCall::Stop);
    }
}

/// A player for builds without libmpv (and `--mock`): loading reports a failure that says why.
#[derive(Clone)]
pub struct UnavailablePlayer {
    report: Arc<dyn Fn(PlayerEvent) + Send + Sync>,
}

impl UnavailablePlayer {
    pub fn new(report: impl Fn(PlayerEvent) + Send + Sync + 'static) -> Self {
        Self {
            report: Arc::new(report),
        }
    }
}

impl Player for UnavailablePlayer {
    fn load(&self, _url: &str) {
        (self.report)(PlayerEvent::Failed(
            "this build has no video support (built without the `mpv` feature)".into(),
        ));
    }

    fn set_paused(&self, _paused: bool) {}

    fn stop(&self) {}
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_fake_records_calls_and_emits_events() {
        let player = FakePlayer::new();
        let seen = Arc::new(Mutex::new(Vec::new()));
        let sink = Arc::clone(&seen);
        player.on_event(move |e| sink.lock().unwrap().push(e));

        player.load("https://x/y.m3u8");
        player.set_paused(true);
        player.stop();
        player.emit(PlayerEvent::Playing);

        assert_eq!(
            player.calls(),
            [
                PlayerCall::Load("https://x/y.m3u8".into()),
                PlayerCall::SetPaused(true),
                PlayerCall::Stop
            ]
        );
        assert_eq!(*seen.lock().unwrap(), [PlayerEvent::Playing]);
    }

    #[test]
    fn the_unavailable_player_fails_every_load() {
        let seen = Arc::new(Mutex::new(Vec::new()));
        let sink = Arc::clone(&seen);
        let player = UnavailablePlayer::new(move |e| sink.lock().unwrap().push(e));
        player.load("u");
        player.set_paused(true);
        player.stop();
        let seen = seen.lock().unwrap();
        assert_eq!(seen.len(), 1);
        assert!(matches!(seen[0], PlayerEvent::Failed(_)));
    }
}
