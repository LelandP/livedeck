//! Chat replay (vod-plan.md, phase 6): the chat of a past broadcast, shown as the video plays.
//!
//! Twitch gives a window of about 60 lines around an offset and nothing else (paging by cursor
//! fails its integrity check), so the lines are fetched by offset: one window at the playhead, the
//! next while less than [`LOOKAHEAD_MS`] of lines are left, and a new one from the new spot after
//! any jump. The shown lines live in `AppState::chat`, the buffer live chat uses, so the panel,
//! the emote images and the diff all work as they do for live. What has been fetched but is not
//! due yet waits here, in [`ReplayChat`], inside the `VideoSession`: a new video, or leaving it,
//! drops everything, and a late answer finds nothing to land on.
//!
//! The reducer has no clock. Lines come due as the health ticks (`pos_ms`) pass their offsets.

use std::collections::VecDeque;

use crate::domain::ChatMessage;

/// The next window is asked for while the fetched lines reach less than this far ahead of the
/// playhead.
pub const LOOKAHEAD_MS: u64 = 30_000;

/// One line of a chat replay and when it was said.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReplayLine {
    /// Whole seconds from the start of the video.
    pub offset_s: u32,
    /// `message.id` is Twitch's id of the comment, which tells the lines of two overlapping
    /// windows apart.
    pub message: ChatMessage,
}

impl ReplayLine {
    pub fn at_ms(&self) -> u64 {
        u64::from(self.offset_s) * 1_000
    }
}

/// What the video player keeps for the chat replay of its video.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct ReplayChat {
    /// Fetched and not due yet, oldest first.
    pub(crate) pending: VecDeque<ReplayLine>,
    /// The offset of the request on its way. Only its answer counts.
    pub(crate) in_flight: Option<u32>,
    /// The second of the newest line taken in, and the ids of the lines at that second. The next
    /// window starts before its offset, so it repeats what is already here: lines at or before
    /// this are known, apart from others in the same second that did not fit the last window.
    pub(crate) last_s: Option<u32>,
    pub(crate) last_ids: Vec<String>,
    /// Nothing more to fetch until the next jump: the end of the chat, or a request that failed
    /// after lines had come.
    pub(crate) exhausted: bool,
    /// Lines have come for this video, at some point. A video that never gave any has no
    /// replay (an empty window after a jump near the end is just the end of the chat).
    pub(crate) had_lines: bool,
    /// Twitch had no replay to give for this video, and the viewer was told. Y does nothing more.
    pub(crate) unavailable: bool,
}

impl ReplayChat {
    /// A window is being fetched.
    pub fn loading(&self) -> bool {
        self.in_flight.is_some()
    }

    /// The viewer was told there is no replay for this video.
    pub fn unavailable(&self) -> bool {
        self.unavailable
    }

    /// Lines fetched and waiting for their time.
    pub fn waiting(&self) -> usize {
        self.pending.len()
    }

    /// Forgets the lines and the progress of the windows. What the video is known to have (some
    /// replay, or none) stays.
    pub(crate) fn clear(&mut self) {
        *self = Self {
            had_lines: self.had_lines,
            unavailable: self.unavailable,
            ..Self::default()
        };
    }
}
