//! Config round-trip and corrupted-file handling in a temp dir.

use std::fs;

use htpc_core::{ChatLayout, Quality, Settings};
use htpc_storage::{Config, ConfigStore, Error, Paths, DEFAULT_IMAGE_CACHE_MB, MIN_IMAGE_CACHE_MB};

fn store() -> (tempfile::TempDir, ConfigStore) {
    let dir = tempfile::tempdir().unwrap();
    let store = ConfigStore::new(Paths::under(dir.path()).config_file());
    (dir, store)
}

#[test]
fn missing_file_gives_defaults_without_creating_it() {
    let (_dir, store) = store();
    assert_eq!(store.load().unwrap(), Config::default());
    assert_eq!(store.load_or_default(), Config::default());
    assert!(!store.path().exists());
}

#[test]
fn round_trip_survives_a_restart() {
    let (_dir, store) = store();
    let config = Config {
        settings: Settings {
            chat_layout: ChatLayout::Overlay,
            quality: Quality::P720,
        },
        client_id: Some("abc123".to_owned()),
        image_cache_max_mb: 64,
    };
    store.save(&config).unwrap();
    // A new store on the same path stands in for the next app start.
    let reopened = ConfigStore::new(store.path());
    assert_eq!(reopened.load().unwrap(), config);

    let text = fs::read_to_string(store.path()).unwrap();
    assert!(text.contains("chat_layout = \"overlay\""), "{text}");
    assert!(text.contains("quality = \"720p\""), "{text}");
}

#[test]
fn save_overwrites_and_omits_an_unset_client_id() {
    let (_dir, store) = store();
    store
        .save(&Config {
            client_id: Some("x".to_owned()),
            ..Config::default()
        })
        .unwrap();
    store.save(&Config::default()).unwrap();
    assert_eq!(store.load().unwrap(), Config::default());
    assert!(!fs::read_to_string(store.path())
        .unwrap()
        .contains("client_id"));
}

#[test]
fn partial_and_unknown_values_fall_back_to_defaults() {
    let config = Config::from_toml(
        "quality = \"9000p\"\nchat_layout = \"Overlay\"\nfuture_key = true\nimage_cache_max_mb = 1\nclient_id = \"  \"\n",
    )
    .unwrap();
    assert_eq!(config.settings.quality, Quality::Best);
    assert_eq!(config.settings.chat_layout, ChatLayout::Overlay);
    assert_eq!(config.image_cache_max_mb, MIN_IMAGE_CACHE_MB);
    assert_eq!(config.client_id, None);
    assert_eq!(
        Config::from_toml("").unwrap().image_cache_max_mb,
        DEFAULT_IMAGE_CACHE_MB
    );
}

#[test]
fn corrupt_file_is_an_error_on_strict_load() {
    let (_dir, store) = store();
    fs::create_dir_all(store.path().parent().unwrap()).unwrap();
    fs::write(store.path(), "chat_layout = [unclosed").unwrap();
    assert!(matches!(store.load(), Err(Error::CorruptConfig { .. })));

    // Wrong types are corruption too.
    fs::write(store.path(), "image_cache_max_mb = \"big\"").unwrap();
    assert!(matches!(store.load(), Err(Error::CorruptConfig { .. })));

    fs::write(store.path(), [0xff, 0xfe, 0x00]).unwrap();
    assert!(matches!(store.load(), Err(Error::CorruptConfig { .. })));
}

#[test]
fn corrupt_file_is_quarantined_and_defaults_are_used() {
    let (_dir, store) = store();
    fs::create_dir_all(store.path().parent().unwrap()).unwrap();
    fs::write(store.path(), "not = valid = toml").unwrap();

    assert_eq!(store.load_or_default(), Config::default());
    assert!(!store.path().exists());
    let corrupt = store.path().with_file_name("config.toml.corrupt");
    assert_eq!(fs::read_to_string(&corrupt).unwrap(), "not = valid = toml");

    // The next save works and the next start reads it.
    store.save(&Config::default()).unwrap();
    assert_eq!(store.load().unwrap(), Config::default());
}

#[test]
fn cache_cap_in_bytes() {
    let config = Config {
        image_cache_max_mb: 2,
        ..Config::default()
    };
    assert_eq!(config.image_cache_max_bytes(), 2 * 1024 * 1024);
}
