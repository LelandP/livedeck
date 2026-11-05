use htpc_core::Intent;
use slint::platform::Key;

/// Maps the `text` of a Slint `KeyEvent` to an intent. Unmapped keys return `None` so the
/// window can let them propagate.
pub fn map_key(text: &str) -> Option<Intent> {
    let mut chars = text.chars();
    let c = chars.next()?;
    if chars.next().is_some() {
        // Composed / multi-character input is never a navigation key.
        return None;
    }
    let intent = match c {
        'y' | 'Y' => Intent::ToggleChat,
        'r' | 'R' => Intent::Retry,
        c if c == char::from(Key::UpArrow) => Intent::Up,
        c if c == char::from(Key::DownArrow) => Intent::Down,
        c if c == char::from(Key::LeftArrow) => Intent::Left,
        c if c == char::from(Key::RightArrow) => Intent::Right,
        c if c == char::from(Key::Return) => Intent::Select,
        c if c == char::from(Key::Escape) || c == char::from(Key::Backspace) => Intent::Back,
        c if c == char::from(Key::Menu) => Intent::Menu,
        _ => return None,
    };
    Some(intent)
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
        assert_eq!(map_key(&key(Key::Backspace)), Some(Intent::Back));
        assert_eq!(map_key(&key(Key::Menu)), Some(Intent::Menu));
        assert_eq!(map_key("y"), Some(Intent::ToggleChat));
        assert_eq!(map_key("Y"), Some(Intent::ToggleChat));
        assert_eq!(map_key("r"), Some(Intent::Retry));
        assert_eq!(map_key("R"), Some(Intent::Retry));
    }

    #[test]
    fn unmapped_keys_are_ignored() {
        assert_eq!(map_key(""), None);
        assert_eq!(map_key("a"), None);
        assert_eq!(map_key("yy"), None);
        assert_eq!(map_key(&key(Key::Tab)), None);
        assert_eq!(map_key(&key(Key::Space)), None);
    }
}
