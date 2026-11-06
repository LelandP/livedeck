//! The sleep timer: `StartSleepTimer`, `CancelSleepTimer` and `Suspend`.
//!
//! A running timer is one tokio task. It reports every whole minute (`SleepTimerTick`), warns 60 s
//! before the end (`SleepTimerWarning`), fades the sound out over the last few seconds, and then
//! sends `SleepTimerFired`. The reducer decides what that means (stop, go back, suspend).

use std::sync::{Arc, Mutex};
use std::time::Duration;

use htpc_core::{Command, Event, Msg};
use htpc_player::Player;
use tokio::sync::mpsc::UnboundedSender;
use tokio::task::JoinHandle;
use tokio::time::{sleep_until, Instant};

use crate::power::{self, Suspend};

const MINUTE: Duration = Duration::from_secs(60);
/// "Still watching?" shows this long before the end.
const WARNING: Duration = Duration::from_secs(60);
/// The sound fades out over the last this-long. Only so the stop is gentle, not a setting.
const FADE: Duration = Duration::from_secs(5);

/// The timer that is allowed to act. Changed under the lock, so a task that was just replaced can
/// never start a fade (or fire) after its replacement started.
#[derive(Default)]
struct Current {
    id: Option<u32>,
    /// It has started fading the sound out, which a replacement must undo.
    fading: bool,
    task: Option<JoinHandle<()>>,
}

pub struct Sleep {
    player: Arc<dyn Player>,
    power: Arc<dyn Suspend>,
    tx: UnboundedSender<Msg>,
    current: Arc<Mutex<Current>>,
}

impl Sleep {
    pub fn new(player: Arc<dyn Player>, power: Arc<dyn Suspend>, tx: UnboundedSender<Msg>) -> Self {
        Self {
            player,
            power,
            tx,
            current: Arc::default(),
        }
    }

    /// True for the commands this module runs.
    pub fn handles(cmd: &Command) -> bool {
        matches!(
            cmd,
            Command::StartSleepTimer { .. } | Command::CancelSleepTimer | Command::Suspend
        )
    }

    /// Runs a command. Must be called inside the tokio runtime.
    pub fn execute(&self, cmd: Command) {
        match cmd {
            Command::StartSleepTimer { id, minutes } => {
                let mut current = self.replace(Some(id));
                let task = tokio::spawn(run(
                    id,
                    minutes,
                    Arc::clone(&self.player),
                    self.tx.clone(),
                    Arc::clone(&self.current),
                ));
                current.task = Some(task);
            }
            Command::CancelSleepTimer => drop(self.replace(None)),
            Command::Suspend => {
                tokio::spawn(power::suspend(Arc::clone(&self.power)));
            }
            _ => {}
        }
    }

    /// Stops the running timer (bringing back sound it faded) and makes `id` the current one.
    /// Returns the lock, so the caller can store the new task before anything else looks.
    fn replace(&self, id: Option<u32>) -> std::sync::MutexGuard<'_, Current> {
        let mut current = self.current.lock().expect("sleep timer lock");
        if let Some(task) = current.task.take() {
            task.abort();
        }
        if current.fading {
            self.player.restore_volume();
        }
        current.id = id;
        current.fading = false;
        current
    }
}

/// One timer, from start to `Fired`. Every step checks it is still the current timer.
async fn run(
    id: u32,
    minutes: u16,
    player: Arc<dyn Player>,
    tx: UnboundedSender<Msg>,
    current: Arc<Mutex<Current>>,
) {
    let send = |event: Event| tx.send(event.into()).is_ok();
    let start = Instant::now();
    let total = MINUTE * u32::from(minutes);
    let end = start + total;
    let warn_at = start + total.saturating_sub(WARNING);

    // Whole minutes, until the minute the warning takes over.
    for k in 1..minutes {
        let at = start + MINUTE * u32::from(k);
        if at >= warn_at {
            break;
        }
        sleep_until(at).await;
        if !send(Event::SleepTimerTick {
            id,
            left_min: minutes - k,
        }) {
            return;
        }
    }

    sleep_until(warn_at).await;
    if !send(Event::SleepTimerWarning { id }) {
        return;
    }

    sleep_until(end.checked_sub(FADE).unwrap_or(end).max(warn_at)).await;
    {
        let mut now = current.lock().expect("sleep timer lock");
        if now.id != Some(id) {
            return;
        }
        now.fading = true;
        player.fade_out(FADE);
    }

    sleep_until(end).await;
    {
        let mut now = current.lock().expect("sleep timer lock");
        if now.id != Some(id) {
            return;
        }
        // Done: the next load brings the volume back, and there is nothing left to undo.
        now.id = None;
        now.fading = false;
        now.task = None;
    }
    send(Event::SleepTimerFired { id });
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::power::tests::FakeSuspend;
    use htpc_player::{FakePlayer, PlayerCall};
    use tokio::sync::mpsc::{unbounded_channel, UnboundedReceiver};

    fn sleep_with(player: &FakePlayer) -> (Sleep, UnboundedReceiver<Msg>, Arc<FakeSuspend>) {
        let (tx, rx) = unbounded_channel();
        let power = FakeSuspend::new(Ok(()));
        let sleep = Sleep::new(Arc::new(player.clone()), power.clone(), tx);
        (sleep, rx, power)
    }

    /// Every event until `Fired` (or the channel stays quiet for an hour), with the time since
    /// `start` in seconds.
    async fn events_until_fired(
        rx: &mut UnboundedReceiver<Msg>,
        start: Instant,
    ) -> Vec<(u64, Event)> {
        let mut out = Vec::new();
        while let Ok(Some(Msg::Event(event))) =
            tokio::time::timeout(Duration::from_secs(3600), rx.recv()).await
        {
            let fired = matches!(event, Event::SleepTimerFired { .. });
            out.push((start.elapsed().as_secs(), event));
            if fired {
                break;
            }
        }
        out
    }

    #[tokio::test(start_paused = true)]
    async fn ticks_then_the_warning_then_a_fade_then_fired() {
        let player = FakePlayer::new();
        let (sleep, mut rx, _) = sleep_with(&player);
        let start = Instant::now();
        sleep.execute(Command::StartSleepTimer { id: 7, minutes: 15 });
        let events = events_until_fired(&mut rx, start).await;

        let mut want: Vec<(u64, Event)> = (1..14u16)
            .map(|k| {
                (
                    u64::from(k) * 60,
                    Event::SleepTimerTick {
                        id: 7,
                        left_min: 15 - k,
                    },
                )
            })
            .collect();
        want.push((14 * 60, Event::SleepTimerWarning { id: 7 }));
        want.push((15 * 60, Event::SleepTimerFired { id: 7 }));
        assert_eq!(events, want);
        assert_eq!(player.calls(), [PlayerCall::FadeOut(FADE)]);
    }

    #[tokio::test(start_paused = true)]
    async fn cancelling_stops_everything() {
        let player = FakePlayer::new();
        let (sleep, mut rx, _) = sleep_with(&player);
        sleep.execute(Command::StartSleepTimer { id: 1, minutes: 15 });
        tokio::time::sleep(MINUTE * 2 + Duration::from_secs(1)).await;
        sleep.execute(Command::CancelSleepTimer);
        tokio::time::sleep(MINUTE * 30).await;

        let mut seen = Vec::new();
        while let Ok(Msg::Event(e)) = rx.try_recv() {
            seen.push(e);
        }
        assert_eq!(
            seen,
            [
                Event::SleepTimerTick {
                    id: 1,
                    left_min: 14
                },
                Event::SleepTimerTick {
                    id: 1,
                    left_min: 13
                },
            ]
        );
        assert!(player.calls().is_empty(), "no fade, nothing to restore");
    }

    #[tokio::test(start_paused = true)]
    async fn a_new_timer_mid_fade_brings_the_sound_back() {
        let player = FakePlayer::new();
        let (sleep, mut rx, _) = sleep_with(&player);
        let start = Instant::now();
        sleep.execute(Command::StartSleepTimer { id: 1, minutes: 15 });
        // Two seconds into the fade, the user presses a button: the reducer starts it again.
        tokio::time::sleep(MINUTE * 15 - FADE + Duration::from_secs(2)).await;
        sleep.execute(Command::StartSleepTimer { id: 2, minutes: 15 });
        assert_eq!(
            player.calls(),
            [PlayerCall::FadeOut(FADE), PlayerCall::RestoreVolume]
        );

        let events = events_until_fired(&mut rx, start).await;
        assert!(
            !events
                .iter()
                .any(|(_, e)| *e == Event::SleepTimerFired { id: 1 }),
            "the old timer never fires"
        );
        assert_eq!(
            events.last(),
            Some(&(30 * 60 - 3, Event::SleepTimerFired { id: 2 }))
        );
    }

    #[tokio::test]
    async fn suspend_goes_to_the_power_module() {
        let player = FakePlayer::new();
        let (sleep, _rx, power) = sleep_with(&player);
        assert!(Sleep::handles(&Command::Suspend));
        sleep.execute(Command::Suspend);
        for _ in 0..400 {
            if power.count() == 1 {
                return;
            }
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
        panic!("Suspend never reached the power module");
    }

    #[test]
    fn handles_only_sleep_commands() {
        assert!(Sleep::handles(&Command::StartSleepTimer {
            id: 1,
            minutes: 15
        }));
        assert!(Sleep::handles(&Command::CancelSleepTimer));
        assert!(!Sleep::handles(&Command::StopPlayback));
    }
}
