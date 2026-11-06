//! The on-screen keyboards: fixed grids of keys driven by the D-pad. Search has one board, chat
//! has three pages (letters, digits and punctuation, emotes).
//!
//! ```text
//! Search                              Chat letters
//! a b c d e f g h i j                 a b c d e f g h i j
//! k l m n o p q r s t                 k l m n o p q r s t
//! u v w x y z 0 1 2 3                 u v w x y z . , ! ?
//! 4 5 6 7 8 9 _ - [delete]            [shift] ' " @ # : - [delete]
//! [   space   ] [ clear ] [ done ]    [123] [  space  ] [emotes] [send]
//! ```
//!
//! Every row of every board is [`COLUMNS`] columns wide, every board has the same number of rows,
//! and a key covers one or more columns. The cursor keeps the column it is in, so going up and
//! down a column always comes back to the same keys, and changing the chat page keeps it in place.

/// Columns of every row.
pub const COLUMNS: u8 = 10;

/// Which set of keys is on screen.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Board {
    /// The Search keyboard: lower-case letters, digits, `_` and `-`.
    #[default]
    Search,
    /// Chat, first page: letters (with shift) and common punctuation.
    Letters,
    /// Chat: digits and punctuation.
    Symbols,
    /// Chat: common emotes, as the text codes Twitch turns into pictures.
    Emotes,
}

impl Board {
    /// The chat pages, in the order the shoulder buttons step through them.
    pub const CHAT: [Board; 3] = [Board::Letters, Board::Symbols, Board::Emotes];

    /// The next (or, with `forward == false`, the previous) chat page, wrapping around. Search has
    /// one board and stays.
    pub fn cycled(self, forward: bool) -> Self {
        let Some(at) = Self::CHAT.iter().position(|b| *b == self) else {
            return self;
        };
        let len = Self::CHAT.len();
        Self::CHAT[if forward {
            (at + 1) % len
        } else {
            (at + len - 1) % len
        }]
    }
}

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
    /// Chat: the next letter in capitals, or, pressed again, all of them (see [`Shift`]).
    Shift,
    /// Chat: shows another page of keys.
    Page(Board),
    /// Chat: types an emote's code as a word of its own.
    Emote(&'static str),
    /// Chat: sends the message.
    Send,
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
            Key::Shift => "shift".to_owned(),
            Key::Page(Board::Letters) => "abc".to_owned(),
            Key::Page(Board::Symbols) => "123".to_owned(),
            Key::Page(Board::Emotes) => "emotes".to_owned(),
            Key::Page(Board::Search) => "search".to_owned(),
            Key::Emote(code) => code.to_owned(),
            Key::Send => "send".to_owned(),
        }
    }
}

/// The chat keyboard's shift key: off, on for the next letter, or locked on. Each press steps to
/// the next state, so pressing it twice locks it (a "double tap" without needing a clock).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Shift {
    #[default]
    Off,
    /// The next letter is a capital, then it turns off.
    Once,
    /// Every letter is a capital until shift is pressed again.
    Lock,
}

impl Shift {
    /// The state after one more press of the key.
    pub fn pressed(self) -> Self {
        match self {
            Shift::Off => Shift::Once,
            Shift::Once => Shift::Lock,
            Shift::Lock => Shift::Off,
        }
    }

    /// The state after a letter was typed: a one-letter shift is used up.
    pub fn after_letter(self) -> Self {
        match self {
            Shift::Once => Shift::Off,
            other => other,
        }
    }

    /// `c` as this state types it.
    pub fn apply(self, c: char) -> char {
        match self {
            Shift::Off => c,
            Shift::Once | Shift::Lock => c.to_ascii_uppercase(),
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

const fn e(code: &'static str) -> (Key, u8) {
    (Key::Emote(code), 2)
}

/// Rows of every board.
const ROW_COUNT: usize = 5;

type Rows = [&'static [(Key, u8)]; ROW_COUNT];

/// The Search board's rows, top to bottom, as (key, columns) pairs. Each adds up to [`COLUMNS`].
const ROWS: Rows = [
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

/// The chat pages share their bottom row's shape, so a page key pressed twice comes back.
const LETTERS: Rows = [
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
        c('.'),
        c(','),
        c('!'),
        c('?'),
    ],
    &[
        (Key::Shift, 2),
        c('\''),
        c('"'),
        c('@'),
        c('#'),
        c(':'),
        c('-'),
        (Key::Backspace, 2),
    ],
    &[
        (Key::Page(Board::Symbols), 2),
        (Key::Space, 4),
        (Key::Page(Board::Emotes), 2),
        (Key::Send, 2),
    ],
];

const SYMBOLS: Rows = [
    &[
        c('1'),
        c('2'),
        c('3'),
        c('4'),
        c('5'),
        c('6'),
        c('7'),
        c('8'),
        c('9'),
        c('0'),
    ],
    &[
        c('.'),
        c(','),
        c('!'),
        c('?'),
        c('\''),
        c('"'),
        c('@'),
        c('#'),
        c(':'),
        c(';'),
    ],
    &[
        c('('),
        c(')'),
        c('-'),
        c('_'),
        c('/'),
        c('&'),
        c('+'),
        c('='),
        c('*'),
        c('%'),
    ],
    &[
        c('$'),
        c('~'),
        c('<'),
        c('>'),
        c('['),
        c(']'),
        c('^'),
        c('|'),
        (Key::Backspace, 2),
    ],
    &[
        (Key::Page(Board::Letters), 2),
        (Key::Space, 4),
        (Key::Page(Board::Emotes), 2),
        (Key::Send, 2),
    ],
];

/// Global Twitch emotes, written as the codes Twitch turns into pictures. Short ones: a key is
/// two columns wide.
const EMOTES: Rows = [
    &[e("LUL"), e("Kappa"), e("PogChamp"), e("o7"), e("<3")],
    &[
        e("Kreygasm"),
        e("BibleThump"),
        e("SeemsGood"),
        e("NotLikeThis"),
        e("4Head"),
    ],
    &[
        e("VoHiYo"),
        e("HeyGuys"),
        e("WutFace"),
        e("DansGame"),
        e("CoolCat"),
    ],
    &[
        e("FailFish"),
        e("PJSalt"),
        e("SMOrc"),
        e("MingLee"),
        (Key::Backspace, 2),
    ],
    &[
        (Key::Page(Board::Letters), 2),
        (Key::Space, 4),
        (Key::Page(Board::Symbols), 2),
        (Key::Send, 2),
    ],
];

fn rows_of(board: Board) -> &'static Rows {
    match board {
        Board::Search => &ROWS,
        Board::Letters => &LETTERS,
        Board::Symbols => &SYMBOLS,
        Board::Emotes => &EMOTES,
    }
}

/// Every key of the Search board, row by row, left to right.
pub fn layout() -> Vec<KeySlot> {
    layout_of(Board::Search)
}

/// Every key of `board`, row by row, left to right.
pub fn layout_of(board: Board) -> Vec<KeySlot> {
    let mut out = Vec::new();
    for (row, keys) in rows_of(board).iter().enumerate() {
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

/// The key of `board` under the cursor and where it sits in [`layout_of`].
fn find(board: Board, row: u8, col: u8) -> (usize, KeySlot) {
    layout_of(board)
        .into_iter()
        .enumerate()
        .find(|(_, k)| k.row == row && (k.col..k.col + k.span).contains(&col))
        .expect("every column of every row has a key")
}

/// Which board is shown and where the cursor is. Starts on `a` of the Search board.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct KeyboardState {
    board: Board,
    row: u8,
    /// The column the cursor stands in. A wide key covers several, and the cursor keeps its
    /// column on it, so Up and Down lead back to where they came from.
    col: u8,
}

impl KeyboardState {
    /// A keyboard showing `board`, its cursor on the first key.
    pub fn on(board: Board) -> Self {
        Self {
            board,
            ..Self::default()
        }
    }

    pub fn board(&self) -> Board {
        self.board
    }

    /// Shows another board. The cursor stays where it is: every board has the same grid.
    pub fn set_board(&mut self, board: Board) {
        self.board = board;
    }

    /// The key under the cursor.
    pub fn key(&self) -> Key {
        self.here().key
    }

    /// The focused key's index in [`layout_of`] of the board shown.
    pub fn index(&self) -> usize {
        find(self.board, self.row, self.col).0
    }

    /// True on the top row, where Up leaves the keys.
    pub fn on_top_row(&self) -> bool {
        self.row == 0
    }

    fn here(&self) -> KeySlot {
        find(self.board, self.row, self.col).1
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
        if self.row as usize + 1 >= ROW_COUNT {
            return false;
        }
        self.row += 1;
        true
    }

    /// Moves to the key on the left. False at the left edge (nothing moved).
    pub fn left(&mut self) -> bool {
        let here = self.here();
        if here.col == 0 {
            return false;
        }
        self.col = find(self.board, self.row, here.col - 1).1.col;
        true
    }

    /// Moves to the key on the right. False at the right edge (nothing moved).
    pub fn right(&mut self) -> bool {
        let here = self.here();
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

/// Longest chat message Twitch takes, in characters.
pub const CHAT_MAX: usize = 500;

/// Adds `c` to a chat message. False if it was not added: a control character, the message is
/// full, or a space at the start or after another space. Any other character goes, so a physical
/// keyboard (or Steam's) can type what the on-screen one has no key for.
pub fn chat_type(text: &mut String, c: char) -> bool {
    if c.is_control() || text.chars().count() >= CHAT_MAX {
        return false;
    }
    if c == ' ' && (text.is_empty() || text.ends_with(' ')) {
        return false;
    }
    text.push(c);
    true
}

/// Adds `word` (an emote code) to a chat message as a word of its own: a space before it unless
/// the message is empty or already ends in one, and a space after it so the next word does not
/// stick to it. False if the word does not fit; the trailing space is left out if only it does not.
pub fn chat_type_word(text: &mut String, word: &str) -> bool {
    let gap = !text.is_empty() && !text.ends_with(' ');
    let len = text.chars().count() + usize::from(gap) + word.chars().count();
    if word.is_empty() || len > CHAT_MAX {
        return false;
    }
    if gap {
        text.push(' ');
    }
    text.push_str(word);
    if len < CHAT_MAX {
        text.push(' ');
    }
    true
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_row_of_every_board_fills_the_width() {
        for board in [Board::Search, Board::Letters, Board::Symbols, Board::Emotes] {
            for keys in rows_of(board) {
                let width: u8 = keys.iter().map(|(_, span)| span).sum();
                assert_eq!(width, COLUMNS, "{board:?}");
            }
        }
        assert_eq!(layout().len(), 10 * 3 + 9 + 3);
    }

    #[test]
    fn chat_pages_cycle_and_keep_the_cursor() {
        assert_eq!(Board::Letters.cycled(true), Board::Symbols);
        assert_eq!(Board::Emotes.cycled(true), Board::Letters);
        assert_eq!(Board::Letters.cycled(false), Board::Emotes);
        assert_eq!(Board::Search.cycled(true), Board::Search);

        let mut k = KeyboardState::on(Board::Letters);
        assert_eq!(k.key(), Key::Char('a'));
        for _ in 0..4 {
            k.down();
        }
        // The page key on the bottom left leads to digits, and the same spot there leads back.
        assert_eq!(k.key(), Key::Page(Board::Symbols));
        k.set_board(Board::Symbols);
        assert_eq!(k.key(), Key::Page(Board::Letters));
        k.up();
        assert_eq!(k.key(), Key::Char('$'));
        k.set_board(Board::Emotes);
        assert_eq!(k.key(), Key::Emote("FailFish"));
        assert!(!k.on_top_row());
    }

    #[test]
    fn every_chat_page_can_delete_space_and_send() {
        for board in Board::CHAT {
            let keys: Vec<Key> = layout_of(board).into_iter().map(|k| k.key).collect();
            for want in [Key::Backspace, Key::Space, Key::Send] {
                assert!(keys.contains(&want), "{board:?} has no {want:?}");
            }
            for other in Board::CHAT.into_iter().filter(|b| *b != board) {
                assert!(keys.contains(&Key::Page(other)), "{board:?} -> {other:?}");
            }
        }
    }

    #[test]
    fn shift_once_then_locked_then_off() {
        let s = Shift::Off.pressed();
        assert_eq!(s, Shift::Once);
        assert_eq!(s.apply('a'), 'A');
        assert_eq!(s.after_letter(), Shift::Off);
        let lock = s.pressed();
        assert_eq!(lock, Shift::Lock);
        assert_eq!(lock.after_letter(), Shift::Lock);
        assert_eq!(lock.pressed(), Shift::Off);
        assert_eq!(Shift::Off.apply('a'), 'a');
        assert_eq!(Shift::Lock.apply('!'), '!');
    }

    #[test]
    fn chat_text_takes_anything_printable_up_to_the_limit() {
        let mut text = String::new();
        assert!(!chat_type(&mut text, ' '), "no leading space");
        for c in "Hi é!".chars() {
            assert!(chat_type(&mut text, c));
        }
        assert!(!chat_type(&mut text, '\n'));
        assert_eq!(text, "Hi é!");
        text = "x".repeat(CHAT_MAX);
        assert!(!chat_type(&mut text, 'y'), "full");
    }

    #[test]
    fn emote_codes_are_words_of_their_own() {
        let mut text = String::new();
        assert!(chat_type_word(&mut text, "Kappa"));
        assert_eq!(text, "Kappa ");
        assert!(chat_type_word(&mut text, "LUL"));
        assert_eq!(text, "Kappa LUL ");
        let mut glued = "nice".to_owned();
        assert!(chat_type_word(&mut glued, "o7"));
        assert_eq!(glued, "nice o7 ");

        // Fits without its trailing space, then nothing fits.
        let mut almost = "x".repeat(CHAT_MAX - 4);
        assert!(chat_type_word(&mut almost, "LUL"));
        assert_eq!(almost.chars().count(), CHAT_MAX);
        assert!(!chat_type_word(&mut almost, "o7"));
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
        assert_eq!(Key::Page(Board::Symbols).label(), "123");
        assert_eq!(Key::Emote("Kappa").label(), "Kappa");
    }
}
