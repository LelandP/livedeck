//! Input mapping: Slint key events and `gilrs` gamepad events -> `htpc_core::Intent`.
//!
//! | Intent     | Keyboard            | Gamepad (Xbox layout / Steam Input) |
//! |------------|---------------------|-------------------------------------|
//! | Directions | Arrow keys          | D-pad                               |
//! | Select     | Return              | A (South)                           |
//! | Back       | Escape              | B (East)                            |
//! | ToggleChat | (`y`, see Text)     | Y (North)                           |
//! | Retry      | (`r`, see Text)     | X (West)                            |
//! | Menu       | Menu key            | Start                               |
//! | Text       | Printable keys      | -                                   |
//! | Erase      | Backspace           | -                                   |
//!
//! `Text` and `Erase` type on the Search screen. Everywhere else the reducer reads `y` as
//! ToggleChat, `r` as Retry and Backspace as Back, as before.
//!
//! Letting go of ◀ or ▶ (the D-pad's left and right, the arrow keys) is `Intent::Released`: a
//! video tells a tap from a hold by it. A held arrow key's auto-repeat is dropped, so a key and a
//! D-pad both give one press and one release. A controller that goes away lets go of what it
//! held.

mod gamepad;
mod keys;

pub use gamepad::{held_dir, map_button, spawn_gamepad_thread};
pub use keys::{is_dropped_repeat, map_key, map_key_release};
