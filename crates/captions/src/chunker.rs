use crate::{secs, SAMPLE_RATE};

/// A stretch of speech, in seconds from the start of the audio given to the detector.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Span {
    pub start_s: f64,
    pub end_s: f64,
}

impl Span {
    fn len(&self) -> f64 {
        self.end_s - self.start_s
    }
}

/// Finds speech in a run of audio. The Whisper build uses Silero VAD; tests use [`EnergyDetector`].
pub trait SpeechDetector: Send {
    /// Speech spans in `samples`, in time order, as seconds from its first sample.
    fn detect(&mut self, samples: &[f32]) -> Vec<Span>;
}

/// A speech segment ready for the model. `start_s` is on the stream clock.
#[derive(Debug, Clone, PartialEq)]
pub struct SpeechChunk {
    pub start_s: f64,
    pub samples: Vec<f32>,
}

impl SpeechChunk {
    pub fn duration_s(&self) -> f64 {
        secs(self.samples.len())
    }

    pub fn rms(&self) -> f32 {
        if self.samples.is_empty() {
            return 0.0;
        }
        let sum: f32 = self.samples.iter().map(|s| s * s).sum();
        (sum / self.samples.len() as f32).sqrt()
    }
}

#[derive(Debug, Clone)]
pub struct ChunkerConfig {
    /// Shortest segment sent on purpose. Shorter speech waits for more, up to `lone_silence_s`.
    pub min_s: f64,
    /// Longest segment. Longer speech is cut here.
    pub max_s: f64,
    /// Pauses shorter than this inside speech don't split a segment.
    pub merge_gap_s: f64,
    /// Silence after speech of at least `min_s` that ends the segment.
    pub close_silence_s: f64,
    /// Silence after speech shorter than `min_s` that ends it anyway (a lone "Yes.").
    pub lone_silence_s: f64,
    /// Audio kept around each segment.
    pub pad_s: f64,
    /// Silence kept ahead of speech that may be starting.
    pub preroll_s: f64,
    /// A push this far from where the last one ended starts a new run.
    pub max_jump_s: f64,
}

impl Default for ChunkerConfig {
    fn default() -> Self {
        Self {
            min_s: 2.0,
            // A caption is made only once its segment is cut, so this bounds how late it comes
            // (captions-plan.md, phase 3b: 8 s made long speech up to ~10 s late).
            max_s: 5.0,
            merge_gap_s: 0.4,
            close_silence_s: 0.6,
            lone_silence_s: 1.5,
            pad_s: 0.15,
            preroll_s: 0.5,
            max_jump_s: 0.25,
        }
    }
}

/// Cuts a stream of audio into speech segments of about 2-5 s, each emitted once, with no overlap.
///
/// Audio goes in as it arrives. The detector looks at everything still buffered after each push,
/// and a segment comes out once its speech has ended (or hit `max_s`).
pub struct Chunker<D> {
    cfg: ChunkerConfig,
    detector: D,
    buf: Vec<f32>,
    buf_start_s: f64,
}

impl<D: SpeechDetector> Chunker<D> {
    pub fn new(detector: D, cfg: ChunkerConfig) -> Self {
        Self {
            cfg,
            detector,
            buf: Vec::new(),
            buf_start_s: 0.0,
        }
    }

    /// Seconds of audio held.
    pub fn buffered_s(&self) -> f64 {
        secs(self.buf.len())
    }

    /// Drop everything held.
    pub fn reset(&mut self) {
        self.buf.clear();
    }

    /// Add audio and return the segments it completed.
    pub fn push(&mut self, start_s: f64, samples: &[f32]) -> Vec<SpeechChunk> {
        let mut out = Vec::new();
        if !self.buf.is_empty() {
            let expected = self.buf_start_s + secs(self.buf.len());
            if (start_s - expected).abs() > self.cfg.max_jump_s {
                // The audio is not a continuation: finish what we have, then start over.
                self.drain(true, &mut out);
                self.buf.clear();
            }
        }
        if self.buf.is_empty() {
            self.buf_start_s = start_s;
        }
        self.buf.extend_from_slice(samples);
        self.drain(false, &mut out);
        out
    }

    /// Emit whatever speech is held, ended or not (the end of a run).
    pub fn flush(&mut self) -> Vec<SpeechChunk> {
        let mut out = Vec::new();
        self.drain(true, &mut out);
        self.buf.clear();
        out
    }

    fn idx(s: f64) -> usize {
        (s.max(0.0) * f64::from(SAMPLE_RATE)).round() as usize
    }

    fn drop_front(&mut self, to_s: f64) {
        let n = Self::idx(to_s).min(self.buf.len());
        self.buf.drain(..n);
        self.buf_start_s += secs(n);
    }

    fn emit(&mut self, start_s: f64, end_s: f64, out: &mut Vec<SpeechChunk>) {
        let a = Self::idx(start_s - self.cfg.pad_s).min(self.buf.len());
        let b = Self::idx(end_s + self.cfg.pad_s).min(self.buf.len());
        if b > a {
            out.push(SpeechChunk {
                start_s: self.buf_start_s + secs(a),
                samples: self.buf[a..b].to_vec(),
            });
        }
        self.drop_front(end_s);
    }

    fn drain(&mut self, force: bool, out: &mut Vec<SpeechChunk>) {
        let cfg = self.cfg.clone();
        loop {
            let spans = self.detector.detect(&self.buf);
            let end_t = secs(self.buf.len());
            let Some(&first) = spans.first() else {
                // Nothing but silence: keep only a little in case speech starts.
                self.drop_front(end_t - cfg.preroll_s);
                return;
            };
            // Join following spans across pauses. Short speech joins across longer pauses.
            let mut cur = first;
            let mut i = 1;
            while let Some(next) = spans.get(i) {
                let limit = if cur.len() < cfg.min_s {
                    cfg.lone_silence_s
                } else {
                    cfg.merge_gap_s
                };
                if next.start_s - cur.end_s < limit {
                    cur.end_s = next.end_s;
                    i += 1;
                } else {
                    break;
                }
            }
            let next = spans.get(i);

            if cur.len() >= cfg.max_s {
                let cut = cur.start_s + cfg.max_s;
                self.emit(cur.start_s, cut, out);
                continue;
            }
            let silence_after = next.map_or(end_t - cur.end_s, |n| n.start_s - cur.end_s);
            let needed = if cur.len() >= cfg.min_s {
                cfg.close_silence_s
            } else {
                cfg.lone_silence_s
            };
            // A following span means the pause was already long enough to keep them apart.
            if force || next.is_some() || silence_after >= needed {
                self.emit(cur.start_s, cur.end_s, out);
                continue;
            }
            // Speech is still going, or may continue: drop the silence before it and wait.
            self.drop_front(cur.start_s - cfg.preroll_s);
            return;
        }
    }
}

/// A plain loudness gate: a 0.1 s window is speech when its RMS is over `threshold`. For tests
/// and as a stand-in where no VAD model is available.
#[derive(Debug, Clone)]
pub struct EnergyDetector {
    pub threshold: f32,
}

impl Default for EnergyDetector {
    fn default() -> Self {
        Self { threshold: 0.02 }
    }
}

impl SpeechDetector for EnergyDetector {
    fn detect(&mut self, samples: &[f32]) -> Vec<Span> {
        let win = (SAMPLE_RATE / 10) as usize;
        let mut spans: Vec<Span> = Vec::new();
        for (i, w) in samples.chunks(win).enumerate() {
            let rms = (w.iter().map(|s| s * s).sum::<f32>() / w.len() as f32).sqrt();
            if rms >= self.threshold {
                let start_s = secs(i * win);
                let end_s = secs(i * win + w.len());
                match spans.last_mut() {
                    Some(last) if (start_s - last.end_s).abs() < 1e-9 => last.end_s = end_s,
                    _ => spans.push(Span { start_s, end_s }),
                }
            }
        }
        spans
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const HALF: usize = 8000;

    fn tone(secs_: f64) -> Vec<f32> {
        vec![0.3; (secs_ * 16000.0) as usize]
    }

    fn quiet(secs_: f64) -> Vec<f32> {
        vec![0.0; (secs_ * 16000.0) as usize]
    }

    /// Feed `audio` in 0.5 s pushes from `t0`, collecting chunks.
    fn feed(c: &mut Chunker<EnergyDetector>, t0: f64, audio: &[f32]) -> Vec<SpeechChunk> {
        let mut out = Vec::new();
        for (i, part) in audio.chunks(HALF).enumerate() {
            out.extend(c.push(t0 + i as f64 * 0.5, part));
        }
        out
    }

    fn chunker() -> Chunker<EnergyDetector> {
        Chunker::new(EnergyDetector::default(), ChunkerConfig::default())
    }

    #[test]
    fn silence_makes_nothing_and_stays_small() {
        let mut c = chunker();
        let out = feed(&mut c, 0.0, &quiet(60.0));
        assert!(out.is_empty());
        assert!(c.buffered_s() <= 1.0);
    }

    #[test]
    fn one_utterance_is_emitted_once_after_the_pause() {
        let mut c = chunker();
        let mut audio = quiet(1.0);
        audio.extend(tone(3.0));
        audio.extend(quiet(2.0));
        let out = feed(&mut c, 100.0, &audio);
        assert_eq!(out.len(), 1);
        let ch = &out[0];
        // Speech is 101.0-104.0; padded by 0.15 s each side.
        assert!((ch.start_s - 100.85).abs() < 0.01, "{}", ch.start_s);
        assert!((ch.duration_s() - 3.3).abs() < 0.02, "{}", ch.duration_s());
    }

    #[test]
    fn nothing_comes_out_while_speech_is_still_going() {
        let mut c = chunker();
        let out = feed(&mut c, 0.0, &tone(4.5));
        assert!(out.is_empty());
    }

    #[test]
    fn long_speech_is_cut_at_the_maximum_with_no_overlap() {
        let mut c = chunker();
        let mut audio = tone(20.0);
        audio.extend(quiet(2.0));
        let out = feed(&mut c, 0.0, &audio);
        assert!(out.len() >= 4, "{}", out.len());
        for ch in &out {
            assert!(ch.duration_s() <= 5.0 + 0.31, "{}", ch.duration_s());
        }
        // Speech parts (without the 0.15 s pads) do not overlap.
        for pair in out.windows(2) {
            let end_a = pair[0].start_s + pair[0].duration_s() - 0.15;
            let start_b = pair[1].start_s + 0.15;
            assert!(start_b >= end_a - 0.02, "{end_a} {start_b}");
        }
        let total: f64 = out.iter().map(SpeechChunk::duration_s).sum();
        assert!(total >= 19.5, "{total}");
    }

    #[test]
    fn short_pauses_do_not_split() {
        let mut c = chunker();
        let mut audio = tone(2.0);
        audio.extend(quiet(0.3));
        audio.extend(tone(2.0));
        audio.extend(quiet(2.0));
        let out = feed(&mut c, 0.0, &audio);
        assert_eq!(out.len(), 1);
        assert!(out[0].duration_s() > 4.2);
    }

    #[test]
    fn short_speech_waits_then_joins_nearby_speech() {
        let mut c = chunker();
        let mut audio = tone(0.8);
        audio.extend(quiet(1.0));
        audio.extend(tone(1.5));
        audio.extend(quiet(2.0));
        let out = feed(&mut c, 0.0, &audio);
        assert_eq!(out.len(), 1, "{out:?}");
        assert!(out[0].duration_s() > 3.2);
    }

    #[test]
    fn a_lone_short_word_comes_out_after_a_long_pause() {
        let mut c = chunker();
        let mut audio = tone(0.6);
        audio.extend(quiet(3.0));
        let out = feed(&mut c, 0.0, &audio);
        assert_eq!(out.len(), 1);
        assert!(out[0].duration_s() < 1.2);
    }

    #[test]
    fn a_pause_under_the_close_silence_but_over_the_merge_gap_still_splits() {
        // 2.2 s of speech, a 0.5 s pause, more speech still going: the first part must not wait.
        let mut c = chunker();
        let mut audio = tone(2.2);
        audio.extend(quiet(0.5));
        audio.extend(tone(2.5));
        let out = feed(&mut c, 0.0, &audio);
        assert_eq!(out.len(), 1, "{out:?}");
        assert!(
            (out[0].duration_s() - 2.35).abs() < 0.1,
            "{}",
            out[0].duration_s()
        );
    }

    #[test]
    fn two_utterances_make_two_chunks() {
        let mut c = chunker();
        let mut audio = tone(3.0);
        audio.extend(quiet(2.0));
        audio.extend(tone(3.0));
        audio.extend(quiet(2.0));
        let out = feed(&mut c, 0.0, &audio);
        assert_eq!(out.len(), 2);
        assert!(out[1].start_s > out[0].start_s + 4.0);
    }

    #[test]
    fn a_jump_in_time_flushes_and_starts_over() {
        let mut c = chunker();
        let mut out = feed(&mut c, 0.0, &tone(3.0));
        assert!(out.is_empty());
        out.extend(c.push(500.0, &quiet(0.5)));
        assert_eq!(out.len(), 1);
        assert!((out[0].start_s - 0.0).abs() < 0.01);
        assert!(c.buffered_s() <= 0.6);
    }

    #[test]
    fn reset_forgets_pending_speech() {
        let mut c = chunker();
        feed(&mut c, 0.0, &tone(3.0));
        c.reset();
        let out = feed(&mut c, 50.0, &quiet(5.0));
        assert!(out.is_empty());
    }

    #[test]
    fn flush_emits_unfinished_speech() {
        let mut c = chunker();
        feed(&mut c, 0.0, &tone(3.0));
        let out = c.flush();
        assert_eq!(out.len(), 1);
        assert!(c.flush().is_empty());
    }
}
