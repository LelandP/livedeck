//! Players for tests and for builds without libmpv.

use std::sync::{Arc, Mutex};
use std::time::Duration;

use crate::{LoadOptions, Player, PlayerEvent, PlayerOption};

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PlayerCall {
    Load(String, LoadOptions),
    SetPaused(bool),
    Stop,
    SetOption(PlayerOption),
    SetStats(bool),
    FadeOut(Duration),
    RestoreVolume,
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
    fn load(&self, url: &str, options: LoadOptions) {
        self.record(PlayerCall::Load(url.to_owned(), options));
    }

    fn set_paused(&self, paused: bool) {
        self.record(PlayerCall::SetPaused(paused));
    }

    fn stop(&self) {
        self.record(PlayerCall::Stop);
    }

    fn set_option(&self, option: PlayerOption) {
        self.record(PlayerCall::SetOption(option));
    }

    fn set_stats(&self, on: bool) {
        self.record(PlayerCall::SetStats(on));
    }

    fn fade_out(&self, duration: Duration) {
        self.record(PlayerCall::FadeOut(duration));
    }

    fn restore_volume(&self) {
        self.record(PlayerCall::RestoreVolume);
    }
}

/// A player for builds without libmpv (and `--mock`), or for when libmpv failed to start: loading
/// reports a failure that says why.
#[derive(Clone)]
pub struct UnavailablePlayer {
    report: Arc<dyn Fn(PlayerEvent) + Send + Sync>,
    reason: Arc<str>,
}

impl UnavailablePlayer {
    /// For a build without the `mpv` feature.
    pub fn new(report: impl Fn(PlayerEvent) + Send + Sync + 'static) -> Self {
        Self::with_reason(
            report,
            "this build has no video support (built without the `mpv` feature)",
        )
    }

    /// For when video should work but does not, with what went wrong.
    pub fn with_reason(
        report: impl Fn(PlayerEvent) + Send + Sync + 'static,
        reason: impl Into<String>,
    ) -> Self {
        Self {
            report: Arc::new(report),
            reason: reason.into().into(),
        }
    }
}

impl Player for UnavailablePlayer {
    fn load(&self, _url: &str, _options: LoadOptions) {
        (self.report)(PlayerEvent::Failed(self.reason.to_string()));
    }

    fn set_paused(&self, _paused: bool) {}

    fn stop(&self) {}

    fn set_option(&self, _option: PlayerOption) {}

    fn set_stats(&self, _on: bool) {}

    fn fade_out(&self, _duration: Duration) {}

    fn restore_volume(&self) {}
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

        player.load("https://x/y.m3u8", LoadOptions::default());
        player.set_paused(true);
        player.set_option(PlayerOption::Hwdec(false));
        player.set_stats(true);
        player.fade_out(Duration::from_secs(5));
        player.restore_volume();
        player.stop();
        player.emit(PlayerEvent::Playing);

        assert_eq!(
            player.calls(),
            [
                PlayerCall::Load("https://x/y.m3u8".into(), LoadOptions::default()),
                PlayerCall::SetPaused(true),
                PlayerCall::SetOption(PlayerOption::Hwdec(false)),
                PlayerCall::SetStats(true),
                PlayerCall::FadeOut(Duration::from_secs(5)),
                PlayerCall::RestoreVolume,
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
        player.load("u", LoadOptions::default());
        player.set_paused(true);
        player.set_option(PlayerOption::Zoom(crate::Zoom::Fill));
        player.set_stats(true);
        player.stop();
        let seen = seen.lock().unwrap();
        assert_eq!(seen.len(), 1);
        assert!(matches!(seen[0], PlayerEvent::Failed(_)));
    }

    #[test]
    fn the_unavailable_player_reports_its_reason() {
        let seen = Arc::new(Mutex::new(Vec::new()));
        let sink = Arc::clone(&seen);
        let player =
            UnavailablePlayer::with_reason(move |e| sink.lock().unwrap().push(e), "no GPU");
        player.load("u", LoadOptions::default());
        assert_eq!(
            *seen.lock().unwrap(),
            [PlayerEvent::Failed("no GPU".into())]
        );
    }
}
