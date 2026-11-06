//! Pure application core: domain types, `AppState`, `Intent` / `Command` / `Event` and `reduce`.
//!
//! Data flow: `Intent | Event -> reduce(state) -> (new state, Vec<Command>)`. The app runs each
//! `Command` and feeds the result back as an `Event`. This crate has no UI, network or IO
//! dependencies, so everything here is unit-testable.

pub mod chat;
pub mod domain;
pub mod keyboard;
pub mod msg;
pub mod reduce;
pub mod settings;
pub mod state;

pub use chat::{ChatBuffer, CHAT_CAPACITY};
pub use domain::{
    grid_cols, grid_rows, Category, Channel, ChatKind, ChatMessage, ChatSpan, ChatStatus,
    DeviceCode, FeedId, LoginProblem, Moderation, Paging, PlayerStats, Rgb, RoomMode,
    RoomModeChange, Row, RowKind, SendProblem, Stream, Tile, User,
};
pub use keyboard::{Board, Key, KeySlot, KeyboardState, Shift, CHAT_MAX};
pub use msg::{Command, Event, Intent, Msg, PageOutcome, PlayerOption, StreamKind};
pub use reduce::{init, reduce, PREFETCH_DISTANCE};
pub use settings::{ChatLayout, Hwdec, Latency, Quality, Settings, UnknownSetting, Zoom};
pub use state::{
    default_quick_messages, subscribe_url, AppState, CategoryFeed, Compose, Feed, Focus, Playback,
    PlayerMenuRow, RaidOffer, RailEntry, RailState, Screen, SearchState, Sending, SessionOptions,
    SettingsRow, SleepTimer, Toast, DEFAULT_QUICK_MESSAGES, RAID_SECONDS, SEARCH_MIN_CHARS,
    SLEEP_STEPS, TOASTS_KEPT, TOASTS_SHOWN,
};
