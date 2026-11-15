use std::collections::VecDeque;

use crate::secs;

/// One thing for the worker to do.
#[derive(Debug, Clone, PartialEq)]
pub enum QueueItem {
    Audio {
        start_s: f64,
        samples: Vec<f32>,
    },
    /// Drop the chunker's buffered audio.
    Reset,
}

/// A bounded queue of audio. When more than `cap_s` seconds are waiting, the oldest audio goes.
/// The newest push is always kept, even if it alone is over the cap. Audio that the playhead has
/// passed ([`AudioQueue::set_floor_s`]) is skipped when its turn comes.
#[derive(Debug)]
pub struct AudioQueue {
    items: VecDeque<QueueItem>,
    queued_s: f64,
    cap_s: f64,
    dropped_s: f64,
    /// Audio that ends at or before this is of no use any more.
    floor_s: f64,
}

impl AudioQueue {
    pub fn new(cap_s: f64) -> Self {
        Self {
            items: VecDeque::new(),
            queued_s: 0.0,
            cap_s,
            dropped_s: 0.0,
            floor_s: f64::NEG_INFINITY,
        }
    }

    /// A new cap, for the next push.
    pub fn set_cap_s(&mut self, cap_s: f64) {
        self.cap_s = cap_s;
    }

    /// Audio that ends at or before `floor_s` is skipped, not given to the thread: it would be
    /// made after the picture has passed it. `f64::NEG_INFINITY` takes the limit away.
    pub fn set_floor_s(&mut self, floor_s: f64) {
        self.floor_s = floor_s;
    }

    pub fn push_audio(&mut self, start_s: f64, samples: Vec<f32>) {
        if samples.is_empty() {
            return;
        }
        self.queued_s += secs(samples.len());
        self.items.push_back(QueueItem::Audio { start_s, samples });
        while self.queued_s > self.cap_s {
            // The oldest audio goes. A reset marker is never dropped (the chunker must see it), and
            // the newest audio is kept even over the cap.
            let is_audio = |i: &QueueItem| matches!(i, QueueItem::Audio { .. });
            let Some(oldest) = self.items.iter().position(is_audio) else {
                break;
            };
            if self.items.iter().rposition(is_audio) == Some(oldest) {
                break;
            }
            if let Some(QueueItem::Audio { samples, .. }) = self.items.remove(oldest) {
                let s = secs(samples.len());
                self.queued_s -= s;
                self.dropped_s += s;
            }
        }
    }

    /// Throw everything waiting away and queue a [`QueueItem::Reset`].
    pub fn reset(&mut self) {
        self.items.clear();
        self.queued_s = 0.0;
        self.items.push_back(QueueItem::Reset);
    }

    pub fn pop(&mut self) -> Option<QueueItem> {
        loop {
            let item = self.items.pop_front()?;
            let QueueItem::Audio { start_s, samples } = &item else {
                return Some(item);
            };
            let length_s = secs(samples.len());
            self.queued_s = (self.queued_s - length_s).max(0.0);
            if start_s + length_s <= self.floor_s {
                self.dropped_s += length_s;
                continue;
            }
            return Some(item);
        }
    }

    pub fn is_empty(&self) -> bool {
        self.items.is_empty()
    }

    /// Seconds of audio waiting.
    pub fn queued_s(&self) -> f64 {
        self.queued_s
    }

    /// Seconds of audio dropped for being too old, since the queue was made.
    pub fn dropped_s(&self) -> f64 {
        self.dropped_s
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn half_second() -> Vec<f32> {
        vec![0.0; 8000]
    }

    fn start_of(item: Option<QueueItem>) -> f64 {
        match item {
            Some(QueueItem::Audio { start_s, .. }) => start_s,
            other => panic!("expected audio, got {other:?}"),
        }
    }

    #[test]
    fn keeps_order_under_the_cap() {
        let mut q = AudioQueue::new(10.0);
        q.push_audio(0.0, half_second());
        q.push_audio(0.5, half_second());
        assert_eq!(start_of(q.pop()), 0.0);
        assert_eq!(start_of(q.pop()), 0.5);
        assert!(q.pop().is_none());
        assert_eq!(q.dropped_s(), 0.0);
    }

    #[test]
    fn drops_the_oldest_over_the_cap() {
        let mut q = AudioQueue::new(2.0);
        for i in 0..8 {
            q.push_audio(f64::from(i) * 0.5, half_second());
        }
        assert!((q.queued_s() - 2.0).abs() < 1e-9);
        assert!((q.dropped_s() - 2.0).abs() < 1e-9);
        // The four newest are left.
        assert_eq!(start_of(q.pop()), 2.0);
    }

    #[test]
    fn never_drops_the_newest_even_over_the_cap() {
        let mut q = AudioQueue::new(0.1);
        q.push_audio(5.0, half_second());
        assert_eq!(start_of(q.pop()), 5.0);
    }

    #[test]
    fn reset_clears_and_queues_a_marker() {
        let mut q = AudioQueue::new(10.0);
        q.push_audio(0.0, half_second());
        q.reset();
        assert_eq!(q.pop(), Some(QueueItem::Reset));
        assert!(q.is_empty());
        assert_eq!(q.queued_s(), 0.0);
    }

    #[test]
    fn audio_the_playhead_has_passed_is_skipped_when_its_turn_comes() {
        let mut q = AudioQueue::new(60.0);
        for i in 0..8 {
            q.push_audio(f64::from(i) * 0.5, half_second());
        }
        // The playhead is at 2.0 s: the first four halves end at or before it.
        q.set_floor_s(2.0);
        assert_eq!(start_of(q.pop()), 2.0);
        assert!((q.dropped_s() - 2.0).abs() < 1e-9);
        assert!((q.queued_s() - 1.5).abs() < 1e-9);
        // It moves on while they wait; the limit can be taken away again.
        q.set_floor_s(3.5);
        assert_eq!(start_of(q.pop()), 3.5);
        q.push_audio(0.0, half_second());
        q.set_floor_s(f64::NEG_INFINITY);
        assert_eq!(start_of(q.pop()), 0.0);
        // A marker is never skipped.
        q.set_floor_s(1e9);
        q.push_audio(1.0, half_second());
        q.reset();
        assert_eq!(q.pop(), Some(QueueItem::Reset));
    }

    #[test]
    fn a_reset_marker_survives_the_backlog_cap() {
        // A seek, then more audio than the cap before the thread takes the marker: the marker must
        // still come out first, or the chunker keeps the audio from before the seek.
        let mut q = AudioQueue::new(1.0);
        q.push_audio(0.0, half_second());
        q.reset();
        for i in 0..6 {
            q.push_audio(f64::from(i) * 0.5, half_second());
        }
        assert_eq!(q.pop(), Some(QueueItem::Reset));
        assert_eq!(start_of(q.pop()), 2.0);
        assert!((q.dropped_s() - 2.0).abs() < 1e-9, "{}", q.dropped_s());
    }

    #[test]
    fn empty_pushes_are_ignored() {
        let mut q = AudioQueue::new(1.0);
        q.push_audio(0.0, Vec::new());
        assert!(q.is_empty());
    }
}
