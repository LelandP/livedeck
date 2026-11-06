//! The on-screen keyboard: a fixed grid of keys driven by the D-pad. Search uses it, and chat
//! sending can later.
//!
//! ```text
//! a b c d e f g h i j
//! k l m n o p q r s t
//! u v w x y z 0 1 2 3
//! 4 5 6 7 8 9 _ - [delete]
//! [   space   ] [ clear ] [ done ]
//! ```
//!
//! Every row is [`COLUMNS`] columns wide and a key covers one or more of them. The cursor keeps the
//! column it is in, so going up and down a column always comes back to the same keys.

/// Columns of every row.
pub const COLUMNS: u8 = 10;

/// What a key does.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Key {
    /// Types this character.
    Char(char),
    /// Deletes the last character.
    Backspace,
    Space,
    /// Empties the text.
    Clear,
    /// Leaves the keyboard (Search: to the results).
    Done,
}

impl Key {
    /// The text on the key. Words, not symbols: the app's font has no backspace glyph.
    pub fn label(self) -> String {
        match self {
            Key::Char(c) => c.to_string(),
            Key::Backspace => "delete".to_owned(),
            Key::Space => "space".to_owned(),
            Key::Clear => "clear".to_owned(),
            Key::Done => "done".to_owned(),
        }
    }
}

/// One key and the columns it covers.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct KeySlot {
    pub key: Key,
    pub row: u8,
    /// First column.
    pub col: u8,
    /// Columns covered.
    pub span: u8,
}

const fn c(ch: char) -> (Key, u8) {
    (Key::Char(ch), 1)
}

/// The rows, top to bottom, as (key, columns) pairs. Each adds up to [`COLUMNS`].
const ROWS: [&[(Key, u8)]; 5] = [
    &[
        c('a'),
        c('b'),
        c('c'),
        c('d'),
        c('e'),
        c('f'),
        c('g'),
        c('h'),
        c('i'),
        c('j'),
    ],
    &[
        c('k'),
        c('l'),
        c('m'),
        c('n'),
        c('o'),
        c('p'),
        c('q'),
        c('r'),
        c('s'),
        c('t'),
    ],
    &[
        c('u'),
        c('v'),
        c('w'),
        c('x'),
        c('y'),
        c('z'),
        c('0'),
        c('1'),
        c('2'),
        c('3'),
    ],
    &[
        c('4'),
        c('5'),
        c('6'),
        c('7'),
        c('8'),
        c('9'),
        c('_'),
        c('-'),
        (Key::Backspace, 2),
    ],
    &[(Key::Space, 4), (Key::Clear, 3), (Key::Done, 3)],
];

/// Every key, row by row, left to right.
pub fn layout() -> Vec<KeySlot> {
    let mut out = Vec::new();
    for (row, keys) in ROWS.iter().enumerate() {
        let mut col = 0;
        for &(key, span) in keys.iter() {
            out.push(KeySlot {
                key,
                row: row as u8,
                col,
                span,
            });
            col += span;
        }
    }
    out
}

/// The key under the cursor and where it sits in [`layout`].
fn find(row: u8, col: u8) -> (usize, KeySlot) {
    layout()
        .into_iter()
        .enumerate()
        .find(|(_, k)| k.row == row && (k.col..k.col + k.span).contains(&col))
        .expect("every column of every row has a key")
}

/// Where the cursor is. Starts on `a`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct KeyboardState {
    row: u8,
    /// The column the cursor stands in. A wide key covers several, and the cursor keeps its
    /// column on it, so Up and Down lead back to where they came from.
    col: u8,
}

impl KeyboardState {
    /// The key under the cursor.
    pub fn key(&self) -> Key {
        find(self.row, self.col).1.key
    }

    /// The focused key's index in [`layout`].
    pub fn index(&self) -> usize {
        find(self.row, self.col).0
    }

    /// Moves up a row. False on the top row (nothing moved).
    pub fn up(&mut self) -> bool {
        if self.row == 0 {
            return false;
        }
        self.row -= 1;
        true
    }

    /// Moves down a row. False on the bottom row (nothing moved).
    pub fn down(&mut self) -> bool {
        if self.row as usize + 1 >= ROWS.len() {
            return false;
        }
        self.row += 1;
        true
    }

    /// Moves to the key on the left. False at the left edge (nothing moved).
    pub fn left(&mut self) -> bool {
        let here = find(self.row, self.col).1;
        if here.col == 0 {
            return false;
        }
        self.col = find(self.row, here.col - 1).1.col;
        true
    }

    /// Moves to the key on the right. False at the right edge (nothing moved).
    pub fn right(&mut self) -> bool {
        let here = find(self.row, self.col).1;
        let next = here.col + here.span;
        if next >= COLUMNS {
            return false;
        }
        self.col = next;
        true
    }
}

/// Longest text the keyboard writes, in characters.
pub const MAX_TEXT: usize = 40;

/// Characters the keyboard writes: those on its keys. Twitch logins are `[a-z0-9_]`, and search
/// ignores case and most punctuation, so nothing else is needed.
pub fn accepts(c: char) -> bool {
    c.is_ascii_lowercase() || c.is_ascii_digit() || matches!(c, '_' | '-' | ' ')
}

/// Adds `c` to `text`. False if it was not added: not a key's character, the text is full, or a
/// space at the start or after another space.
pub fn type_char(text: &mut String, c: char) -> bool {
    if !accepts(c) || text.chars().count() >= MAX_TEXT {
        return false;
    }
    if c == ' ' && (text.is_empty() || text.ends_with(' ')) {
        return false;
    }
    text.push(c);
    true
}

/// Removes the last character. False if there was none.
pub fn backspace(text: &mut String) -> bool {
    text.pop().is_some()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_row_fills_the_width() {
        for keys in ROWS {
            let width: u8 = keys.iter().map(|(_, span)| span).sum();
            assert_eq!(width, COLUMNS);
        }
        assert_eq!(layout().len(), 10 * 3 + 9 + 3);
    }

    #[test]
    fn starts_on_a_and_moves_around_the_grid() {
        let mut k = KeyboardState::default();
        assert_eq!(k.key(), Key::Char('a'));
        assert!(k.right());
        assert_eq!(k.key(), Key::Char('b'));
        assert!(k.down() && k.down());
        assert_eq!(k.key(), Key::Char('v'));
        assert!(k.down());
        assert_eq!(k.key(), Key::Char('5'));
        assert!(k.left());
        assert_eq!(k.key(), Key::Char('4'));
        assert_eq!(k.index(), 30);
    }

    #[test]
    fn the_edges_stop_the_cursor() {
        let mut k = KeyboardState::default();
        assert!(!k.up(), "top row");
        assert!(!k.left(), "left edge");
        for _ in 0..9 {
            assert!(k.right());
        }
        assert_eq!(k.key(), Key::Char('j'));
        assert!(!k.right(), "right edge");
        for _ in 0..4 {
            assert!(k.down());
        }
        assert_eq!(k.key(), Key::Done);
        assert!(!k.down(), "bottom row");
    }

    #[test]
    fn wide_keys_keep_the_column_for_up_and_down() {
        let mut k = KeyboardState::default();
        // Down from `c` to the space bar and back up to `c`.
        k.right();
        k.right();
        for _ in 0..4 {
            k.down();
        }
        assert_eq!(k.key(), Key::Space);
        for _ in 0..4 {
            k.up();
        }
        assert_eq!(k.key(), Key::Char('c'));

        // Along the bottom row and up onto backspace, which covers the last two columns.
        for _ in 0..4 {
            k.down();
        }
        assert!(k.right() && k.right());
        assert_eq!(k.key(), Key::Done);
        assert!(k.up());
        assert_eq!(k.key(), Key::Char('-'));
        assert!(k.right());
        assert_eq!(k.key(), Key::Backspace);
        assert!(!k.right());
        assert!(k.left());
        assert_eq!(k.key(), Key::Char('-'));
    }

    #[test]
    fn typing_backspace_and_limits() {
        let mut text = String::new();
        assert!(!type_char(&mut text, ' '), "no leading space");
        assert!(type_char(&mut text, 'a'));
        assert!(type_char(&mut text, ' '));
        assert!(!type_char(&mut text, ' '), "no double space");
        assert!(type_char(&mut text, '_') && type_char(&mut text, '9'));
        assert!(!type_char(&mut text, 'A'), "only the keys' characters");
        assert!(!type_char(&mut text, '!'));
        assert_eq!(text, "a _9");
        assert!(backspace(&mut text));
        assert_eq!(text, "a _");
        text.clear();
        assert!(!backspace(&mut text));
        for _ in 0..MAX_TEXT {
            assert!(type_char(&mut text, 'x'));
        }
        assert!(!type_char(&mut text, 'x'), "full");
    }

    #[test]
    fn labels() {
        assert_eq!(Key::Char('q').label(), "q");
        assert_eq!(Key::Backspace.label(), "delete");
        assert_eq!(Key::Done.label(), "done");
    }
}
