//! Chat lines kept for the watched channel: the bounded ring buffer and how moderation edits it.

use std::collections::VecDeque;
use std::ops::Index;

use crate::domain::{ChatMessage, Moderation};

/// Chat lines kept in state. Older ones fall off the front.
pub const CHAT_CAPACITY: usize = 200;

/// The most recent chat lines of the channel on screen, oldest first. Holds at most
/// [`CHAT_CAPACITY`] lines (or the capacity it was built with): pushing past that drops the oldest.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ChatBuffer {
    lines: VecDeque<ChatMessage>,
    capacity: usize,
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
        }
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

    pub fn clear(&mut self) {
        self.lines.clear();
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
    pub fn moderate(&mut self, action: &Moderation) {
        match action {
            Moderation::ClearAll => self.lines.clear(),
            Moderation::ClearUser(login) => {
                self.lines.retain(|m| !m.login.eq_ignore_ascii_case(login))
            }
            // Notices have an empty id and must never match.
            Moderation::Delete(id) => self.lines.retain(|m| m.id.is_empty() || m.id != *id),
        }
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
