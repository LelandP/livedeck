use crate::{secs, spread_words, Caption, Transcriber};

/// A scripted [`Transcriber`]. Every `every_s` seconds of continuous audio it makes one caption
/// from the next line of its script (the last line repeats once the script runs out), its words
/// spread over the caption's time.
#[derive(Debug)]
pub struct FakeTranscriber {
    lines: Vec<String>,
    every_s: f64,
    next_line: usize,
    run_start_s: Option<f64>,
    run_len: usize,
    out: Vec<Caption>,
    pushed_samples: usize,
    resets: usize,
}

impl FakeTranscriber {
    pub fn new<S: Into<String>>(lines: impl IntoIterator<Item = S>, every_s: f64) -> Self {
        Self {
            lines: lines.into_iter().map(Into::into).collect(),
            every_s,
            next_line: 0,
            run_start_s: None,
            run_len: 0,
            out: Vec::new(),
            pushed_samples: 0,
            resets: 0,
        }
    }

    /// Total samples pushed so far.
    pub fn pushed_samples(&self) -> usize {
        self.pushed_samples
    }

    /// How many times [`Transcriber::reset`] was called.
    pub fn resets(&self) -> usize {
        self.resets
    }

    fn text(&mut self) -> String {
        let i = self.next_line.min(self.lines.len().saturating_sub(1));
        self.next_line += 1;
        self.lines.get(i).cloned().unwrap_or_default()
    }
}

impl Transcriber for FakeTranscriber {
    fn push(&mut self, start_s: f64, samples: &[f32]) {
        self.pushed_samples += samples.len();
        let run_start = *self.run_start_s.get_or_insert(start_s);
        self.run_len += samples.len();
        if secs(self.run_len) >= self.every_s {
            let end_s = start_s + secs(samples.len());
            let text = self.text();
            let words = spread_words(run_start, end_s, &text);
            self.out.push(Caption {
                start_s: run_start,
                end_s,
                text,
                words,
            });
            self.run_start_s = None;
            self.run_len = 0;
        }
    }

    fn poll(&mut self) -> Vec<Caption> {
        std::mem::take(&mut self.out)
    }

    fn reset(&mut self) {
        self.resets += 1;
        self.run_start_s = None;
        self.run_len = 0;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn makes_a_caption_per_interval() {
        let mut t = FakeTranscriber::new(["one", "two"], 1.0);
        for i in 0..6 {
            t.push(f64::from(i) * 0.5, &[0.0; 8000]);
        }
        let got = t.poll();
        assert_eq!(got.len(), 3);
        assert_eq!(got[0].text, "one");
        assert_eq!((got[0].start_s, got[0].end_s), (0.0, 1.0));
        assert_eq!(got[0].words, [crate::Word::new(0.0, "one")]);
        assert_eq!(got[1].text, "two");
        assert_eq!(got[2].text, "two");
        assert!(t.poll().is_empty());
    }

    #[test]
    fn reset_drops_the_partial_run() {
        let mut t = FakeTranscriber::new(["x"], 1.0);
        t.push(0.0, &[0.0; 8000]);
        t.reset();
        t.push(10.0, &[0.0; 8000]);
        assert!(t.poll().is_empty());
        assert_eq!(t.resets(), 1);
        assert_eq!(t.pushed_samples(), 16000);
    }
}
