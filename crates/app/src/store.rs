//! The store loop: `Msg -> reduce -> (state, Commands)`.
//!
//! One tokio task owns `AppState` and handles messages one at a time, so the reducer never runs
//! concurrently. Each `Command` runs as its own tokio task and sends its `Event`s back into the
//! same channel. After every reduce, the store pushes a view-model diff to the UI thread with
//! `slint::invoke_from_event_loop` (via `Weak::upgrade_in_event_loop`), so the UI never blocks.

use std::collections::HashSet;
use std::sync::Arc;

use htpc_core::{reduce, AppState, Command, Msg, Playback as CorePlayback, Screen, WakeCheck};
use htpc_ui::{
    apply_chat_image, apply_thumbnail, view_model, MainWindow, Thumbnail, Update, ViewModel,
};
use tokio::sync::mpsc::{UnboundedReceiver, UnboundedSender};
use tokio::sync::Semaphore;

use htpc_resolver::StreamResolver;

use crate::awake::KeepAwake;
use crate::chat::Chat;
use crate::hype_train::HypeWatch;
use crate::live_watch::LiveWatch;
use crate::playback::Playback;
use crate::provider::Provider;
use crate::raids::Raids;
use crate::search::Searcher;
use crate::session::Session;
use crate::sleep::Sleep;

/// Thumbnail downloads running at once. A page of 30 cards must not open 30 connections.
const THUMBNAIL_CONCURRENCY: usize = 6;

/// Anything that can show a view-model update. The Slint window in the app, a recorder in tests.
/// Cloned into the tasks that deliver thumbnails.
pub trait Sink: Clone + Send + 'static {
    fn show(&self, update: Update);
    /// Delivers a decoded thumbnail for the cards that show `url`.
    fn thumbnail(&self, url: String, image: Thumbnail);
    /// Delivers a decoded emote or badge for the chat lines that show `url`.
    fn chat_image(&self, url: String, image: Thumbnail);
    /// Closes the window, which ends the app (`run_app` shuts the runtime down after it).
    fn quit(&self);
}

impl Sink for slint::Weak<MainWindow> {
    fn show(&self, update: Update) {
        // Fails only once the event loop has quit, when there is nothing left to update.
        if let Err(err) = self.upgrade_in_event_loop(move |ui| update.apply(&ui)) {
            tracing::debug!(%err, "UI gone, dropping update");
        }
    }

    fn thumbnail(&self, url: String, image: Thumbnail) {
        // Queued behind the row update that created the cards, so the cards exist by then.
        if let Err(err) = self.upgrade_in_event_loop(move |ui| apply_thumbnail(&ui, &url, &image)) {
            tracing::debug!(%err, "UI gone, dropping thumbnail");
        }
    }

    fn chat_image(&self, url: String, image: Thumbnail) {
        if let Err(err) = self.upgrade_in_event_loop(move |ui| apply_chat_image(&ui, &url, &image))
        {
            tracing::debug!(%err, "UI gone, dropping chat image");
        }
    }

    fn quit(&self) {
        // Ends `ui.run()` on the UI thread. Already gone means the app is quitting anyway.
        if let Err(err) = slint::invoke_from_event_loop(|| {
            if let Err(err) = slint::quit_event_loop() {
                tracing::debug!(%err, "event loop already stopped");
            }
        }) {
            tracing::debug!(%err, "UI gone, nothing to quit");
        }
    }
}

/// Writes the settings to the config file. Blocking, so the store runs it off the async workers.
pub type SaveSettings = Arc<dyn Fn(htpc_core::Settings) + Send + Sync>;

/// Writes the favorites to `favorites.toml`. Blocking, like [`SaveSettings`].
pub type SaveFavorites = Arc<dyn Fn(&[htpc_core::ChannelRef]) + Send + Sync>;

/// Writes the last multiview session to `multiview.toml`. Blocking, like [`SaveSettings`].
pub type SaveMultiview = Arc<dyn Fn(&htpc_core::SavedSet) + Send + Sync>;

/// Writes *Continue watching* to `continue.toml`. Blocking, like [`SaveSettings`].
pub type SaveContinue = Arc<dyn Fn(&[htpc_core::Progress]) + Send + Sync>;

/// Writes a file off the async workers, newest last: every star and unstar (or multiview change)
/// writes, and two writes in flight must not land in the wrong order.
struct LatestWriter<T> {
    save: Arc<dyn Fn(&T) + Send + Sync>,
    /// The value still to write. A write takes the newest, so a burst writes it once or twice.
    pending: Arc<std::sync::Mutex<Option<T>>>,
    /// One write at a time.
    writing: Arc<std::sync::Mutex<()>>,
}

impl<T> Clone for LatestWriter<T> {
    fn clone(&self) -> Self {
        Self {
            save: Arc::clone(&self.save),
            pending: Arc::clone(&self.pending),
            writing: Arc::clone(&self.writing),
        }
    }
}

impl<T: Send + 'static> LatestWriter<T> {
    fn new(save: impl Fn(&T) + Send + Sync + 'static) -> Self {
        Self {
            save: Arc::new(save),
            pending: Arc::new(std::sync::Mutex::new(None)),
            writing: Arc::default(),
        }
    }

    fn write(&self, value: T) {
        *self.pending.lock().unwrap_or_else(|e| e.into_inner()) = Some(value);
        let this = self.clone();
        tokio::task::spawn_blocking(move || {
            let _one = this.writing.lock().unwrap_or_else(|e| e.into_inner());
            let next = this
                .pending
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .take();
            if let Some(value) = next {
                (this.save)(&value);
            }
        });
    }

    /// Writes what is still pending now, on this thread, and returns once it is on disk (or once
    /// the write that had taken it is). For the end of the app, which cannot wait for a task.
    fn flush(&self) {
        let _one = self.writing.lock().unwrap_or_else(|e| e.into_inner());
        let next = self
            .pending
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .take();
        if let Some(value) = next {
            (self.save)(&value);
        }
    }
}

/// A video's seek timer command: how long to wait, and the event to send then. `None` for any
/// other command.
fn seek_timer(cmd: &Command) -> Option<(u64, htpc_core::Event)> {
    use htpc_core::seek::{HOLD_AFTER_MS, SCRUB_TICK_MS};
    use htpc_core::Event;
    match *cmd {
        Command::SettleSeek { id, ms } => Some((ms, Event::SeekSettleDue(id))),
        Command::HoldCheck { id } => Some((HOLD_AFTER_MS, Event::HoldDue(id))),
        Command::ScrubTick { id } => Some((SCRUB_TICK_MS, Event::ScrubTickDue(id))),
        _ => None,
    }
}

/// True while a stream is on screen and starting or playing: the time to keep the screen on. In
/// multiview, `playback` is the stream with sound's. Also while the channel page covers a stream
/// that still plays (it has sound).
fn watching(state: &AppState) -> bool {
    (matches!(state.screen, Screen::Player | Screen::Multiview) || state.live_covered())
        && matches!(
            state.playback,
            CorePlayback::Loading | CorePlayback::Playing
        )
        && !state.paused
}

pub struct Store<P, R, S> {
    state: AppState,
    /// `None` when there is nowhere to save (no config folder, tests, `--mock`).
    save_settings: Option<SaveSettings>,
    /// The same for the favorites.
    save_favorites: Option<LatestWriter<Vec<htpc_core::ChannelRef>>>,
    /// And for the last multiview session.
    save_multiview: Option<LatestWriter<htpc_core::SavedSet>>,
    /// And for *Continue watching*.
    save_continue: Option<LatestWriter<Vec<htpc_core::Progress>>>,
    /// Told once `Event::Quitting` is reduced and what it saved is on disk. `None` in tests that
    /// do not care.
    quit_saved: Option<std::sync::mpsc::Sender<()>>,
    /// Keeps the screen on during a stream. `None` in tests that do not care.
    keep_awake: Option<KeepAwake>,
    /// What `keep_awake` was last told.
    awake: bool,
    /// Runs the sleep timer and suspends. `None` in tests that do not care.
    sleep: Option<Sleep>,
    /// Watches the channel on screen for raids and counts the raid card down. `None` in tests
    /// that do not care.
    raids: Option<Raids>,
    shown: Option<ViewModel>,
    provider: Arc<P>,
    playback: Arc<Playback<R>>,
    /// Joins and leaves the chat of the channel on screen.
    chat: Chat,
    /// Chat images already requested for the channel on screen. Emotes repeat all the time, and
    /// each is fetched and sent to the window once.
    chat_images: HashSet<String>,
    /// Runs the login and sign-out commands.
    session: Arc<Session>,
    /// Runs searches once typing pauses.
    searcher: Searcher,
    /// Watches for followed channels going live, and times the toasts.
    live_watch: LiveWatch,
    /// Watches the channel on screen for hype trains.
    hype_watch: HypeWatch,
    sink: S,
    tx: UnboundedSender<Msg>,
    downloads: Arc<Semaphore>,
    /// Tells a sleep from a pause or a load in the player's health reports (`Event::DeviceWoke`).
    wake: WakeCheck,
}

impl<P: Provider, R: StreamResolver, S: Sink> Store<P, R, S> {
    /// `tx` must be the sender of the channel later passed to [`Store::run`]. Command results
    /// are sent through it.
    pub fn new(
        state: AppState,
        provider: P,
        playback: Playback<R>,
        chat: Chat,
        session: Arc<Session>,
        sink: S,
        tx: UnboundedSender<Msg>,
    ) -> Self {
        // The saved favorites and multiview set, before the first Home load asks for their row
        // and the resume card.
        provider.set_favorites(state.favorites.clone());
        if let Some(set) = &state.last_multiview {
            provider.set_resume_set(set.channels.clone());
        }
        Self {
            state,
            save_settings: None,
            save_favorites: None,
            save_multiview: None,
            save_continue: None,
            quit_saved: None,
            keep_awake: None,
            awake: false,
            sleep: None,
            raids: None,
            shown: None,
            provider: Arc::new(provider),
            playback: Arc::new(playback),
            chat,
            chat_images: HashSet::new(),
            session,
            searcher: Searcher::default(),
            live_watch: LiveWatch::default(),
            hype_watch: HypeWatch::default(),
            sink,
            tx,
            downloads: Arc::new(Semaphore::new(THUMBNAIL_CONCURRENCY)),
            wake: WakeCheck::default(),
        }
    }

    /// Persists changed settings with `save`.
    pub fn with_settings_saver(mut self, save: SaveSettings) -> Self {
        self.save_settings = Some(save);
        self
    }

    /// Persists changed favorites with `save`.
    pub fn with_favorites_saver(mut self, save: SaveFavorites) -> Self {
        self.save_favorites = Some(LatestWriter::new(
            move |list: &Vec<htpc_core::ChannelRef>| save(list),
        ));
        self
    }

    /// Persists the multiview set with `save` whenever it changes.
    pub fn with_multiview_saver(mut self, save: SaveMultiview) -> Self {
        self.save_multiview = Some(LatestWriter::new(move |set: &htpc_core::SavedSet| {
            save(set)
        }));
        self
    }

    /// Persists *Continue watching* with `save` whenever it changes.
    pub fn with_continue_saver(mut self, save: SaveContinue) -> Self {
        self.save_continue = Some(LatestWriter::new(move |list: &Vec<htpc_core::Progress>| {
            save(list)
        }));
        self
    }

    /// Sends `()` on `saved` when the window is closing (`Event::Quitting`) and the files are
    /// written, so the closing window can wait for them: the runtime stops soon after it.
    pub fn with_quit_signal(mut self, saved: std::sync::mpsc::Sender<()>) -> Self {
        self.quit_saved = Some(saved);
        self
    }

    /// Keeps the screen on with `keep_awake` while a stream plays.
    pub fn with_keep_awake(mut self, keep_awake: KeepAwake) -> Self {
        self.keep_awake = Some(keep_awake);
        self
    }

    /// Runs the sleep timer commands with `sleep`.
    pub fn with_sleep(mut self, sleep: Sleep) -> Self {
        self.sleep = Some(sleep);
        self
    }

    /// Runs the raid commands with `raids`.
    pub fn with_raids(mut self, raids: Raids) -> Self {
        self.raids = Some(raids);
        self
    }

    /// Shows the initial state, runs the startup commands, then handles messages. The store holds
    /// a sender itself, so this runs until the tokio runtime shuts down.
    pub async fn run(mut self, startup: Vec<Command>, mut rx: UnboundedReceiver<Msg>) {
        self.publish();
        self.spawn_all(startup);
        while let Some(msg) = rx.recv().await {
            let quitting = matches!(msg, Msg::Event(htpc_core::Event::Quitting));
            self.handle(msg);
            if quitting {
                self.finish_saving();
            }
        }
        tracing::debug!("store loop finished");
    }

    /// The window is closing: what the last message asked to save is written before the signal,
    /// not left to tasks the runtime may drop.
    fn finish_saving(&self) {
        if let Some(w) = &self.save_favorites {
            w.flush();
        }
        if let Some(w) = &self.save_multiview {
            w.flush();
        }
        if let Some(w) = &self.save_continue {
            w.flush();
        }
        if let Some(saved) = &self.quit_saved {
            let _ = saved.send(());
        }
    }

    fn handle(&mut self, msg: Msg) {
        // A gap in the single player's health reports means the device slept. The reducer hears
        // it first, while its state still holds the last report before the gap (vod-plan.md,
        // phase 7). The gap is measured on the wall clock, which counts through a suspend.
        if let Some(woke) = self.wake.before(&msg, crate::live_watch::wall_ms()) {
            tracing::info!(?woke, "the device slept");
            self.reduce_one(woke.into());
        }
        self.reduce_one(msg);
    }

    fn reduce_one(&mut self, msg: Msg) {
        tracing::trace!(?msg, "reduce");
        let (next, cmds) = reduce(&self.state, msg);
        self.wake.after(&cmds);
        self.state = next;
        self.publish();
        self.update_awake();
        self.spawn_all(cmds);
    }

    /// Asks for the screen to stay on while a stream is starting or playing (not paused, not
    /// failed or ended), and lets it sleep otherwise. Only changes are passed on.
    fn update_awake(&mut self) {
        let want = watching(&self.state);
        if want == self.awake {
            return;
        }
        self.awake = want;
        if let Some(keep_awake) = &self.keep_awake {
            keep_awake(want);
        }
    }

    /// Sends a view-model to the UI if anything visible changed.
    fn publish(&mut self) {
        let next = view_model(&self.state);
        let update = match &self.shown {
            None => Some(Update::full(next.clone())),
            Some(prev) => Update::diff(prev, next.clone()),
        };
        if let Some(update) = update {
            self.shown = Some(next);
            self.sink.show(update);
        }
    }

    /// Downloads `urls` in order with at most [`THUMBNAIL_CONCURRENCY`] in flight (across all
    /// commands). One task takes a download slot per URL before starting it, so within a command
    /// the earlier URLs (the hero, the first row) always start first.
    ///
    /// `deliver` hands each decoded image to the window: card thumbnails and chat images take
    /// the same road and differ only in which models they patch.
    fn spawn_images(&self, urls: Vec<String>, deliver: fn(&S, String, Thumbnail)) {
        let provider = Arc::clone(&self.provider);
        let sink = self.sink.clone();
        let downloads = Arc::clone(&self.downloads);
        tokio::spawn(async move {
            for url in urls {
                let Ok(slot) = Arc::clone(&downloads).acquire_owned().await else {
                    return;
                };
                let provider = Arc::clone(&provider);
                let sink = sink.clone();
                tokio::spawn(async move {
                    if let Some(image) = provider.thumbnail(&url).await {
                        deliver(&sink, url, image);
                    }
                    drop(slot);
                });
            }
        });
    }

    fn spawn_all(&mut self, cmds: Vec<Command>) {
        for cmd in cmds {
            tracing::debug!(?cmd, "command");
            if cmd == Command::Quit {
                tracing::info!("quitting");
                self.sink.quit();
                continue;
            }
            if let Command::SetKick(on) = cmd {
                // At once, not in a task: the `LoadFeed` after it must already see it.
                self.provider.set_kick(on);
                continue;
            }
            if let Command::SaveFavorites(list) = cmd {
                // At once, not in a task: a `LoadFeed` after it must already check the new list.
                self.provider.set_favorites(list.clone());
                if let Some(writer) = &self.save_favorites {
                    writer.write(list);
                }
                continue;
            }
            if let Command::SaveMultiview(set) = cmd {
                // At once, like the favorites: the next Home load checks the new set.
                self.provider.set_resume_set(set.channels.clone());
                if let Some(writer) = &self.save_multiview {
                    writer.write(set);
                }
                continue;
            }
            if let Command::SaveContinue(list) = cmd {
                if let Some(writer) = &self.save_continue {
                    writer.write(list);
                }
                continue;
            }
            if let Command::SaveSettings(settings) = cmd {
                if let Some(save) = self.save_settings.clone() {
                    // File IO: keep it off the async workers.
                    tokio::task::spawn_blocking(move || save(settings));
                }
                continue;
            }
            if let Command::OpenUrl(url) = cmd {
                // Starting the browser can block for a moment (ShellExecute, xdg-open).
                tokio::task::spawn_blocking(move || crate::browser::open_url(&url));
                continue;
            }
            if let Command::ScheduleOfflineRecheck { id } = cmd {
                let tx = self.tx.clone();
                tokio::spawn(async move {
                    tokio::time::sleep(crate::playback::OFFLINE_RECHECK).await;
                    let _ = tx.send(htpc_core::Event::OfflineRecheckDue(id).into());
                });
                continue;
            }
            if let Command::SettleQuality { id } = cmd {
                let tx = self.tx.clone();
                tokio::spawn(async move {
                    tokio::time::sleep(crate::playback::QUALITY_SETTLE).await;
                    let _ = tx.send(htpc_core::Event::QualitySettleDue(id).into());
                });
                continue;
            }
            // A video's seek timers (htpc_core::seek): the waits are core's, the clock is here.
            if let Some((ms, due)) = seek_timer(&cmd) {
                let tx = self.tx.clone();
                tokio::spawn(async move {
                    tokio::time::sleep(std::time::Duration::from_millis(ms)).await;
                    let _ = tx.send(due.into());
                });
                continue;
            }
            if let Command::FetchThumbnails { urls } = cmd {
                self.spawn_images(urls, S::thumbnail);
                continue;
            }
            if let Command::FetchChatImages { urls } = cmd {
                // Only the ones this channel has not asked for yet.
                let fresh: Vec<String> = urls
                    .into_iter()
                    .filter(|u| self.chat_images.insert(u.clone()))
                    .collect();
                if !fresh.is_empty() {
                    self.spawn_images(fresh, S::chat_image);
                }
                continue;
            }
            if Searcher::handles(&cmd) {
                self.searcher
                    .run(Arc::clone(&self.provider), self.tx.clone(), cmd);
                continue;
            }
            if LiveWatch::handles(&cmd) {
                self.live_watch
                    .execute(Arc::clone(&self.provider), self.tx.clone(), cmd);
                continue;
            }
            if HypeWatch::handles(&cmd) {
                self.hype_watch
                    .execute(Arc::clone(&self.provider), self.tx.clone(), cmd);
                continue;
            }
            if Sleep::handles(&cmd) {
                match &self.sleep {
                    Some(sleep) => sleep.execute(cmd),
                    None => tracing::debug!(?cmd, "no sleep timer here"),
                }
                continue;
            }
            if Raids::handles(&cmd) {
                match &self.raids {
                    Some(raids) => raids.execute(cmd),
                    None => tracing::debug!(?cmd, "no raid watch here"),
                }
                continue;
            }
            if Chat::handles(&cmd) {
                // A new channel starts with a clean slate: its lines are new, and the window may
                // have dropped the images of the last one.
                self.chat_images.clear();
                self.chat.execute(cmd);
                continue;
            }
            if Session::handles(&cmd) {
                // The session reports back through the `Notify` it was built with.
                let session = Arc::clone(&self.session);
                tokio::spawn(async move { session.execute(cmd).await });
                continue;
            }
            if Playback::<R>::handles(&cmd) {
                let playback = Arc::clone(&self.playback);
                let tx = self.tx.clone();
                // The ordered part now, in the order of the batch: a stop and the play after it
                // are two tasks, and whichever ran last would otherwise win the slot.
                let prepared = playback.prepare(cmd);
                tokio::spawn(async move {
                    for event in playback.finish(prepared).await {
                        if tx.send(event.into()).is_err() {
                            break;
                        }
                    }
                });
                continue;
            }
            let provider = Arc::clone(&self.provider);
            let tx = self.tx.clone();
            tokio::spawn(async move {
                for event in provider.execute(cmd).await {
                    // The store has stopped (shutdown), so the result is no longer needed.
                    if tx.send(event.into()).is_err() {
                        break;
                    }
                }
            });
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::mock::{fake_api, fake_chat, MOCK_USER, MOCK_USER_CODE};
    use crate::provider::ApiProvider;
    use crate::session::{AppAuth, Apply, Notify, Viewer};
    use htpc_chat::{ChatEvent, FakeChat};
    use htpc_core::{init, ChatMessage, ChatSpan, ChatStatus, Event, Intent, RailEntry};
    use htpc_player::PlayerPool;
    use htpc_resolver::FakeResolver;
    use htpc_twitch_api::{DeviceGrant, FakeApi, FakeAuth, Game};
    use htpc_ui::{ScreenId, Shape};
    use std::sync::Mutex;
    use std::time::Duration;
    use tokio::sync::mpsc::unbounded_channel;

    /// Records what the store would do to the window: the updates shown, the chat images
    /// delivered, and whether it was asked to quit.
    #[derive(Clone, Default)]
    struct Recorder(
        Arc<Mutex<Vec<Update>>>,
        Arc<Mutex<Vec<String>>>,
        Arc<std::sync::atomic::AtomicBool>,
        /// Every keep-awake switch, in order.
        Arc<Mutex<Vec<bool>>>,
    );

    impl Sink for Recorder {
        fn show(&self, update: Update) {
            self.0.lock().unwrap().push(update);
        }

        fn thumbnail(&self, _url: String, _image: Thumbnail) {}

        fn chat_image(&self, url: String, _image: Thumbnail) {
            self.1.lock().unwrap().push(url);
        }

        fn quit(&self) {
            self.2.store(true, std::sync::atomic::Ordering::SeqCst);
        }
    }

    impl Recorder {
        fn count(&self) -> usize {
            self.0.lock().unwrap().len()
        }

        /// Waits until the latest view-model matches, so tests do not depend on task timing.
        async fn wait_for(&self, what: &str, pred: impl Fn(&ViewModel) -> bool) -> ViewModel {
            for _ in 0..400 {
                if let Some(u) = self.0.lock().unwrap().last() {
                    if pred(&u.vm) {
                        return u.vm.clone();
                    }
                }
                tokio::time::sleep(Duration::from_millis(5)).await;
            }
            panic!("timed out waiting for: {what}");
        }
    }

    fn start_with(api: FakeApi) -> (UnboundedSender<Msg>, Recorder) {
        let (tx, rx) = unbounded_channel();
        let rec = Recorder::default();
        let (state, startup) = init();
        // The token's owner is looked up once, as with an environment token: Home has its
        // Followed row from the start.
        let provider = ApiProvider::new(api);
        let playback = Playback::new(
            FakeResolver::playing("https://example.test/live.m3u8"),
            PlayerPool::fake().0,
        );
        let session = fast_session(&tx, Viewer::default());
        let chat = Chat::new(
            Arc::new(fake_chat(false)),
            Arc::new(fake_chat(false)),
            tx.clone(),
        );
        let awake = Arc::clone(&rec.3);
        let store = Store::new(
            state,
            provider,
            playback,
            chat,
            session,
            rec.clone(),
            tx.clone(),
        )
        .with_keep_awake(Arc::new(move |want| awake.lock().unwrap().push(want)));
        tokio::spawn(store.run(startup, rx));
        (tx, rec)
    }

    /// A session whose fake Twitch approves a code on the second poll, 5 ms apart.
    fn fast_session(tx: &UnboundedSender<Msg>, viewer: Viewer) -> Arc<Session> {
        let grant = DeviceGrant {
            device_code: "dev".into(),
            user_code: MOCK_USER_CODE.into(),
            verification_uri: "https://www.twitch.tv/activate?device-code=KQRT7ZMP".into(),
            expires_in: Duration::from_secs(60),
            interval: Duration::from_millis(5),
        };
        let auth = FakeAuth::approving().with_grant(grant);
        let tx = tx.clone();
        let notify: Notify = Arc::new(move |event| {
            let _ = tx.send(event.into());
        });
        let apply: Apply = Arc::new(|_| {});
        Session::new(AppAuth::Fake(auth), "client", None, apply, notify, viewer)
    }

    /// Like `start`, but the provider follows the session's account, as in the real app: Home
    /// is signed out until the login finishes.
    fn start_signed_out() -> (UnboundedSender<Msg>, Recorder) {
        let (tx, rx) = unbounded_channel();
        let rec = Recorder::default();
        let (state, startup) = init();
        let viewer = Viewer::default();
        let provider = ApiProvider::new(fake_api()).with_account(Arc::clone(&viewer));
        let playback = Playback::new(
            FakeResolver::playing("https://example.test/live.m3u8"),
            PlayerPool::fake().0,
        );
        let session = fast_session(&tx, viewer);
        let chat = Chat::new(
            Arc::new(fake_chat(false)),
            Arc::new(fake_chat(false)),
            tx.clone(),
        );
        let store = Store::new(
            state,
            provider,
            playback,
            chat,
            session,
            rec.clone(),
            tx.clone(),
        );
        tokio::spawn(store.run(startup, rx));
        (tx, rec)
    }

    fn start() -> (UnboundedSender<Msg>, Recorder) {
        start_with(fake_api())
    }

    async fn press(tx: &UnboundedSender<Msg>, intent: Intent, times: usize) {
        for _ in 0..times {
            tx.send(intent.into()).unwrap();
        }
    }

    #[tokio::test]
    async fn startup_shows_skeletons_then_loads_mock_rows() {
        let (_tx, rec) = start();
        // The first update is published before any command runs: skeleton cards, no error.
        while rec.count() == 0 {
            tokio::time::sleep(Duration::from_millis(1)).await;
        }
        let first = rec.0.lock().unwrap()[0].clone();
        assert!(
            first.rows_changed && first.chat_changed,
            "first update is full"
        );
        assert!(first
            .vm
            .rows
            .iter()
            .all(|r| r.cards.iter().all(|c| c.loading)));

        let vm = rec.wait_for("rows", |vm| vm.rows.len() == 4).await;
        let titles: Vec<_> = vm.rows.iter().map(|r| r.title.as_str()).collect();
        assert_eq!(titles, ["", "Followed · Live", "Top Live", "Categories"]);
        assert_eq!(vm.rows[0].shape, Shape::Hero);
        assert_eq!(vm.rows[0].cards[0].channel, "grandmaster_k");
        assert_eq!(vm.rows[1].cards[0].viewers, "4.2K");
        assert_eq!(vm.rows[3].shape, Shape::Portrait);
        assert_eq!(vm.screen, ScreenId::Home);
        assert!(vm.rows.iter().all(|r| r.cards.iter().all(|c| !c.loading)));
    }

    #[tokio::test]
    async fn play_receives_chat_and_back_clears_it() {
        let (tx, rec) = start();
        rec.wait_for("rows", |vm| vm.rows.len() == 4).await;
        tx.send(Intent::Select.into()).unwrap();
        let vm = rec.wait_for("chat", |vm| vm.chat.len() == 6).await;
        assert_eq!(vm.screen, ScreenId::Player);
        assert_eq!(vm.now_playing.channel, "grandmaster_k");

        tx.send(Intent::Back.into()).unwrap();
        let vm = rec.wait_for("home", |vm| vm.screen == ScreenId::Home).await;
        assert!(vm.chat.is_empty());
    }

    #[tokio::test]
    async fn the_screen_stays_awake_while_a_stream_is_on_and_not_after() {
        let (tx, rec) = start();
        rec.wait_for("rows", |vm| vm.rows.len() == 4).await;
        assert!(
            rec.3.lock().unwrap().is_empty(),
            "browsing keeps nothing awake"
        );

        tx.send(Intent::Select.into()).unwrap();
        rec.wait_for("player", |vm| vm.screen == ScreenId::Player)
            .await;
        tx.send(Intent::Back.into()).unwrap();
        rec.wait_for("home", |vm| vm.screen == ScreenId::Home).await;
        assert_eq!(*rec.3.lock().unwrap(), [true, false]);
    }

    #[test]
    fn seek_timers_wait_as_long_as_core_says() {
        use htpc_core::seek::{HOLD_AFTER_MS, SCRUB_TICK_MS};
        assert_eq!(
            seek_timer(&Command::SettleSeek { id: 3, ms: 300 }),
            Some((300, Event::SeekSettleDue(3)))
        );
        assert_eq!(
            seek_timer(&Command::HoldCheck { id: 4 }),
            Some((HOLD_AFTER_MS, Event::HoldDue(4)))
        );
        assert_eq!(
            seek_timer(&Command::ScrubTick { id: 5 }),
            Some((SCRUB_TICK_MS, Event::ScrubTickDue(5)))
        );
        assert_eq!(seek_timer(&Command::LeaveChat), None);
    }

    #[test]
    fn pausing_or_a_failed_stream_lets_the_screen_sleep() {
        let (mut s, _) = init();
        s.screen = htpc_core::Screen::Player;
        s.playback = CorePlayback::Playing;
        assert!(watching(&s));
        s.paused = true;
        assert!(!watching(&s), "paused");
        s.paused = false;
        for ended in [
            CorePlayback::Offline,
            CorePlayback::Failed("x".into()),
            CorePlayback::Idle,
        ] {
            s.playback = ended;
            assert!(!watching(&s), "{:?}", s.playback);
        }
        s.playback = CorePlayback::Loading;
        assert!(watching(&s), "starting counts");
    }

    #[tokio::test]
    async fn a_starred_stream_is_on_the_favorites_screen_and_saved() {
        let (tx, rx) = unbounded_channel();
        let rec = Recorder::default();
        let (state, startup) = init();
        let playback = Playback::new(
            FakeResolver::playing("https://example.test/live.m3u8"),
            PlayerPool::fake().0,
        );
        let chat = Chat::new(
            Arc::new(fake_chat(false)),
            Arc::new(fake_chat(false)),
            tx.clone(),
        );
        let saved: Arc<Mutex<Vec<Vec<htpc_core::ChannelRef>>>> = Arc::default();
        let into = Arc::clone(&saved);
        let store = Store::new(
            state,
            ApiProvider::new(fake_api()),
            playback,
            chat,
            fast_session(&tx, Viewer::default()),
            rec.clone(),
            tx.clone(),
        )
        .with_favorites_saver(Arc::new(move |list: &[htpc_core::ChannelRef]| {
            into.lock().unwrap().push(list.to_vec());
        }));
        tokio::spawn(store.run(startup, rx));
        rec.wait_for("rows", |vm| vm.rows.len() == 4).await;
        // The reducer needs a user to star: the token's owner signs in.
        tx.send(
            Event::SignedIn(htpc_core::User {
                login: MOCK_USER.into(),
                can_chat: true,
            })
            .into(),
        )
        .unwrap();
        rec.wait_for("signed in", |vm| vm.user_name == MOCK_USER)
            .await;

        // View on the hero (grandmaster_k): *Add to multiview*, *Add to favorites*.
        tx.send(Intent::View.into()).unwrap();
        let vm = rec.wait_for("popup", |vm| vm.mv.popup_open).await;
        assert_eq!(vm.mv.popup_rows[1].label, "Add to favorites");
        press(&tx, Intent::Down, 1).await;
        tx.send(Intent::Select.into()).unwrap();
        rec.wait_for("closed", |vm| !vm.mv.popup_open).await;

        // The provider already checks the new list when Favorites opens.
        tx.send(Intent::RailPick(RailEntry::Favorites).into())
            .unwrap();
        let vm = rec
            .wait_for("favorites", |vm| {
                vm.heading == "Favorites"
                    && vm
                        .rows
                        .first()
                        .is_some_and(|r| r.cards.first().is_some_and(|c| !c.loading))
            })
            .await;
        assert_eq!(vm.rail_current, 2);
        assert_eq!(vm.rows[0].title, "Live");
        assert_eq!(vm.rows[0].cards[0].channel, "grandmaster_k");
        for _ in 0..400 {
            if !saved.lock().unwrap().is_empty() {
                break;
            }
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
        assert_eq!(
            *saved.lock().unwrap(),
            [vec![htpc_core::ChannelRef::twitch("grandmaster_k")]]
        );
    }

    #[tokio::test]
    async fn the_resume_card_starts_the_saved_set_and_the_change_is_saved() {
        let (tx, rx) = unbounded_channel();
        let rec = Recorder::default();
        let (mut state, startup) = init();
        state.user = Some(htpc_core::User {
            login: MOCK_USER.into(),
            can_chat: true,
        });
        // `sleepy_fox` is offline in the fixtures, the other two live.
        state.last_multiview = Some(htpc_core::SavedSet {
            channels: vec![
                htpc_core::ChannelRef::twitch("tidewatch"),
                htpc_core::ChannelRef::twitch("sleepy_fox"),
                htpc_core::ChannelRef::twitch("lofi_lena"),
            ],
            layout: htpc_core::Layout::Spotlight,
            sound: 2,
        });
        let playback = Playback::new(
            FakeResolver::playing("https://example.test/live.m3u8"),
            PlayerPool::fake().0,
        );
        let chat = Chat::new(
            Arc::new(fake_chat(false)),
            Arc::new(fake_chat(false)),
            tx.clone(),
        );
        let saved: Arc<Mutex<Vec<htpc_core::SavedSet>>> = Arc::default();
        let into = Arc::clone(&saved);
        let store = Store::new(
            state,
            ApiProvider::new(fake_api()),
            playback,
            chat,
            fast_session(&tx, Viewer::default()),
            rec.clone(),
            tx.clone(),
        )
        .with_multiview_saver(Arc::new(move |set: &htpc_core::SavedSet| {
            into.lock().unwrap().push(set.clone());
        }));
        tokio::spawn(store.run(startup, rx));

        // The card opens Followed Live, with the two live ones.
        let vm = rec
            .wait_for("card", |vm| {
                vm.rows
                    .get(1)
                    .is_some_and(|r| r.cards.first().is_some_and(|c| c.resume))
            })
            .await;
        let card = &vm.rows[1].cards[0];
        assert_eq!(card.title, "2 streams");
        assert_eq!(card.mosaic.len(), 2);

        press(&tx, Intent::Down, 1).await;
        tx.send(Intent::Select.into()).unwrap();
        let vm = rec
            .wait_for("multiview", |vm| vm.screen == ScreenId::Multiview)
            .await;
        assert_eq!(vm.toasts.len(), 1);
        assert_eq!(vm.toasts[0].name, "1 offline, skipped");
        assert!(vm.toasts[0].note);

        for _ in 0..400 {
            if !saved.lock().unwrap().is_empty() {
                break;
            }
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
        assert_eq!(
            *saved.lock().unwrap(),
            [htpc_core::SavedSet {
                channels: vec![
                    htpc_core::ChannelRef::twitch("tidewatch"),
                    htpc_core::ChannelRef::twitch("lofi_lena"),
                ],
                layout: htpc_core::Layout::Spotlight,
                sound: 1,
            }]
        );
    }

    #[tokio::test]
    async fn favorites_are_written_newest_last() {
        let written: Arc<Mutex<Vec<usize>>> = Arc::default();
        let into = Arc::clone(&written);
        let writer = LatestWriter::new(move |list: &Vec<htpc_core::ChannelRef>| {
            // A slow disk, so writes pile up.
            std::thread::sleep(Duration::from_millis(2));
            into.lock().unwrap().push(list.len());
        });
        for n in 1..=20 {
            let list = (0..n)
                .map(|i| htpc_core::ChannelRef::twitch(format!("c{i}")))
                .collect();
            writer.write(list);
        }
        for _ in 0..400 {
            if written.lock().unwrap().last() == Some(&20) {
                break;
            }
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
        // Let any write still queued finish: none may land after the newest.
        tokio::time::sleep(Duration::from_millis(60)).await;
        let written = written.lock().unwrap();
        assert_eq!(written.last(), Some(&20), "{written:?}");
        assert!(written.windows(2).all(|w| w[0] < w[1]), "{written:?}");
    }

    #[test]
    fn a_flush_writes_what_is_pending_before_it_returns() {
        let written: Arc<Mutex<Vec<usize>>> = Arc::default();
        let into = Arc::clone(&written);
        let writer = LatestWriter::new(move |list: &Vec<usize>| {
            std::thread::sleep(Duration::from_millis(20));
            into.lock().unwrap().push(list.len());
        });
        // No runtime here: the write's task is never run, only the flush can write.
        *writer.pending.lock().unwrap() = Some(vec![1, 2, 3]);
        writer.flush();
        assert_eq!(*written.lock().unwrap(), [3]);
        writer.flush();
        assert_eq!(
            *written.lock().unwrap(),
            [3],
            "nothing pending, nothing written"
        );
    }

    #[tokio::test]
    async fn closing_the_window_saves_the_place_before_the_signal() {
        use htpc_core::{ChannelRef, Progress, Rgb, Video, VideoItem, VideoKind, VideoSession};
        let video = Video {
            id: "77".into(),
            channel: ChannelRef::twitch("sleepy"),
            channel_name: "Sleepy Fox".into(),
            kind: VideoKind::Archive,
            title: "late stream".into(),
            category: "Hollow Knight".into(),
            length_s: 15_128,
            created_at: 1_794_335_302,
            views: 18_000,
            tint: Rgb::default(),
            thumbnail: None,
            muted: Vec::new(),
            sub_only: false,
            recording: false,
        };
        let (mut state, _) = init();
        let mut session = VideoSession::new(VideoItem::Video(video), 0, 1_794_346_102);
        session.pos_ms = 42 * 60_000;
        state.video = Some(session);
        state.screen = Screen::Player;
        state.playback = CorePlayback::Playing;

        let (tx, rx) = unbounded_channel();
        let saved: Arc<Mutex<Vec<Vec<Progress>>>> = Arc::default();
        let into = Arc::clone(&saved);
        let (quit_tx, quit_rx) = std::sync::mpsc::channel();
        let store = Store::new(
            state,
            ApiProvider::new(fake_api()),
            Playback::new(
                FakeResolver::playing("https://example.test/v.m3u8"),
                PlayerPool::fake().0,
            ),
            Chat::new(
                Arc::new(fake_chat(false)),
                Arc::new(fake_chat(false)),
                tx.clone(),
            ),
            fast_session(&tx, Viewer::default()),
            Recorder::default(),
            tx.clone(),
        )
        .with_continue_saver(Arc::new(move |list: &[Progress]| {
            // A slow disk.
            std::thread::sleep(Duration::from_millis(50));
            into.lock().unwrap().push(list.to_vec());
        }))
        .with_quit_signal(quit_tx);
        tokio::spawn(store.run(Vec::new(), rx));

        tx.send(Event::Quitting.into()).unwrap();
        let waited =
            tokio::task::spawn_blocking(move || quit_rx.recv_timeout(Duration::from_secs(5)))
                .await
                .unwrap();
        assert!(waited.is_ok(), "the store signals once it has saved");
        // On disk by the time of the signal: the window may close and the runtime stop now.
        let saved = saved.lock().unwrap();
        assert_eq!(saved.len(), 1, "{saved:?}");
        assert_eq!(saved[0][0].video.id, "77");
        assert_eq!(saved[0][0].pos_s, 42 * 60);
    }

    #[tokio::test]
    async fn b_on_home_asks_and_a_quits() {
        use std::sync::atomic::Ordering;
        let (tx, rec) = start();
        rec.wait_for("rows", |vm| vm.rows.len() == 4).await;
        // Back opens the sidebar, the second Back asks to quit.
        tx.send(Intent::Back.into()).unwrap();
        rec.wait_for("sidebar", |vm| vm.rail_focused).await;
        tx.send(Intent::Back.into()).unwrap();
        rec.wait_for("prompt", |vm| vm.exit_prompt).await;
        assert!(!rec.2.load(Ordering::SeqCst), "asking must not quit");

        tx.send(Intent::Select.into()).unwrap();
        for _ in 0..400 {
            if rec.2.load(Ordering::SeqCst) {
                return;
            }
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
        panic!("A on the prompt did not quit");
    }

    #[tokio::test]
    async fn sign_in_polls_until_approved_then_shows_the_followed_row() {
        let (tx, rec) = start_signed_out();
        // Signed out: no Followed row (hero, Top Live, Categories).
        let vm = rec.wait_for("rows", |vm| vm.rows.len() == 3).await;
        assert!(vm.user_name.is_empty());
        tx.send(Intent::RailPick(RailEntry::Account).into())
            .unwrap();
        let vm = rec
            .wait_for("code", |vm| vm.device_code == MOCK_USER_CODE)
            .await;
        assert_eq!(vm.screen, ScreenId::SignIn);
        assert_eq!(vm.verification_url, "www.twitch.tv/activate");
        assert!(vm.qr_width > 0, "the QR code is drawn for the link");

        // Nobody presses anything: the fake approves on its second poll.
        let vm = rec
            .wait_for("signed in", |vm| vm.user_name == MOCK_USER)
            .await;
        assert_eq!(vm.screen, ScreenId::Home);
        // Home reloads for the account and gains the Followed row.
        let vm = rec.wait_for("followed", |vm| vm.rows.len() == 4).await;
        assert_eq!(vm.rows[1].title, "Followed \u{b7} Live");
    }

    #[tokio::test]
    async fn following_lists_live_channels_then_dims_the_offline_ones() {
        let (tx, rec) = start_signed_out();
        rec.wait_for("rows", |vm| vm.rows.len() == 3).await;
        // Following while signed out asks to sign in, and continues there once approved.
        tx.send(Intent::RailPick(RailEntry::Following).into())
            .unwrap();
        let vm = rec
            .wait_for("sign-in", |vm| vm.screen == ScreenId::SignIn)
            .await;
        assert!(vm.device_code.is_empty() || vm.device_code == MOCK_USER_CODE);

        let vm = rec
            .wait_for("following", |vm| {
                vm.heading == "Following" && vm.rows.len() == 2 && !vm.rows[0].cards[0].loading
            })
            .await;
        assert_eq!(vm.screen, ScreenId::Home);
        assert_eq!(vm.rail_current, 1);
        let live: Vec<_> = vm.rows[0]
            .cards
            .iter()
            .map(|c| c.channel.as_str())
            .collect();
        assert_eq!(
            live,
            [
                "nightowl_dev",
                "pixelpilgrim",
                "marrow",
                "lofi_lena",
                "quietcoast"
            ]
        );
        assert_eq!(vm.rows[1].title, "Offline");
        let off: Vec<_> = vm.rows[1]
            .cards
            .iter()
            .map(|c| c.channel.as_str())
            .collect();
        assert_eq!(off, ["sleepy_fox", "driftwood", "kettle_kai"]);
        assert!(vm.rows[1].cards.iter().all(|c| c.offline));
    }

    #[tokio::test]
    async fn sign_out_from_settings_returns_to_the_anonymous_home() {
        let (tx, rec) = start_signed_out();
        rec.wait_for("rows", |vm| vm.rows.len() == 3).await;
        tx.send(Intent::RailPick(RailEntry::Account).into())
            .unwrap();
        rec.wait_for("signed in", |vm| vm.user_name == MOCK_USER)
            .await;
        rec.wait_for("followed", |vm| vm.rows.len() == 4).await;

        // The account entry is Settings now, and A there signs out.
        tx.send(Intent::RailPick(RailEntry::Account).into())
            .unwrap();
        let vm = rec
            .wait_for("settings", |vm| vm.screen == ScreenId::Settings)
            .await;
        assert_eq!(vm.select_label, "Sign out");
        tx.send(Intent::Select.into()).unwrap();
        let vm = rec
            .wait_for("signed out", |vm| vm.user_name.is_empty())
            .await;
        assert_eq!(vm.select_label, "Sign in");
        tx.send(Intent::RailPick(RailEntry::Home).into()).unwrap();
        let vm = rec
            .wait_for("anonymous home", |vm| {
                vm.screen == ScreenId::Home && vm.rows.len() == 3
            })
            .await;
        assert!(vm.rows.iter().all(|r| r.title != "Followed \u{b7} Live"));
    }

    #[tokio::test]
    async fn unchanged_state_sends_no_update() {
        let (tx, rec) = start();
        rec.wait_for("rows", |vm| vm.rows.len() == 4).await;
        let before = rec.count();
        // Up on the first row and X (retry, with nothing failed) on Home change nothing. (B would
        // open the quit prompt, Y the hero's channel page.)
        tx.send(Intent::Up.into()).unwrap();
        tx.send(Intent::Retry.into()).unwrap();
        // A visible change afterwards proves both no-ops were processed in order.
        tx.send(Intent::Down.into()).unwrap();
        rec.wait_for("moved", |vm| vm.sel_row == 1).await;
        assert_eq!(rec.count(), before + 1);
    }

    /// 70 games, so the directory has a second and a third page (30 per page).
    fn many_games() -> FakeApi {
        let mut api = fake_api();
        api.games = (0..70)
            .map(|i| Game {
                id: format!("g{i}"),
                name: format!("Game {i}"),
                box_art_url: String::new(),
            })
            .collect();
        api
    }

    #[tokio::test]
    async fn browse_loads_on_first_open_and_pages_on_scroll() {
        let (tx, rec) = start_with(many_games());
        rec.wait_for("home", |vm| vm.rows.len() == 4).await;
        tx.send(Intent::RailPick(RailEntry::Browse).into()).unwrap();

        // Skeleton grid first, then 30 categories in rows of 5.
        let vm = rec
            .wait_for("browse", |vm| vm.screen == ScreenId::Grid)
            .await;
        assert_eq!(vm.heading, "Categories");
        let vm = rec.wait_for("grid", |vm| vm.rows.len() == 6).await;
        assert!(vm
            .rows
            .iter()
            .all(|r| r.shape == Shape::Portrait && r.cards.len() == 5));
        assert_eq!(vm.rail_current, 3);

        // Scrolling to the last rows loads the next page without blocking input.
        press(&tx, Intent::Down, 5).await;
        let vm = rec.wait_for("page 2", |vm| vm.rows.len() >= 12).await;
        assert_eq!(vm.rows[6].cards[0].channel, "Game 30");
        assert!(!vm.can_retry);
    }

    #[tokio::test]
    async fn category_opens_with_its_streams_and_back_returns() {
        let (tx, rec) = start_with(fake_api());
        rec.wait_for("home", |vm| vm.rows.len() == 4).await;
        // Down to the Categories row, then A on "Just Chatting".
        press(&tx, Intent::Down, 3).await;
        tx.send(Intent::Select.into()).unwrap();

        let vm = rec
            .wait_for("category", |vm| vm.screen == ScreenId::Grid)
            .await;
        assert_eq!(vm.heading, "Just Chatting");
        assert!(vm.can_back);
        // The mock's only Just Chatting stream is `sable`. Skeletons give way to its card.
        let vm = rec
            .wait_for("streams", |vm| {
                vm.rows.iter().any(|r| r.cards.iter().any(|c| !c.loading))
            })
            .await;
        assert_eq!(vm.rows.len(), 1);
        assert_eq!(vm.rows[0].cards[0].channel, "sable");
        assert_eq!(vm.rows[0].shape, Shape::Landscape);
        assert!(vm.feed_error.is_empty() && !vm.feed_empty);

        tx.send(Intent::Back.into()).unwrap();
        let vm = rec.wait_for("home", |vm| vm.screen == ScreenId::Home).await;
        assert_eq!(vm.sel_row, 3, "focus is kept on the category card");
    }

    /// Hands out a 1x1 image for every URL and remembers which ones were asked for.
    struct CountingProvider {
        inner: ApiProvider<FakeApi>,
        asked: Arc<Mutex<Vec<String>>>,
    }

    impl Provider for CountingProvider {
        async fn execute(&self, cmd: Command) -> Vec<Event> {
            self.inner.execute(cmd).await
        }

        async fn thumbnail(&self, url: &str) -> Option<Thumbnail> {
            self.asked.lock().unwrap().push(url.to_owned());
            Some(Thumbnail {
                width: 1,
                height: 1,
                rgba: vec![255; 4],
            })
        }
    }

    #[tokio::test]
    async fn chat_images_are_fetched_once_per_channel() {
        let line = |id: &str| ChatMessage {
            id: id.into(),
            badges: vec!["https://cdn/mod.png".into()],
            spans: vec![ChatSpan::Emote {
                name: "Kappa".into(),
                url: "https://cdn/kappa.png".into(),
            }],
            ..ChatMessage::plain("a", "")
        };
        // The same badge and emote in two batches.
        let script = vec![
            ChatEvent::Status(ChatStatus::Live),
            ChatEvent::Messages(vec![line("1")]),
            ChatEvent::Messages(vec![line("2")]),
        ];
        let (tx, rx) = unbounded_channel();
        let rec = Recorder::default();
        let asked = Arc::new(Mutex::new(Vec::new()));
        let provider = CountingProvider {
            inner: ApiProvider::new(fake_api()),
            asked: Arc::clone(&asked),
        };
        let playback = Playback::new(
            FakeResolver::playing("https://example.test/live.m3u8"),
            PlayerPool::fake().0,
        );
        let chat = Chat::new(
            Arc::new(FakeChat::new(script)),
            Arc::new(FakeChat::new(Vec::new())),
            tx.clone(),
        );
        let session = fast_session(&tx, Viewer::default());
        let (state, startup) = init();
        let store = Store::new(
            state,
            provider,
            playback,
            chat,
            session,
            rec.clone(),
            tx.clone(),
        );
        tokio::spawn(store.run(startup, rx));

        let chat_urls = |rec: &Recorder| rec.1.lock().unwrap().len();
        let wait_for_images = |rec: Recorder, n: usize| async move {
            for _ in 0..400 {
                if chat_urls(&rec) >= n {
                    return;
                }
                tokio::time::sleep(Duration::from_millis(5)).await;
            }
            panic!("timed out waiting for {n} chat images");
        };

        rec.wait_for("home", |vm| vm.rows.len() == 4).await;
        tx.send(Intent::Select.into()).unwrap();
        rec.wait_for("two lines", |vm| vm.chat.len() == 2).await;
        wait_for_images(rec.clone(), 2).await;
        // Give a wrongly repeated fetch time to show up.
        tokio::time::sleep(Duration::from_millis(60)).await;
        assert_eq!(chat_urls(&rec), 2, "badge and emote, once each");
        let delivered = rec.1.lock().unwrap().clone();
        assert!(delivered.contains(&"https://cdn/mod.png".to_owned()));
        assert!(delivered.contains(&"https://cdn/kappa.png".to_owned()));

        // Watching again starts over: the window may have dropped the images meanwhile.
        tx.send(Intent::Back.into()).unwrap();
        rec.wait_for("home", |vm| vm.screen == ScreenId::Home).await;
        tx.send(Intent::Select.into()).unwrap();
        wait_for_images(rec.clone(), 4).await;
    }
}
