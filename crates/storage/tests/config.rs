//! Config round-trip and corrupted-file handling in a temp dir.

use std::fs;

use htpc_core::{
    CaptionBackground, CaptionPosition, CaptionSize, CaptionsQuality, ChatLayout, Quality, Settings,
};
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
            live_notifications: false,
            background_play: true,
            show_kick: true,
            captions: true,
            captions_quality: CaptionsQuality::Fast,
            caption_delay: false,
            caption_size: CaptionSize::Large,
            caption_position: CaptionPosition::Top,
            caption_background: CaptionBackground::None,
        },
        client_id: Some("abc123".to_owned()),
        image_cache_max_mb: 64,
        quick_messages: vec!["hello".to_owned(), "gg wp".to_owned()],
    };
    store.save(&config).unwrap();
    // A new store on the same path stands in for the next app start.
    let reopened = ConfigStore::new(store.path());
    assert_eq!(reopened.load().unwrap(), config);

    let text = fs::read_to_string(store.path()).unwrap();
    assert!(text.contains("chat_layout = \"overlay\""), "{text}");
    assert!(text.contains("quality = \"720p\""), "{text}");
    assert!(text.contains("live_notifications = false"), "{text}");
    assert!(text.contains("background_play = true"), "{text}");
    assert!(text.contains("show_kick = true"), "{text}");
    assert!(text.contains("captions = true"), "{text}");
    assert!(text.contains("captions_quality = \"fast\""), "{text}");
    assert!(text.contains("caption_delay = false"), "{text}");
    assert!(text.contains("caption_size = \"large\""), "{text}");
    assert!(text.contains("caption_position = \"top\""), "{text}");
    assert!(text.contains("caption_background = \"none\""), "{text}");
}

#[test]
fn caption_looks_default_to_medium_bottom_dark_and_survive_nonsense() {
    let old = Config::from_toml("captions = true\n").unwrap();
    assert_eq!(old.settings.caption_size, CaptionSize::Medium);
    assert_eq!(old.settings.caption_position, CaptionPosition::Bottom);
    assert_eq!(old.settings.caption_background, CaptionBackground::Dark);
    let bad = Config::from_toml("caption_size = \"huge\"\ncaption_position = \"left\"\n").unwrap();
    assert_eq!(bad.settings.caption_size, CaptionSize::Medium);
    assert_eq!(bad.settings.caption_position, CaptionPosition::Bottom);
}

#[test]
fn an_older_file_without_the_caption_delay_has_it_on() {
    let config = Config::from_toml("captions = true\n").unwrap();
    assert!(config.settings.caption_delay);
    let off = Config::from_toml("caption_delay = false\n").unwrap();
    assert!(!off.settings.caption_delay);
}

#[test]
fn captions_quality_defaults_to_standard_and_survives_nonsense() {
    let old = Config::from_toml("captions = true\n").unwrap();
    assert_eq!(old.settings.captions_quality, CaptionsQuality::Standard);
    let best = Config::from_toml("captions_quality = \"Best\"\n").unwrap();
    assert_eq!(best.settings.captions_quality, CaptionsQuality::Best);
    let bad = Config::from_toml("captions_quality = \"huge\"\n").unwrap();
    assert_eq!(bad.settings.captions_quality, CaptionsQuality::Standard);
}

#[test]
fn an_older_file_without_captions_keeps_them_off() {
    let config = Config::from_toml("chat_layout = \"side\"\n").unwrap();
    assert!(!config.settings.captions);
    let on = Config::from_toml("captions = true\n").unwrap();
    assert!(on.settings.captions);
}

#[test]
fn an_older_file_without_show_kick_keeps_kick_off() {
    let config = Config::from_toml("chat_layout = \"side\"\n").unwrap();
    assert!(!config.settings.show_kick);
    let on = Config::from_toml("show_kick = true\n").unwrap();
    assert!(on.settings.show_kick);
}

#[test]
fn an_older_file_without_live_notifications_turns_them_on() {
    let config = Config::from_toml("chat_layout = \"side\"\nquality = \"best\"\n").unwrap();
    assert!(config.settings.live_notifications);
    // Background play is a newer setting still: older files get its default, off.
    assert!(!config.settings.background_play);
    let off = Config::from_toml("live_notifications = false\n").unwrap();
    assert!(!off.settings.live_notifications);
}

#[test]
fn quick_messages_default_and_are_tidied() {
    let missing = Config::from_toml("quality = \"best\"\n").unwrap();
    assert_eq!(
        missing.quick_messages,
        ["GG", "LUL", "Hi!", "o7", "Thanks for the stream!"]
    );
    let mine = Config::from_toml("quick_messages = [\" hi \", \"\", \"o7\"]\n").unwrap();
    assert_eq!(mine.quick_messages, ["hi", "o7"]);
    // An empty list means none, not the defaults.
    let none = Config::from_toml("quick_messages = []\n").unwrap();
    assert!(none.quick_messages.is_empty());
    let long = format!("quick_messages = [\"{}\"]\n", "x".repeat(600));
    assert_eq!(
        Config::from_toml(&long).unwrap().quick_messages[0].len(),
        500
    );
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
