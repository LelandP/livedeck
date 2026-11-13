//! `continue.toml`: where each video was left (*Continue watching*), newest first.
//!
//! ```toml
//! [[video]]
//! id = "2926143952"        # Twitch's video id: digits only
//! login = "summit1g"
//! name = "summit1g"        # the channel's name as the streamer writes it
//! kind = "archive"         # or "highlight"
//! title = "Back at it"
//! category = "Hollow Knight"
//! length = 15120           # seconds
//! created = 1_794_000_000  # Unix seconds
//! views = 18_000
//! thumbnail = "https://…"  # left out when there is none
//! tint = 8_421_504         # 0xRRGGBB, the card's placeholder colour
//! muted = [[720, 180]]     # [start, length] in seconds
//! pos = 4360               # where it was left, in seconds
//! saved = 1_794_100_000    # Unix seconds
//! ```
//!
//! The app writes it every time a video is left, so it lives in the data folder next to
//! `favorites.toml`, off the async workers. Whole cards are kept so Home's row needs no request.
//! Loading is forgiving: an entry with a bad id (not 1 to 12 digits) or no login is dropped with a
//! warning, repeats keep the newest, the list is cut to ten, a place past the end is moved back
//! to it, and a file that is not valid TOML is moved aside to `continue.toml.corrupt`.

use std::path::{Path, PathBuf};

use htpc_core::{ChannelRef, Progress, Rgb, Span, Video, VideoKind, CONTINUE_MAX};
use serde::{Deserialize, Serialize};

use crate::fs_util::{self, Access};
use crate::{Error, Result};

/// Twitch video ids are at most this many digits (the resolver checks the same).
const MAX_ID_LEN: usize = 12;

#[derive(Debug, Default, Serialize, Deserialize)]
#[serde(default)]
struct ContinueFile {
    #[serde(rename = "video")]
    videos: Vec<Entry>,
}

#[derive(Debug, Default, Serialize, Deserialize)]
#[serde(default)]
struct Entry {
    id: String,
    login: String,
    name: String,
    kind: String,
    title: String,
    category: String,
    length: u32,
    created: u64,
    views: u64,
    #[serde(skip_serializing_if = "String::is_empty")]
    thumbnail: String,
    tint: u32,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    muted: Vec<[u32; 2]>,
    pos: u32,
    saved: u64,
}

fn kind_name(kind: VideoKind) -> &'static str {
    match kind {
        VideoKind::Archive => "archive",
        VideoKind::Highlight => "highlight",
    }
}

fn kind_of(name: &str) -> VideoKind {
    match name.trim().to_ascii_lowercase().as_str() {
        "highlight" => VideoKind::Highlight,
        _ => VideoKind::Archive,
    }
}

fn pack(rgb: Rgb) -> u32 {
    (u32::from(rgb.r) << 16) | (u32::from(rgb.g) << 8) | u32::from(rgb.b)
}

fn entry_of(p: &Progress) -> Entry {
    let v = &p.video;
    Entry {
        id: v.id.clone(),
        login: v.channel.login.clone(),
        name: v.channel_name.clone(),
        kind: kind_name(v.kind).to_owned(),
        title: v.title.clone(),
        category: v.category.clone(),
        length: v.length_s,
        created: v.created_at,
        views: v.views,
        thumbnail: v.thumbnail.clone().unwrap_or_default(),
        tint: pack(v.tint),
        muted: v.muted.iter().map(|m| [m.start_s, m.length_s]).collect(),
        pos: p.pos_s,
        saved: p.saved_at,
    }
}

/// `None` for an entry that cannot be a video of Twitch.
fn progress_of(entry: Entry) -> Option<Progress> {
    let id = entry.id.trim();
    let login = entry.login.trim();
    if id.is_empty() || id.len() > MAX_ID_LEN || !id.bytes().all(|b| b.is_ascii_digit()) {
        tracing::warn!(id, "bad video id in continue.toml, skipped");
        return None;
    }
    if login.is_empty() {
        tracing::warn!(id, "no channel in continue.toml, skipped");
        return None;
    }
    let pos_s = if entry.length > 0 {
        entry.pos.min(entry.length)
    } else {
        entry.pos
    };
    Some(Progress {
        video: Video {
            id: id.to_owned(),
            channel: ChannelRef::twitch(login),
            channel_name: entry.name,
            kind: kind_of(&entry.kind),
            title: entry.title,
            category: entry.category,
            length_s: entry.length,
            created_at: entry.created,
            views: entry.views,
            tint: Rgb::from_hex(entry.tint & 0x00ff_ffff),
            thumbnail: Some(entry.thumbnail).filter(|t| !t.trim().is_empty()),
            muted: entry
                .muted
                .into_iter()
                .map(|[start_s, length_s]| Span { start_s, length_s })
                .collect(),
            // A video that cannot be played is never saved, and a broadcast's length is settled
            // by the time it is left.
            sub_only: false,
            recording: false,
        },
        pos_s,
        saved_at: entry.saved,
    })
}

/// Parses the file's text. Fails only if it is not valid TOML or a key has the wrong type.
pub fn continue_from_toml(text: &str) -> std::result::Result<Vec<Progress>, String> {
    let file: ContinueFile = toml::from_str(text).map_err(|e| e.to_string())?;
    let mut list: Vec<Progress> = file.videos.into_iter().filter_map(progress_of).collect();
    // Newest first, whatever order a hand edit left (stable: equal times keep the file's order).
    list.sort_by_key(|p| std::cmp::Reverse(p.saved_at));
    let mut seen = std::collections::HashSet::new();
    list.retain(|p| seen.insert(p.video.id.clone()));
    list.truncate(CONTINUE_MAX);
    Ok(list)
}

pub fn continue_to_toml(list: &[Progress]) -> Result<String> {
    let file = ContinueFile {
        videos: list.iter().take(CONTINUE_MAX).map(entry_of).collect(),
    };
    toml::to_string(&file).map_err(|e| Error::Serialize(e.to_string()))
}

/// Loads and saves *Continue watching* at a fixed path.
#[derive(Debug, Clone)]
pub struct ContinueStore {
    path: PathBuf,
}

impl ContinueStore {
    pub fn new(path: impl Into<PathBuf>) -> Self {
        Self { path: path.into() }
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    /// Strict load: nothing if the file does not exist, [`Error::CorruptConfig`] if it is
    /// invalid, [`Error::Io`] if it cannot be read.
    pub fn load(&self) -> Result<Vec<Progress>> {
        let Some(bytes) = fs_util::read_if_exists(&self.path)? else {
            return Ok(Vec::new());
        };
        let text = String::from_utf8(bytes).map_err(|_| Error::CorruptConfig {
            path: self.path.clone(),
            message: "not UTF-8".to_owned(),
        })?;
        continue_from_toml(&text).map_err(|message| Error::CorruptConfig {
            path: self.path.clone(),
            message,
        })
    }

    /// Startup load that never fails: a corrupt file is moved aside to `continue.toml.corrupt`
    /// and the list starts empty. Read errors are logged and also give an empty list.
    pub fn load_or_default(&self) -> Vec<Progress> {
        match self.load() {
            Ok(list) => list,
            Err(Error::CorruptConfig { message, .. }) => {
                match fs_util::quarantine(&self.path) {
                    Ok(moved) => tracing::warn!(
                        %message, moved_to = %moved.display(),
                        "continue file is corrupt, starting without it"
                    ),
                    Err(err) => tracing::warn!(
                        %message, %err,
                        "continue file is corrupt, starting without it"
                    ),
                }
                Vec::new()
            }
            Err(err) => {
                tracing::warn!(%err, "could not read the continue file");
                Vec::new()
            }
        }
    }

    /// Writes the list atomically, creating the folder if needed.
    pub fn save(&self, list: &[Progress]) -> Result<()> {
        let text = continue_to_toml(list)?;
        fs_util::write_atomic(&self.path, text.as_bytes(), Access::Normal)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn video(id: &str) -> Video {
        Video {
            id: id.to_owned(),
            channel: ChannelRef::twitch("sleepy_fox"),
            channel_name: "Sleepy_Fox".to_owned(),
            kind: VideoKind::Highlight,
            title: "Boss \"rush\" night".to_owned(),
            category: "Hollow Knight".to_owned(),
            length_s: 7_200,
            created_at: 1_794_000_000,
            views: 1_234,
            tint: Rgb::new(0x12, 0x34, 0x56),
            thumbnail: Some("https://cdn.example/t.jpg".to_owned()),
            muted: vec![Span {
                start_s: 720,
                length_s: 180,
            }],
            sub_only: false,
            recording: false,
        }
    }

    fn at(id: &str, pos_s: u32, saved_at: u64) -> Progress {
        Progress {
            video: video(id),
            pos_s,
            saved_at,
        }
    }

    #[test]
    fn a_missing_file_is_nothing_to_continue() {
        let dir = tempfile::tempdir().unwrap();
        let store = ContinueStore::new(dir.path().join("continue.toml"));
        assert_eq!(store.load().unwrap(), Vec::new());
    }

    #[test]
    fn saving_and_loading_keeps_every_card_field_and_the_order() {
        let dir = tempfile::tempdir().unwrap();
        let store = ContinueStore::new(dir.path().join("data/continue.toml"));
        let list = vec![
            at("300", 4_360, 30),
            at("200", 10, 20),
            at("100", 7_000, 10),
        ];
        store.save(&list).unwrap();
        assert_eq!(store.load().unwrap(), list);
        let text = std::fs::read_to_string(store.path()).unwrap();
        assert!(text.contains("[[video]]"), "{text}");
        assert!(text.contains("kind = \"highlight\""), "{text}");
    }

    #[test]
    fn a_card_without_a_thumbnail_or_muted_parts_round_trips() {
        let mut p = at("5", 60, 1);
        p.video.thumbnail = None;
        p.video.muted.clear();
        let text = continue_to_toml(std::slice::from_ref(&p)).unwrap();
        assert!(!text.contains("thumbnail"), "{text}");
        assert!(!text.contains("muted"), "{text}");
        assert_eq!(continue_from_toml(&text).unwrap(), [p]);
    }

    #[test]
    fn bad_ids_blank_channels_and_repeats_are_dropped() {
        let text = r#"
            [[video]]
            id = "111"
            login = "a"
            length = 1000
            pos = 5
            saved = 5

            [[video]]
            id = "../etc"
            login = "a"

            [[video]]
            id = ""
            login = "a"

            [[video]]
            id = "1234567890123"
            login = "a"

            [[video]]
            id = "222"
            login = "  "

            [[video]]
            id = "111"
            login = "a"
            length = 1000
            pos = 9
            saved = 9
        "#;
        let list = continue_from_toml(text).unwrap();
        // The repeat that was saved last wins.
        assert_eq!(list.len(), 1);
        assert_eq!((list[0].video.id.as_str(), list[0].pos_s), ("111", 9));
    }

    #[test]
    fn more_than_ten_are_cut_and_a_hand_edit_is_put_in_newest_first_order() {
        let mut text = String::new();
        for i in 1..=12u64 {
            text.push_str(&format!(
                "[[video]]\nid = \"{i}\"\nlogin = \"a\"\nlength = 600\nsaved = {i}\n\n"
            ));
        }
        let list = continue_from_toml(&text).unwrap();
        let ids: Vec<&str> = list.iter().map(|p| p.video.id.as_str()).collect();
        assert_eq!(ids, ["12", "11", "10", "9", "8", "7", "6", "5", "4", "3"]);
    }

    #[test]
    fn a_place_past_the_end_goes_back_to_it_and_an_unknown_kind_is_a_broadcast() {
        let text = r#"
            [[video]]
            id = "7"
            login = "a"
            kind = "upload"
            length = 100
            pos = 900
        "#;
        let list = continue_from_toml(text).unwrap();
        assert_eq!(list[0].pos_s, 100);
        assert_eq!(list[0].video.kind, VideoKind::Archive);
    }

    #[test]
    fn writing_never_keeps_more_than_ten() {
        let list: Vec<Progress> = (1..=14).map(|i| at(&i.to_string(), 60, 100 - i)).collect();
        let text = continue_to_toml(&list).unwrap();
        assert_eq!(continue_from_toml(&text).unwrap().len(), 10);
    }

    #[test]
    fn a_corrupt_file_is_moved_aside_and_starts_empty() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("continue.toml");
        std::fs::write(&path, "[[video]\nnot toml").unwrap();
        let store = ContinueStore::new(&path);
        assert!(store.load_or_default().is_empty());
        assert!(!path.exists());
        assert!(dir.path().join("continue.toml.corrupt").exists());
    }

    #[test]
    fn a_wrong_type_is_corrupt_too() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("continue.toml");
        std::fs::write(
            &path,
            "[[video]]\nid = \"1\"\nlogin = \"a\"\npos = \"soon\"\n",
        )
        .unwrap();
        let store = ContinueStore::new(&path);
        assert!(store.load_or_default().is_empty());
        assert!(dir.path().join("continue.toml.corrupt").exists());
    }
}
