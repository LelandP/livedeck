//! Follow raids: `WatchRaids`, `StopRaids`, `DisconnectRaids` and `StartRaidCountdown`.
//!
//! The EventSub connection opens on the first `WatchRaids` and stays until `DisconnectRaids`
//! (sign-out), so changing channels only swaps the one subscription. Its raids reach the store as
//! `Event::Raided`; the reducer decides whether they concern the channel on screen.
//!
//! The countdown works like the sleep timer: one task, a tick every second, then done.

use std::sync::{Arc, Mutex, MutexGuard};
use std::time::Duration;

use htpc_core::{Command, Event, Msg};
use htpc_twitch_api::{EventSubEvent, RaidWatch};
use tokio::sync::mpsc::{unbounded_channel, UnboundedSender};
use tokio::task::JoinHandle;
use tokio::time::{sleep_until, Instant};

/// Opens a raid watch that reports into the sender. `None` if it cannot be made (logged).
pub type Connect =
    Arc<dyn Fn(UnboundedSender<EventSubEvent>) -> Option<Box<dyn RaidWatch>> + Send + Sync>;

/// An open raid watch and the task that hands its events to the store.
struct Connection {
    watch: Box<dyn RaidWatch>,
    forward: JoinHandle<()>,
}

impl Drop for Connection {
    fn drop(&mut self) {
        self.forward.abort();
    }
}

pub struct Raids {
    connect: Connect,
    tx: UnboundedSender<Msg>,
    connection: Mutex<Option<Connection>>,
    countdown: Mutex<Option<JoinHandle<()>>>,
}

fn lock<T>(m: &Mutex<T>) -> MutexGuard<'_, T> {
    m.lock().unwrap_or_else(|e| e.into_inner())
}

impl Raids {
    pub fn new(connect: Connect, tx: UnboundedSender<Msg>) -> Self {
        Self {
            connect,
            tx,
            connection: Mutex::new(None),
            countdown: Mutex::new(None),
        }
    }

    /// True for the commands this module runs.
    pub fn handles(cmd: &Command) -> bool {
        matches!(
            cmd,
            Command::WatchRaids { .. }
                | Command::StopRaids
                | Command::DisconnectRaids
                | Command::StartRaidCountdown { .. }
        )
    }

    /// Runs a command. Must be called inside the tokio runtime.
    pub fn execute(&self, cmd: Command) {
        match cmd {
            Command::WatchRaids { broadcaster_id } => {
                let mut connection = lock(&self.connection);
                if connection.is_none() {
                    *connection = self.open();
                }
                if let Some(c) = connection.as_ref() {
                    c.watch.watch_raids(&broadcaster_id);
                }
            }
            Command::StopRaids => {
                if let Some(c) = lock(&self.connection).as_ref() {
                    c.watch.clear();
                }
            }
            // Dropping the watch closes the socket.
            Command::DisconnectRaids => drop(lock(&self.connection).take()),
            Command::StartRaidCountdown { id, seconds } => {
                let task = tokio::spawn(countdown(id, seconds, self.tx.clone()));
                if let Some(old) = lock(&self.countdown).replace(task) {
                    old.abort();
                }
            }
            _ => {}
        }
    }

    fn open(&self) -> Option<Connection> {
        let (events, mut rx) = unbounded_channel();
        let watch = (self.connect)(events)?;
        let tx = self.tx.clone();
        let forward = tokio::spawn(async move {
            while let Some(event) = rx.recv().await {
                let EventSubEvent::Raid(raid) = event;
                let event = Event::Raided {
                    from_login: raid.from_login,
                    to_login: raid.to_login,
                    to_name: raid.to_name,
                    viewers: raid.viewers,
                };
                if tx.send(event.into()).is_err() {
                    return;
                }
            }
        });
        Some(Connection { watch, forward })
    }
}

/// Ticks once a second with the whole seconds left, then reports the end.
async fn countdown(id: u32, seconds: u8, tx: UnboundedSender<Msg>) {
    let start = Instant::now();
    for k in 1..seconds {
        sleep_until(start + Duration::from_secs(u64::from(k))).await;
        let tick = Event::RaidTick {
            id,
            left: seconds - k,
        };
        if tx.send(tick.into()).is_err() {
            return;
        }
    }
    sleep_until(start + Duration::from_secs(u64::from(seconds))).await;
    let _ = tx.send(Event::RaidCountdownDone { id }.into());
}

#[cfg(test)]
mod tests {
    use super::*;
    use htpc_core::ChannelRef;
    use htpc_twitch_api::{FakeEventSub, FakeEventSubCall, Raid};
    use tokio::sync::mpsc::UnboundedReceiver;

    /// The event sender of every connection opened so far, oldest first.
    type Opened = Arc<Mutex<Vec<UnboundedSender<EventSubEvent>>>>;

    /// A raids module whose connections are fakes. Returns the store's receiver, the fake (shared
    /// by every connection) and the connections opened.
    fn raids() -> (Raids, UnboundedReceiver<Msg>, FakeEventSub, Opened) {
        let (tx, rx) = unbounded_channel();
        let fake = FakeEventSub::new();
        let opened = Arc::new(Mutex::new(Vec::new()));
        let connect: Connect = {
            let fake = fake.clone();
            let opened = Arc::clone(&opened);
            Arc::new(move |events| {
                opened.lock().unwrap().push(events);
                Some(Box::new(fake.clone()) as Box<dyn RaidWatch>)
            })
        };
        (Raids::new(connect, tx), rx, fake, opened)
    }

    fn watch(id: &str) -> Command {
        Command::WatchRaids {
            broadcaster_id: id.into(),
        }
    }

    #[tokio::test]
    async fn one_connection_serves_every_channel_until_sign_out() {
        let (raids, _rx, fake, opened) = raids();
        raids.execute(Command::StopRaids);
        assert!(opened.lock().unwrap().is_empty(), "nothing to stop yet");
        raids.execute(watch("1"));
        raids.execute(Command::StopRaids);
        raids.execute(watch("2"));
        assert_eq!(
            opened.lock().unwrap().len(),
            1,
            "the channel changes, not the socket"
        );
        assert_eq!(
            fake.calls(),
            [
                FakeEventSubCall::Watch("1".into()),
                FakeEventSubCall::Clear,
                FakeEventSubCall::Watch("2".into()),
            ]
        );
        raids.execute(Command::DisconnectRaids);
        raids.execute(watch("3"));
        assert_eq!(
            opened.lock().unwrap().len(),
            2,
            "signed in again: a new one"
        );
    }

    #[tokio::test]
    async fn raids_reach_the_store_as_events() {
        let (raids, mut rx, _fake, opened) = raids();
        raids.execute(watch("1"));
        let events = opened.lock().unwrap()[0].clone();
        events
            .send(EventSubEvent::Raid(Raid {
                from_id: "1".into(),
                from_login: "ender".into(),
                from_name: "Ender".into(),
                to_id: "2".into(),
                to_login: "next_up".into(),
                to_name: "Next_Up".into(),
                viewers: 1234,
            }))
            .unwrap();
        let got = tokio::time::timeout(Duration::from_secs(1), rx.recv())
            .await
            .unwrap();
        assert_eq!(
            got,
            Some(Msg::Event(Event::Raided {
                from_login: "ender".into(),
                to_login: "next_up".into(),
                to_name: "Next_Up".into(),
                viewers: 1234,
            }))
        );
    }

    #[tokio::test]
    async fn a_watch_that_cannot_open_is_tried_again_next_time() {
        let (tx, _rx) = unbounded_channel();
        let tries = Arc::new(Mutex::new(0));
        let connect: Connect = {
            let tries = Arc::clone(&tries);
            Arc::new(move |_| {
                *tries.lock().unwrap() += 1;
                None
            })
        };
        let raids = Raids::new(connect, tx);
        raids.execute(watch("1"));
        raids.execute(watch("1"));
        assert_eq!(*tries.lock().unwrap(), 2);
    }

    /// Every message until `RaidCountdownDone`, with the seconds since `start`.
    async fn until_done(rx: &mut UnboundedReceiver<Msg>, start: Instant) -> Vec<(u64, Event)> {
        let mut out = Vec::new();
        while let Ok(Some(Msg::Event(event))) =
            tokio::time::timeout(Duration::from_secs(60), rx.recv()).await
        {
            let done = matches!(event, Event::RaidCountdownDone { .. });
            out.push((start.elapsed().as_secs(), event));
            if done {
                break;
            }
        }
        out
    }

    #[tokio::test(start_paused = true)]
    async fn the_countdown_ticks_each_second_then_finishes() {
        let (raids, mut rx, _, _) = raids();
        let start = Instant::now();
        raids.execute(Command::StartRaidCountdown { id: 4, seconds: 10 });
        let got = until_done(&mut rx, start).await;
        let mut want: Vec<(u64, Event)> = (1..10u8)
            .map(|k| {
                (
                    u64::from(k),
                    Event::RaidTick {
                        id: 4,
                        left: 10 - k,
                    },
                )
            })
            .collect();
        want.push((10, Event::RaidCountdownDone { id: 4 }));
        assert_eq!(got, want);
    }

    #[tokio::test(start_paused = true)]
    async fn a_new_countdown_replaces_the_old_one() {
        let (raids, mut rx, _, _) = raids();
        let start = Instant::now();
        raids.execute(Command::StartRaidCountdown { id: 1, seconds: 10 });
        tokio::time::sleep(Duration::from_millis(3500)).await;
        raids.execute(Command::StartRaidCountdown { id: 2, seconds: 10 });
        let got = until_done(&mut rx, start).await;
        assert!(
            !got.iter()
                .any(|(_, e)| *e == Event::RaidCountdownDone { id: 1 }),
            "{got:?}"
        );
        assert_eq!(got.last(), Some(&(13, Event::RaidCountdownDone { id: 2 })));
    }

    #[test]
    fn handles_only_raid_commands() {
        assert!(Raids::handles(&watch("1")));
        assert!(Raids::handles(&Command::StopRaids));
        assert!(Raids::handles(&Command::DisconnectRaids));
        assert!(Raids::handles(&Command::StartRaidCountdown {
            id: 1,
            seconds: 10
        }));
        assert!(!Raids::handles(&Command::FetchStreamInfo {
            channel: ChannelRef::twitch("x")
        }));
        assert!(!Raids::handles(&Command::StopPlayback));
    }
}
