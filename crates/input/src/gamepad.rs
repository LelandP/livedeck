use std::thread::JoinHandle;
use std::time::Duration;

use gilrs::{Button, EventType, Gilrs};
use htpc_core::Intent;

/// Maps a gamepad button (gilrs uses positional names: South is A on an Xbox pad) to an intent.
pub fn map_button(button: Button) -> Option<Intent> {
    let intent = match button {
        Button::DPadUp => Intent::Up,
        Button::DPadDown => Intent::Down,
        Button::DPadLeft => Intent::Left,
        Button::DPadRight => Intent::Right,
        Button::South => Intent::Select,
        Button::East => Intent::Back,
        Button::North => Intent::ToggleChat,
        Button::West => Intent::Retry,
        Button::Start => Intent::Menu,
        _ => return None,
    };
    Some(intent)
}

/// How long one blocking poll waits before looping again.
const POLL: Duration = Duration::from_millis(250);

/// Polls gamepads on a dedicated thread (`Gilrs` is not `Send`) and calls `on_intent` for each
/// mapped button press. The thread is detached: it ends when `on_intent` returns `false`
/// (the receiver is gone) or when the process exits.
///
/// Returns `None` if no gamepad backend is available. Keyboard input (and Steam Input's
/// keyboard mapping) keeps working in that case.
pub fn spawn_gamepad_thread<F>(mut on_intent: F) -> Option<JoinHandle<()>>
where
    F: FnMut(Intent) -> bool + Send + 'static,
{
    let spawned = std::thread::Builder::new()
        .name("gamepad".into())
        .spawn(move || {
            let mut gilrs = match Gilrs::new() {
                Ok(g) => g,
                Err(err) => {
                    tracing::warn!(%err, "gamepad input unavailable, keyboard only");
                    return;
                }
            };
            for (_, pad) in gilrs.gamepads() {
                tracing::info!(name = pad.name(), "gamepad connected");
            }
            loop {
                let Some(ev) = gilrs.next_event_blocking(Some(POLL)) else {
                    continue;
                };
                let intent = match ev.event {
                    EventType::ButtonPressed(button, _) => map_button(button),
                    EventType::Connected => {
                        tracing::info!(name = gilrs.gamepad(ev.id).name(), "gamepad connected");
                        None
                    }
                    EventType::Disconnected => {
                        tracing::info!("gamepad disconnected");
                        None
                    }
                    _ => None,
                };
                if let Some(intent) = intent {
                    if !on_intent(intent) {
                        return;
                    }
                }
            }
        });
    match spawned {
        Ok(handle) => Some(handle),
        Err(err) => {
            tracing::warn!(%err, "could not start gamepad thread");
            None
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn dpad_and_face_buttons() {
        assert_eq!(map_button(Button::DPadUp), Some(Intent::Up));
        assert_eq!(map_button(Button::DPadDown), Some(Intent::Down));
        assert_eq!(map_button(Button::DPadLeft), Some(Intent::Left));
        assert_eq!(map_button(Button::DPadRight), Some(Intent::Right));
        assert_eq!(map_button(Button::South), Some(Intent::Select));
        assert_eq!(map_button(Button::East), Some(Intent::Back));
        assert_eq!(map_button(Button::North), Some(Intent::ToggleChat));
        assert_eq!(map_button(Button::West), Some(Intent::Retry));
        assert_eq!(map_button(Button::Start), Some(Intent::Menu));
    }

    #[test]
    fn other_buttons_are_ignored() {
        for b in [
            Button::Mode,
            Button::Select,
            Button::LeftTrigger,
            Button::Unknown,
        ] {
            assert_eq!(map_button(b), None, "{b:?}");
        }
    }
}
