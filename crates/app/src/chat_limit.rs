//! The app's own limit on chat messages, in front of `Command::SendChat`. Twitch lets a normal
//! user send about 20 messages in 30 seconds and briefly locks out anyone who sends more. Quick
//! messages make that easy to hit, so the app stops at the same rate and says so.

use std::collections::VecDeque;
use std::time::{Duration, Instant};

/// Messages allowed in [`WINDOW`].
pub const LIMIT: usize = 20;
/// The window Twitch counts in.
pub const WINDOW: Duration = Duration::from_secs(30);

/// When the messages of the last [`WINDOW`] were sent, oldest first.
#[derive(Debug, Default)]
pub struct SendLimit {
    sent: VecDeque<Instant>,
}

impl SendLimit {
    /// Takes a slot for a message sent at `now`. False if [`LIMIT`] messages already went in the
    /// last [`WINDOW`]: the message must not be sent.
    pub fn try_take(&mut self, now: Instant) -> bool {
        while self
            .sent
            .front()
            .is_some_and(|t| now.saturating_duration_since(*t) >= WINDOW)
        {
            self.sent.pop_front();
        }
        if self.sent.len() >= LIMIT {
            return false;
        }
        self.sent.push_back(now);
        true
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn twenty_in_thirty_seconds_then_wait() {
        let start = Instant::now();
        let mut limit = SendLimit::default();
        for i in 0..LIMIT {
            assert!(limit.try_take(start + Duration::from_millis(i as u64 * 100)));
        }
        assert!(!limit.try_take(start + Duration::from_secs(5)), "the 21st");
        assert!(!limit.try_take(start + Duration::from_millis(29_999)));
        // The first slot frees up 30 s after it was taken, the next 100 ms later.
        assert!(limit.try_take(start + WINDOW));
        assert!(!limit.try_take(start + WINDOW));
        assert!(limit.try_take(start + WINDOW + Duration::from_millis(100)));
    }

    #[test]
    fn a_refused_message_takes_no_slot() {
        let start = Instant::now();
        let mut limit = SendLimit::default();
        for _ in 0..LIMIT {
            assert!(limit.try_take(start));
        }
        for _ in 0..50 {
            assert!(!limit.try_take(start + Duration::from_secs(1)));
        }
        assert!(limit.try_take(start + WINDOW));
    }
}
