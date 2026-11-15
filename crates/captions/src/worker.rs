use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::mpsc;
use std::sync::{Arc, Condvar, Mutex, MutexGuard};
use std::thread::JoinHandle;

use crate::chunker::{Chunker, ChunkerConfig, SpeechChunk, SpeechDetector};
use crate::guard::is_hallucination;
use crate::queue::{AudioQueue, QueueItem};
use crate::{spread_words, Caption, Transcriber, Word};

/// One piece of text from the model, in seconds from the start of the chunk it was given.
#[derive(Debug, Clone, PartialEq)]
pub struct RawSegment {
    pub start_s: f64,
    pub end_s: f64,
    pub text: String,
    /// Its words with their start times (seconds from the start of the chunk), if the model
    /// gives them. Empty: they are spread over the segment by length.
    pub words: Vec<Word>,
}

/// The model: speech audio in, text out. Runs on the worker thread only.
pub trait SegmentEngine {
    fn transcribe(&mut self, samples: &[f32]) -> Result<Vec<RawSegment>, String>;
}

/// Seconds of audio the queue may hold before the oldest is dropped, unless
/// [`CaptionWorker::set_backlog_s`] says otherwise. A live picture held back for captions trails
/// the newest audio by 10 to 12.5 s (captions-plan.md, phase 3b), so a caption for audio further
/// back than this would be late anyway: a tap starting 28 s back (a new stream) skips the first
/// 13 s instead of spending ~25 s catching up on them.
pub const DEFAULT_BACKLOG_S: f64 = 15.0;

struct Shared {
    queue: Mutex<AudioQueue>,
    wake: Condvar,
    out: Mutex<Vec<Caption>>,
    stop: AtomicBool,
    /// Goes up with every reset. A chunk that was being transcribed when it did is the old
    /// audio's: its captions are dropped.
    epoch: AtomicU64,
}

fn lock<T>(m: &Mutex<T>) -> MutexGuard<'_, T> {
    m.lock().unwrap_or_else(std::sync::PoisonError::into_inner)
}

/// A [`Transcriber`] that runs a [`Chunker`] and a [`SegmentEngine`] on its own thread.
///
/// `push` only adds to a bounded queue. If the engine is slower than the audio, the oldest queued
/// audio is dropped (see [`CaptionWorker::dropped_s`]).
pub struct CaptionWorker {
    shared: Arc<Shared>,
    handle: Option<JoinHandle<()>>,
}

impl CaptionWorker {
    /// Start the thread. `make` builds the detector and engine on that thread (so they need not be
    /// `Send`) and its error is returned here.
    pub fn spawn<D, E, F>(make: F, cfg: ChunkerConfig) -> Result<Self, String>
    where
        D: SpeechDetector + 'static,
        E: SegmentEngine + 'static,
        F: FnOnce() -> Result<(D, E), String> + Send + 'static,
    {
        let shared = Arc::new(Shared {
            queue: Mutex::new(AudioQueue::new(DEFAULT_BACKLOG_S)),
            wake: Condvar::new(),
            out: Mutex::new(Vec::new()),
            stop: AtomicBool::new(false),
            epoch: AtomicU64::new(0),
        });
        let (ready_tx, ready_rx) = mpsc::channel::<Result<(), String>>();
        let thread_shared = Arc::clone(&shared);
        let handle = std::thread::Builder::new()
            .name("captions".into())
            .spawn(move || match make() {
                Ok((detector, engine)) => {
                    let _ = ready_tx.send(Ok(()));
                    run(&thread_shared, Chunker::new(detector, cfg), engine);
                }
                Err(e) => {
                    let _ = ready_tx.send(Err(e));
                }
            })
            .map_err(|e| e.to_string())?;
        match ready_rx.recv() {
            Ok(Ok(())) => Ok(Self {
                shared,
                handle: Some(handle),
            }),
            Ok(Err(e)) => {
                let _ = handle.join();
                Err(e)
            }
            Err(_) => {
                let _ = handle.join();
                Err("the captions thread stopped while starting".into())
            }
        }
    }

    /// Seconds of audio waiting at most before the oldest is dropped ([`DEFAULT_BACKLOG_S`]).
    pub fn set_backlog_s(&self, backlog_s: f64) {
        lock(&self.shared.queue).set_cap_s(backlog_s);
    }

    /// Seconds of audio dropped for being too old.
    pub fn dropped_s(&self) -> f64 {
        lock(&self.shared.queue).dropped_s()
    }

    /// Seconds of audio waiting for the thread.
    pub fn queued_s(&self) -> f64 {
        lock(&self.shared.queue).queued_s()
    }
}

impl Transcriber for CaptionWorker {
    fn push(&mut self, start_s: f64, samples: &[f32]) {
        lock(&self.shared.queue).push_audio(start_s, samples.to_vec());
        self.shared.wake.notify_one();
    }

    fn poll(&mut self) -> Vec<Caption> {
        std::mem::take(&mut *lock(&self.shared.out))
    }

    fn reset(&mut self) {
        // Under the queue's lock, with the clearing: the thread takes its item and the epoch
        // under it too, so an item is of the epoch it is taken with.
        let mut queue = lock(&self.shared.queue);
        self.shared.epoch.fetch_add(1, Ordering::SeqCst);
        queue.reset();
        drop(queue);
        self.shared.wake.notify_one();
    }

    fn set_backlog_s(&mut self, backlog_s: f64) {
        CaptionWorker::set_backlog_s(self, backlog_s);
    }

    fn set_playhead_s(&mut self, playhead_s: f64) {
        lock(&self.shared.queue).set_floor_s(playhead_s);
    }
}

impl Drop for CaptionWorker {
    fn drop(&mut self) {
        self.shared.stop.store(true, Ordering::SeqCst);
        // Take the lock so the thread is either before its stop check or already waiting.
        drop(lock(&self.shared.queue));
        self.shared.wake.notify_all();
        // Not joined: a chunk in the model can take seconds, and this runs on the UI thread. The
        // thread checks `stop` after each chunk and before each wait, so it exits on its own.
        drop(self.handle.take());
    }
}

fn run<D: SpeechDetector, E: SegmentEngine>(
    shared: &Shared,
    mut chunker: Chunker<D>,
    mut engine: E,
) {
    loop {
        let (item, epoch) = {
            let mut q = lock(&shared.queue);
            loop {
                if shared.stop.load(Ordering::SeqCst) {
                    return;
                }
                if let Some(item) = q.pop() {
                    break (item, shared.epoch.load(Ordering::SeqCst));
                }
                q = shared
                    .wake
                    .wait(q)
                    .unwrap_or_else(std::sync::PoisonError::into_inner);
            }
        };
        match item {
            QueueItem::Reset => chunker.reset(),
            QueueItem::Audio { start_s, samples } => {
                for chunk in chunker.push(start_s, &samples) {
                    if shared.stop.load(Ordering::SeqCst) {
                        return;
                    }
                    // Reset while the model ran (a seek): this audio is not wanted any more.
                    if shared.epoch.load(Ordering::SeqCst) != epoch {
                        break;
                    }
                    let captions = transcribe_chunk(&mut engine, &chunk);
                    if shared.epoch.load(Ordering::SeqCst) != epoch {
                        break;
                    }
                    if !captions.is_empty() {
                        lock(&shared.out).extend(captions);
                    }
                }
            }
        }
    }
}

/// Run the engine on one chunk and turn its segments into captions on the stream clock.
fn transcribe_chunk<E: SegmentEngine>(engine: &mut E, chunk: &SpeechChunk) -> Vec<Caption> {
    let segments = match engine.transcribe(&chunk.samples) {
        Ok(s) => s,
        Err(e) => {
            tracing::warn!("captions: transcription failed: {e}");
            return Vec::new();
        }
    };
    let rms = chunk.rms();
    let len = chunk.duration_s();
    segments
        .into_iter()
        .filter_map(|s| {
            // `clamp` panics on NaN bounds, and a NaN time would take the thread down with it.
            if !(s.start_s.is_finite() && s.end_s.is_finite()) {
                return None;
            }
            let start = s.start_s.clamp(0.0, len);
            let end = s.end_s.clamp(start, len);
            if is_hallucination(&s.text, end - start, rms) {
                return None;
            }
            let text = s.text.trim().to_string();
            // On the stream clock, inside the segment, in order. Without times from the model the
            // words are spread over the segment.
            let mut words: Vec<Word> = s
                .words
                .iter()
                .filter(|w| !w.text.trim().is_empty())
                .map(|w| Word::new(w.start_s.clamp(start, end), w.text.trim()))
                .collect();
            if words.is_empty() {
                words = spread_words(start, end, &text);
            }
            let mut last = start;
            for w in &mut words {
                last = w.start_s.max(last);
                w.start_s = chunk.start_s + last;
            }
            Some(Caption {
                start_s: chunk.start_s + start,
                end_s: chunk.start_s + end,
                text,
                words,
            })
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use std::time::{Duration, Instant};

    use super::*;
    use crate::chunker::EnergyDetector;

    /// Says "heard N" for chunk N, after an optional delay.
    struct CountingEngine {
        n: usize,
        delay: Duration,
        text: Option<&'static str>,
    }

    impl SegmentEngine for CountingEngine {
        fn transcribe(&mut self, samples: &[f32]) -> Result<Vec<RawSegment>, String> {
            std::thread::sleep(self.delay);
            self.n += 1;
            let end_s = samples.len() as f64 / 16000.0;
            // Timed words for "heard N": "heard" at 0.4 s, N at 1.0 s.
            let words = if self.text.is_none() {
                vec![
                    Word::new(0.4, " heard"),
                    Word::new(1.0, format!(" {}", self.n)),
                ]
            } else {
                Vec::new()
            };
            Ok(vec![RawSegment {
                start_s: 0.0,
                end_s,
                text: self
                    .text
                    .map_or_else(|| format!("heard {}", self.n), String::from),
                words,
            }])
        }
    }

    fn worker(delay_ms: u64, text: Option<&'static str>) -> CaptionWorker {
        CaptionWorker::spawn(
            move || {
                Ok((
                    EnergyDetector::default(),
                    CountingEngine {
                        n: 0,
                        delay: Duration::from_millis(delay_ms),
                        text,
                    },
                ))
            },
            ChunkerConfig::default(),
        )
        .expect("spawn")
    }

    fn wait_for(w: &mut CaptionWorker, count: usize) -> Vec<Caption> {
        let mut got = Vec::new();
        let t = Instant::now();
        while got.len() < count && t.elapsed() < Duration::from_secs(10) {
            got.extend(w.poll());
            std::thread::sleep(Duration::from_millis(10));
        }
        got
    }

    fn push_speech_then_silence(w: &mut CaptionWorker, t0: f64) {
        let mut t = t0;
        for i in 0..14 {
            let v = if i < 6 { 0.3 } else { 0.0 };
            w.push(t, &vec![v; 8000]);
            t += 0.5;
        }
    }

    #[test]
    fn captions_come_out_on_the_stream_clock() {
        let mut w = worker(0, None);
        push_speech_then_silence(&mut w, 200.0);
        let got = wait_for(&mut w, 1);
        assert_eq!(got.len(), 1);
        assert_eq!(got[0].text, "heard 1");
        assert!((got[0].start_s - 200.0).abs() < 0.5, "{}", got[0].start_s);
        assert!(got[0].end_s > got[0].start_s + 2.5);
        // The words, on the same clock: the chunk's start plus their own time, trimmed.
        let words: Vec<(f64, &str)> = got[0]
            .words
            .iter()
            .map(|w| (w.start_s - got[0].start_s, w.text.as_str()))
            .collect();
        assert_eq!(words.len(), 2);
        assert!((words[0].0 - 0.4).abs() < 1e-9 && words[0].1 == "heard");
        assert!((words[1].0 - 1.0).abs() < 1e-9 && words[1].1 == "1");
    }

    #[test]
    fn words_stay_inside_their_segment_and_in_order() {
        struct Odd;
        impl SegmentEngine for Odd {
            fn transcribe(&mut self, _samples: &[f32]) -> Result<Vec<RawSegment>, String> {
                Ok(vec![RawSegment {
                    start_s: 1.0,
                    end_s: 2.0,
                    text: "a b c d".into(),
                    words: vec![
                        Word::new(0.5, "a"),
                        Word::new(1.6, "b"),
                        Word::new(1.2, "c"),
                        Word::new(9.0, " d "),
                        Word::new(1.5, "  "),
                    ],
                }])
            }
        }
        let chunk = SpeechChunk {
            start_s: 100.0,
            samples: vec![0.3; 48_000],
        };
        let got = transcribe_chunk(&mut Odd, &chunk);
        let words: Vec<(f64, &str)> = got[0]
            .words
            .iter()
            .map(|w| (w.start_s, w.text.as_str()))
            .collect();
        assert_eq!(
            words,
            [(101.0, "a"), (101.6, "b"), (101.6, "c"), (102.0, "d")]
        );
    }

    #[test]
    fn a_segment_with_a_time_that_is_not_a_number_is_dropped() {
        struct NotANumber;
        impl SegmentEngine for NotANumber {
            fn transcribe(&mut self, _samples: &[f32]) -> Result<Vec<RawSegment>, String> {
                Ok(vec![RawSegment {
                    start_s: f64::NAN,
                    end_s: 1.0,
                    text: "hello there".into(),
                    words: Vec::new(),
                }])
            }
        }
        let chunk = SpeechChunk {
            start_s: 10.0,
            samples: vec![0.3; 16_000],
        };
        assert!(transcribe_chunk(&mut NotANumber, &chunk).is_empty());
    }

    #[test]
    fn a_segment_without_word_times_gets_them_spread() {
        let mut w = worker(0, Some("one two three"));
        push_speech_then_silence(&mut w, 50.0);
        let got = wait_for(&mut w, 1);
        let words = &got[0].words;
        assert_eq!(words.len(), 3);
        assert_eq!(words[0].start_s, got[0].start_s);
        assert!(words[1].start_s > words[0].start_s && words[2].start_s < got[0].end_s);
    }

    #[test]
    fn silence_gives_nothing() {
        let mut w = worker(0, None);
        for i in 0..40 {
            w.push(f64::from(i) * 0.5, &vec![0.0; 8000]);
        }
        std::thread::sleep(Duration::from_millis(200));
        assert!(w.poll().is_empty());
    }

    #[test]
    fn hallucinated_text_is_dropped() {
        let mut w = worker(0, Some("Thank you."));
        push_speech_then_silence(&mut w, 0.0);
        std::thread::sleep(Duration::from_millis(300));
        // A 3.3 s, loud segment is not what the guard targets, so the phrase stays.
        let kept = w.poll();
        assert_eq!(kept.len(), 1);
        let mut w = worker(0, Some("[BLANK_AUDIO]"));
        push_speech_then_silence(&mut w, 0.0);
        std::thread::sleep(Duration::from_millis(300));
        assert!(w.poll().is_empty());
    }

    #[test]
    fn falling_behind_drops_old_audio_and_keeps_the_newest() {
        // 80 ms of inference per chunk, and a burst of 120 s of speech-then-pause audio.
        let mut w = worker(80, None);
        let mut t = 0.0;
        for _ in 0..12 {
            push_speech_then_silence(&mut w, t);
            t += 7.0;
        }
        for _ in 0..12 {
            push_speech_then_silence(&mut w, t);
            t += 7.0;
        }
        assert!(w.dropped_s() > 0.0);
        let got = wait_for(&mut w, 2);
        std::thread::sleep(Duration::from_millis(1500));
        let mut all = got;
        all.extend(w.poll());
        let last = all.last().expect("some captions");
        // The newest speech (the last burst starts at 161 s) was not dropped.
        assert!(last.start_s > 160.0, "{}", last.start_s);
    }

    #[test]
    fn the_backlog_is_15_s_unless_set() {
        // A tap's first burst (28 s at once) while the engine is busy: only the newest 15 s wait.
        let mut w = worker(500, None);
        for i in 0..56 {
            w.push(f64::from(i) * 0.5, &[0.3; 8000]);
        }
        assert!(w.queued_s() <= DEFAULT_BACKLOG_S, "{}", w.queued_s());
        assert!(w.dropped_s() > 0.0);
        let mut w = worker(500, None);
        w.set_backlog_s(3.0);
        for i in 0..56 {
            w.push(f64::from(i) * 0.5, &[0.3; 8000]);
        }
        assert!(w.queued_s() <= 3.5, "{}", w.queued_s());
    }

    #[test]
    fn reset_clears_pending_audio() {
        let mut w = worker(0, None);
        for i in 0..6 {
            w.push(f64::from(i) * 0.5, &[0.3; 8000]);
        }
        w.reset();
        for i in 0..20 {
            w.push(100.0 + f64::from(i) * 0.5, &[0.0; 8000]);
        }
        std::thread::sleep(Duration::from_millis(300));
        assert!(w.poll().is_empty());
    }

    #[test]
    fn a_reset_while_the_model_runs_drops_that_chunks_captions() {
        // A seek: the chunk being transcribed belongs to the old place. Its caption must not
        // arrive after the reset (nor those of the audio that came with it).
        let mut w = worker(400, None);
        push_speech_then_silence(&mut w, 200.0);
        // Into the first chunk's 400 ms of inference.
        std::thread::sleep(Duration::from_millis(150));
        w.reset();
        std::thread::sleep(Duration::from_millis(900));
        assert!(w.poll().is_empty(), "a caption of the old place came out");
        // The new place works as before.
        push_speech_then_silence(&mut w, 900.0);
        let got = wait_for(&mut w, 1);
        assert_eq!(got.len(), 1);
        assert!((got[0].start_s - 900.0).abs() < 0.5, "{}", got[0].start_s);
    }

    #[test]
    fn the_backlog_can_be_set_through_the_trait() {
        let mut w = worker(500, None);
        Transcriber::set_backlog_s(&mut w, 45.0);
        for i in 0..90 {
            w.push(f64::from(i) * 0.5, &[0.3; 8000]);
        }
        // 45 s of audio, none dropped (the default would keep 15 s): the first burst of a video
        // read ahead. A few items are already with the thread.
        assert!(w.queued_s() > 30.0, "{}", w.queued_s());
        assert_eq!(w.dropped_s(), 0.0);
    }

    #[test]
    fn audio_the_playhead_has_passed_is_not_transcribed() {
        // 300 ms of inference. The first speech is being made when the second run is pushed
        // and the picture is told to be far past it: only the first comes out.
        let mut w = worker(300, None);
        push_speech_then_silence(&mut w, 0.0);
        std::thread::sleep(Duration::from_millis(100));
        push_speech_then_silence(&mut w, 100.0);
        Transcriber::set_playhead_s(&mut w, 200.0);
        std::thread::sleep(Duration::from_millis(900));
        let got = w.poll();
        assert_eq!(got.len(), 1, "{got:?}");
        assert!(got[0].start_s < 1.0);
        assert!(w.dropped_s() > 5.0, "{}", w.dropped_s());
        // Without the limit the next run is made.
        Transcriber::set_playhead_s(&mut w, f64::NEG_INFINITY);
        push_speech_then_silence(&mut w, 300.0);
        assert_eq!(wait_for(&mut w, 1).len(), 1);
    }

    #[test]
    fn a_failing_factory_is_reported() {
        let r = CaptionWorker::spawn(
            || -> Result<(EnergyDetector, CountingEngine), String> { Err("no model".into()) },
            ChunkerConfig::default(),
        );
        assert_eq!(r.err().as_deref(), Some("no model"));
    }

    #[test]
    fn dropping_stops_the_thread() {
        let w = worker(0, None);
        drop(w);
    }
}
