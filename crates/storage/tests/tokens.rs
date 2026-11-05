//! TokenStore: keyring path, file fallback, restart persistence and corrupted data.
//! The keyring is a fake, so tests never touch the real OS credential store.

use std::fs;
use std::sync::{Arc, Mutex};

use htpc_storage::{
    BackendError, BackendResult, FileBackend, Location, Paths, SecretBackend, TokenStore, Tokens,
};

/// In-memory keyring. Shared state so a "restart" (new store) sees the same secrets.
#[derive(Clone, Default)]
struct FakeKeyring {
    secret: Arc<Mutex<Option<String>>>,
    available: bool,
}

impl FakeKeyring {
    fn working() -> Self {
        Self {
            available: true,
            ..Self::default()
        }
    }

    fn missing() -> Self {
        Self::default()
    }

    fn check(&self) -> BackendResult<()> {
        if self.available {
            Ok(())
        } else {
            Err(BackendError::Unavailable("no Secret Service".to_owned()))
        }
    }

    fn stored(&self) -> Option<String> {
        self.secret.lock().unwrap().clone()
    }
}

impl SecretBackend for FakeKeyring {
    fn get(&self) -> BackendResult<Option<String>> {
        self.check()?;
        Ok(self.stored())
    }

    fn set(&self, secret: &str) -> BackendResult<()> {
        self.check()?;
        *self.secret.lock().unwrap() = Some(secret.to_owned());
        Ok(())
    }

    fn delete(&self) -> BackendResult<()> {
        self.check()?;
        *self.secret.lock().unwrap() = None;
        Ok(())
    }

    fn name(&self) -> &'static str {
        "fake-keyring"
    }
}

fn tokens() -> Tokens {
    Tokens {
        access_token: "access-123".to_owned(),
        refresh_token: Some("refresh-456".to_owned()),
        expires_at: Some(1_900_000_000),
        scopes: vec!["user:read:follows".to_owned()],
        login: Some("nightowl_dev".to_owned()),
        user_id: Some("141981764".to_owned()),
    }
}

fn store(keyring: &FakeKeyring, root: &std::path::Path) -> TokenStore {
    TokenStore::with_backends(
        Box::new(keyring.clone()),
        Box::new(FileBackend::new(Paths::under(root).token_file())),
    )
}

#[test]
fn keyring_round_trip_survives_a_restart() {
    let dir = tempfile::tempdir().unwrap();
    let keyring = FakeKeyring::working();
    assert_eq!(store(&keyring, dir.path()).load().unwrap(), None);
    assert_eq!(
        store(&keyring, dir.path()).save(&tokens()).unwrap(),
        Location::Keyring
    );
    assert_eq!(store(&keyring, dir.path()).load().unwrap(), Some(tokens()));
    assert!(!Paths::under(dir.path()).token_file().exists());
}

#[test]
fn falls_back_to_the_file_without_a_keyring() {
    let dir = tempfile::tempdir().unwrap();
    let keyring = FakeKeyring::missing();
    let file = Paths::under(dir.path()).token_file();

    assert_eq!(
        store(&keyring, dir.path()).save(&tokens()).unwrap(),
        Location::File
    );
    assert!(file.exists());
    assert_eq!(store(&keyring, dir.path()).load().unwrap(), Some(tokens()));

    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mode = fs::metadata(&file).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode, 0o600, "token file must be owner-only");
    }
}

#[test]
fn file_tokens_are_found_and_cleaned_up_once_the_keyring_returns() {
    let dir = tempfile::tempdir().unwrap();
    let file = Paths::under(dir.path()).token_file();
    // First start: Game Mode without Secret Service.
    store(&FakeKeyring::missing(), dir.path())
        .save(&tokens())
        .unwrap();

    // Next start: the keyring works but is empty. Tokens come from the file.
    let keyring = FakeKeyring::working();
    let s = store(&keyring, dir.path());
    assert_eq!(s.load().unwrap(), Some(tokens()));

    // A refresh saves to the keyring and removes the stale file.
    assert_eq!(s.save(&tokens()).unwrap(), Location::Keyring);
    assert!(!file.exists());
    assert!(keyring.stored().is_some());
}

#[test]
fn clear_removes_both_copies() {
    let dir = tempfile::tempdir().unwrap();
    let file = Paths::under(dir.path()).token_file();
    store(&FakeKeyring::missing(), dir.path())
        .save(&tokens())
        .unwrap();
    let keyring = FakeKeyring::working();
    keyring.set("{\"access_token\":\"k\"}").unwrap();

    store(&keyring, dir.path()).clear().unwrap();
    assert!(keyring.stored().is_none());
    assert!(!file.exists());
    assert_eq!(store(&keyring, dir.path()).load().unwrap(), None);

    // Clearing with no keyring and no file is fine too.
    store(&FakeKeyring::missing(), dir.path()).clear().unwrap();
}

#[test]
fn corrupt_file_is_deleted_and_reads_as_signed_out() {
    let dir = tempfile::tempdir().unwrap();
    let file = Paths::under(dir.path()).token_file();
    fs::create_dir_all(file.parent().unwrap()).unwrap();
    for junk in [
        &b"{not json"[..],
        b"{\"access_token\":\"\"}",
        &[0xff, 0x00, 0xfe],
    ] {
        fs::write(&file, junk).unwrap();
        let s = store(&FakeKeyring::missing(), dir.path());
        assert_eq!(s.load().unwrap(), None, "{junk:?}");
        assert!(!file.exists(), "corrupt token file is removed");
    }
}

#[test]
fn corrupt_keyring_entry_falls_through_to_the_file() {
    let dir = tempfile::tempdir().unwrap();
    store(&FakeKeyring::missing(), dir.path())
        .save(&tokens())
        .unwrap();
    let keyring = FakeKeyring::working();
    keyring.set("garbage").unwrap();

    assert_eq!(store(&keyring, dir.path()).load().unwrap(), Some(tokens()));
    assert!(keyring.stored().is_none(), "corrupt keyring entry deleted");
}

#[test]
fn debug_output_redacts_secrets() {
    let text = format!("{:?}", tokens());
    assert!(
        !text.contains("access-123") && !text.contains("refresh-456"),
        "{text}"
    );
    assert!(text.contains("user:read:follows"));
}

/// Uses the real OS credential store under a throwaway account name. Opt-in, because CI runners
/// have no unlocked keyring: `cargo test -p htpc-storage --test tokens -- --ignored live_`.
#[test]
#[ignore]
fn live_keyring_round_trip() {
    use htpc_storage::KeyringBackend;
    let dir = tempfile::tempdir().unwrap();
    let s = TokenStore::with_backends(
        Box::new(KeyringBackend::new("livedeck-test", "live-test")),
        Box::new(FileBackend::new(dir.path().join("tokens.json"))),
    );
    assert_eq!(s.save(&tokens()).unwrap(), Location::Keyring);
    assert_eq!(s.load().unwrap(), Some(tokens()));
    s.clear().unwrap();
    assert_eq!(s.load().unwrap(), None);
}

#[test]
fn expiry() {
    let t = tokens();
    assert!(!t.is_expired(1_899_999_999));
    assert!(t.is_expired(1_900_000_000));
    let no_expiry = Tokens {
        expires_at: None,
        ..tokens()
    };
    assert!(!no_expiry.is_expired(u64::MAX));
}
