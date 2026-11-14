//! Generated captions on the live player and on videos (captions-plan.md, phases 3, 3b and 5): the
//! words made by the speech engine wait in a queue until the player's stream clock reaches them,
//! and come on one by one, rolling up: two lines at most, the last one filling word by word, the
//! oldest scrolling off when a third starts. After a pause in the speech the lines clear.
//!
//! The reducer has no clock. The stream clock comes from the player's health reports
//! (`Health::stream_ms`, about once a second) and between them from a timer the app runs
//! (`Command::CaptionTick`, every [`TICK_MS`] while there is something to show or wait for): each
//! tick moves the clock on by that much, and the next report sets it right. A paused stream sends
//! no reports and its clock stands still, as does one the player holds for the delay (the ticks
//! go on then, to count how long the hold lasts: chat arriving meanwhile waits that much longer).
//!
//! **The delay** (`Settings::caption_delay`): a caption is made a few seconds after its speech, so
//! the picture is held back until it trails the audio the captions are made from by
//! [`CAPTION_LEAD_MS`] (`Command::SetLiveDelay`), and chat is held back by as much as the picture
//! is, so it stays in step with it.

use std::collections::VecDeque;

use crate::domain::Platform;
use crate::settings::{CaptionsQuality, DEFAULT_CAPTIONS_QUALITY};
use crate::state::{AppState, Playback, Screen};

/// Words kept waiting at most. A flood beyond this drops the oldest. A video is transcribed 30 s
/// ahead of the playhead (bursts of up to ~40 s), which for a fast talker's 4 to 5 words a second
/// is up to 200 words: twice that leaves room.
pub const MAX_QUEUED: usize = 400;
/// Lines on screen at once. A third one pushes the oldest out.
pub const MAX_LINES: usize = 2;
/// The lines clear when no word has come on for this long, in milliseconds.
pub const CLEAR_AFTER_MS: u64 = 3_000;
/// The time between ticks of the stream clock, in milliseconds: how late a word can show. A
/// video at another speed moves its clock by this much times the speed on each tick.
pub const TICK_MS: u64 = 250;
/// How long after its start a word comes on, in milliseconds. Whisper's word times mark where a
/// word begins, and a word shown there reads as early (the user's test run, 2026-11-14): shown a
/// little later it lands as it is heard. The app can change it (`HTPC_CAPTION_OFFSET_MS`).
pub const SHOW_AFTER_MS: u64 = 500;
/// Characters in a line on screen. A word that does not fit starts the next line.
pub const LINE_CHARS: usize = 44;
/// How far the picture trails the newest audio the captions are made from, with the delay on:
/// a caption takes up to a 5 s segment, the pause that ends it and ~2 s of the model to come.
pub const CAPTION_LEAD_MS: u64 = 10_000;
/// What the player is asked to keep buffered for that ([`Command::SetLiveDelay`]). The lead is
/// the buffer ±1 s (the tap's audio comes a packet of 1 s at a time), and once a hold lets go the
/// buffer saws down to 2.3 s under where it let go as 2 s segments arrive (measured 2026-11-13 on
/// live Twitch): 2.5 s more keeps the lead at 10 s or more.
///
/// [`Command::SetLiveDelay`]: crate::Command::SetLiveDelay
pub const DELAY_BUFFER_MS: u32 = CAPTION_LEAD_MS as u32 + 2_500;
/// What a live stream keeps buffered without the delay (low latency: 3.7 to 4.9 s, measured
/// 2026-11-13). The picture's extra delay, the one chat gets, is its buffer beyond this.
pub const NORMAL_BUFFER_MS: u32 = 4_500;

/// One word of a caption and when it was spoken, on the stream clock.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CaptionWord {
    pub start_ms: u64,
    pub text: String,
}

impl CaptionWord {
    pub fn new(start_ms: u64, text: impl Into<String>) -> Self {
        Self {
            start_ms,
            text: text.into(),
        }
    }
}

/// The captions of the stream on screen. Empty unless captions are on and a Twitch stream plays on
/// the Player screen (`AppState::captions_live`).
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Captions {
    /// Words waiting for their time, earliest first, at most [`MAX_QUEUED`].
    pub queue: VecDeque<CaptionWord>,
    /// On screen, oldest first, at most [`MAX_LINES`]. The last one is filling.
    pub shown: Vec<String>,
    /// The clock when the last word came on: the lines clear [`CLEAR_AFTER_MS`] after it.
    pub last_word_ms: Option<u64>,
    /// Where the stream is, as far as is known: the last report moved on by the ticks since.
    /// `None` until the first report, so nothing shows before it.
    pub clock_ms: Option<u64>,
    /// The player's buffer in the last report, which tells how far the delay holds it back.
    pub buffer_ms: u32,
    /// The player holds the picture for the delay (`Event::PlayerHeld`): the clock stands still.
    pub held: bool,
    /// How long the picture has been held since the last report, counted by the ticks (which go
    /// on while it is held). The buffer grows by as much meanwhile, so chat arriving during a
    /// hold waits that much longer.
    pub held_ms: u64,
    /// The id of the last tick asked for, kept so ids never repeat.
    pub last_tick: u32,
    /// A tick is on its way.
    pub ticking: bool,
    /// The delay the app was last told (`Command::SetLiveDelay`). Kept by [`Captions::clear`].
    pub delaying: bool,
    /// How long after its start a word comes on; `None`: [`SHOW_AFTER_MS`]. Set by the app at
    /// startup, kept by [`Captions::clear`].
    pub show_after: Option<u64>,
}

impl Captions {
    /// How long after its start a word comes on, in milliseconds.
    pub fn show_after_ms(&self) -> u64 {
        self.show_after.unwrap_or(SHOW_AFTER_MS)
    }

    /// The text on screen, one entry per line.
    pub fn lines(&self) -> Vec<&str> {
        self.shown.iter().map(String::as_str).collect()
    }

    /// Forgets the words, the lines and the clock, and orphans a tick on its way.
    pub(crate) fn clear(&mut self) {
        self.queue.clear();
        self.shown.clear();
        self.last_word_ms = None;
        self.clock_ms = None;
        self.buffer_ms = 0;
        self.held = false;
        self.held_ms = 0;
        if self.ticking {
            self.ticking = false;
            self.last_tick = self.last_tick.wrapping_add(1);
        }
    }

    /// Puts `word` in its place by start time (captions come in order, so mostly at the end).
    pub(crate) fn enqueue(&mut self, word: CaptionWord) {
        let at = self.queue.partition_point(|w| w.start_ms <= word.start_ms);
        self.queue.insert(at, word);
        while self.queue.len() > MAX_QUEUED {
            self.queue.pop_front();
        }
    }

    /// Adds `word` to the screen: to the last line if it fits in [`LINE_CHARS`], else on a new
    /// line, the oldest scrolling off. A word longer than a line is cut.
    pub(crate) fn show(&mut self, word: &str) {
        for piece in split_lines(word, LINE_CHARS) {
            let fits = self
                .shown
                .last()
                .is_some_and(|line| line.chars().count() + 1 + piece.chars().count() <= LINE_CHARS);
            match self.shown.last_mut() {
                Some(line) if fits => {
                    line.push(' ');
                    line.push_str(&piece);
                }
                _ => self.shown.push(piece),
            }
            while self.shown.len() > MAX_LINES {
                self.shown.remove(0);
            }
        }
    }
}

/// Where the speech model for the chosen quality is (captions-plan.md, phase 4). The app sends it
/// as `Event::CaptionsModel` while captions are on.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum ModelState {
    /// Nothing is known to be wrong: the model is on disk, loading, or not asked for yet.
    #[default]
    Ready,
    /// Being downloaded, this many percent done.
    Downloading(u8),
    /// The download failed (no network, a bad file). A press on the row tries again.
    Failed,
}

/// What the player menu's *Captions* row says.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CaptionsRow {
    Off,
    On,
    /// Captions are on and the model is being downloaded (percent).
    Downloading(u8),
    /// Captions are on, but the model could not be downloaded.
    DownloadFailed,
    /// This build cannot make captions (no speech engine).
    Unavailable,
    NotInMultiview,
    /// Kick's streams have none yet.
    TwitchOnly,
    /// Sound only plays another rendition, whose clock the captions do not share.
    NotWithSoundOnly,
}

impl CaptionsRow {
    /// A press on the row changes the setting (a failed download is retried instead).
    pub fn can_toggle(self) -> bool {
        matches!(
            self,
            CaptionsRow::Off
                | CaptionsRow::On
                | CaptionsRow::Downloading(_)
                | CaptionsRow::DownloadFailed
        )
    }
}

impl AppState {
    /// What the player menu's *Captions* row shows for the stream on screen.
    pub fn captions_row(&self) -> CaptionsRow {
        let twitch = self
            .now_playing
            .as_ref()
            .is_none_or(|p| p.channel.platform == Platform::Twitch);
        if self.multiview.is_some() {
            CaptionsRow::NotInMultiview
        } else if !twitch {
            CaptionsRow::TwitchOnly
        } else if self.audio_only {
            CaptionsRow::NotWithSoundOnly
        } else if self.captions_unavailable {
            CaptionsRow::Unavailable
        } else if !self.settings.captions {
            CaptionsRow::Off
        } else {
            match self.captions_models {
                ModelState::Ready => CaptionsRow::On,
                ModelState::Downloading(percent) => CaptionsRow::Downloading(percent),
                ModelState::Failed => CaptionsRow::DownloadFailed,
            }
        }
    }

    /// The qualities the *Caption quality* row cycles through: *Best* only where a GPU build
    /// can keep up with it (`captions_gpu`).
    pub fn captions_qualities(&self) -> &'static [CaptionsQuality] {
        if self.captions_gpu {
            &CaptionsQuality::ALL
        } else {
            &[CaptionsQuality::Fast, CaptionsQuality::Standard]
        }
    }

    /// The quality captions actually use: the saved one, or *Standard* when it is *Best* and this
    /// build has no GPU.
    pub fn captions_quality(&self) -> CaptionsQuality {
        let saved = self.settings.captions_quality;
        let offered = self.captions_qualities();
        if offered.contains(&saved) {
            saved
        } else if offered.contains(&DEFAULT_CAPTIONS_QUALITY) {
            DEFAULT_CAPTIONS_QUALITY
        } else {
            offered[0]
        }
    }

    /// Captions are showing, or waiting to: on, and a Twitch live stream or a video or clip with a
    /// picture is on the Player screen, not (yet) over, offline or failed. Anything else keeps the
    /// queue empty. (The end card of a video is over: it shows no captions.)
    pub fn captions_live(&self) -> bool {
        let on_screen = match &self.video {
            Some(v) => v.end_card.is_none(),
            None => self.now_playing.is_some(),
        };
        self.screen == Screen::Player
            && on_screen
            && self.captions_row() == CaptionsRow::On
            && !matches!(self.playback, Playback::Offline | Playback::Failed(_))
    }

    /// How far the stream clock moves on one tick ([`TICK_MS`]): a video moves at its speed.
    pub fn caption_tick_step_ms(&self) -> u64 {
        self.video
            .as_ref()
            .map_or(TICK_MS, |v| (TICK_MS * u64::from(v.speed) + 500) / 1_000)
    }

    /// The video's picture is held still while the playhead is scrubbed: its clock stands still.
    pub fn video_scrubbing(&self) -> bool {
        self.video
            .as_ref()
            .is_some_and(|v| v.seeking.scrub().is_some())
    }

    /// The single player's live stream is held back for captions: the delay is on and captions
    /// run on it. Unlike `captions_live`, the screen does not matter: the channel page over the
    /// stream leaves it as it is, so coming back neither jumps nor waits.
    pub fn caption_delay_wanted(&self) -> bool {
        self.settings.caption_delay
            && self.video.is_none()
            && self.now_playing.is_some()
            && self.captions_row() == CaptionsRow::On
    }

    /// The stream clock a chat line arriving now is shown at, while chat is held back with the
    /// picture: the clock plus how far the delay holds the picture behind where it would be (its
    /// buffer beyond a normal one, and while it is held the time held as well: the buffer grows
    /// by that much). `None`: chat shows at once (no delay, or no clock yet).
    pub fn chat_due_ms(&self) -> Option<u64> {
        // A video's chat has its own timing (the replay), and no delay.
        if !self.settings.caption_delay || self.video.is_some() || !self.captions_live() {
            return None;
        }
        let buffer = u64::from(self.captions.buffer_ms) + self.captions.held_ms;
        let behind = buffer.saturating_sub(u64::from(NORMAL_BUFFER_MS));
        self.captions.clock_ms.map(|now| now + behind)
    }
}

/// `text` cut at spaces into lines of at most `max` characters. A word longer than that is cut
/// where it must be. Whitespace runs count as one space.
pub fn split_lines(text: &str, max: usize) -> Vec<String> {
    let mut lines: Vec<String> = Vec::new();
    let mut line = String::new();
    let mut len = 0;
    for word in text.split_whitespace() {
        let mut word: &str = word;
        loop {
            let w = word.chars().count();
            let sep = usize::from(len > 0);
            if len + sep + w <= max {
                if sep > 0 {
                    line.push(' ');
                }
                line.push_str(word);
                len += sep + w;
                break;
            }
            if len > 0 {
                lines.push(std::mem::take(&mut line));
                len = 0;
                continue;
            }
            // A word longer than a line, on a line of its own: cut it.
            let cut = word.char_indices().nth(max).map_or(word.len(), |(i, _)| i);
            lines.push(word[..cut].to_owned());
            word = &word[cut..];
        }
    }
    if len > 0 {
        lines.push(line);
    }
    lines
}

/// The words of a caption spoken from `start_ms` to `end_ms` with no times of their own, each
/// starting at its share of the time (by characters).
pub fn words_of(start_ms: u64, end_ms: u64, text: &str) -> Vec<CaptionWord> {
    let span = end_ms.saturating_sub(start_ms);
    let words: Vec<&str> = text.split_whitespace().collect();
    let total: u64 = words.iter().map(|w| w.chars().count() as u64).sum();
    let mut before = 0u64;
    words
        .into_iter()
        .map(|w| {
            let at = start_ms + span * before / total.max(1);
            before += w.chars().count() as u64;
            CaptionWord::new(at, w)
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn short_text_is_one_line() {
        assert_eq!(split_lines("  hello   there ", 44), ["hello there"]);
        assert!(split_lines("   ", 44).is_empty());
    }

    #[test]
    fn long_text_is_cut_at_spaces() {
        let lines = split_lines("the quick brown fox jumps over the lazy dog", 20);
        assert_eq!(lines, ["the quick brown fox", "jumps over the lazy", "dog"]);
        assert!(lines.iter().all(|l| l.chars().count() <= 20));
    }

    #[test]
    fn a_word_longer_than_a_line_is_cut() {
        assert_eq!(
            split_lines("abcdefghij kl", 4),
            ["abcd", "efgh", "ij", "kl"]
        );
        // Multi-byte letters are cut between characters, not inside one.
        let e = "\u{e9}";
        assert_eq!(
            split_lines(&e.repeat(5), 2),
            [e.repeat(2), e.repeat(2), e.to_owned()]
        );
    }

    #[test]
    fn words_without_times_share_the_caption_by_characters() {
        let words = words_of(10_000, 18_000, "aaaa bbbb cccccccc");
        let starts: Vec<u64> = words.iter().map(|w| w.start_ms).collect();
        assert_eq!(starts, [10_000, 12_000, 14_000]);
        // An end before the start puts them all at the start.
        assert!(words_of(5_000, 4_000, "x y")
            .iter()
            .all(|w| w.start_ms == 5_000));
    }

    #[test]
    fn words_fill_a_line_then_roll_up() {
        let mut c = Captions::default();
        for w in ["one", "two"] {
            c.show(w);
        }
        assert_eq!(c.lines(), ["one two"]);
        // 40 + 1 + 3 = 44 fits; one more character does not.
        let mut c = Captions::default();
        c.show(&"a".repeat(40));
        c.show("bbb");
        assert_eq!(c.lines(), [format!("{} bbb", "a".repeat(40))]);
        c.show("c");
        c.show("d");
        assert_eq!(c.lines().len(), 2);
        assert_eq!(c.lines()[1], "c d");
        // A third line pushes the oldest off.
        c.show(&"e".repeat(44));
        assert_eq!(c.lines(), ["c d".to_owned(), "e".repeat(44)]);
        // A word longer than a line is cut over lines.
        c.show(&"f".repeat(50));
        assert_eq!(c.lines(), ["f".repeat(44), "f".repeat(6)]);
    }

    #[test]
    fn the_queue_stays_in_order_and_capped() {
        let mut c = Captions::default();
        for start in [3_000u64, 1_000, 2_000] {
            c.enqueue(CaptionWord::new(start, start.to_string()));
        }
        let order: Vec<u64> = c.queue.iter().map(|w| w.start_ms).collect();
        assert_eq!(order, [1_000, 2_000, 3_000]);
        for i in 0..400u64 {
            c.enqueue(CaptionWord::new(10_000 + i, ""));
        }
        assert_eq!(c.queue.len(), MAX_QUEUED);
        assert_eq!(c.queue.back().map(|w| w.start_ms), Some(10_399));
    }
}
