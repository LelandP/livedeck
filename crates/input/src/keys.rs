use htpc_core::Intent;
use slint::platform::Key;

/// Maps the `text` of a Slint `KeyEvent` to an intent. Unmapped keys return `None` so the
/// window can let them propagate.
///
/// Printable characters become `Intent::Text`: the Search screen types them, and the reducer
/// turns `y` and `r` into the chat and retry shortcuts everywhere else. Backspace is
/// `Intent::Erase` for the same reason (a deleted character on Search, Back elsewhere).
pub fn map_key(text: &str) -> Option<Intent> {
    let mut chars = text.chars();
    let c = chars.next()?;
    if chars.next().is_some() {
        // Composed / multi-character input is never a navigation key.
        return None;
    }
    let intent = match c {
        c if c == char::from(Key::UpArrow) => Intent::Up,
        c if c == char::from(Key::DownArrow) => Intent::Down,
        c if c == char::from(Key::LeftArrow) => Intent::Left,
        c if c == char::from(Key::RightArrow) => Intent::Right,
        c if c == char::from(Key::Return) => Intent::Select,
        c if c == char::from(Key::Escape) => Intent::Back,
        c if c == char::from(Key::Backspace) => Intent::Erase,
        c if c == char::from(Key::Menu) => Intent::Menu,
        c if c == char::from(Key::PageUp) => Intent::PageLeft,
        c if c == char::from(Key::PageDown) => Intent::PageRight,
        c if is_printable(c) => Intent::Text(c),
        _ => return None,
    };
    Some(intent)
}

/// A character a key types. Slint sends special keys (arrows, F-keys) as characters in the
/// Unicode private use area, and control characters for Tab, Delete and the like.
fn is_printable(c: char) -> bool {
    let private_use = ('\u{E000}'..='\u{F8FF}').contains(&c);
    !c.is_control() && !private_use
}

#[cfg(test)]
mod tests {
    use super::*;
    use slint::SharedString;

    fn key(k: Key) -> SharedString {
        k.into()
    }

    #[test]
    fn arrows_map_to_directions() {
        assert_eq!(map_key(&key(Key::UpArrow)), Some(Intent::Up));
        assert_eq!(map_key(&key(Key::DownArrow)), Some(Intent::Down));
        assert_eq!(map_key(&key(Key::LeftArrow)), Some(Intent::Left));
        assert_eq!(map_key(&key(Key::RightArrow)), Some(Intent::Right));
    }

    #[test]
    fn action_keys() {
        assert_eq!(map_key(&key(Key::Return)), Some(Intent::Select));
        assert_eq!(map_key(&key(Key::Escape)), Some(Intent::Back));
        assert_eq!(map_key(&key(Key::Backspace)), Some(Intent::Erase));
        assert_eq!(map_key(&key(Key::Menu)), Some(Intent::Menu));
        assert_eq!(map_key(&key(Key::PageUp)), Some(Intent::PageLeft));
        assert_eq!(map_key(&key(Key::PageDown)), Some(Intent::PageRight));
    }

    #[test]
    fn printable_keys_are_text_for_the_reducer_to_interpret() {
        // `y` and `r` stay the chat and retry shortcuts off the Search screen (see the core's
        // `typed_key`), so they come through as text too.
        for c in ['y', 'Y', 'r', 'R', 'a', 'Z', '7', '_', '-', '!'] {
            assert_eq!(map_key(&c.to_string()), Some(Intent::Text(c)), "{c:?}");
        }
        assert_eq!(map_key(&key(Key::Space)), Some(Intent::Text(' ')));
    }

    #[test]
    fn unmapped_keys_are_ignored() {
        assert_eq!(map_key(""), None);
        assert_eq!(map_key("yy"), None);
        assert_eq!(map_key(&key(Key::Tab)), None);
        assert_eq!(map_key(&key(Key::Delete)), None);
        assert_eq!(map_key(&key(Key::F1)), None);
        assert_eq!(map_key(&key(Key::Home)), None);
    }
}
