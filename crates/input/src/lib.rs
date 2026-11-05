//! Input mapping: Slint key events and `gilrs` gamepad events -> `htpc_core::Intent`.
//!
//! | Intent     | Keyboard            | Gamepad (Xbox layout / Steam Input) |
//! |------------|---------------------|-------------------------------------|
//! | Directions | Arrow keys          | D-pad                               |
//! | Select     | Return              | A (South)                           |
//! | Back       | Escape, Backspace   | B (East)                            |
//! | ToggleChat | `y`, `Y`            | Y (North)                           |
//! | Retry      | `r`, `R`            | X (West)                            |
//! | Menu       | Menu key            | Start                               |

mod gamepad;
mod keys;

pub use gamepad::{map_button, spawn_gamepad_thread};
pub use keys::map_key;
