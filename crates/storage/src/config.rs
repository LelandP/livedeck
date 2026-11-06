//! `config.toml`: user settings plus a few app options.
//!
//! ```toml
//! chat_layout = "side"        # side | overlay
//! quality = "best"            # best | 1080p | 720p | 480p | worst
//! live_notifications = true   # a popup when a followed channel goes live
//! image_cache_max_mb = 256
//! # client_id = "..."         # public Twitch client ID (TWITCH_CLIENT_ID overrides it)
//! ```
//!
//! Loading is forgiving: missing keys take their defaults, unknown keys are ignored (so older
//! builds read newer files), and an unknown enum value falls back to the default with a warning.
//! A file that is not valid TOML is moved aside to `config.toml.corrupt` by
//! [`ConfigStore::load_or_default`].

use std::path::{Path, PathBuf};

use htpc_core::{ChatLayout, Quality, Settings};
use serde::{Deserialize, Serialize};

use crate::fs_util::{self, Access};
use crate::{Error, Result};

/// Default image cache cap in MiB.
pub const DEFAULT_IMAGE_CACHE_MB: u64 = 256;
/// Smallest accepted cache cap. Smaller values in the file are raised to this.
pub const MIN_IMAGE_CACHE_MB: u64 = 16;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Config {
    pub settings: Settings,
    /// Public Twitch application client ID (not a secret, rule 7). `None` = not configured.
    pub client_id: Option<String>,
    /// Image cache size cap in MiB.
    pub image_cache_max_mb: u64,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            settings: Settings::default(),
            client_id: None,
            image_cache_max_mb: DEFAULT_IMAGE_CACHE_MB,
        }
    }
}

impl Config {
    /// The image cache cap in bytes.
    pub fn image_cache_max_bytes(&self) -> u64 {
        self.image_cache_max_mb.saturating_mul(1024 * 1024)
    }

    /// Parses TOML text. Fails only if the text is not valid TOML or a key has the wrong type.
    pub fn from_toml(text: &str) -> std::result::Result<Self, String> {
        let file: ConfigFile = toml::from_str(text).map_err(|e| e.to_string())?;
        Ok(file.into())
    }

    pub fn to_toml(&self) -> Result<String> {
        toml::to_string(&ConfigFile::from(self)).map_err(|e| Error::Serialize(e.to_string()))
    }
}

/// On-disk shape. Enums are stored by name so `htpc-core` needs no serde.
#[derive(Debug, Default, Serialize, Deserialize)]
#[serde(default)]
struct ConfigFile {
    chat_layout: Option<String>,
    quality: Option<String>,
    live_notifications: Option<bool>,
    image_cache_max_mb: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    client_id: Option<String>,
}

/// Parses a named setting, warning and falling back to the default on unknown names.
fn named<T: std::str::FromStr + Default>(key: &str, value: Option<String>) -> T {
    let Some(value) = value else {
        return T::default();
    };
    value.parse().unwrap_or_else(|_| {
        tracing::warn!(key, %value, "unknown config value, using the default");
        T::default()
    })
}

impl From<ConfigFile> for Config {
    fn from(f: ConfigFile) -> Self {
        Self {
            settings: Settings {
                chat_layout: named::<ChatLayout>("chat_layout", f.chat_layout),
                quality: named::<Quality>("quality", f.quality),
                // Files from before the setting existed turn it on, like a fresh install.
                live_notifications: f
                    .live_notifications
                    .unwrap_or(Settings::default().live_notifications),
            },
            client_id: f
                .client_id
                .map(|id| id.trim().to_owned())
                .filter(|id| !id.is_empty()),
            image_cache_max_mb: f
                .image_cache_max_mb
                .unwrap_or(DEFAULT_IMAGE_CACHE_MB)
                .max(MIN_IMAGE_CACHE_MB),
        }
    }
}

impl From<&Config> for ConfigFile {
    fn from(c: &Config) -> Self {
        Self {
            chat_layout: Some(c.settings.chat_layout.as_str().to_owned()),
            quality: Some(c.settings.quality.as_str().to_owned()),
            live_notifications: Some(c.settings.live_notifications),
            image_cache_max_mb: Some(c.image_cache_max_mb),
            client_id: c.client_id.clone(),
        }
    }
}

/// Loads and saves [`Config`] at a fixed path.
#[derive(Debug, Clone)]
pub struct ConfigStore {
    path: PathBuf,
}

impl ConfigStore {
    pub fn new(path: impl Into<PathBuf>) -> Self {
        Self { path: path.into() }
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    /// Strict load: defaults if the file does not exist, [`Error::CorruptConfig`] if it is
    /// invalid, [`Error::Io`] if it cannot be read.
    pub fn load(&self) -> Result<Config> {
        let Some(bytes) = fs_util::read_if_exists(&self.path)? else {
            return Ok(Config::default());
        };
        let text = String::from_utf8(bytes).map_err(|_| Error::CorruptConfig {
            path: self.path.clone(),
            message: "not UTF-8".to_owned(),
        })?;
        Config::from_toml(&text).map_err(|message| Error::CorruptConfig {
            path: self.path.clone(),
            message,
        })
    }

    /// Startup load that never fails: a corrupt file is moved aside to `config.toml.corrupt`
    /// and defaults are used. Read errors are logged and also give defaults.
    pub fn load_or_default(&self) -> Config {
        match self.load() {
            Ok(config) => config,
            Err(Error::CorruptConfig { message, .. }) => {
                match fs_util::quarantine(&self.path) {
                    Ok(moved) => tracing::warn!(
                        %message, moved_to = %moved.display(),
                        "config file is corrupt, using defaults"
                    ),
                    Err(err) => {
                        tracing::warn!(%message, %err, "config file is corrupt, using defaults")
                    }
                }
                Config::default()
            }
            Err(err) => {
                tracing::warn!(%err, "could not read the config file, using defaults");
                Config::default()
            }
        }
    }

    /// Writes the config atomically, creating the folder if needed.
    pub fn save(&self, config: &Config) -> Result<()> {
        let text = config.to_toml()?;
        fs_util::write_atomic(&self.path, text.as_bytes(), Access::Normal)
    }
}
