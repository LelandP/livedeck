//! Where files live, from the `directories` crate.
//!
//! | | Linux / SteamOS | Windows |
//! |---|---|---|
//! | config | `$XDG_CONFIG_HOME/livedeck` | `%APPDATA%\livedeck\config` |
//! | data (token fallback, favorites, multiview) | `$XDG_DATA_HOME/livedeck` | `%APPDATA%\livedeck\data` |
//! | cache (images) | `$XDG_CACHE_HOME/livedeck` | `%LOCALAPPDATA%\livedeck\cache` |
//!
//! Inside a Flatpak the XDG variables point into `~/.var/app/<id>/`, which is what we want.

use std::path::{Path, PathBuf};

use crate::{Error, Result};

/// Application name used for the folders and the keyring service.
pub const APP_NAME: &str = "livedeck";

const CONFIG_FILE: &str = "config.toml";
const TOKEN_FILE: &str = "tokens.json";
const FAVORITES_FILE: &str = "favorites.toml";
const MULTIVIEW_FILE: &str = "multiview.toml";
const IMAGE_DIR: &str = "images";

/// The three base folders. Nothing is created until something is written.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Paths {
    pub config_dir: PathBuf,
    pub data_dir: PathBuf,
    pub cache_dir: PathBuf,
}

impl Paths {
    /// The platform folders for this user.
    pub fn from_os() -> Result<Self> {
        let dirs = directories::ProjectDirs::from("", "", APP_NAME).ok_or(Error::NoHomeDir)?;
        Ok(Self {
            config_dir: dirs.config_dir().to_path_buf(),
            data_dir: dirs.data_dir().to_path_buf(),
            cache_dir: dirs.cache_dir().to_path_buf(),
        })
    }

    /// Everything under one root (`root/config`, `root/data`, `root/cache`). For tests and
    /// portable installs.
    pub fn under(root: &Path) -> Self {
        Self {
            config_dir: root.join("config"),
            data_dir: root.join("data"),
            cache_dir: root.join("cache"),
        }
    }

    pub fn config_file(&self) -> PathBuf {
        self.config_dir.join(CONFIG_FILE)
    }

    /// The token fallback file used when no OS credential store is available.
    pub fn token_file(&self) -> PathBuf {
        self.data_dir.join(TOKEN_FILE)
    }

    /// The starred channels. The app writes it often, so it is not in the config folder.
    pub fn favorites_file(&self) -> PathBuf {
        self.data_dir.join(FAVORITES_FILE)
    }

    /// The last multiview session. Written whenever a running one changes, so next to the
    /// favorites.
    pub fn multiview_file(&self) -> PathBuf {
        self.data_dir.join(MULTIVIEW_FILE)
    }

    pub fn image_cache_dir(&self) -> PathBuf {
        self.cache_dir.join(IMAGE_DIR)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn under_root_layout() {
        let p = Paths::under(Path::new("/r"));
        assert_eq!(p.config_file(), Path::new("/r/config/config.toml"));
        assert_eq!(p.token_file(), Path::new("/r/data/tokens.json"));
        assert_eq!(p.favorites_file(), Path::new("/r/data/favorites.toml"));
        assert_eq!(p.multiview_file(), Path::new("/r/data/multiview.toml"));
        assert_eq!(p.image_cache_dir(), Path::new("/r/cache/images"));
    }

    #[test]
    fn os_paths_mention_the_app() {
        // Every CI runner and dev box has a home directory.
        let p = Paths::from_os().unwrap();
        for dir in [&p.config_dir, &p.data_dir, &p.cache_dir] {
            assert!(dir.to_string_lossy().contains(APP_NAME), "{dir:?}");
        }
    }
}
