//! `favorites.toml`: LiveDeck's own starred channels, Kick and Twitch, in the order starred.
//!
//! ```toml
//! [[channel]]
//! platform = "twitch"
//! login = "lofi_lena"
//!
//! [[channel]]
//! platform = "kick"
//! login = "ziggy_k"   # a Kick channel's slug
//! ```
//!
//! The app writes it on every star and unstar, so it lives apart from `config.toml` (whose
//! hand-edited quick messages stay untouched) in the data folder. Loading is forgiving: an
//! unknown platform or an empty login drops that entry with a warning, repeats are dropped, and a
//! file that is not valid TOML is moved aside to `favorites.toml.corrupt`.

use std::path::{Path, PathBuf};

use htpc_core::{ChannelRef, Platform};
use serde::{Deserialize, Serialize};

use crate::fs_util::{self, Access};
use crate::{Error, Result};

#[derive(Debug, Default, Serialize, Deserialize)]
#[serde(default)]
struct FavoritesFile {
    #[serde(rename = "channel")]
    channels: Vec<Entry>,
}

#[derive(Debug, Default, Serialize, Deserialize)]
#[serde(default)]
struct Entry {
    platform: String,
    login: String,
}

pub(crate) fn platform_name(p: Platform) -> &'static str {
    match p {
        Platform::Twitch => "twitch",
        Platform::Kick => "kick",
    }
}

pub(crate) fn platform_of(name: &str) -> Option<Platform> {
    match name.trim().to_ascii_lowercase().as_str() {
        "twitch" => Some(Platform::Twitch),
        "kick" => Some(Platform::Kick),
        _ => None,
    }
}

/// Parses the file's text. Fails only if it is not valid TOML or a key has the wrong type.
pub fn favorites_from_toml(text: &str) -> std::result::Result<Vec<ChannelRef>, String> {
    let file: FavoritesFile = toml::from_str(text).map_err(|e| e.to_string())?;
    let mut out: Vec<ChannelRef> = Vec::new();
    for entry in file.channels {
        let login = entry.login.trim();
        let Some(platform) = platform_of(&entry.platform) else {
            tracing::warn!(platform = %entry.platform, login, "unknown platform in favorites, skipped");
            continue;
        };
        if login.is_empty() {
            continue;
        }
        let channel = ChannelRef::new(platform, login);
        if !out.iter().any(|c| c.is_same(&channel)) {
            out.push(channel);
        }
    }
    Ok(out)
}

pub fn favorites_to_toml(channels: &[ChannelRef]) -> Result<String> {
    let file = FavoritesFile {
        channels: channels
            .iter()
            .map(|c| Entry {
                platform: platform_name(c.platform).to_owned(),
                login: c.login.clone(),
            })
            .collect(),
    };
    toml::to_string(&file).map_err(|e| Error::Serialize(e.to_string()))
}

/// Loads and saves the favorites at a fixed path.
#[derive(Debug, Clone)]
pub struct FavoritesStore {
    path: PathBuf,
}

impl FavoritesStore {
    pub fn new(path: impl Into<PathBuf>) -> Self {
        Self { path: path.into() }
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    /// Strict load: no favorites if the file does not exist, [`Error::CorruptConfig`] if it is
    /// invalid, [`Error::Io`] if it cannot be read.
    pub fn load(&self) -> Result<Vec<ChannelRef>> {
        let Some(bytes) = fs_util::read_if_exists(&self.path)? else {
            return Ok(Vec::new());
        };
        let text = String::from_utf8(bytes).map_err(|_| Error::CorruptConfig {
            path: self.path.clone(),
            message: "not UTF-8".to_owned(),
        })?;
        favorites_from_toml(&text).map_err(|message| Error::CorruptConfig {
            path: self.path.clone(),
            message,
        })
    }

    /// Startup load that never fails: a corrupt file is moved aside to `favorites.toml.corrupt`
    /// and the list starts empty. Read errors are logged and also give an empty list.
    pub fn load_or_default(&self) -> Vec<ChannelRef> {
        match self.load() {
            Ok(favorites) => favorites,
            Err(Error::CorruptConfig { message, .. }) => {
                match fs_util::quarantine(&self.path) {
                    Ok(moved) => tracing::warn!(
                        %message, moved_to = %moved.display(),
                        "favorites file is corrupt, starting without favorites"
                    ),
                    Err(err) => tracing::warn!(
                        %message, %err,
                        "favorites file is corrupt, starting without favorites"
                    ),
                }
                Vec::new()
            }
            Err(err) => {
                tracing::warn!(%err, "could not read the favorites file");
                Vec::new()
            }
        }
    }

    /// Writes the list atomically, creating the folder if needed.
    pub fn save(&self, channels: &[ChannelRef]) -> Result<()> {
        let text = favorites_to_toml(channels)?;
        fs_util::write_atomic(&self.path, text.as_bytes(), Access::Normal)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_missing_file_is_no_favorites() {
        let dir = tempfile::tempdir().unwrap();
        let store = FavoritesStore::new(dir.path().join("favorites.toml"));
        assert_eq!(store.load().unwrap(), Vec::new());
    }

    #[test]
    fn saving_and_loading_keeps_the_order_and_the_platforms() {
        let dir = tempfile::tempdir().unwrap();
        let store = FavoritesStore::new(dir.path().join("data/favorites.toml"));
        let list = vec![
            ChannelRef::twitch("lofi_lena"),
            ChannelRef::kick("ziggy_k"),
            ChannelRef::twitch("marrow"),
        ];
        store.save(&list).unwrap();
        assert_eq!(store.load().unwrap(), list);
        let text = std::fs::read_to_string(store.path()).unwrap();
        assert!(text.contains("[[channel]]"), "{text}");
        assert!(text.contains("platform = \"kick\""), "{text}");
    }

    #[test]
    fn unknown_platforms_blank_logins_and_repeats_are_dropped() {
        let text = r#"
            [[channel]]
            platform = "Twitch"
            login = " Marrow "

            [[channel]]
            platform = "youtube"
            login = "someone"

            [[channel]]
            platform = "kick"
            login = ""

            [[channel]]
            platform = "twitch"
            login = "marrow"

            [[channel]]
            platform = "kick"
            login = "marrow"
        "#;
        assert_eq!(
            favorites_from_toml(text).unwrap(),
            [ChannelRef::twitch("Marrow"), ChannelRef::kick("marrow")]
        );
    }

    #[test]
    fn a_corrupt_file_is_moved_aside_and_starts_empty() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("favorites.toml");
        std::fs::write(&path, "[[channel]\nnot toml").unwrap();
        let store = FavoritesStore::new(&path);
        assert!(store.load_or_default().is_empty());
        assert!(!path.exists());
        assert!(dir.path().join("favorites.toml.corrupt").exists());
    }
}
