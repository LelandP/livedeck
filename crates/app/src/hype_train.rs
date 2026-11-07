//! Hype trains: `WatchHypeTrain` and `StopHypeTrain`.
//!
//! A running watch is one tokio task that asks the website's GQL API for the channel's hype train
//! (Helix shows it to the broadcaster only). Most of the time no train runs, so it asks only every
//! [`SLOW`]; once one is seen it asks every [`FAST`] until it is gone. Only changes reach the
//! store, as `Event::HypeTrain`. Twitch's own website has no faster source since PubSub closed.

use std::sync::{Arc, Mutex};
use std::time::Duration;

use htpc_core::{Command, Event, HypeTrain, Msg};
use tokio::sync::mpsc::UnboundedSender;
use tokio::task::JoinHandle;

use crate::provider::Provider;

/// How often a channel without a train is checked.
pub const SLOW: Duration = Duration::from_secs(60);
/// How often a running train is checked: often enough for the bar to move with it.
pub const FAST: Duration = Duration::from_secs(10);

/// The wait before the next check, after one that found `train`.
pub fn next_check(train: Option<&HypeTrain>) -> Duration {
    if train.is_some() {
        FAST
    } else {
        SLOW
    }
}

#[derive(Default)]
pub struct HypeWatch {
    task: Mutex<Option<JoinHandle<()>>>,
}

impl HypeWatch {
    /// True for the commands this module runs.
    pub fn handles(cmd: &Command) -> bool {
        matches!(cmd, Command::WatchHypeTrain { .. } | Command::StopHypeTrain)
    }

    /// Runs a command. Must be called inside the tokio runtime.
    pub fn execute<P: Provider>(&self, provider: Arc<P>, tx: UnboundedSender<Msg>, cmd: Command) {
        let mut task = self.task.lock().unwrap_or_else(|e| e.into_inner());
        if let Some(old) = task.take() {
            old.abort();
        }
        if let Command::WatchHypeTrain { channel } = cmd {
            *task = Some(tokio::spawn(watch(provider, tx, channel)));
        }
    }
}

/// Checks until aborted (or the store is gone). A failed check changes nothing and keeps the
/// pace it had.
async fn watch<P: Provider>(provider: Arc<P>, tx: UnboundedSender<Msg>, channel: String) {
    // The reducer starts every channel without a train.
    let mut last: Option<HypeTrain> = None;
    let mut wait = SLOW;
    loop {
        if let Some(train) = provider.hype_train(&channel).await {
            if train != last {
                tracing::debug!(channel, ?train, "hype train changed");
                let event = Event::HypeTrain {
                    channel: channel.clone(),
                    train,
                };
                if tx.send(event.into()).is_err() {
                    return;
                }
                last = train;
            }
            wait = next_check(last.as_ref());
        }
        tokio::time::sleep(wait).await;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use htpc_ui::Thumbnail;
    use std::collections::VecDeque;
    use tokio::sync::mpsc::{unbounded_channel, UnboundedReceiver};
    use tokio::time::Instant;

    fn train(level: u32, progress: u32) -> HypeTrain {
        HypeTrain {
            level,
            progress,
            goal: 1600,
            seconds_left: 200,
        }
    }

    /// Answers each check with the next result (`None` = a failed request), then the last one
    /// again, and notes when each check came (seconds since `start`) and for which channel.
    struct Scripted {
        answers: Mutex<VecDeque<Option<Option<HypeTrain>>>>,
        checks: Mutex<Vec<(u64, String)>>,
        start: Instant,
    }

    impl Scripted {
        fn new(answers: Vec<Option<Option<HypeTrain>>>) -> Arc<Self> {
            Arc::new(Self {
                answers: Mutex::new(answers.into()),
                checks: Mutex::new(Vec::new()),
                start: Instant::now(),
            })
        }

        fn checks(&self) -> Vec<u64> {
            self.checks.lock().unwrap().iter().map(|c| c.0).collect()
        }
    }

    impl Provider for Scripted {
        async fn execute(&self, _cmd: Command) -> Vec<Event> {
            Vec::new()
        }

        async fn thumbnail(&self, _url: &str) -> Option<Thumbnail> {
            None
        }

        async fn hype_train(&self, channel: &str) -> Option<Option<HypeTrain>> {
            self.checks
                .lock()
                .unwrap()
                .push((self.start.elapsed().as_secs(), channel.to_owned()));
            let mut answers = self.answers.lock().unwrap();
            if answers.len() > 1 {
                answers.pop_front().flatten()
            } else {
                answers.front().cloned().flatten()
            }
        }
    }

    fn events(rx: &mut UnboundedReceiver<Msg>) -> Vec<Option<HypeTrain>> {
        let mut out = Vec::new();
        while let Ok(Msg::Event(Event::HypeTrain { train, .. })) = rx.try_recv() {
            out.push(train);
        }
        out
    }

    fn watch_cmd(channel: &str) -> Command {
        Command::WatchHypeTrain {
            channel: channel.into(),
        }
    }

    #[tokio::test(start_paused = true)]
    async fn slow_until_a_train_then_fast_until_it_is_gone() {
        let provider = Scripted::new(vec![
            Some(None),                // 0 s: nothing
            Some(Some(train(1, 100))), // 60 s: a train
            Some(Some(train(1, 100))), // 70 s: unchanged
            None,                      // 80 s: failed, keeps the fast pace
            Some(Some(train(2, 50))),  // 90 s: level up
            Some(None),                // 100 s: over
            Some(None),                // 160 s
        ]);
        let (tx, mut rx) = unbounded_channel();
        let hype = HypeWatch::default();
        hype.execute(Arc::clone(&provider), tx, watch_cmd("ender"));
        tokio::time::sleep(Duration::from_secs(161)).await;
        assert_eq!(provider.checks(), [0, 60, 70, 80, 90, 100, 160]);
        assert_eq!(
            events(&mut rx),
            [Some(train(1, 100)), Some(train(2, 50)), None],
            "only changes"
        );
    }

    #[tokio::test(start_paused = true)]
    async fn stop_ends_the_checks_and_a_new_channel_replaces_the_old() {
        let provider = Scripted::new(vec![Some(Some(train(1, 1)))]);
        let (tx, mut rx) = unbounded_channel();
        let hype = HypeWatch::default();
        hype.execute(Arc::clone(&provider), tx.clone(), watch_cmd("a"));
        tokio::time::sleep(Duration::from_secs(5)).await;
        hype.execute(Arc::clone(&provider), tx.clone(), watch_cmd("b"));
        tokio::time::sleep(Duration::from_secs(25)).await;
        let channels: Vec<String> = provider
            .checks
            .lock()
            .unwrap()
            .iter()
            .map(|c| c.1.clone())
            .collect();
        assert_eq!(channels, ["a", "b", "b", "b"]);
        // `b` starts without a train too, so its first answer is passed on.
        assert_eq!(events(&mut rx).len(), 2);

        hype.execute(Arc::clone(&provider), tx, Command::StopHypeTrain);
        let n = provider.checks().len();
        tokio::time::sleep(SLOW * 5).await;
        assert_eq!(provider.checks().len(), n, "no requests after Stop");
    }

    #[test]
    fn handles_only_its_commands() {
        assert!(HypeWatch::handles(&watch_cmd("x")));
        assert!(HypeWatch::handles(&Command::StopHypeTrain));
        assert!(!HypeWatch::handles(&Command::StopRaids));
    }
}
