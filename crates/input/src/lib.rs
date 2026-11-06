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

mod gamepad;
mod keys;

pub use gamepad::{map_button, spawn_gamepad_thread};
pub use keys::map_key;
