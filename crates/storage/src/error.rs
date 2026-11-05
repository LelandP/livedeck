//! Error type for every storage operation.

use std::path::PathBuf;

#[derive(Debug, thiserror::Error)]
pub enum Error {
    /// The OS reported no home / profile directory, so there is nowhere to store anything.
    #[error("no home directory found for the config, data and cache folders")]
    NoHomeDir,
    /// A filesystem operation failed.
    #[error("I/O error on {}: {source}", path.display())]
    Io {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },
    /// The config file exists but is not valid TOML for [`crate::Config`].
    #[error("config file {} is invalid: {message}", path.display())]
    CorruptConfig { path: PathBuf, message: String },
    /// A value could not be serialised (should not happen for our own types).
    #[error("could not serialise: {0}")]
    Serialize(String),
    /// The OS credential store failed (other than "no such entry").
    #[error("credential store error: {0}")]
    Keyring(String),
    /// The image download did not complete (DNS, connect, TLS, timeout).
    #[error("image download failed: {0}")]
    Network(#[source] reqwest::Error),
    /// The image server answered with a non-success status.
    #[error("image download failed with HTTP {0}")]
    HttpStatus(u16),
    /// The image is larger than the per-image limit.
    #[error("image is too large ({0} bytes)")]
    TooLarge(u64),
    /// The bytes are not a JPEG, PNG or WebP image we can decode.
    #[error("image could not be decoded: {0}")]
    Decode(String),
}

impl Error {
    pub(crate) fn io(path: impl Into<PathBuf>, source: std::io::Error) -> Self {
        Error::Io {
            path: path.into(),
            source,
        }
    }
}

pub type Result<T, E = Error> = std::result::Result<T, E>;
