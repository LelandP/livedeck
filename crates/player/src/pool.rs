//! `PlayerPool`: one player per slot, made when a slot is first used and dropped after it has been
//! idle for a minute. Slot 0 (the single player) is never dropped.
//!
//! A slot can also have a *spare*: a second player that loads the same stream at another quality
//! behind the one on screen, muted and drawn into a tiny hidden target. Its first frame swaps the
//! two, so changing quality never leaves a gap (see [`PlayerPool::spare`]).

use std::collections::BTreeMap;
use std::mem::discriminant;
use std::sync::{Arc, Mutex, MutexGuard, Weak};
use std::time::{Duration, Instant};

use crate::{
    FakePlayer, LoadOptions, Player, PlayerEvent, PlayerOption, Slot, UnavailablePlayer, MAIN_SLOT,
    SLOTS,
};

/// How long a stopped player is kept for a quick restart before it is dropped. An mpv player holds
/// a few hundred MB while it has a stream open, and a little while stopped.
pub const IDLE_DROP: Duration = Duration::from_secs(60);

/// Where a slot's player sends its events. The pool tags them with the slot.
pub type Report = Arc<dyn Fn(PlayerEvent) + Send + Sync>;

/// Which of a slot's players is being made.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Seat {
    /// The one on screen.
    Main,
    /// The spare that loads a new quality behind it. Never shown until it takes over.
    Spare,
}

type Make = dyn Fn(Slot, Seat, Report) -> Result<Arc<dyn Player>, String> + Send + Sync;
type OnEvent = Arc<dyn Fn(Slot, PlayerEvent) + Send + Sync>;
type OnSwap = Arc<dyn Fn(Slot) + Send + Sync>;

/// A player and the number its events are known by.
struct Held {
    id: u64,
    player: Arc<dyn Player>,
}

#[derive(Default)]
struct Entry {
    main: Option<Held>,
    /// Loading another quality of the main player's stream, see [`PlayerPool::spare`].
    spare: Option<Held>,
    /// Stopped since then. `None` while it plays (or was never stopped).
    idle_since: Option<Instant>,
    /// The main player shows a stream: it reported its first frame, and has not ended, failed or
    /// been stopped since.
    playing: bool,
    /// The slot's sound is off ([`PlayerPool::set_muted`]). A spare is always muted until it takes
    /// over, then gets this.
    muted: bool,
    /// Stats are on for the slot ([`PlayerPool::set_stats`]), so a spare that takes over gets them.
    stats: bool,
}

struct Inner {
    slots: [Entry; SLOTS],
    /// Options that hold for every player, also the ones made later: the last of each kind.
    shared: Vec<PlayerOption>,
    /// The id the next player gets.
    next_id: u64,
}

/// The players, one per [`Slot`]. Every method returns at once except when a player is made,
/// which starts its core (a few milliseconds for mpv).
pub struct PlayerPool {
    make: Box<Make>,
    on_event: OnEvent,
    on_swap: OnSwap,
    inner: Arc<Mutex<Inner>>,
}

impl PlayerPool {
    /// `make` creates a player of a slot, which sends its events to the given [`Report`]. Those
    /// reach `on_event` with the slot. A slot whose player cannot be made gets an
    /// [`UnavailablePlayer`] that says why, and the next use tries again.
    pub fn new(
        make: impl Fn(Slot, Seat, Report) -> Result<Arc<dyn Player>, String> + Send + Sync + 'static,
        on_event: impl Fn(Slot, PlayerEvent) + Send + Sync + 'static,
    ) -> Self {
        Self {
            make: Box::new(make),
            on_event: Arc::new(on_event),
            on_swap: Arc::new(|_| {}),
            inner: Arc::new(Mutex::new(Inner {
                slots: std::array::from_fn(|_| Entry::default()),
                shared: Vec::new(),
                next_id: 1,
            })),
        }
    }

    /// Runs `on_swap` with the slot whenever a spare takes over (before the old player stops), so
    /// the renderer can show the spare's frames in the slot from then on.
    #[must_use]
    pub fn with_swap(mut self, on_swap: impl Fn(Slot) + Send + Sync + 'static) -> Self {
        self.on_swap = Arc::new(on_swap);
        self
    }

    /// A pool of [`FakePlayer`]s for tests, and the handle that reaches them.
    pub fn fake() -> (Arc<Self>, FakePool) {
        let fakes = FakePool::default();
        let made = fakes.clone();
        let events = fakes.clone();
        let swapped = fakes.clone();
        let pool = Self::new(
            move |slot, seat, report| {
                let player = FakePlayer::new();
                player.on_event(move |e| report(e));
                let mut map = match seat {
                    Seat::Main => made.players.lock(),
                    Seat::Spare => made.spares.lock(),
                }
                .expect("fake pool lock");
                map.insert(slot, player.clone());
                Ok(Arc::new(player))
            },
            move |slot, event| {
                events
                    .events
                    .lock()
                    .expect("fake pool lock")
                    .push((slot, event))
            },
        )
        .with_swap(move |slot| swapped.swap(slot));
        (Arc::new(pool), fakes)
    }

    fn lock(&self) -> MutexGuard<'_, Inner> {
        self.inner.lock().expect("player pool lock")
    }

    /// Makes a player for `seat` of `slot`, its events routed by its id. Called with the lock held.
    fn start(&self, inner: &mut Inner, slot: Slot, seat: Seat) -> Result<Held, String> {
        let id = inner.next_id;
        inner.next_id += 1;
        let route = Route {
            inner: Arc::downgrade(&self.inner),
            on_event: Arc::clone(&self.on_event),
            on_swap: Arc::clone(&self.on_swap),
            slot,
            id,
        };
        let report: Report = Arc::new(move |event| route.event(event));
        let player = (self.make)(slot, seat, report)?;
        for option in &inner.shared {
            player.set_option(*option);
        }
        Ok(Held { id, player })
    }

    /// The player of `slot`, made now if it has none. It counts as in use until [`Self::stop`].
    ///
    /// # Panics
    /// If `slot` is not below [`SLOTS`].
    pub fn player(&self, slot: Slot) -> Arc<dyn Player> {
        let mut inner = self.lock();
        let i = index(slot);
        inner.slots[i].idle_since = None;
        if let Some(held) = &inner.slots[i].main {
            return Arc::clone(&held.player);
        }
        match self.start(&mut inner, slot, Seat::Main) {
            Ok(held) => {
                tracing::debug!(slot, "player started");
                let player = Arc::clone(&held.player);
                inner.slots[i].main = Some(held);
                player
            }
            Err(err) => {
                tracing::error!(slot, %err, "video is off");
                let on_event = Arc::clone(&self.on_event);
                // Shown on the player's error screen, so the real cause is visible without a log.
                Arc::new(UnavailablePlayer::with_reason(
                    move |event| on_event(slot, event),
                    format!("video could not start: {err}"),
                ))
            }
        }
    }

    /// The player of `slot` for a new stream: any spare is dropped, as it loads the old one.
    pub fn fresh(&self, slot: Slot) -> Arc<dyn Player> {
        let spare = {
            let mut inner = self.lock();
            let entry = &mut inner.slots[index(slot)];
            entry.playing = false;
            entry.spare.take()
        };
        if let Some(spare) = spare {
            spare.player.stop();
        }
        self.player(slot)
    }

    /// The main player of `slot` shows a stream now (see [`Self::spare`]).
    pub fn is_playing(&self, slot: Slot) -> bool {
        let inner = self.lock();
        let entry = &inner.slots[index(slot)];
        entry.playing && entry.main.is_some()
    }

    /// A spare for `slot`, to load another quality of the stream on screen (made if needed, or the
    /// one already loading). It is muted, and the renderer draws it hidden. When it reports its
    /// first frame it takes over the slot: it gets the slot's sound and stats, the renderer shows
    /// it (`with_swap`), and the old player stops. If it fails or ends first, it is dropped and the
    /// stream on screen plays on. `None` if no player could be made.
    pub fn spare(&self, slot: Slot) -> Option<Arc<dyn Player>> {
        let mut inner = self.lock();
        let i = index(slot);
        inner.slots[i].idle_since = None;
        if let Some(held) = &inner.slots[i].spare {
            return Some(Arc::clone(&held.player));
        }
        match self.start(&mut inner, slot, Seat::Spare) {
            Ok(held) => {
                tracing::debug!(slot, "spare player started");
                held.player.set_option(PlayerOption::Muted(true));
                let player = Arc::clone(&held.player);
                inner.slots[i].spare = Some(held);
                Some(player)
            }
            Err(err) => {
                tracing::warn!(slot, %err, "no spare player, reloading in place");
                None
            }
        }
    }

    /// The player of `slot` if it has one. Never makes one.
    pub fn existing(&self, slot: Slot) -> Option<Arc<dyn Player>> {
        self.lock().slots[index(slot)]
            .main
            .as_ref()
            .map(|h| Arc::clone(&h.player))
    }

    /// Turns the sound of `slot` off or on (made if needed: a muted tile is muted before its first
    /// load). A spare stays muted until it takes over, then follows this.
    pub fn set_muted(&self, slot: Slot, muted: bool) {
        self.lock().slots[index(slot)].muted = muted;
        self.player(slot).set_option(PlayerOption::Muted(muted));
    }

    /// Turns the stats reports of `slot` on or off, for its player and a spare that takes over.
    pub fn set_stats(&self, slot: Slot, on: bool) {
        self.lock().slots[index(slot)].stats = on;
        if let Some(player) = self.existing(slot) {
            player.set_stats(on);
        }
    }

    /// Stops the stream of `slot`, and drops its spare. Its player is kept for [`IDLE_DROP`] in
    /// case it is needed again.
    pub fn stop(&self, slot: Slot) {
        let (player, spare) = {
            let mut inner = self.lock();
            let entry = &mut inner.slots[index(slot)];
            if entry.main.is_some() && entry.idle_since.is_none() {
                entry.idle_since = Some(Instant::now());
            }
            entry.playing = false;
            (
                entry.main.as_ref().map(|h| Arc::clone(&h.player)),
                entry.spare.take(),
            )
        };
        if let Some(spare) = spare {
            spare.player.stop();
        }
        if let Some(player) = player {
            player.stop();
        }
    }

    /// Sets an option on every player, and on the ones made later (hardware decoding, zoom).
    pub fn set_shared_option(&self, option: PlayerOption) {
        let players: Vec<_> = {
            let mut inner = self.lock();
            inner
                .shared
                .retain(|o| discriminant(o) != discriminant(&option));
            inner.shared.push(option);
            inner
                .slots
                .iter()
                .flat_map(|e| [&e.main, &e.spare])
                .flatten()
                .map(|h| Arc::clone(&h.player))
                .collect()
        };
        for player in players {
            player.set_option(option);
        }
    }

    /// Drops the players that have been stopped for [`IDLE_DROP`]. Returns how many. Call about
    /// every 15 s.
    pub fn drop_idle(&self) -> usize {
        self.drop_idle_at(Instant::now())
    }

    /// [`Self::drop_idle`] as if it were `now`.
    pub fn drop_idle_at(&self, now: Instant) -> usize {
        let dropped: Vec<_> = {
            let mut inner = self.lock();
            inner
                .slots
                .iter_mut()
                .enumerate()
                .filter(|(i, _)| *i != usize::from(MAIN_SLOT))
                .filter(|(_, e)| {
                    e.idle_since
                        .is_some_and(|since| now.saturating_duration_since(since) >= IDLE_DROP)
                })
                .filter_map(|(i, e)| {
                    e.idle_since = None;
                    e.spare = None;
                    e.main.take().map(|p| (i, p))
                })
                .collect()
        };
        for (slot, _) in &dropped {
            tracing::debug!(slot, "idle player dropped");
        }
        // The players drop here, outside the lock.
        dropped.len()
    }

    /// The slots that have a player.
    pub fn slots_in_use(&self) -> Vec<Slot> {
        self.lock()
            .slots
            .iter()
            .enumerate()
            .filter(|(_, e)| e.main.is_some())
            .map(|(i, _)| i as Slot)
            .collect()
    }

    /// A [`Player`] that always reaches the current player of `slot`, for code that holds on to a
    /// player (the sleep timer). Loads and options make the slot's player if it has none; the
    /// other calls do nothing then.
    pub fn handle(self: &Arc<Self>, slot: Slot) -> Arc<dyn Player> {
        index(slot);
        Arc::new(SlotPlayer {
            pool: Arc::clone(self),
            slot,
        })
    }
}

fn index(slot: Slot) -> usize {
    let i = usize::from(slot);
    assert!(
        i < SLOTS,
        "slot {slot} is out of range (0 to {})",
        SLOTS - 1
    );
    i
}

/// Where one player's events go: forwarded while it is its slot's main player, the swap when it
/// is the spare, and nowhere once it is neither (it was replaced or dropped).
struct Route {
    inner: Weak<Mutex<Inner>>,
    on_event: OnEvent,
    on_swap: OnSwap,
    slot: Slot,
    id: u64,
}

/// What a spare's report leads to.
enum SpareOutcome {
    /// It took over: the new main player, the old one, and the slot's sound and stats.
    TookOver {
        new: Arc<dyn Player>,
        old: Option<Held>,
        muted: bool,
        stats: bool,
    },
    /// It failed or ended before its first frame: dropped.
    Dropped(Held),
}

impl Route {
    fn event(&self, event: PlayerEvent) {
        let Some(inner) = self.inner.upgrade() else {
            return;
        };
        let mut outcome = None;
        let forward = {
            let mut inner = inner.lock().expect("player pool lock");
            let entry = &mut inner.slots[usize::from(self.slot)];
            if entry.main.as_ref().is_some_and(|h| h.id == self.id) {
                match event {
                    PlayerEvent::Playing => entry.playing = true,
                    PlayerEvent::Ended | PlayerEvent::Failed(_) => entry.playing = false,
                    _ => {}
                }
                Some(event)
            } else if entry.spare.as_ref().is_some_and(|h| h.id == self.id) {
                match event {
                    PlayerEvent::Playing => {
                        let spare = entry.spare.take().expect("checked above");
                        let new = Arc::clone(&spare.player);
                        let old = entry.main.replace(spare);
                        entry.playing = true;
                        outcome = Some(SpareOutcome::TookOver {
                            new,
                            old,
                            muted: entry.muted,
                            stats: entry.stats,
                        });
                        Some(PlayerEvent::Playing)
                    }
                    PlayerEvent::Ended | PlayerEvent::Failed(_) => {
                        tracing::warn!(
                            slot = self.slot,
                            ?event,
                            "the new quality did not start, the old one plays on"
                        );
                        outcome = entry.spare.take().map(SpareOutcome::Dropped);
                        None
                    }
                    // Not on screen yet: its health, stats and pause say nothing about the slot.
                    _ => None,
                }
            } else {
                None
            }
        };
        match outcome {
            Some(SpareOutcome::TookOver {
                new,
                old,
                muted,
                stats,
            }) => {
                tracing::debug!(slot = self.slot, "the spare took over");
                new.set_option(PlayerOption::Muted(muted));
                new.set_stats(stats);
                (self.on_swap)(self.slot);
                if let Some(old) = old {
                    old.player.stop();
                }
            }
            Some(SpareOutcome::Dropped(spare)) => spare.player.stop(),
            None => {}
        }
        if let Some(event) = forward {
            (self.on_event)(self.slot, event);
        }
    }
}

struct SlotPlayer {
    pool: Arc<PlayerPool>,
    slot: Slot,
}

impl SlotPlayer {
    fn existing(&self, f: impl FnOnce(&dyn Player)) {
        if let Some(player) = self.pool.existing(self.slot) {
            f(&*player);
        }
    }
}

impl Player for SlotPlayer {
    fn load(&self, url: &str, options: LoadOptions) {
        self.pool.player(self.slot).load(url, options);
    }

    fn set_paused(&self, paused: bool) {
        self.existing(|p| p.set_paused(paused));
    }

    fn stop(&self) {
        self.pool.stop(self.slot);
    }

    fn set_option(&self, option: PlayerOption) {
        self.pool.player(self.slot).set_option(option);
    }

    fn set_stats(&self, on: bool) {
        self.existing(|p| p.set_stats(on));
    }

    fn fade_out(&self, duration: Duration) {
        self.existing(|p| p.fade_out(duration));
    }

    fn restore_volume(&self) {
        self.existing(|p| p.restore_volume());
    }
}

/// Reaches the [`FakePlayer`]s of a pool made by [`PlayerPool::fake`].
#[derive(Clone, Default)]
pub struct FakePool {
    /// The main player of each slot: the last one made, or the spare that took over.
    players: Arc<Mutex<BTreeMap<Slot, FakePlayer>>>,
    /// The spare of each slot, until it takes over.
    spares: Arc<Mutex<BTreeMap<Slot, FakePlayer>>>,
    /// Every event the pool reported, with its slot.
    events: Arc<Mutex<Vec<(Slot, PlayerEvent)>>>,
}

impl FakePool {
    /// The main player of `slot`, if it has one.
    pub fn player(&self, slot: Slot) -> Option<FakePlayer> {
        self.players
            .lock()
            .expect("fake pool lock")
            .get(&slot)
            .cloned()
    }

    /// The last spare made for `slot` that has not taken over.
    pub fn spare(&self, slot: Slot) -> Option<FakePlayer> {
        self.spares
            .lock()
            .expect("fake pool lock")
            .get(&slot)
            .cloned()
    }

    /// The calls made to the main player of `slot`. Empty if it has none.
    pub fn calls(&self, slot: Slot) -> Vec<crate::PlayerCall> {
        self.player(slot).map(|p| p.calls()).unwrap_or_default()
    }

    /// Pretends the main player of `slot` reported `event`. Does nothing if the slot has none.
    pub fn emit(&self, slot: Slot, event: PlayerEvent) {
        if let Some(player) = self.player(slot) {
            player.emit(event);
        }
    }

    /// What the pool reported so far, with the slots.
    pub fn events(&self) -> Vec<(Slot, PlayerEvent)> {
        self.events.lock().expect("fake pool lock").clone()
    }

    fn swap(&self, slot: Slot) {
        let spare = self.spares.lock().expect("fake pool lock").remove(&slot);
        if let Some(spare) = spare {
            self.players
                .lock()
                .expect("fake pool lock")
                .insert(slot, spare);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{PlayerCall, Zoom};
    use std::sync::atomic::{AtomicUsize, Ordering};

    #[test]
    fn players_are_made_on_first_use_and_kept() {
        let (pool, fakes) = PlayerPool::fake();
        assert!(pool.existing(3).is_none());
        let a = pool.player(3);
        let b = pool.player(3);
        assert!(Arc::ptr_eq(&a, &b));
        assert_eq!(pool.slots_in_use(), [3]);
        a.load("u", LoadOptions::default());
        assert_eq!(
            fakes.calls(3),
            [PlayerCall::Load("u".into(), LoadOptions::default())]
        );
    }

    #[test]
    fn events_come_back_tagged_with_the_slot() {
        let (pool, fakes) = PlayerPool::fake();
        pool.player(0);
        pool.player(5);
        fakes.emit(5, PlayerEvent::Playing);
        fakes.emit(0, PlayerEvent::Ended);
        assert_eq!(
            fakes.events(),
            [(5, PlayerEvent::Playing), (0, PlayerEvent::Ended)]
        );
    }

    #[test]
    fn idle_players_are_dropped_after_a_minute_but_never_slot_0() {
        let (pool, _) = PlayerPool::fake();
        let start = Instant::now();
        for slot in [0, 1, 2] {
            pool.player(slot);
            pool.stop(slot);
        }
        assert_eq!(pool.drop_idle_at(start + Duration::from_secs(30)), 0);

        // Slot 2 is used again before the minute is up, so it stays.
        pool.player(2);
        assert_eq!(
            pool.drop_idle_at(start + IDLE_DROP + Duration::from_secs(1)),
            1
        );
        assert_eq!(pool.slots_in_use(), [0, 2]);
    }

    #[test]
    fn a_dropped_slot_gets_a_new_player_next_time() {
        let made = Arc::new(AtomicUsize::new(0));
        let count = Arc::clone(&made);
        let pool = PlayerPool::new(
            move |_, _, _| {
                count.fetch_add(1, Ordering::SeqCst);
                Ok(Arc::new(FakePlayer::new()))
            },
            |_, _| {},
        );
        pool.player(4);
        pool.stop(4);
        pool.drop_idle_at(Instant::now() + IDLE_DROP);
        assert!(pool.existing(4).is_none());
        pool.player(4);
        assert_eq!(made.load(Ordering::SeqCst), 2);
    }

    #[test]
    fn shared_options_reach_every_player_and_later_ones() {
        let (pool, fakes) = PlayerPool::fake();
        pool.player(0);
        pool.set_shared_option(PlayerOption::Hwdec(true));
        pool.set_shared_option(PlayerOption::Zoom(Zoom::Fill));
        // The newer hwdec replaces the older for players made later.
        pool.set_shared_option(PlayerOption::Hwdec(false));
        pool.player(1);
        assert_eq!(
            fakes.calls(1),
            [
                PlayerCall::SetOption(PlayerOption::Zoom(Zoom::Fill)),
                PlayerCall::SetOption(PlayerOption::Hwdec(false)),
            ]
        );
        assert_eq!(fakes.calls(0).len(), 3);
    }

    #[test]
    fn a_player_that_cannot_start_reports_why_and_is_tried_again() {
        let seen = Arc::new(Mutex::new(Vec::new()));
        let sink = Arc::clone(&seen);
        let pool = PlayerPool::new(
            |_, _, _| Err("no libmpv".into()),
            move |slot, e| sink.lock().unwrap().push((slot, e)),
        );
        pool.player(2).load("u", LoadOptions::default());
        assert!(pool.existing(2).is_none());
        assert_eq!(
            *seen.lock().unwrap(),
            [(
                2,
                PlayerEvent::Failed("video could not start: no libmpv".into())
            )]
        );
    }

    #[test]
    fn a_handle_follows_the_slots_current_player() {
        let (pool, fakes) = PlayerPool::fake();
        let handle = pool.handle(1);
        // Nothing to fade yet, and no player is made for it.
        handle.fade_out(Duration::from_secs(5));
        assert!(pool.existing(1).is_none());

        handle.load("u", LoadOptions::default());
        handle.fade_out(Duration::from_secs(5));
        handle.stop();
        assert_eq!(
            fakes.calls(1),
            [
                PlayerCall::Load("u".into(), LoadOptions::default()),
                PlayerCall::FadeOut(Duration::from_secs(5)),
                PlayerCall::Stop,
            ]
        );
    }

    #[test]
    #[should_panic(expected = "out of range")]
    fn slots_past_the_last_are_a_bug() {
        let (pool, _) = PlayerPool::fake();
        pool.player(9);
    }

    /// Slot 2 plays `old`, sound on and stats on.
    fn playing_slot() -> (Arc<PlayerPool>, FakePool) {
        let (pool, fakes) = PlayerPool::fake();
        pool.set_muted(2, false);
        pool.set_stats(2, true);
        pool.player(2).load("old", LoadOptions::default());
        fakes.emit(2, PlayerEvent::Playing);
        assert!(pool.is_playing(2));
        (pool, fakes)
    }

    #[test]
    fn a_spare_takes_over_at_its_first_frame_with_the_slots_sound_and_stats() {
        let (pool, fakes) = playing_slot();
        let old = fakes.player(2).unwrap();
        let spare = pool.spare(2).expect("a spare");
        spare.load("new", LoadOptions::default());
        // Asked again while loading: the same spare.
        assert!(Arc::ptr_eq(&spare, &pool.spare(2).unwrap()));
        let fake_spare = fakes.spare(2).unwrap();
        assert_eq!(
            fake_spare.calls(),
            [
                PlayerCall::SetOption(PlayerOption::Muted(true)),
                PlayerCall::Load("new".into(), LoadOptions::default()),
            ]
        );
        // Its health says nothing until it is on screen.
        fake_spare.emit(PlayerEvent::Health(crate::PlayerHealth::default()));
        assert_eq!(fakes.events(), [(2, PlayerEvent::Playing)]);

        fake_spare.emit(PlayerEvent::Playing);
        assert_eq!(
            fakes.events(),
            [(2, PlayerEvent::Playing), (2, PlayerEvent::Playing)]
        );
        assert!(Arc::ptr_eq(&pool.existing(2).unwrap(), &spare));
        assert_eq!(
            fake_spare.calls()[2..],
            [
                PlayerCall::SetOption(PlayerOption::Muted(false)),
                PlayerCall::SetStats(true),
            ]
        );
        assert_eq!(old.calls().last(), Some(&PlayerCall::Stop));
        // The old one is gone: what it still says reaches nobody.
        old.emit(PlayerEvent::Ended);
        assert_eq!(fakes.events().len(), 2);
        assert!(pool.is_playing(2));
    }

    #[test]
    fn a_spare_that_fails_is_dropped_and_the_old_stream_plays_on() {
        let (pool, fakes) = playing_slot();
        let old = fakes.player(2).unwrap();
        pool.spare(2).unwrap().load("new", LoadOptions::default());
        let spare = fakes.spare(2).unwrap();
        spare.emit(PlayerEvent::Failed("no".into()));
        assert_eq!(fakes.events(), [(2, PlayerEvent::Playing)]);
        assert_eq!(spare.calls().last(), Some(&PlayerCall::Stop));
        assert!(!old.calls().contains(&PlayerCall::Stop));
        assert!(pool.is_playing(2));
        // A later spare is a new one.
        let again = pool.spare(2).unwrap();
        again.load("newer", LoadOptions::default());
        assert_eq!(
            fakes.spare(2).unwrap().calls().last(),
            Some(&PlayerCall::Load("newer".into(), LoadOptions::default()))
        );
    }

    #[test]
    fn stopping_or_a_fresh_stream_drops_the_spare() {
        let (pool, fakes) = playing_slot();
        pool.spare(2).unwrap();
        pool.stop(2);
        assert_eq!(
            fakes.spare(2).unwrap().calls().last(),
            Some(&PlayerCall::Stop)
        );
        assert!(!pool.is_playing(2));

        let (pool, fakes) = playing_slot();
        pool.spare(2).unwrap();
        let main = pool.fresh(2);
        assert!(Arc::ptr_eq(&main, &pool.existing(2).unwrap()));
        assert!(!pool.is_playing(2));
        let spare = fakes.spare(2).unwrap();
        assert_eq!(spare.calls().last(), Some(&PlayerCall::Stop));
        // Its late first frame changes nothing.
        spare.emit(PlayerEvent::Playing);
        assert_eq!(fakes.events(), [(2, PlayerEvent::Playing)]);
    }

    #[test]
    fn a_main_player_that_ends_is_no_longer_playing() {
        let (pool, fakes) = playing_slot();
        fakes.emit(2, PlayerEvent::Ended);
        assert!(!pool.is_playing(2));
    }
}
