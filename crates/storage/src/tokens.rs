//! OAuth token persistence: the OS credential store (`keyring`) first, a permission-restricted
//! JSON file as the fallback.
//!
//! Game Mode on SteamOS often has no Secret Service running, so the keyring can be missing at
//! any start. [`TokenStore`] therefore:
//! - **save**: tries the keyring. If that fails, writes the fallback file (0600 on Unix,
//!   user-profile ACL on Windows). After a successful keyring save the file is removed, so no
//!   stale secret lingers on disk.
//! - **load**: keyring first, then the file (it may hold tokens from a start without keyring).
//! - **clear**: removes both.
//!
//! A corrupt entry (not our JSON) is deleted and reported as "signed out": the user just signs
//! in again, which beats failing the start.
//!
//! All calls block (the keyring talks to D-Bus / Credential Manager). From async code use
//! `tokio::task::spawn_blocking`.

use std::fmt;
use std::path::PathBuf;

use serde::{Deserialize, Serialize};

use crate::fs_util::{self, Access};
use crate::paths::APP_NAME;
use crate::{Error, Result};

/// The keyring account name for the Twitch tokens.
pub const KEYRING_USER: &str = "twitch-oauth";

/// OAuth tokens from the Device Code Grant.
#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Tokens {
    pub access_token: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub refresh_token: Option<String>,
    /// Expiry as Unix seconds, if Twitch reported one.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub expires_at: Option<u64>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub scopes: Vec<String>,
    /// The Twitch login these tokens belong to, so a start can show who is signed in without
    /// asking the network. Not a secret.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub login: Option<String>,
    /// The Twitch user id, which the follows endpoints need. Not a secret.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub user_id: Option<String>,
}

impl Tokens {
    /// True if the access token has expired at `now` (Unix seconds). Unknown expiry is "valid".
    pub fn is_expired(&self, now: u64) -> bool {
        self.expires_at.is_some_and(|at| now >= at)
    }
}

/// Secrets never appear in logs.
impl fmt::Debug for Tokens {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Tokens")
            .field("access_token", &"<redacted>")
            .field(
                "refresh_token",
                &self.refresh_token.as_ref().map(|_| "<redacted>"),
            )
            .field("expires_at", &self.expires_at)
            .field("scopes", &self.scopes)
            .field("login", &self.login)
            .field("user_id", &self.user_id)
            .finish()
    }
}

/// Why a backend call failed.
#[derive(Debug)]
pub enum BackendError {
    /// The store does not exist or cannot be reached (no Secret Service, locked, no D-Bus).
    Unavailable(String),
    /// The store works but this operation failed.
    Failed(Error),
}

pub type BackendResult<T> = std::result::Result<T, BackendError>;

/// One place to keep a single secret string: the keyring, the fallback file, or a test fake.
pub trait SecretBackend: Send + Sync {
    /// `Ok(None)` if nothing is stored.
    fn get(&self) -> BackendResult<Option<String>>;
    fn set(&self, secret: &str) -> BackendResult<()>;
    /// Deleting a missing secret succeeds.
    fn delete(&self) -> BackendResult<()>;
    /// For logs.
    fn name(&self) -> &'static str;
}

/// The OS credential store: Windows Credential Manager, macOS Keychain, or the Secret Service
/// on Linux (`keyring` 4, default `v1` feature).
#[derive(Debug, Clone)]
pub struct KeyringBackend {
    service: String,
    user: String,
}

impl KeyringBackend {
    pub fn new(service: impl Into<String>, user: impl Into<String>) -> Self {
        Self {
            service: service.into(),
            user: user.into(),
        }
    }

    fn entry(&self) -> BackendResult<keyring::Entry> {
        keyring::Entry::new(&self.service, &self.user).map_err(map_keyring)
    }
}

impl Default for KeyringBackend {
    fn default() -> Self {
        Self::new(APP_NAME, KEYRING_USER)
    }
}

fn map_keyring(err: keyring::Error) -> BackendError {
    use keyring::Error as K;
    match err {
        K::NoDefaultStore | K::NoStorageAccess(_) | K::PlatformFailure(_) => {
            BackendError::Unavailable(err.to_string())
        }
        other => BackendError::Failed(Error::Keyring(other.to_string())),
    }
}

impl SecretBackend for KeyringBackend {
    fn get(&self) -> BackendResult<Option<String>> {
        match self.entry()?.get_password() {
            Ok(secret) => Ok(Some(secret)),
            Err(keyring::Error::NoEntry) => Ok(None),
            Err(err) => Err(map_keyring(err)),
        }
    }

    fn set(&self, secret: &str) -> BackendResult<()> {
        self.entry()?.set_password(secret).map_err(map_keyring)
    }

    fn delete(&self) -> BackendResult<()> {
        match self.entry()?.delete_credential() {
            Ok(()) | Err(keyring::Error::NoEntry) => Ok(()),
            Err(err) => Err(map_keyring(err)),
        }
    }

    fn name(&self) -> &'static str {
        "keyring"
    }
}

/// The fallback: a file only the owner can read.
#[derive(Debug, Clone)]
pub struct FileBackend {
    path: PathBuf,
}

impl FileBackend {
    pub fn new(path: impl Into<PathBuf>) -> Self {
        Self { path: path.into() }
    }
}

impl SecretBackend for FileBackend {
    fn get(&self) -> BackendResult<Option<String>> {
        let Some(bytes) = fs_util::read_if_exists(&self.path).map_err(BackendError::Failed)? else {
            return Ok(None);
        };
        // Non-UTF-8 is corruption. Pass it on lossily so the JSON check rejects it.
        Ok(Some(String::from_utf8_lossy(&bytes).into_owned()))
    }

    fn set(&self, secret: &str) -> BackendResult<()> {
        fs_util::write_atomic(&self.path, secret.as_bytes(), Access::Private)
            .map_err(BackendError::Failed)
    }

    fn delete(&self) -> BackendResult<()> {
        fs_util::remove_if_exists(&self.path).map_err(BackendError::Failed)
    }

    fn name(&self) -> &'static str {
        "file"
    }
}
/// Which backend holds the tokens after a save.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Location {
    Keyring,
    File,
}

/// Token persistence with keyring-then-file fallback. See the module docs.
pub struct TokenStore {
    primary: Box<dyn SecretBackend>,
    fallback: Box<dyn SecretBackend>,
}

impl fmt::Debug for TokenStore {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("TokenStore")
            .field("primary", &self.primary.name())
            .field("fallback", &self.fallback.name())
            .finish()
    }
}

impl TokenStore {
    /// The OS keyring, with the fallback file at `fallback_file` (see `Paths::token_file`).
    pub fn new(fallback_file: impl Into<PathBuf>) -> Self {
        Self::with_backends(
            Box::new(KeyringBackend::default()),
            Box::new(FileBackend::new(fallback_file)),
        )
    }

    /// Custom backends, for tests.
    pub fn with_backends(
        primary: Box<dyn SecretBackend>,
        fallback: Box<dyn SecretBackend>,
    ) -> Self {
        Self { primary, fallback }
    }

    /// The stored tokens, or `None` when signed out (or the entry was corrupt and got deleted).
    pub fn load(&self) -> Result<Option<Tokens>> {
        match self.primary.get() {
            Ok(Some(text)) => {
                if let Some(tokens) = parse_or_delete(&*self.primary, &text) {
                    return Ok(Some(tokens));
                }
            }
            Ok(None) => {}
            Err(BackendError::Unavailable(why)) => {
                tracing::debug!(%why, "keyring unavailable, reading the token file");
            }
            Err(BackendError::Failed(err)) => {
                tracing::warn!(%err, "keyring read failed, trying the token file");
            }
        }
        match self.fallback.get() {
            Ok(Some(text)) => Ok(parse_or_delete(&*self.fallback, &text)),
            Ok(None) => Ok(None),
            Err(e) => Err(into_error(e)),
        }
    }

    /// Stores `tokens`, replacing any previous ones. Returns where they ended up.
    pub fn save(&self, tokens: &Tokens) -> Result<Location> {
        let json = serde_json::to_string(tokens).map_err(|e| Error::Serialize(e.to_string()))?;
        match self.primary.set(&json) {
            Ok(()) => {
                // A start without keyring may have left a file. Do not keep that secret around.
                if let Err(e) = self.fallback.delete() {
                    tracing::warn!(err = ?e, "could not remove the old token file");
                }
                Ok(Location::Keyring)
            }
            Err(e) => {
                tracing::info!(err = ?e, "keyring unusable, storing tokens in the private file");
                self.fallback.set(&json).map_err(into_error)?;
                Ok(Location::File)
            }
        }
    }

    /// Signs out: removes the tokens from both places. An unavailable keyring is not an error.
    pub fn clear(&self) -> Result<()> {
        match self.primary.delete() {
            Ok(()) | Err(BackendError::Unavailable(_)) => {}
            Err(BackendError::Failed(err)) => return Err(err),
        }
        self.fallback.delete().map_err(into_error)
    }
}

/// Parses stored JSON. Corrupt data is deleted from `backend` and treated as signed out.
fn parse_or_delete(backend: &dyn SecretBackend, text: &str) -> Option<Tokens> {
    match serde_json::from_str::<Tokens>(text) {
        Ok(tokens) if !tokens.access_token.is_empty() => Some(tokens),
        _ => {
            tracing::warn!(
                backend = backend.name(),
                "stored tokens are corrupt, deleting them (sign in again)"
            );
            if let Err(e) = backend.delete() {
                tracing::warn!(err = ?e, "could not delete the corrupt tokens");
            }
            None
        }
    }
}

fn into_error(e: BackendError) -> Error {
    match e {
        BackendError::Unavailable(why) => Error::Keyring(why),
        BackendError::Failed(err) => err,
    }
}
