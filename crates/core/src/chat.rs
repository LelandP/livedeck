//! Chat lines kept for the watched channel: the bounded ring buffer and how moderation edits it.

use std::collections::VecDeque;
use std::ops::Index;

use crate::domain::{ChatMessage, Moderation};

/// Chat lines kept in state. Older ones fall off the front.
pub const CHAT_CAPACITY: usize = 200;

/// Chat lines held back at most, while chat runs behind the picture (a long pause). More drops
/// the oldest.
pub const HELD_CAPACITY: usize = 500;

/// The most recent chat lines of the channel on screen, oldest first. Holds at most
/// [`CHAT_CAPACITY`] lines (or the capacity it was built with): pushing past that drops the oldest.
///
/// Lines can also be held back, each until a moment on the stream clock (captions' delay keeps
/// chat in step with the delayed picture): [`ChatBuffer::hold`], then [`ChatBuffer::release`]
/// as the clock moves on. Clearing and moderation reach the held lines too.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ChatBuffer {
    lines: VecDeque<ChatMessage>,
    capacity: usize,
    /// Held lines in arrival order, each with the stream time it is due at.
    held: VecDeque<(u64, ChatMessage)>,
}

impl Default for ChatBuffer {
    fn default() -> Self {
        Self::new(CHAT_CAPACITY)
    }
}

impl From<Vec<ChatMessage>> for ChatBuffer {
    fn from(lines: Vec<ChatMessage>) -> Self {
        let mut buffer = Self::default();
        buffer.extend(lines);
        buffer
    }
}

impl Index<usize> for ChatBuffer {
    type Output = ChatMessage;

    fn index(&self, index: usize) -> &ChatMessage {
        &self.lines[index]
    }
}

impl ChatBuffer {
    /// An empty buffer that keeps `capacity` lines. A capacity of 0 is raised to 1.
    pub fn new(capacity: usize) -> Self {
        let capacity = capacity.max(1);
        Self {
            lines: VecDeque::with_capacity(capacity),
            capacity,
            held: VecDeque::new(),
        }
    }

    /// Holds `line` back until the stream clock reaches `due_ms`. Lines come out in the order
    /// they were held, so a line never overtakes an older one.
    pub fn hold(&mut self, due_ms: u64, line: ChatMessage) {
        if self.held.len() == HELD_CAPACITY {
            self.held.pop_front();
        }
        self.held.push_back((due_ms, line));
    }

    /// Shows the held lines whose time has come by `now_ms`, oldest first. Returns how many.
    pub fn release(&mut self, now_ms: u64) -> usize {
        let mut n = 0;
        while self.held.front().is_some_and(|(due, _)| *due <= now_ms) {
            if let Some((_, line)) = self.held.pop_front() {
                self.push(line);
                n += 1;
            }
        }
        n
    }

    /// Shows every held line at once (chat is no longer held back).
    pub fn release_all(&mut self) {
        while let Some((_, line)) = self.held.pop_front() {
            self.push(line);
        }
    }

    /// Lines held back, waiting for their time.
    pub fn held_len(&self) -> usize {
        self.held.len()
    }

    pub fn capacity(&self) -> usize {
        self.capacity
    }

    pub fn len(&self) -> usize {
        self.lines.len()
    }

    pub fn is_empty(&self) -> bool {
        self.lines.is_empty()
    }

    /// The lines, oldest first.
    pub fn iter(&self) -> impl DoubleEndedIterator<Item = &ChatMessage> + ExactSizeIterator {
        self.lines.iter()
    }

    /// Empties it, the held lines too.
    pub fn clear(&mut self) {
        self.lines.clear();
        self.held.clear();
    }

    /// Adds a line as the newest, dropping the oldest if the buffer is full.
    pub fn push(&mut self, line: ChatMessage) {
        if self.lines.len() == self.capacity {
            self.lines.pop_front();
        }
        self.lines.push_back(line);
    }

    pub fn extend(&mut self, lines: impl IntoIterator<Item = ChatMessage>) {
        for line in lines {
            self.push(line);
        }
    }

    /// Applies a moderator action: a cleared chat, a timed-out or banned user, a deleted line.
    /// Held lines are edited the same way.
    pub fn moderate(&mut self, action: &Moderation) {
        let keep = |m: &ChatMessage| match action {
            Moderation::ClearAll => false,
            Moderation::ClearUser(login) => !m.login.eq_ignore_ascii_case(login),
            // Notices have an empty id and must never match.
            Moderation::Delete(id) => m.id.is_empty() || m.id != *id,
        };
        self.lines.retain(|m| keep(m));
        self.held.retain(|(_, m)| keep(m));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn line(n: usize) -> ChatMessage {
        ChatMessage {
            id: format!("id{n}"),
            login: format!("user{}", n % 3),
            ..ChatMessage::plain(&format!("User{n}"), &format!("line {n}"))
        }
    }

    #[test]
    fn keeps_the_newest_lines_in_order() {
        let mut b = ChatBuffer::new(3);
        b.extend((0..5).map(line));
        assert_eq!(b.len(), 3);
        let ids: Vec<_> = b.iter().map(|m| m.id.as_str()).collect();
        assert_eq!(ids, ["id2", "id3", "id4"]);
        assert_eq!(b[0].id, "id2");
    }

    #[test]
    fn the_default_holds_the_documented_capacity() {
        let mut b = ChatBuffer::default();
        b.extend((0..CHAT_CAPACITY + 25).map(line));
        assert_eq!(b.len(), CHAT_CAPACITY);
        assert_eq!(b[0].id, "id25");
        assert_eq!(b.capacity(), CHAT_CAPACITY);
    }

    #[test]
    fn a_capacity_of_zero_still_holds_one_line() {
        let mut b = ChatBuffer::new(0);
        b.extend((0..4).map(line));
        assert_eq!(b.len(), 1);
        assert_eq!(b[0].id, "id3");
    }

    #[test]
    fn clearing_empties_it_but_keeps_the_capacity() {
        let mut b = ChatBuffer::new(4);
        b.extend((0..4).map(line));
        b.clear();
        assert!(b.is_empty());
        b.extend((0..9).map(line));
        assert_eq!(b.len(), 4);
    }

    #[test]
    fn converting_from_a_vec_applies_the_cap() {
        let b = ChatBuffer::from((0..CHAT_CAPACITY + 1).map(line).collect::<Vec<_>>());
        assert_eq!(b.len(), CHAT_CAPACITY);
        assert_eq!(b[0].id, "id1");
    }

    #[test]
    fn held_lines_come_out_in_order_when_their_time_comes() {
        let mut b = ChatBuffer::new(10);
        b.hold(5_000, line(0));
        b.hold(7_000, line(1));
        // A later line due earlier (the delay shrank) waits for the one before it.
        b.hold(6_000, line(2));
        assert!(b.is_empty());
        assert_eq!(b.held_len(), 3);
        assert_eq!(b.release(4_999), 0);
        assert_eq!(b.release(6_500), 1);
        assert_eq!(b[0].id, "id0");
        assert_eq!(b.release(7_000), 2);
        let ids: Vec<_> = b.iter().map(|m| m.id.as_str()).collect();
        assert_eq!(ids, ["id0", "id1", "id2"]);
        assert_eq!(b.held_len(), 0);
    }

    #[test]
    fn held_lines_are_capped_moderated_cleared_and_released_at_once() {
        let mut b = ChatBuffer::new(10);
        for n in 0..HELD_CAPACITY + 2 {
            b.hold(1_000, line(n));
        }
        assert_eq!(b.held_len(), HELD_CAPACITY);
        // user0 wrote every third line.
        b.moderate(&Moderation::ClearUser("user0".into()));
        b.moderate(&Moderation::Delete(format!("id{}", HELD_CAPACITY + 1)));
        b.release_all();
        assert_eq!(b.held_len(), 0);
        assert!(b.iter().all(|m| m.login != "user0"));
        assert!(b.iter().all(|m| m.id != format!("id{}", HELD_CAPACITY + 1)));
        b.hold(1, line(1));
        b.clear();
        assert_eq!(b.held_len(), 0);
        b.hold(1, line(1));
        b.moderate(&Moderation::ClearAll);
        assert_eq!(b.held_len(), 0);
    }

    #[test]
    fn moderation_clears_all_one_user_or_one_line() {
        let mut b = ChatBuffer::new(10);
        b.extend((0..6).map(line));
        b.push(ChatMessage::notice("slow mode"));

        // user0 wrote lines 0 and 3. Logins match without regard to case.
        let mut after_user = b.clone();
        after_user.moderate(&Moderation::ClearUser("USER0".into()));
        assert_eq!(after_user.len(), 5);
        assert!(after_user.iter().all(|m| m.login != "user0"));

        let mut after_delete = b.clone();
        after_delete.moderate(&Moderation::Delete("id4".into()));
        assert_eq!(after_delete.len(), 6);
        assert!(after_delete.iter().all(|m| m.id != "id4"));

        // The notice has no id: deleting "" must not remove it.
        let mut after_empty = b.clone();
        after_empty.moderate(&Moderation::Delete(String::new()));
        assert_eq!(after_empty.len(), 7);

        b.moderate(&Moderation::ClearAll);
        assert!(b.is_empty());
    }
}
