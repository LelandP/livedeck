//! Captions in the app (captions-plan.md, phase 3): the speech engine and the thread that feeds it.
//!
//! `Command::SetCaptions(true)` makes a [`Transcriber`] (off the async workers: whisper blocks
//! while its models load), starts a thread that owns it, and gives the single player's audio tap
//! (`Playback::set_captions_feed`) a feed that sends into the thread. The thread pushes every chunk
//! of audio into the transcriber, resets it when a stream starts or stops, polls it and sends
//! each finished caption to the store as `Event::Caption`. `SetCaptions(false)` takes the feed
//! away, drops the thread's channel and with it the transcriber.
//!
//! A video (phase 5) is read ahead of its playhead, so its audio comes in bursts: the engine gets
//! a longer backlog for it (`VIDEO_BACKLOG_S`), and is told where the video is (the reducer's
//! clock, [`Captions::set_playhead`]) so it skips audio the picture has passed.
//!
//! Which engine is made is decided at startup: whisper in a build with the `captions` feature, a
//! scripted one in `--mock`, none otherwise. With none, the player menu says "Captions
//! unavailable" and nothing runs. Whisper needs its model files (`models.rs`): turning captions on
//! (or changing the quality) first makes sure they are in the data folder, downloading them while
//! `Event::CaptionsModel` reports the progress, and only then loads the engine. A newer request
//! (captions off, another quality) cancels a download in progress.

use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::mpsc::{Receiver, RecvTimeoutError};
use std::sync::Arc;
use std::time::Duration;

use htpc_captions::{Caption, Transcriber, DEFAULT_BACKLOG_S};
use htpc_core::{CaptionWord, CaptionsQuality, Event, ModelState, Msg};
use htpc_resolver::StreamResolver;
use tokio::sync::mpsc::UnboundedSender;

use crate::captions_tap::{CaptionAudio, CaptionFeed};
use crate::models::{Models, Outcome};
use crate::playback::Playback;

/// How often the thread looks for finished captions while no audio arrives.
const POLL: Duration = Duration::from_millis(100);

/// Seconds of a video's audio the engine may have waiting. The tap reads 30 s beyond the playhead
/// and a burst can overshoot it by ~10 s (measured), so the first one, and one after a pause or a
/// seek, comes in at 30 to 40 s at once.
const VIDEO_BACKLOG_S: f64 = 45.0;

/// Audio that ends this long before the video's playhead is still made: a caption is shown up to
/// `CLEAR_AFTER_MS` (3 s) after its words, and the model needs ~2 s, so only a little is worth it.
const PLAYHEAD_SLACK_MS: u64 = 1_000;

/// Makes the speech engine for a quality. Blocks (whisper loads its models), so it runs off the
/// async workers.
pub type MakeTranscriber =
    Arc<dyn Fn(CaptionsQuality) -> Result<Box<dyn Transcriber>, String> + Send + Sync>;

/// What the app has to make captions with.
pub struct Engine {
    pub make: MakeTranscriber,
    /// Where the models come from. `None`: the engine needs no files (`--mock`).
    pub models: Option<Arc<Models>>,
    /// The engine runs on a GPU, so *Best* keeps up.
    pub gpu: bool,
}

/// The whisper engine, with its models in `models_dir`, when this build has it.
#[cfg(feature = "captions")]
pub fn whisper_engine(models_dir: std::path::PathBuf) -> Option<Engine> {
    use htpc_captions::{WhisperConfig, WhisperTranscriber};

    let models = Arc::new(Models::new(models_dir).with_env_override());
    let gpu = cfg!(feature = "vulkan");
    let files = Arc::clone(&models);
    Some(Engine {
        make: Arc::new(move |quality| {
            let (model, vad) = files.paths(quality);
            let mut config = WhisperConfig::new(model, vad);
            config.use_gpu = gpu;
            let t = WhisperTranscriber::new(config)?;
            Ok(Box::new(t) as Box<dyn Transcriber>)
        }),
        models: Some(models),
        gpu,
    })
}

/// No whisper in this build.
#[cfg(not(feature = "captions"))]
pub fn whisper_engine(_models_dir: std::path::PathBuf) -> Option<Engine> {
    tracing::info!(
        "captions are unavailable: this build has no speech engine (feature `captions`)"
    );
    None
}

/// `--mock`: a line every few seconds of audio, whatever the audio says.
pub fn mock_engine() -> Engine {
    Engine {
        make: mock_factory(),
        models: None,
        gpu: false,
    }
}

fn mock_factory() -> MakeTranscriber {
    Arc::new(|_| {
        Ok(Box::new(htpc_captions::FakeTranscriber::new(MOCK_LINES, 3.0)) as Box<dyn Transcriber>)
    })
}

/// What the mock engine "hears", in turn (the last repeats).
const MOCK_LINES: [&str; 5] = [
    "Welcome back, everyone. Today we try out captions.",
    "These words are made up: the --mock engine writes them.",
    "A real engine listens to the stream and writes what it hears, a few seconds behind.",
    "Two lines at most, so the picture stays clear.",
    "Turn them off in the player menu whenever you like.",
];

/// The speech engine's switch.
pub struct Captions {
    /// `None`: no engine in this build or no models.
    make: Option<MakeTranscriber>,
    /// Finds and downloads the model files. `None`: the engine needs none.
    models: Option<Arc<Models>>,
    /// The quality the next engine is made for (`Command::SetCaptionsQuality`).
    quality: std::sync::Mutex<CaptionsQuality>,
    /// The number of the latest request. A request that finds a newer one gave up.
    wanted: AtomicU64,
    /// The running engine's thread, if any: dropping it closes its channel and ends the thread.
    running: tokio::sync::Mutex<Option<Running>>,
    /// The end of the newest audio the engine got, in ms on the stream clock (0: none since the
    /// last start). For the log of how far the picture trails it (`Captions::newest_audio_ms`).
    newest_audio: Arc<AtomicU64>,
    /// Where the video the engine hears is, in ms on the stream clock, for the engine to skip
    /// audio the picture has passed (0: not known, or a live stream). Set by
    /// [`Captions::set_playhead`] from the reducer's clock.
    playhead: Arc<AtomicU64>,
}

struct Running {
    /// The quality it was made for: another one needs another engine.
    quality: CaptionsQuality,
    /// What the tap's feed sends into.
    _audio: std::sync::mpsc::Sender<CaptionAudio>,
}

impl Captions {
    pub fn new(make: Option<MakeTranscriber>) -> Self {
        Self {
            make,
            models: None,
            quality: std::sync::Mutex::new(htpc_core::DEFAULT_CAPTIONS_QUALITY),
            wanted: AtomicU64::new(0),
            running: tokio::sync::Mutex::new(None),
            newest_audio: Arc::new(AtomicU64::new(0)),
            playhead: Arc::new(AtomicU64::new(0)),
        }
    }

    /// Where the video on screen is on the stream clock (`None`: not known yet, or a live
    /// stream): the engine skips audio before it, as the captions made from it would come after
    /// the picture. The store reports it after every message.
    pub fn set_playhead(&self, ms: Option<u64>) {
        self.playhead.store(ms.unwrap_or(0), Ordering::Relaxed);
    }

    /// The end of the newest audio fed to the engine, on the stream clock in milliseconds. `None`
    /// while captions are off or before the stream's first audio.
    pub fn newest_audio_ms(&self) -> Option<u64> {
        Some(self.newest_audio.load(Ordering::Relaxed)).filter(|ms| *ms > 0)
    }

    /// From an [`Engine`](crate::captions::Engine), for `quality`.
    pub fn with_engine(engine: Option<Engine>, quality: CaptionsQuality) -> Self {
        let mut captions = Self::new(engine.as_ref().map(|e| Arc::clone(&e.make)));
        captions.models = engine.and_then(|e| e.models);
        captions.set_quality(quality);
        captions
    }

    /// The quality of the next engine. Takes effect with the `apply` that follows.
    pub fn set_quality(&self, quality: CaptionsQuality) {
        *self.quality.lock().unwrap() = quality;
    }

    /// Captions can run: there is an engine to make.
    pub fn available(&self) -> bool {
        self.make.is_some()
    }

    /// Notes that captions should now be on or off. Run in the order of the commands: the number
    /// goes to [`Captions::apply`], which does nothing if a newer request came meanwhile.
    pub fn request(&self) -> u64 {
        self.wanted.fetch_add(1, Ordering::SeqCst) + 1
    }

    /// Turns captions on or off for request `ticket`, with the single player of `playback`.
    pub async fn apply<R: StreamResolver>(
        &self,
        ticket: u64,
        on: bool,
        playback: &Playback<R>,
        events: &UnboundedSender<Msg>,
    ) {
        let mut running = self.running.lock().await;
        if ticket != self.wanted.load(Ordering::SeqCst) {
            return;
        }
        if !on {
            playback.set_captions_feed(None).await;
            *running = None;
            self.newest_audio.store(0, Ordering::Relaxed);
            return;
        }
        let quality = *self.quality.lock().unwrap();
        if running.as_ref().is_some_and(|r| r.quality == quality) {
            return;
        }
        let Some(make) = self.make.clone() else {
            tracing::debug!("captions asked for, but there is no engine");
            return;
        };
        // Another quality: forget the old engine. The tap's feed still reaches it, so it keeps
        // captioning while the new model downloads and loads. Its thread ends on its next poll
        // after the new feed replaces that one (or captions go off).
        *running = None;
        // Not held across the download: turning captions off must not wait for it. The request
        // number is already bumped (`request`), so the download's `stale` check sees it.
        drop(running);
        if let Some(models) = &self.models {
            let say = |state| {
                let _ = events.send(Event::CaptionsModel(state).into());
            };
            let progress = |percent| say(ModelState::Downloading(percent));
            let stale = || ticket != self.wanted.load(Ordering::SeqCst);
            match models.ensure(quality, &progress, &stale).await {
                Ok(Outcome::Ready) => say(ModelState::Ready),
                // Turned off, or another quality asked for: that request takes over.
                Ok(Outcome::Cancelled) => return,
                Err(err) => {
                    tracing::warn!(%err, "could not get the captions model");
                    if !stale() {
                        say(ModelState::Failed);
                    }
                    return;
                }
            }
        }
        let transcriber = match tokio::task::spawn_blocking(move || make(quality)).await {
            Ok(Ok(t)) => t,
            Ok(Err(err)) => {
                tracing::warn!(%err, "could not make the captions engine");
                if ticket == self.wanted.load(Ordering::SeqCst) {
                    let _ = events.send(Event::CaptionsModel(ModelState::Failed).into());
                }
                return;
            }
            Err(err) => {
                tracing::warn!(%err, "the captions engine's start-up failed");
                return;
            }
        };
        // Turned off (or on again) while the models loaded: this engine is not wanted. Checked
        // under the lock again, so the install below is in order with an off request.
        let mut running = self.running.lock().await;
        if ticket != self.wanted.load(Ordering::SeqCst) {
            return;
        }
        let (audio, input) = std::sync::mpsc::channel();
        let events = events.clone();
        let newest = Arc::clone(&self.newest_audio);
        let playhead = Arc::clone(&self.playhead);
        let spawned = std::thread::Builder::new()
            .name("htpc-captions".into())
            .spawn(move || run(input, transcriber, &events, &newest, &playhead));
        if let Err(err) = spawned {
            tracing::warn!(%err, "could not start the captions thread");
            return;
        }
        let to_engine = audio.clone();
        let feed: CaptionFeed = Arc::new(move |a| {
            // The engine is gone (captions were turned off): the feed is about to be replaced.
            let _ = to_engine.send(a);
        });
        *running = Some(Running {
            quality,
            _audio: audio,
        });
        playback.set_captions_feed(Some(feed)).await;
    }
}

/// The engine's thread: audio in, captions out, until the feed's senders are all gone. `newest`
/// keeps the end of the newest audio, in ms. `playhead` is where the video is (0: not known),
/// which the engine is told of whenever it changes.
fn run(
    input: Receiver<CaptionAudio>,
    mut t: Box<dyn Transcriber>,
    events: &UnboundedSender<Msg>,
    newest: &AtomicU64,
    playhead: &AtomicU64,
) {
    // The limit the engine has been given, in ms.
    let mut floor_ms = 0;
    loop {
        match input.recv_timeout(POLL) {
            // Another stream or clock: what was heard before is no use, made or not.
            Ok(CaptionAudio::Start { video }) => {
                t.reset();
                let _ = t.poll();
                // A video's tap reads up to 30 s ahead in bursts, and none of it is late: keep it
                // all. A live stream's audio further back than the picture's lead is late anyway.
                t.set_backlog_s(if video {
                    VIDEO_BACKLOG_S
                } else {
                    DEFAULT_BACKLOG_S
                });
                newest.store(0, Ordering::Relaxed);
                // Another place: the old playhead means nothing here.
                t.set_playhead_s(f64::NEG_INFINITY);
                floor_ms = 0;
            }
            Ok(CaptionAudio::Chunk(chunk)) => {
                newest.store(ms(chunk.end_s()), Ordering::Relaxed);
                t.push(chunk.start_s, &chunk.samples);
            }
            // Captions already made stay (the clock is the same until the next start).
            Ok(CaptionAudio::Stop) => t.reset(),
            Err(RecvTimeoutError::Timeout) => {}
            Err(RecvTimeoutError::Disconnected) => return,
        }
        // A video that is ahead of what the engine has made leaves that audio: captions made
        // after the picture has passed it come too late to show. A little before the playhead
        // is still worth it (a line is shown up to 3 s after its words).
        let now = playhead
            .load(Ordering::Relaxed)
            .saturating_sub(PLAYHEAD_SLACK_MS);
        if now != floor_ms {
            floor_ms = now;
            t.set_playhead_s(if now == 0 {
                f64::NEG_INFINITY
            } else {
                now as f64 / 1000.0
            });
        }
        for caption in t.poll() {
            if events.send(event_of(&caption).into()).is_err() {
                return;
            }
        }
    }
}

/// Seconds on the stream clock as whole milliseconds.
fn ms(s: f64) -> u64 {
    (s * 1000.0).round().max(0.0) as u64
}

/// A caption as the reducer takes it: milliseconds on the stream clock.
fn event_of(c: &Caption) -> Event {
    Event::Caption {
        start_ms: ms(c.start_s),
        end_ms: ms(c.end_s),
        text: c.text.clone(),
        words: c
            .words
            .iter()
            .map(|w| CaptionWord::new(ms(w.start_s), w.text.clone()))
            .collect(),
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Mutex;

    use htpc_captions::FakeTranscriber;
    use htpc_core::{ChannelRef, Command, Latency, Quality, StreamKind, MAIN_SLOT};
    use htpc_player::{FakeTaps, PcmChunk, PlayerPool, TapEvent};
    use htpc_resolver::FakeResolver;
    use tokio::sync::mpsc::{unbounded_channel, UnboundedReceiver};

    use super::*;

    /// Everything done to a transcriber, and whether it was dropped.
    #[derive(Default)]
    struct Log {
        ops: Vec<String>,
        dropped: bool,
    }

    /// A transcriber that notes what it is asked and says "heard N" for every push.
    struct Spy(Arc<Mutex<Log>>, Vec<Caption>);

    impl Transcriber for Spy {
        fn push(&mut self, start_s: f64, samples: &[f32]) {
            self.0
                .lock()
                .unwrap()
                .ops
                .push(format!("push {start_s:.2} {}", samples.len()));
            self.1.push(Caption {
                start_s,
                end_s: start_s + 0.5,
                text: format!("heard {start_s:.1}"),
                words: Vec::new(),
            });
        }

        fn poll(&mut self) -> Vec<Caption> {
            self.0.lock().unwrap().ops.push("poll".into());
            std::mem::take(&mut self.1)
        }

        fn reset(&mut self) {
            self.0.lock().unwrap().ops.push("reset".into());
        }

        fn set_backlog_s(&mut self, backlog_s: f64) {
            self.0
                .lock()
                .unwrap()
                .ops
                .push(format!("backlog {backlog_s}"));
        }

        fn set_playhead_s(&mut self, playhead_s: f64) {
            self.0
                .lock()
                .unwrap()
                .ops
                .push(format!("playhead {playhead_s}"));
        }
    }

    impl Drop for Spy {
        fn drop(&mut self) {
            self.0.lock().unwrap().dropped = true;
        }
    }

    struct Setup {
        captions: Captions,
        playback: Playback<FakeResolver>,
        taps: FakeTaps,
        tx: UnboundedSender<Msg>,
        rx: UnboundedReceiver<Msg>,
    }

    impl Setup {
        fn new(make: Option<MakeTranscriber>) -> Self {
            let taps = FakeTaps::new();
            let playback = Playback::new(FakeResolver::playing("u"), PlayerPool::fake().0)
                .with_taps(Arc::new(taps.clone()));
            let (tx, rx) = unbounded_channel();
            Self {
                captions: Captions::new(make),
                playback,
                taps,
                tx,
                rx,
            }
        }

        async fn set(&self, on: bool) {
            let ticket = self.captions.request();
            self.captions
                .apply(ticket, on, &self.playback, &self.tx)
                .await;
        }

        async fn play(&self) {
            self.playback
                .execute(Command::Play {
                    slot: MAIN_SLOT,
                    channel: ChannelRef::twitch("chan"),
                    stream: StreamKind::Video(Quality::Best),
                    latency: Latency::Low,
                })
                .await;
        }

        async fn play_video(&self, start_ms: u64) {
            self.playback
                .execute(Command::PlayVideo {
                    slot: MAIN_SLOT,
                    target: htpc_core::VideoTarget::Video("42".into()),
                    start_ms,
                    stream: StreamKind::Video(Quality::Best),
                })
                .await;
        }

        /// The next caption event, within a couple of seconds.
        async fn caption_words(&mut self) -> ((u64, u64, String), Vec<CaptionWord>) {
            let msg = tokio::time::timeout(Duration::from_secs(5), self.rx.recv())
                .await
                .expect("a caption in time")
                .expect("the channel is open");
            match msg {
                Msg::Event(Event::Caption {
                    start_ms,
                    end_ms,
                    text,
                    words,
                }) => ((start_ms, end_ms, text), words),
                other => panic!("not a caption: {other:?}"),
            }
        }
    }

    /// Half a second of audio from `start_s`.
    fn audio(start_s: f64) -> TapEvent {
        TapEvent::Audio(PcmChunk {
            start_s,
            samples: vec![0.0; 8_000],
        })
    }

    fn fake(lines: &'static [&'static str], every_s: f64) -> Option<MakeTranscriber> {
        Some(Arc::new(move |_| {
            Ok(
                Box::new(FakeTranscriber::new(lines.iter().copied(), every_s))
                    as Box<dyn Transcriber>,
            )
        }))
    }

    fn spy(log: &Arc<Mutex<Log>>) -> Option<MakeTranscriber> {
        let log = Arc::clone(log);
        Some(Arc::new(move |_| {
            Ok(Box::new(Spy(Arc::clone(&log), Vec::new())) as Box<dyn Transcriber>)
        }))
    }

    async fn until(what: &str, mut ok: impl FnMut() -> bool) {
        for _ in 0..400 {
            if ok() {
                return;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
        panic!("timed out waiting for: {what}");
    }

    #[tokio::test]
    async fn the_streams_audio_comes_back_as_captions_on_the_stream_clock() {
        let mut s = Setup::new(fake(&["hello there"], 1.0));
        s.set(true).await;
        s.play().await;
        assert_eq!(
            s.taps.running().len(),
            1,
            "the tap runs once captions are on"
        );
        s.taps.emit(0, audio(100.0));
        s.taps.emit(0, audio(100.5));
        let (caption, words) = s.caption_words().await;
        assert_eq!(caption, (100_000, 101_000, "hello there".to_owned()));
        // Word by word, on the same clock (the fake spreads them over the line).
        assert_eq!(
            words,
            [
                CaptionWord::new(100_000, "hello"),
                CaptionWord::new(100_500, "there")
            ]
        );
        // The newest audio, for the log of the picture's lead over it.
        assert_eq!(s.captions.newest_audio_ms(), Some(101_000));
        s.set(false).await;
        assert_eq!(s.captions.newest_audio_ms(), None);
    }

    #[tokio::test]
    async fn turning_them_off_stops_the_tap_and_drops_the_engine() {
        let log = Arc::new(Mutex::new(Log::default()));
        let s = Setup::new(spy(&log));
        s.set(true).await;
        s.play().await;
        s.taps.emit(0, audio(1.0));
        until("the audio reaches the engine", || {
            log.lock()
                .unwrap()
                .ops
                .iter()
                .any(|o| o.starts_with("push"))
        })
        .await;
        assert!(!log.lock().unwrap().dropped);

        s.set(false).await;
        assert!(s.taps.running().is_empty());
        until("the engine is dropped", || log.lock().unwrap().dropped).await;
        // On again: a new engine, and the tap starts on the stream that is playing.
        s.set(true).await;
        assert_eq!(s.taps.running().len(), 1);
    }

    #[tokio::test]
    async fn a_start_and_a_stop_reset_the_engine_and_chunks_are_pushed() {
        let log = Arc::new(Mutex::new(Log::default()));
        let s = Setup::new(spy(&log));
        s.set(true).await;
        s.play().await;
        s.taps.emit(0, audio(100.0));
        s.taps.emit(0, audio(100.5));
        s.playback
            .execute(Command::StopPlayback { slot: MAIN_SLOT })
            .await;
        until("the stop", || {
            log.lock()
                .unwrap()
                .ops
                .iter()
                .filter(|o| *o == "reset")
                .count()
                == 2
        })
        .await;
        let ops: Vec<String> = log
            .lock()
            .unwrap()
            .ops
            .iter()
            .filter(|o| *o != "poll")
            .cloned()
            .collect();
        assert_eq!(
            ops,
            [
                "reset",
                "backlog 15",
                "playhead -inf",
                "push 100.00 8000",
                "push 100.50 8000",
                "reset"
            ]
        );
    }

    #[tokio::test]
    async fn a_videos_audio_gets_a_long_backlog_and_a_seek_resets_the_engine() {
        let log = Arc::new(Mutex::new(Log::default()));
        let s = Setup::new(spy(&log));
        s.set(true).await;
        s.play_video(60_000).await;
        assert_eq!(s.taps.running().len(), 1, "the video is tapped");
        s.taps.emit(0, audio(122.0));
        s.playback
            .execute(Command::Seek {
                slot: MAIN_SLOT,
                to_ms: 600_000,
            })
            .await;
        s.taps.emit(1, audio(662.0));
        until("the second start", || {
            log.lock()
                .unwrap()
                .ops
                .iter()
                .filter(|o| o.starts_with("push 662"))
                .count()
                == 1
        })
        .await;
        let ops: Vec<String> = log
            .lock()
            .unwrap()
            .ops
            .iter()
            .filter(|o| *o != "poll")
            .cloned()
            .collect();
        // Each place: the engine starts afresh (the seek's stop and start), with 45 s of room
        // for the burst the tap reads ahead.
        assert_eq!(
            ops,
            [
                "reset",
                "backlog 45",
                "playhead -inf",
                "push 122.00 8000",
                "reset",
                "reset",
                "backlog 45",
                "playhead -inf",
                "push 662.00 8000"
            ]
        );
    }

    #[tokio::test]
    async fn the_engine_is_told_where_the_video_is_and_skips_what_it_has_passed() {
        let log = Arc::new(Mutex::new(Log::default()));
        let s = Setup::new(spy(&log));
        s.set(true).await;
        s.play_video(60_000).await;
        s.taps.emit(0, audio(122.0));
        let ops = || -> Vec<String> {
            log.lock()
                .unwrap()
                .ops
                .iter()
                .filter(|o| o.starts_with("playhead"))
                .cloned()
                .collect()
        };
        // The reducer's clock, a second of slack less: only the changes are passed on.
        s.captions.set_playhead(Some(130_000));
        until("the playhead reaches the engine", || ops().len() == 2).await;
        s.captions.set_playhead(Some(130_000));
        s.captions.set_playhead(Some(131_000));
        until("it moves", || ops().len() == 3).await;
        // After a seek there is no clock: the limit goes.
        s.captions.set_playhead(None);
        until("it is taken away", || ops().len() == 4).await;
        assert_eq!(
            ops(),
            [
                "playhead -inf",
                "playhead 129",
                "playhead 130",
                "playhead -inf"
            ]
        );
    }

    // ---- the models ----

    use crate::models::testing;

    /// Every `CaptionsModel` state sent so far.
    fn model_states(s: &mut Setup) -> Vec<ModelState> {
        let mut out = Vec::new();
        while let Ok(Msg::Event(Event::CaptionsModel(state))) = s.rx.try_recv() {
            out.push(state);
        }
        out
    }

    /// A setup whose engine needs the test models, downloaded from `server` into `dir`.
    fn with_models(
        dir: &std::path::Path,
        server: &wiremock::MockServer,
        make: Option<MakeTranscriber>,
    ) -> Setup {
        let mut s = Setup::new(make);
        s.captions.models = Some(Arc::new(testing::models(dir, server)));
        s
    }

    #[tokio::test]
    async fn the_first_use_downloads_the_model_then_makes_the_engine() {
        let server = testing::server(Duration::ZERO).await;
        let dir = tempfile::tempdir().unwrap();
        let log = Arc::new(Mutex::new(Log::default()));
        let mut s = with_models(dir.path(), &server, spy(&log));
        s.set(true).await;

        let states = model_states(&mut s);
        assert_eq!(states.first(), Some(&ModelState::Downloading(0)));
        assert!(states.contains(&ModelState::Downloading(100)), "{states:?}");
        assert_eq!(states.last(), Some(&ModelState::Ready));
        assert!(dir.path().join("base.bin").is_file(), "the Standard model");
        assert!(dir.path().join("vad.bin").is_file());
        assert!(
            !dir.path().join("tiny.bin").exists(),
            "only the quality's own"
        );
        // The engine runs: the tap starts on the stream.
        s.play().await;
        assert_eq!(s.taps.running().len(), 1);

        // Turned off and on again: the files are there, nothing is asked of the server again.
        let asked = server.received_requests().await.unwrap().len();
        s.set(false).await;
        s.set(true).await;
        assert_eq!(server.received_requests().await.unwrap().len(), asked);
        assert_eq!(model_states(&mut s), [ModelState::Ready]);
    }

    #[tokio::test]
    async fn a_failed_download_says_so_starts_nothing_and_a_retry_works() {
        let server = wiremock::MockServer::start().await; // every file: 404
        let dir = tempfile::tempdir().unwrap();
        let mut s = with_models(dir.path(), &server, fake(&["x"], 1.0));
        s.set(true).await;
        s.play().await;
        let states = model_states(&mut s);
        assert_eq!(states.last(), Some(&ModelState::Failed), "{states:?}");
        assert!(s.taps.started().is_empty(), "no engine, no tap");

        // The network is back: the same request again (the menu's "A to retry").
        let server = testing::server(Duration::ZERO).await;
        s.captions.models = Some(Arc::new(testing::models(dir.path(), &server)));
        s.set(true).await;
        assert_eq!(model_states(&mut s).last(), Some(&ModelState::Ready));
        assert_eq!(s.taps.running().len(), 1);
    }

    #[tokio::test]
    async fn turning_captions_off_mid_download_cancels_it() {
        let server = testing::server(Duration::from_millis(300)).await;
        let dir = tempfile::tempdir().unwrap();
        let mut s = with_models(dir.path(), &server, fake(&["x"], 1.0));
        let on = s.captions.request();
        let (_, _) = tokio::join!(s.captions.apply(on, true, &s.playback, &s.tx), async {
            // The first file is on its way: now off.
            tokio::time::sleep(Duration::from_millis(100)).await;
            let off = s.captions.request();
            s.captions.apply(off, false, &s.playback, &s.tx).await;
        });
        assert!(!dir.path().join("base.bin").exists(), "no finished model");
        s.play().await;
        assert!(s.taps.started().is_empty(), "captions are off");
        let states = model_states(&mut s);
        assert!(!states.contains(&ModelState::Failed), "{states:?}");
        assert!(!states.contains(&ModelState::Ready), "{states:?}");
    }

    #[tokio::test]
    async fn another_quality_loads_another_model() {
        let server = testing::server(Duration::ZERO).await;
        let dir = tempfile::tempdir().unwrap();
        let log = Arc::new(Mutex::new(Log::default()));
        let mut s = with_models(dir.path(), &server, spy(&log));
        s.set(true).await;
        s.play().await;
        assert_eq!(s.taps.running().len(), 1);
        assert!(!log.lock().unwrap().dropped);

        // Fast: the old engine goes, the other model comes, and the stream gets a new engine.
        s.captions.set_quality(CaptionsQuality::Fast);
        s.set(true).await;
        assert!(dir.path().join("tiny.bin").is_file());
        // The old engine's thread ends on its next poll, not at once.
        let deadline = std::time::Instant::now() + POLL * 20;
        while !log.lock().unwrap().dropped && std::time::Instant::now() < deadline {
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
        assert!(
            log.lock().unwrap().dropped,
            "the Standard engine was dropped"
        );
        assert_eq!(s.taps.running().len(), 1);
        let states = model_states(&mut s);
        assert!(states.contains(&ModelState::Downloading(100)), "{states:?}");
        assert_eq!(states.last(), Some(&ModelState::Ready));
    }

    #[tokio::test]
    async fn the_same_quality_again_keeps_the_engine() {
        let server = testing::server(Duration::ZERO).await;
        let dir = tempfile::tempdir().unwrap();
        let log = Arc::new(Mutex::new(Log::default()));
        let s = with_models(dir.path(), &server, spy(&log));
        s.set(true).await;
        s.captions.set_quality(CaptionsQuality::Standard);
        s.set(true).await;
        assert!(!log.lock().unwrap().dropped);
    }

    #[tokio::test]
    async fn no_engine_means_unavailable_and_nothing_runs() {
        let s = Setup::new(None);
        assert!(!s.captions.available());
        s.set(true).await;
        s.play().await;
        assert!(s.taps.started().is_empty(), "no tap without an engine");
        assert!(Setup::new(fake(&["x"], 1.0)).captions.available());
    }

    #[tokio::test]
    async fn an_older_request_gives_way_to_a_newer_one() {
        let s = Setup::new(fake(&["x"], 1.0));
        let on = s.captions.request();
        let off = s.captions.request();
        s.captions.apply(on, true, &s.playback, &s.tx).await;
        s.play().await;
        assert!(s.taps.started().is_empty(), "the stale request did nothing");
        s.captions.apply(off, false, &s.playback, &s.tx).await;
    }

    #[test]
    fn captions_become_milliseconds_on_the_stream_clock() {
        let c = Caption {
            start_s: 12.3456,
            end_s: 14.0,
            text: "hi there".into(),
            words: vec![
                htpc_captions::Word::new(12.3456, "hi"),
                htpc_captions::Word::new(13.0004, "there"),
            ],
        };
        assert_eq!(
            event_of(&c),
            Event::Caption {
                start_ms: 12_346,
                end_ms: 14_000,
                text: "hi there".into(),
                words: vec![
                    CaptionWord::new(12_346, "hi"),
                    CaptionWord::new(13_000, "there")
                ],
            }
        );
    }

    #[test]
    fn the_mock_engine_writes_lines() {
        let mut t = mock_factory()(CaptionsQuality::Standard).expect("made");
        for i in 0..6 {
            t.push(f64::from(i) * 0.5, &[0.0; 8_000]);
        }
        let got = t.poll();
        assert_eq!(got.len(), 1);
        assert!(got[0].text.starts_with("Welcome"));
    }
}
