//! The last few warnings and errors mpv logged, for the error screen. With no terminal on a Steam
//! Deck in Game Mode, a photo of the screen is the bug report, so it has to say what mpv said.

use std::collections::VecDeque;

/// How many lines the error screen shows.
const KEEP: usize = 6;
/// Longer lines are cut. Playlist URLs are long (and carry a short-lived access token), and the
/// interesting part is at the start.
const MAX_CHARS: usize = 140;

/// A ring of the latest log lines, oldest first.
#[derive(Debug, Default)]
pub(crate) struct RecentLog {
    lines: VecDeque<String>,
}

impl RecentLog {
    /// Adds one mpv log message (`prefix` is the mpv module, such as `ffmpeg` or `cplayer`).
    pub(crate) fn push(&mut self, prefix: &str, level: &str, text: &str) {
        let text = text.trim();
        if text.is_empty() {
            return;
        }
        let mut line = format!("[{prefix}] {level}: {text}");
        if line.chars().count() > MAX_CHARS {
            line = line.chars().take(MAX_CHARS - 1).collect::<String>() + "…";
        }
        if self.lines.len() == KEEP {
            self.lines.pop_front();
        }
        self.lines.push_back(line);
    }

    /// Forgets everything (a new stream starts).
    pub(crate) fn clear(&mut self) {
        self.lines.clear();
    }

    /// `summary`, followed by the recent lines when there are any.
    pub(crate) fn report(&self, summary: &str) -> String {
        if self.lines.is_empty() {
            return summary.to_owned();
        }
        let lines: Vec<&str> = self.lines.iter().map(String::as_str).collect();
        format!("{summary}\n\n{}", lines.join("\n"))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_report_is_the_summary_then_the_latest_lines() {
        let mut log = RecentLog::default();
        assert_eq!(log.report("could not open"), "could not open");
        log.push("ffmpeg", "error", "https: Protocol not found\n");
        log.push("cplayer", "warn", "   ");
        assert_eq!(
            log.report("could not open"),
            "could not open\n\n[ffmpeg] error: https: Protocol not found"
        );
    }

    #[test]
    fn only_the_newest_lines_are_kept_and_long_ones_are_cut() {
        let mut log = RecentLog::default();
        for i in 0..10 {
            log.push("stream", "warn", &format!("line {i}"));
        }
        let report = log.report("x");
        assert!(
            !report.contains("line 3") && report.contains("line 4") && report.contains("line 9")
        );

        log.clear();
        log.push("ffmpeg", "error", &"u".repeat(500));
        let line = log.report("x").lines().last().unwrap().to_owned();
        assert_eq!(line.chars().count(), MAX_CHARS);
        assert!(line.ends_with('…'));
    }
}
