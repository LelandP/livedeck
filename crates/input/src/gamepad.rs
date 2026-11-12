use std::collections::HashSet;
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use gilrs::{Button, EventType, Gamepad, Gilrs};
use htpc_core::{Dir, Intent};

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
        // The small button left of the face buttons: View (Steam Deck, Xbox), Back (older pads),
        // Create/Share (PlayStation). Multiview's button on every screen.
        Button::Select => Intent::View,
        // gilrs calls the bumpers (LB, RB) triggers, and the triggers `LeftTrigger2`.
        Button::LeftTrigger => Intent::PageLeft,
        Button::RightTrigger => Intent::PageRight,
        _ => return None,
    };
    Some(intent)
}

/// The direction of a button whose release is reported (`Intent::Released`): the D-pad's left
/// and right, which seek in a video. Nothing else needs its release.
pub fn held_dir(button: Button) -> Option<Dir> {
    match button {
        Button::DPadLeft => Some(Dir::Left),
        Button::DPadRight => Some(Dir::Right),
        _ => None,
    }
}

/// Which of ◀ ▶ each controller holds. A release is sent only for a press that was sent (a press
/// dropped as a duplicate, or from a controller Steam replaced, has its release dropped too), and
/// a controller that goes away lets go of what it held: its key-up will never come.
#[derive(Debug, Default)]
struct Held {
    down: Vec<(usize, Dir)>,
}

impl Held {
    /// A press of `button` on `pad` was sent.
    fn pressed(&mut self, pad: usize, button: Button) {
        if let Some(dir) = held_dir(button) {
            if !self.down.contains(&(pad, dir)) {
                self.down.push((pad, dir));
            }
        }
    }

    /// `button` was let go on `pad`: the release to send, if its press was sent.
    fn released(&mut self, pad: usize, button: Button) -> Option<Intent> {
        let dir = held_dir(button)?;
        let at = self.down.iter().position(|d| *d == (pad, dir))?;
        self.down.remove(at);
        Some(Intent::Released(dir))
    }

    /// `pad` went away: the releases of what it held.
    fn disconnected(&mut self, pad: usize) -> Vec<Intent> {
        let mut gone = Vec::new();
        self.down.retain(|(p, dir)| {
            if *p == pad {
                gone.push(Intent::Released(*dir));
            }
            *p != pad
        });
        gone
    }
}

/// How long one blocking poll waits before looping again.
const POLL: Duration = Duration::from_millis(250);

/// Steam Input replaces a physical controller with a virtual pad and lists the physical ones here
/// (`0xVVVV/0xPPPP` vendor/product pairs, comma separated). SDL games skip the listed devices.
/// gilrs does not, so without this check every press on a Steam Deck arrives twice: once from the
/// built-in controller and once from Steam's virtual pad.
const STEAM_IGNORE_ENV: &str = "SDL_GAMECONTROLLER_IGNORE_DEVICES";

/// The same button from a second controller within this time is taken as one press, not two.
/// This catches duplicate devices that Steam does not list.
const DUPLICATE_WINDOW: Duration = Duration::from_millis(100);

/// Parses the vendor/product list from [`STEAM_IGNORE_ENV`]. Entries that do not parse are skipped.
pub fn parse_ignore_list(list: &str) -> HashSet<(u16, u16)> {
    fn hex(s: &str) -> Option<u16> {
        let s = s.trim();
        let digits = s
            .strip_prefix("0x")
            .or_else(|| s.strip_prefix("0X"))
            .unwrap_or(s);
        u16::from_str_radix(digits, 16).ok()
    }
    list.split(',')
        .filter_map(|pair| {
            let (vendor, product) = pair.split_once('/')?;
            Some((hex(vendor)?, hex(product)?))
        })
        .collect()
}

fn is_listed(pad: &Gamepad<'_>, ignored: &HashSet<(u16, u16)>) -> bool {
    match (pad.vendor_id(), pad.product_id()) {
        (Some(vendor), Some(product)) => ignored.contains(&(vendor, product)),
        _ => false,
    }
}

/// Drops a press of the same button as the previous press when it comes from a different
/// controller within [`DUPLICATE_WINDOW`]. Repeated presses on one controller always count.
#[derive(Debug, Default)]
struct Dedup {
    last: Option<(Button, usize, Instant)>,
}

impl Dedup {
    fn accept(&mut self, button: Button, pad: usize, now: Instant) -> bool {
        if let Some((last_button, last_pad, at)) = self.last {
            if last_button == button
                && last_pad != pad
                && now.saturating_duration_since(at) < DUPLICATE_WINDOW
            {
                return false;
            }
        }
        self.last = Some((button, pad, now));
        true
    }
}

/// Polls gamepads on a dedicated thread (`Gilrs` is not `Send`) and calls `on_intent` for each
/// mapped button press, and for the release of the D-pad's left and right (`Intent::Released`,
/// also when the controller goes away while holding one). The thread is detached: it ends when
/// `on_intent` returns `false` (the receiver is gone) or when the process exits.
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
            let ignored = std::env::var(STEAM_IGNORE_ENV)
                .map(|list| parse_ignore_list(&list))
                .unwrap_or_default();
            if !ignored.is_empty() {
                tracing::info!(
                    count = ignored.len(),
                    "Steam Input replaces some controllers"
                );
            }
            for (_, pad) in gilrs.gamepads() {
                log_connected(&pad, &ignored);
            }
            let mut dedup = Dedup::default();
            let mut held = Held::default();
            loop {
                let Some(ev) = gilrs.next_event_blocking(Some(POLL)) else {
                    continue;
                };
                let pad_id: usize = ev.id.into();
                let intents: Vec<Intent> = match ev.event {
                    EventType::ButtonPressed(button, _) => {
                        let pad = gilrs.gamepad(ev.id);
                        // Skip a controller Steam has replaced, but only while another one is
                        // there to use (otherwise the only controller would stop working).
                        let replaced = is_listed(&pad, &ignored)
                            && gilrs
                                .gamepads()
                                .any(|(id, other)| id != ev.id && !is_listed(&other, &ignored));
                        if replaced || !dedup.accept(button, pad_id, Instant::now()) {
                            tracing::trace!(?button, name = pad.name(), "duplicate press dropped");
                            Vec::new()
                        } else {
                            let intent = map_button(button);
                            if intent.is_some() {
                                held.pressed(pad_id, button);
                            }
                            intent.into_iter().collect()
                        }
                    }
                    EventType::ButtonReleased(button, _) => {
                        held.released(pad_id, button).into_iter().collect()
                    }
                    EventType::Connected => {
                        log_connected(&gilrs.gamepad(ev.id), &ignored);
                        Vec::new()
                    }
                    EventType::Disconnected => {
                        tracing::info!("gamepad disconnected");
                        held.disconnected(pad_id)
                    }
                    _ => Vec::new(),
                };
                for intent in intents {
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

fn log_connected(pad: &Gamepad<'_>, ignored: &HashSet<(u16, u16)>) {
    tracing::info!(
        name = pad.name(),
        vendor = ?pad.vendor_id(),
        product = ?pad.product_id(),
        replaced_by_steam = is_listed(pad, ignored),
        "gamepad connected"
    );
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
    fn the_view_button_is_multiviews() {
        assert_eq!(map_button(Button::Select), Some(Intent::View));
    }

    #[test]
    fn the_bumpers_turn_keyboard_pages() {
        assert_eq!(map_button(Button::LeftTrigger), Some(Intent::PageLeft));
        assert_eq!(map_button(Button::RightTrigger), Some(Intent::PageRight));
    }

    #[test]
    fn other_buttons_are_ignored() {
        for b in [Button::Mode, Button::LeftTrigger2, Button::Unknown] {
            assert_eq!(map_button(b), None, "{b:?}");
        }
    }

    #[test]
    fn steams_ignore_list_parses() {
        let list = parse_ignore_list("0x28de/0x1205, 0x045E/0x028e,junk,0x1/zz");
        assert_eq!(list, HashSet::from([(0x28de, 0x1205), (0x045e, 0x028e)]));
        assert!(parse_ignore_list("").is_empty());
    }

    #[test]
    fn only_left_and_right_report_their_release() {
        assert_eq!(held_dir(Button::DPadLeft), Some(Dir::Left));
        assert_eq!(held_dir(Button::DPadRight), Some(Dir::Right));
        for b in [
            Button::DPadUp,
            Button::DPadDown,
            Button::South,
            Button::East,
            Button::LeftTrigger,
            Button::Start,
        ] {
            assert_eq!(held_dir(b), None, "{b:?}");
        }
    }

    #[test]
    fn a_release_follows_its_press() {
        let mut held = Held::default();
        held.pressed(0, Button::DPadRight);
        assert_eq!(
            held.released(0, Button::DPadRight),
            Some(Intent::Released(Dir::Right))
        );
        // Once only.
        assert_eq!(held.released(0, Button::DPadRight), None);
        // Other buttons never report a release.
        held.pressed(0, Button::South);
        assert_eq!(held.released(0, Button::South), None);
    }

    #[test]
    fn a_release_whose_press_was_not_sent_is_dropped() {
        // The second pad's press was a duplicate (Steam's virtual pad): it was never sent, so
        // neither is its release, while the first pad's is.
        let mut held = Held::default();
        held.pressed(0, Button::DPadLeft);
        assert_eq!(held.released(1, Button::DPadLeft), None);
        assert_eq!(
            held.released(0, Button::DPadLeft),
            Some(Intent::Released(Dir::Left))
        );
    }

    #[test]
    fn a_controller_that_goes_away_lets_go_of_what_it_held() {
        let mut held = Held::default();
        held.pressed(0, Button::DPadLeft);
        held.pressed(0, Button::DPadRight);
        held.pressed(1, Button::DPadRight);
        assert_eq!(
            held.disconnected(0),
            [Intent::Released(Dir::Left), Intent::Released(Dir::Right)]
        );
        assert!(held.disconnected(0).is_empty(), "nothing twice");
        // The other pad still holds its own.
        assert_eq!(
            held.released(1, Button::DPadRight),
            Some(Intent::Released(Dir::Right))
        );
    }

    #[test]
    fn the_same_press_from_a_second_pad_counts_once() {
        let t = Instant::now();
        let mut d = Dedup::default();
        assert!(d.accept(Button::South, 0, t));
        assert!(!d.accept(Button::South, 1, t + Duration::from_millis(5)));
        // Later, or another button, or the same pad again: real presses.
        assert!(d.accept(Button::South, 1, t + Duration::from_millis(300)));
        assert!(d.accept(Button::East, 0, t + Duration::from_millis(305)));
        assert!(d.accept(Button::East, 0, t + Duration::from_millis(310)));
    }
}
