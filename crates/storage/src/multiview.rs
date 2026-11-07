//! `multiview.toml`: the last multiview session, for Home's *Resume multiview* card.
//!
//! ```toml
//! layout = "grid-6"   # grid-2, grid-3, grid-4, grid-6, grid-9 or spotlight
//!
//! [[channel]]
//! platform = "twitch"
//! login = "lofi_lena"
//!
//! [[channel]]
//! platform = "kick"
//! login = "ziggy_k"
//! sound = true        # the stream with sound
//! ```
//!
//! The channels are in place order. The app writes it whenever a running session changes, so it
//! lives in the data folder next to `favorites.toml`. Loading is forgiving, as there: entries with
//! an unknown platform or an empty login and repeats are dropped, more than nine channels are cut,
//! an unknown layout becomes the smallest grid that fits, no `sound` gives it to the first, and a
//! file that is not valid TOML is moved aside to `multiview.toml.corrupt`.

use std::path::{Path, PathBuf};

use htpc_core::{layout::MAX_STREAMS, ChannelRef, Layout, SavedSet};
use serde::{Deserialize, Serialize};

use crate::favorites::{platform_name, platform_of};
use crate::fs_util::{self, Access};
use crate::{Error, Result};

#[derive(Debug, Default, Serialize, Deserialize)]
#[serde(default)]
struct SetFile {
    layout: String,
    #[serde(rename = "channel")]
    channels: Vec<Entry>,
}

#[derive(Debug, Default, Serialize, Deserialize)]
#[serde(default)]
struct Entry {
    platform: String,
    login: String,
    #[serde(skip_serializing_if = "std::ops::Not::not")]
    sound: bool,
}

fn layout_name(layout: Layout) -> String {
    match layout {
        Layout::Grid(n) => format!("grid-{n}"),
        Layout::Spotlight => "spotlight".to_owned(),
    }
}

fn layout_of(name: &str) -> Option<Layout> {
    let name = name.trim().to_ascii_lowercase();
    if name == "spotlight" {
        return Some(Layout::Spotlight);
    }
    let n: u8 = name.strip_prefix("grid-")?.parse().ok()?;
    Layout::GRID_SIZES.contains(&n).then_some(Layout::Grid(n))
}

/// Parses the file's text. `Ok(None)` when it names no channel. Fails only if it is not valid
/// TOML or a key has the wrong type.
pub fn multiview_from_toml(text: &str) -> std::result::Result<Option<SavedSet>, String> {
    let file: SetFile = toml::from_str(text).map_err(|e| e.to_string())?;
    let mut channels: Vec<ChannelRef> = Vec::new();
    let mut sound = None;
    for entry in file.channels {
        let login = entry.login.trim();
        let Some(platform) = platform_of(&entry.platform) else {
            tracing::warn!(platform = %entry.platform, login, "unknown platform in multiview.toml, skipped");
            continue;
        };
        if login.is_empty() || channels.len() >= MAX_STREAMS {
            continue;
        }
        let channel = ChannelRef::new(platform, login);
        if channels.iter().any(|c| c.is_same(&channel)) {
            continue;
        }
        if entry.sound && sound.is_none() {
            sound = Some(channels.len());
        }
        channels.push(channel);
    }
    if channels.is_empty() {
        return Ok(None);
    }
    let layout = layout_of(&file.layout)
        .filter(|l| l.fits(channels.len()))
        .unwrap_or_else(|| Layout::smallest_grid(channels.len()));
    Ok(Some(SavedSet {
        channels,
        layout,
        sound: sound.unwrap_or(0),
    }))
}

pub fn multiview_to_toml(set: &SavedSet) -> Result<String> {
    let file = SetFile {
        layout: layout_name(set.layout),
        channels: set
            .channels
            .iter()
            .enumerate()
            .map(|(i, c)| Entry {
                platform: platform_name(c.platform).to_owned(),
                login: c.login.clone(),
                sound: i == set.sound,
            })
            .collect(),
    };
    toml::to_string(&file).map_err(|e| Error::Serialize(e.to_string()))
}

/// Loads and saves the last multiview session at a fixed path.
#[derive(Debug, Clone)]
pub struct MultiviewStore {
    path: PathBuf,
}

impl MultiviewStore {
    pub fn new(path: impl Into<PathBuf>) -> Self {
        Self { path: path.into() }
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    /// Strict load: `None` if the file does not exist or names no channel,
    /// [`Error::CorruptConfig`] if it is invalid, [`Error::Io`] if it cannot be read.
    pub fn load(&self) -> Result<Option<SavedSet>> {
        let Some(bytes) = fs_util::read_if_exists(&self.path)? else {
            return Ok(None);
        };
        let text = String::from_utf8(bytes).map_err(|_| Error::CorruptConfig {
            path: self.path.clone(),
            message: "not UTF-8".to_owned(),
        })?;
        multiview_from_toml(&text).map_err(|message| Error::CorruptConfig {
            path: self.path.clone(),
            message,
        })
    }

    /// Startup load that never fails: a corrupt file is moved aside to `multiview.toml.corrupt`
    /// and there is no saved session. Read errors are logged and also give none.
    pub fn load_or_default(&self) -> Option<SavedSet> {
        match self.load() {
            Ok(set) => set,
            Err(Error::CorruptConfig { message, .. }) => {
                match fs_util::quarantine(&self.path) {
                    Ok(moved) => tracing::warn!(
                        %message, moved_to = %moved.display(),
                        "multiview file is corrupt, starting without a saved session"
                    ),
                    Err(err) => tracing::warn!(
                        %message, %err,
                        "multiview file is corrupt, starting without a saved session"
                    ),
                }
                None
            }
            Err(err) => {
                tracing::warn!(%err, "could not read the multiview file");
                None
            }
        }
    }

    /// Writes the set atomically, creating the folder if needed.
    pub fn save(&self, set: &SavedSet) -> Result<()> {
        let text = multiview_to_toml(set)?;
        fs_util::write_atomic(&self.path, text.as_bytes(), Access::Normal)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn set() -> SavedSet {
        SavedSet {
            channels: vec![
                ChannelRef::twitch("lofi_lena"),
                ChannelRef::kick("ziggy_k"),
                ChannelRef::twitch("marrow"),
            ],
            layout: Layout::Spotlight,
            sound: 1,
        }
    }

    #[test]
    fn a_missing_file_is_no_session() {
        let dir = tempfile::tempdir().unwrap();
        let store = MultiviewStore::new(dir.path().join("multiview.toml"));
        assert_eq!(store.load().unwrap(), None);
    }

    #[test]
    fn saving_and_loading_keeps_the_channels_the_layout_and_the_sound() {
        let dir = tempfile::tempdir().unwrap();
        let store = MultiviewStore::new(dir.path().join("data/multiview.toml"));
        store.save(&set()).unwrap();
        assert_eq!(store.load().unwrap(), Some(set()));
        let text = std::fs::read_to_string(store.path()).unwrap();
        assert!(text.contains("layout = \"spotlight\""), "{text}");
        assert_eq!(text.matches("sound = true").count(), 1, "{text}");

        let grid = SavedSet {
            layout: Layout::Grid(4),
            ..set()
        };
        store.save(&grid).unwrap();
        assert_eq!(store.load().unwrap(), Some(grid));
    }

    #[test]
    fn a_forgiving_load() {
        let text = r#"
            layout = "grid-7"

            [[channel]]
            platform = "youtube"
            login = "someone"

            [[channel]]
            platform = "Twitch"
            login = " Marrow "

            [[channel]]
            platform = "twitch"
            login = "marrow"
            sound = true

            [[channel]]
            platform = "kick"
            login = ""

            [[channel]]
            platform = "kick"
            login = "ziggy_k"
        "#;
        // An unknown layout is the smallest grid, and with the repeat gone nobody has sound.
        assert_eq!(
            multiview_from_toml(text).unwrap(),
            Some(SavedSet {
                channels: vec![ChannelRef::twitch("Marrow"), ChannelRef::kick("ziggy_k")],
                layout: Layout::Grid(2),
                sound: 0,
            })
        );
    }

    #[test]
    fn a_layout_too_small_and_too_many_channels_are_mended() {
        let mut text = String::from("layout = \"grid-2\"\n");
        for i in 0..12 {
            text.push_str(&format!(
                "[[channel]]\nplatform = \"twitch\"\nlogin = \"c{i}\"\n"
            ));
        }
        let set = multiview_from_toml(&text).unwrap().unwrap();
        assert_eq!(set.channels.len(), MAX_STREAMS);
        assert_eq!(set.layout, Layout::Grid(9));
    }

    #[test]
    fn no_channels_is_no_session() {
        assert_eq!(multiview_from_toml("layout = \"grid-4\"").unwrap(), None);
        assert_eq!(multiview_from_toml("").unwrap(), None);
    }

    #[test]
    fn a_corrupt_file_is_moved_aside() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("multiview.toml");
        std::fs::write(&path, "[[channel]\nnot toml").unwrap();
        let store = MultiviewStore::new(&path);
        assert_eq!(store.load_or_default(), None);
        assert!(!path.exists());
        assert!(dir.path().join("multiview.toml.corrupt").exists());
    }
}
