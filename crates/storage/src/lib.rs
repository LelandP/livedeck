//! Persistence for the app:
//!
//! - [`ConfigStore`] / [`Config`]: `config.toml` (`serde` + `toml`), forgiving load, atomic save.
//! - [`FavoritesStore`]: `favorites.toml`, the starred channels. Same rules as the config.
//! - [`MultiviewStore`]: `multiview.toml`, the last multiview session. Same rules.
//! - [`TokenStore`]: OAuth tokens in the OS keyring, with an owner-only file as the fallback.
//! - [`ImageCache`]: thumbnails in a size-capped disk LRU, fetched with `reqwest` and decoded
//!   with `image`.
//! - [`Paths`]: the platform folders (`directories`).
//!
//! Depends on `htpc-core` only for the settings enums. No UI types.

mod cache;
mod config;
mod error;
mod favorites;
mod fs_util;
mod multiview;
mod paths;
mod tokens;

pub use cache::{decode, CacheStats, DecodedImage, ImageCache, MAX_DECODED_SIDE, MAX_IMAGE_BYTES};
pub use config::{Config, ConfigStore, DEFAULT_IMAGE_CACHE_MB, MIN_IMAGE_CACHE_MB};
pub use error::{Error, Result};
pub use favorites::{favorites_from_toml, favorites_to_toml, FavoritesStore};
pub use multiview::{multiview_from_toml, multiview_to_toml, MultiviewStore};
pub use paths::{Paths, APP_NAME};
pub use tokens::{
    BackendError, BackendResult, FileBackend, KeyringBackend, Location, SecretBackend, TokenStore,
    Tokens, KEYRING_USER,
};
