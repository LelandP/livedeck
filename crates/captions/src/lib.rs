//! Speech audio in, timed caption lines out.
//!
//! - [`Transcriber`]: push 16 kHz mono `f32` audio with the stream timestamp of its first sample,
//!   poll finished [`Caption`]s on the same clock. Neither call blocks.
//! - [`FakeTranscriber`]: scripted captions for tests, here and in other crates.
//! - [`Chunker`]: cuts a voice-activity decision into speech segments of 2-5 s. Pure code.
//! - [`AudioQueue`]: a bounded queue that drops the oldest audio when inference falls behind.
//! - [`CaptionWorker`]: the thread that joins them with a [`SegmentEngine`] (the model).
//! - `WhisperTranscriber` (feature `whisper`): whisper.cpp with its built-in Silero VAD.

mod chunker;
mod fake;
mod guard;
mod queue;
mod worker;

#[cfg(feature = "whisper")]
mod whisper;

pub use chunker::{Chunker, ChunkerConfig, EnergyDetector, Span, SpeechChunk, SpeechDetector};
pub use fake::FakeTranscriber;
pub use guard::is_hallucination;
pub use queue::{AudioQueue, QueueItem};
pub use worker::{CaptionWorker, RawSegment, SegmentEngine, DEFAULT_BACKLOG_S};

#[cfg(feature = "whisper")]
pub use whisper::{install_logging, DtwModelPreset, WhisperConfig, WhisperTranscriber};

/// The sample rate every input must have.
pub const SAMPLE_RATE: u32 = 16_000;

/// One line of captions. Times are seconds on the stream clock the audio was pushed with.
#[derive(Debug, Clone, PartialEq)]
pub struct Caption {
    pub start_s: f64,
    pub end_s: f64,
    pub text: String,
    /// The words of `text` with the time each starts, in order, within `start_s..=end_s`. Empty
    /// when the engine gives no word times (the caller spreads the words over the line).
    pub words: Vec<Word>,
}

/// One word of a [`Caption`] (with the punctuation that sticks to it).
#[derive(Debug, Clone, PartialEq)]
pub struct Word {
    pub start_s: f64,
    pub text: String,
}

impl Word {
    pub fn new(start_s: f64, text: impl Into<String>) -> Self {
        Self {
            start_s,
            text: text.into(),
        }
    }
}

/// `text`'s words spread over `start_s..end_s` by their length in characters: what a caption
/// without word times is shown with.
pub fn spread_words(start_s: f64, end_s: f64, text: &str) -> Vec<Word> {
    let words: Vec<&str> = text.split_whitespace().collect();
    let total: usize = words.iter().map(|w| w.chars().count()).sum();
    let span = (end_s - start_s).max(0.0);
    let mut before = 0usize;
    words
        .into_iter()
        .map(|w| {
            let at = start_s + span * before as f64 / total.max(1) as f64;
            before += w.chars().count();
            Word::new(at, w)
        })
        .collect()
}

/// Turns speech audio into captions. Implementations do their work off the caller's thread.
pub trait Transcriber: Send {
    /// Queue `samples` (16 kHz mono f32) whose first sample is at `start_s`. Never blocks. Audio
    /// that does not continue the previous push (a gap or a jump back) starts a new run.
    fn push(&mut self, start_s: f64, samples: &[f32]);

    /// Captions finished since the last call, oldest first.
    fn poll(&mut self) -> Vec<Caption>;

    /// Forget queued and buffered audio (a seek, a reload). Captions already made stay for `poll`,
    /// except one that was being made when the reset came: it is dropped, as it belongs to audio
    /// the caller no longer wants.
    fn reset(&mut self);

    /// How many seconds of audio may wait for the engine before the oldest is dropped
    /// ([`DEFAULT_BACKLOG_S`] to begin with). A live stream wants it short (old audio is late
    /// anyway), a video that is read ahead of its playhead wants it long. Engines with no queue
    /// ignore it.
    fn set_backlog_s(&mut self, _backlog_s: f64) {}

    /// Audio that ends at or before `playhead_s` (on the clock the audio was pushed with) is of
    /// no use: the picture has passed it. It is skipped when its turn comes, so an engine that
    /// cannot keep up with a video made faster than it runs makes the captions that are still
    /// ahead instead of working through the ones that are late. `f64::NEG_INFINITY` takes the
    /// limit away. Engines with no queue ignore it.
    fn set_playhead_s(&mut self, _playhead_s: f64) {}
}

/// Seconds of audio in `n` samples.
pub(crate) fn secs(n: usize) -> f64 {
    n as f64 / f64::from(SAMPLE_RATE)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn words_without_times_are_spread_by_length() {
        let words = spread_words(10.0, 18.0, " aaaa  bbbb cccccccc ");
        let got: Vec<(f64, &str)> = words.iter().map(|w| (w.start_s, w.text.as_str())).collect();
        assert_eq!(got, [(10.0, "aaaa"), (12.0, "bbbb"), (14.0, "cccccccc")]);
        assert!(spread_words(1.0, 2.0, "  ").is_empty());
        // An end before the start puts every word at the start.
        assert!(spread_words(5.0, 4.0, "a b")
            .iter()
            .all(|w| w.start_s == 5.0));
    }
}
