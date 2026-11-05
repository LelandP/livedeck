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
