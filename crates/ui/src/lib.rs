//! Slint views (`ui/*.slint`) and the view-model that turns `htpc_core::AppState` into them.
//!
//! Slint is a pure view: it renders the properties set by [`Update::apply`] and reports raw
//! input through the `key`, `card-clicked` and `rail-clicked` callbacks. All navigation
//! logic lives in `htpc_core::reduce`.

slint::include_modules!();

pub mod view_model;

pub use view_model::{
    apply_chat_image, apply_thumbnail, format_ago, format_bitrate, format_date, format_fps,
    format_viewers, layout_name, readable_name_colour, room_mode_text, send_problem_text,
    show_video, video_size, view_model, CardVm, ChatLineKind, ChatLineVm, KeyVm, LayoutChipVm,
    MultiviewVm, MvTileVm, PieceVm, PlaybackId, PopupRowVm, RowVm, ScreenId, Shape, StripVm,
    Thumbnail, ToastVm, Update, ViewModel, CHAT_LINES_SHOWN,
};

/// The size the layout is designed for and the scale that fits it to a window. They live in core,
/// as multiview's tile quality depends on them too.
pub use htpc_core::layout::{fit_scale, DESIGN_HEIGHT, DESIGN_WIDTH};

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
