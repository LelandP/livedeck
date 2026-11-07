//! Live notifications: `StartLiveWatch`, `StopLiveWatch` and `ExpireToast`.
//!
//! A running watch is one tokio task that asks for the live followed channels every [`POLL`]
//! (`GET /streams/followed`: one request for up to 100 live channels) and reports the ones that
//! were not live before as `Event::WentLive`. Polling, not EventSub: a desktop app can only use
//! EventSub over a WebSocket, whose cost limit of 10 per token would cover about ten channels.
//!
//! [`LiveDiff`] holds the rules, and is tested on its own with made-up times:
//! - the first poll only notes who is live, so channels that were already live do not all pop up
//!   at startup or after sign-in;
//! - so does the first poll after a long gap, which means the device slept (the wall clock jumps
//!   while the monotonic one may not, so the gap is measured on the wall clock);
//! - a channel that drops out and comes back within [`RESTART_GRACE_SECS`] restarted its stream
//!   and is not announced again.

use std::collections::{HashMap, HashSet};
use std::sync::{Arc, Mutex};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use htpc_core::{ChannelRef, Command, Event, Msg, Stream};
use tokio::sync::mpsc::UnboundedSender;
use tokio::task::JoinHandle;
use tokio::time::MissedTickBehavior;

use crate::provider::Provider;

/// How often the live followed channels are fetched.
pub const POLL: Duration = Duration::from_secs(30);
/// How long a toast stays on screen.
pub const TOAST_TIME: Duration = Duration::from_secs(8);
/// A channel offline for less than this before it is live again only restarted its stream.
pub const RESTART_GRACE_SECS: u64 = 10 * 60;
/// Polls further apart than this (on the wall clock) mean the device slept in between.
pub const WAKE_GAP_SECS: u64 = 3 * 30;

/// Who was live at the last poll, and when each channel was last seen live.
#[derive(Debug, Default)]
pub struct LiveDiff {
    live: HashSet<ChannelRef>,
    last_seen: HashMap<ChannelRef, u64>,
    /// Wall-clock seconds of the last successful poll. `None` before the first.
    last_poll: Option<u64>,
}

impl LiveDiff {
    /// Takes the live channels of a poll at wall-clock second `now` and returns the ones that
    /// went live since the last poll, in the order given.
    pub fn update(&mut self, now: u64, streams: Vec<Stream>) -> Vec<Stream> {
        let woke = self
            .last_poll
            .is_some_and(|last| now.saturating_sub(last) > WAKE_GAP_SECS);
        let baseline = self.last_poll.is_none() || woke;
        self.last_poll = Some(now);

        let mut went_live = Vec::new();
        let mut live = HashSet::new();
        for stream in streams {
            let key = ChannelRef::new(
                stream.channel.platform,
                stream.channel.login.to_ascii_lowercase(),
            );
            if !live.insert(key.clone()) {
                continue;
            }
            let was_live = self.live.contains(&key);
            let restarted = self
                .last_seen
                .get(&key)
                .is_some_and(|seen| now.saturating_sub(*seen) <= RESTART_GRACE_SECS);
            if !baseline && !was_live && !restarted {
                went_live.push(stream);
            }
            self.last_seen.insert(key, now);
        }
        self.live = live;
        // Only recent sightings matter for the restart rule.
        self.last_seen
            .retain(|_, seen| now.saturating_sub(*seen) <= RESTART_GRACE_SECS);
        went_live
    }
}

/// Seconds since the Unix epoch on the wall clock, which keeps counting while the device sleeps.
fn wall_secs() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| d.as_secs())
}

#[derive(Default)]
pub struct LiveWatch {
    task: Mutex<Option<JoinHandle<()>>>,
}

impl LiveWatch {
    /// True for the commands this module runs.
    pub fn handles(cmd: &Command) -> bool {
        matches!(
            cmd,
            Command::StartLiveWatch | Command::StopLiveWatch | Command::ExpireToast { .. }
        )
    }

    /// Runs a command. Must be called inside the tokio runtime.
    pub fn execute<P: Provider>(&self, provider: Arc<P>, tx: UnboundedSender<Msg>, cmd: Command) {
        match cmd {
            Command::StartLiveWatch => {
                let mut task = self.task.lock().unwrap_or_else(|e| e.into_inner());
                if let Some(old) = task.take() {
                    old.abort();
                }
                *task = Some(tokio::spawn(watch(provider, tx, wall_secs)));
            }
            Command::StopLiveWatch => {
                let mut task = self.task.lock().unwrap_or_else(|e| e.into_inner());
                if let Some(old) = task.take() {
                    old.abort();
                }
            }
            Command::ExpireToast { id } => {
                tokio::spawn(async move {
                    tokio::time::sleep(TOAST_TIME).await;
                    let _ = tx.send(Event::ToastExpired(id).into());
                });
            }
            _ => {}
        }
    }
}

/// Polls until aborted (or the store is gone). A failed poll is skipped and changes nothing.
async fn watch<P: Provider>(provider: Arc<P>, tx: UnboundedSender<Msg>, now: fn() -> u64) {
    let mut diff = LiveDiff::default();
    let mut tick = tokio::time::interval(POLL);
    tick.set_missed_tick_behavior(MissedTickBehavior::Delay);
    loop {
        tick.tick().await;
        let Some(streams) = provider.followed_live().await else {
            continue;
        };
        let fresh = diff.update(now(), streams);
        if fresh.is_empty() {
            continue;
        }
        tracing::debug!(count = fresh.len(), "followed channels went live");
        if tx.send(Event::WentLive(fresh).into()).is_err() {
            return;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use htpc_core::Rgb;
    use htpc_ui::Thumbnail;
    use std::collections::VecDeque;
    use tokio::sync::mpsc::unbounded_channel;

    fn stream(channel: &str) -> Stream {
        Stream {
            channel: ChannelRef::twitch(channel),
            user_id: String::new(),
            title: String::new(),
            category: String::new(),
            viewers: 1,
            tint: Rgb::default(),
            thumbnail: None,
        }
    }

    fn live(channels: &[&str]) -> Vec<Stream> {
        channels.iter().map(|c| stream(c)).collect()
    }

    fn names(streams: &[Stream]) -> Vec<&str> {
        streams.iter().map(|s| s.channel.login.as_str()).collect()
    }

    // ---- the rules -------------------------------------------------------------------------

    #[test]
    fn the_first_poll_is_only_a_baseline() {
        let mut d = LiveDiff::default();
        assert!(d.update(1000, live(&["a", "b"])).is_empty());
        let fresh = d.update(1030, live(&["a", "b", "c"]));
        assert_eq!(names(&fresh), ["c"]);
        // Still live: not announced again.
        assert!(d.update(1060, live(&["a", "b", "c"])).is_empty());
    }

    #[test]
    fn a_quick_restart_is_not_announced_but_a_real_return_is() {
        let mut d = LiveDiff::default();
        d.update(0, live(&["a"]));
        // `a` drops out for one poll and comes back: a stream restart.
        assert!(d.update(30, Vec::new()).is_empty());
        assert!(d.update(60, live(&["a"])).is_empty());
        // Offline for longer than the grace period, then live again: announced.
        let mut t = 60;
        while t < 60 + RESTART_GRACE_SECS + 60 {
            t += 30;
            d.update(t, Vec::new());
        }
        assert_eq!(names(&d.update(t + 30, live(&["a"]))), ["a"]);
    }

    #[test]
    fn a_long_gap_means_the_device_slept_and_starts_a_new_baseline() {
        let mut d = LiveDiff::default();
        d.update(0, live(&["a"]));
        // An hour later (asleep), `b` and `c` are live: no burst of toasts.
        assert!(d.update(3600, live(&["a", "b", "c"])).is_empty());
        // Normal polling again from there.
        assert_eq!(names(&d.update(3630, live(&["a", "b", "c", "d"]))), ["d"]);
    }

    #[test]
    fn case_and_repeats_do_not_count_as_new() {
        let mut d = LiveDiff::default();
        d.update(0, live(&["Marrow"]));
        assert!(d.update(30, live(&["marrow", "MARROW"])).is_empty());
        let fresh = d.update(60, live(&["marrow", "new", "new"]));
        assert_eq!(names(&fresh), ["new"]);
    }

    // ---- the task --------------------------------------------------------------------------

    /// Answers each poll with the next list (`None` = a failed request), then the last one again.
    struct Scripted(Mutex<VecDeque<Option<Vec<Stream>>>>, Mutex<usize>);

    impl Scripted {
        fn new(polls: Vec<Option<Vec<Stream>>>) -> Arc<Self> {
            Arc::new(Self(Mutex::new(polls.into()), Mutex::new(0)))
        }

        fn polls(&self) -> usize {
            *self.1.lock().unwrap()
        }
    }

    impl Provider for Scripted {
        async fn execute(&self, _cmd: Command) -> Vec<Event> {
            Vec::new()
        }

        async fn thumbnail(&self, _url: &str) -> Option<Thumbnail> {
            None
        }

        async fn followed_live(&self) -> Option<Vec<Stream>> {
            *self.1.lock().unwrap() += 1;
            let mut polls = self.0.lock().unwrap();
            if polls.len() > 1 {
                polls.pop_front().flatten()
            } else {
                polls.front().cloned().flatten()
            }
        }
    }

    fn went_live(rx: &mut tokio::sync::mpsc::UnboundedReceiver<Msg>) -> Vec<Vec<String>> {
        let mut out = Vec::new();
        while let Ok(Msg::Event(Event::WentLive(streams))) = rx.try_recv() {
            out.push(streams.into_iter().map(|s| s.channel.login).collect());
        }
        out
    }

    #[tokio::test(start_paused = true)]
    async fn polls_every_30_s_and_reports_only_new_channels() {
        let provider = Scripted::new(vec![
            Some(live(&["a"])),
            None,
            Some(live(&["a", "b"])),
            Some(live(&["a", "b"])),
        ]);
        let (tx, mut rx) = unbounded_channel();
        let watch = LiveWatch::default();
        watch.execute(Arc::clone(&provider), tx, Command::StartLiveWatch);
        // The first poll is immediate, then one every 30 s.
        tokio::time::sleep(POLL * 3 + Duration::from_secs(1)).await;
        assert_eq!(provider.polls(), 4);
        assert_eq!(went_live(&mut rx), [vec!["b".to_owned()]]);
    }

    #[tokio::test(start_paused = true)]
    async fn stop_ends_the_polling_and_start_begins_with_a_new_baseline() {
        let provider = Scripted::new(vec![Some(live(&["a"]))]);
        let (tx, mut rx) = unbounded_channel();
        let watch = LiveWatch::default();
        watch.execute(Arc::clone(&provider), tx.clone(), Command::StartLiveWatch);
        tokio::time::sleep(POLL + Duration::from_secs(1)).await;
        watch.execute(Arc::clone(&provider), tx.clone(), Command::StopLiveWatch);
        let polls = provider.polls();
        tokio::time::sleep(POLL * 10).await;
        assert_eq!(provider.polls(), polls, "no requests after Stop");

        // A new watch does not announce `a`, who was live all along.
        watch.execute(Arc::clone(&provider), tx, Command::StartLiveWatch);
        tokio::time::sleep(POLL * 2).await;
        assert!(went_live(&mut rx).is_empty());
    }

    #[tokio::test(start_paused = true)]
    async fn a_toast_expires_after_its_time() {
        let provider = Scripted::new(vec![None]);
        let (tx, mut rx) = unbounded_channel();
        let watch = LiveWatch::default();
        watch.execute(provider, tx, Command::ExpireToast { id: 4 });
        tokio::time::sleep(TOAST_TIME - Duration::from_millis(100)).await;
        assert!(rx.try_recv().is_err(), "too early");
        tokio::time::sleep(Duration::from_millis(200)).await;
        assert!(matches!(
            rx.try_recv(),
            Ok(Msg::Event(Event::ToastExpired(4)))
        ));
    }

    #[test]
    fn handles_only_its_commands() {
        assert!(LiveWatch::handles(&Command::StartLiveWatch));
        assert!(LiveWatch::handles(&Command::StopLiveWatch));
        assert!(LiveWatch::handles(&Command::ExpireToast { id: 1 }));
        assert!(!LiveWatch::handles(&Command::StopPlayback));
    }
}
