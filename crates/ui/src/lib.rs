//! Slint views (`ui/*.slint`) and the view-model that turns `htpc_core::AppState` into them.
//!
//! Slint is a pure view: it renders the properties set by [`Update::apply`] and reports raw
//! input through the `key`, `card-clicked` and `rail-clicked` callbacks. All navigation
//! logic lives in `htpc_core::reduce`.

slint::include_modules!();

pub mod view_model;

pub use view_model::{
    apply_chat_image, apply_thumbnail, format_viewers, readable_name_colour, view_model, CardVm,
    ChatLineKind, ChatLineVm, PieceVm, PlaybackId, RowVm, ScreenId, Shape, Thumbnail, Update,
    ViewModel, CHAT_LINES_SHOWN,
};

/// The size the layout is designed for, in logical px (`Theme` in `theme.slint`).
pub const DESIGN_WIDTH: f32 = 1280.0;
pub const DESIGN_HEIGHT: f32 = 800.0;

/// The scale factor that makes the 1280x800 design fill a window of `width` x `height` physical
/// pixels with nothing cut off: 1.0 on a Steam Deck, 1.35 on a 1080p TV, 2.7 on a 4K one. A window
/// of another shape gets extra room on one axis. `None` for an empty (minimised) window.
pub fn fit_scale(width: u32, height: u32) -> Option<f32> {
    if width == 0 || height == 0 {
        return None;
    }
    Some((width as f32 / DESIGN_WIDTH).min(height as f32 / DESIGN_HEIGHT))
}

#[cfg(test)]
mod tests {
    use super::fit_scale;

    #[test]
    fn the_design_fits_any_window() {
        assert_eq!(fit_scale(1280, 800), Some(1.0));
        assert_eq!(fit_scale(2560, 1600), Some(2.0));
        assert_eq!(fit_scale(3840, 2160), Some(2.7));
        // Desktop mode on the Deck: the taskbar takes some height, so height decides.
        assert_eq!(fit_scale(1280, 720), Some(0.9));
        assert_eq!(fit_scale(0, 800), None);
    }
}
