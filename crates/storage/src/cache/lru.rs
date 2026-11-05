//! A size-capped, least-recently-used set of files in one folder.
//!
//! Each entry is `<key>.img`. The index (size and recency per key) lives in memory and is rebuilt
//! from the folder on open, using file modification times as recency, so the LRU order survives
//! restarts. A hit bumps the file's mtime (best effort) for the same reason.
//!
//! Blocking IO. [`super::ImageCache`] calls it from `spawn_blocking`.

use std::collections::HashMap;
use std::fs::{self, File};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::time::SystemTime;

use crate::fs_util;
use crate::{Error, Result};

const EXT: &str = "img";

#[derive(Debug, Clone, Copy)]
struct Meta {
    size: u64,
    /// Logical clock value of the last use. Higher is more recent.
    used: u64,
}

#[derive(Debug)]
pub(crate) struct DiskLru {
    dir: PathBuf,
    max_bytes: u64,
    entries: HashMap<String, Meta>,
    total: u64,
    clock: u64,
}

impl DiskLru {
    /// Opens (creating) `dir`, indexes existing entries, removes stray temp files and evicts
    /// down to `max_bytes` (the cap may have shrunk since the last run).
    pub(crate) fn open(dir: &Path, max_bytes: u64) -> Result<Self> {
        fs::create_dir_all(dir).map_err(|e| Error::io(dir, e))?;
        let mut found: Vec<(SystemTime, String, u64)> = Vec::new();
        for item in fs::read_dir(dir).map_err(|e| Error::io(dir, e))? {
            let Ok(item) = item else { continue };
            let path = item.path();
            let Ok(meta) = item.metadata() else { continue };
            if !meta.is_file() {
                continue;
            }
            match key_of(&path) {
                Some(key) => {
                    let mtime = meta.modified().unwrap_or(SystemTime::UNIX_EPOCH);
                    found.push((mtime, key, meta.len()));
                }
                // Leftovers of an interrupted write, or foreign files.
                None => {
                    let _ = fs::remove_file(&path);
                }
            }
        }
        found.sort();
        let mut lru = Self {
            dir: dir.to_path_buf(),
            max_bytes,
            entries: HashMap::with_capacity(found.len()),
            total: 0,
            clock: 0,
        };
        for (_, key, size) in found {
            lru.clock += 1;
            lru.total += size;
            let used = lru.clock;
            lru.entries.insert(key, Meta { size, used });
        }
        lru.evict_to(max_bytes);
        Ok(lru)
    }

    pub(crate) fn total_bytes(&self) -> u64 {
        self.total
    }

    pub(crate) fn max_bytes(&self) -> u64 {
        self.max_bytes
    }

    pub(crate) fn len(&self) -> usize {
        self.entries.len()
    }

    pub(crate) fn contains(&self, key: &str) -> bool {
        self.entries.contains_key(key)
    }

    /// Reads an entry and marks it most recently used. `Ok(None)` on a miss. An entry whose
    /// file vanished is dropped from the index.
    pub(crate) fn get(&mut self, key: &str) -> Result<Option<Vec<u8>>> {
        if !self.entries.contains_key(key) {
            return Ok(None);
        }
        let path = self.path(key);
        match fs_util::read_if_exists(&path)? {
            Some(bytes) => {
                self.touch(key, &path);
                Ok(Some(bytes))
            }
            None => {
                self.forget(key);
                Ok(None)
            }
        }
    }

    /// Stores an entry (replacing an old one) and evicts least recently used entries until the
    /// total fits the cap. Entries larger than the whole cap are not stored. Returns whether the
    /// entry was stored.
    pub(crate) fn put(&mut self, key: &str, bytes: &[u8]) -> Result<bool> {
        let size = bytes.len() as u64;
        self.remove(key)?;
        if size > self.max_bytes {
            return Ok(false);
        }
        self.evict_to(self.max_bytes - size);
        write_unsynced(&self.path(key), bytes)?;
        self.clock += 1;
        self.total += size;
        let used = self.clock;
        self.entries.insert(key.to_owned(), Meta { size, used });
        Ok(true)
    }

    /// Deletes an entry (for example after it failed to decode).
    pub(crate) fn remove(&mut self, key: &str) -> Result<()> {
        if self.entries.contains_key(key) {
            fs_util::remove_if_exists(&self.path(key))?;
            self.forget(key);
        }
        Ok(())
    }

    fn evict_to(&mut self, limit: u64) {
        while self.total > limit {
            let Some(oldest) = self
                .entries
                .iter()
                .min_by_key(|(_, m)| m.used)
                .map(|(k, _)| k.clone())
            else {
                break;
            };
            if let Err(err) = fs_util::remove_if_exists(&self.path(&oldest)) {
                // Forget it anyway: the next open re-indexes the folder.
                tracing::warn!(%err, "could not evict a cached image");
            }
            tracing::trace!(key = %oldest, "evicted");
            self.forget(&oldest);
        }
    }

    fn touch(&mut self, key: &str, path: &Path) {
        self.clock += 1;
        if let Some(m) = self.entries.get_mut(key) {
            m.used = self.clock;
        }
        // Best effort: persist the recency for the next start.
        if let Ok(f) = File::options().write(true).open(path) {
            let _ = f.set_modified(SystemTime::now());
        }
    }

    fn forget(&mut self, key: &str) {
        if let Some(m) = self.entries.remove(key) {
            self.total -= m.size;
        }
    }

    fn path(&self, key: &str) -> PathBuf {
        self.dir.join(format!("{key}.{EXT}"))
    }
}

/// The key of a cache file name, or `None` for anything that is not `<hex>.img`.
fn key_of(path: &Path) -> Option<String> {
    if path.extension()? != EXT {
        return None;
    }
    let stem = path.file_stem()?.to_str()?;
    (!stem.is_empty() && stem.bytes().all(|b| b.is_ascii_hexdigit())).then(|| stem.to_owned())
}

/// Temp file + rename, without fsync: a lost cache entry after a power cut is harmless, and an
/// fsync per thumbnail is slow.
fn write_unsynced(path: &Path, bytes: &[u8]) -> Result<()> {
    let tmp = path.with_extension("tmp");
    let result = File::create(&tmp)
        .and_then(|mut f| f.write_all(bytes))
        .and_then(|()| fs::rename(&tmp, path));
    if let Err(e) = result {
        let _ = fs::remove_file(&tmp);
        return Err(Error::io(path, e));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn files(dir: &Path) -> usize {
        fs::read_dir(dir).unwrap().count()
    }

    #[test]
    fn evicts_least_recently_used_to_stay_under_cap() {
        let dir = tempfile::tempdir().unwrap();
        let mut lru = DiskLru::open(dir.path(), 30).unwrap();
        lru.put("aa", &[1; 10]).unwrap();
        lru.put("bb", &[2; 10]).unwrap();
        lru.put("cc", &[3; 10]).unwrap();
        // Use "aa", so "bb" is now the oldest.
        assert_eq!(lru.get("aa").unwrap(), Some(vec![1; 10]));
        lru.put("dd", &[4; 10]).unwrap();
        assert!(lru.contains("aa") && lru.contains("cc") && lru.contains("dd"));
        assert!(!lru.contains("bb"));
        assert_eq!(lru.total_bytes(), 30);
        assert_eq!(files(dir.path()), 3);
    }

    #[test]
    fn replacing_an_entry_updates_the_size() {
        let dir = tempfile::tempdir().unwrap();
        let mut lru = DiskLru::open(dir.path(), 100).unwrap();
        lru.put("aa", &[1; 40]).unwrap();
        lru.put("aa", &[1; 5]).unwrap();
        assert_eq!((lru.len(), lru.total_bytes()), (1, 5));
    }

    #[test]
    fn oversized_entries_are_not_stored() {
        let dir = tempfile::tempdir().unwrap();
        let mut lru = DiskLru::open(dir.path(), 10).unwrap();
        lru.put("aa", &[1; 5]).unwrap();
        assert!(!lru.put("bb", &[2; 11]).unwrap());
        assert!(lru.contains("aa"), "a rejected entry evicts nothing");
        assert_eq!(lru.total_bytes(), 5);
    }

    #[test]
    fn reopen_reindexes_cleans_up_and_applies_a_smaller_cap() {
        let dir = tempfile::tempdir().unwrap();
        {
            let mut lru = DiskLru::open(dir.path(), 100).unwrap();
            for key in ["01", "02", "03", "04"] {
                lru.put(key, &[0; 10]).unwrap();
            }
        }
        fs::write(dir.path().join("01.tmp"), b"partial").unwrap();
        fs::write(dir.path().join("notes.txt"), b"junk").unwrap();

        let lru = DiskLru::open(dir.path(), 25).unwrap();
        assert_eq!(lru.len(), 2);
        assert!(lru.total_bytes() <= 25);
        assert_eq!(files(dir.path()), 2, "temp and foreign files removed");
    }

    #[test]
    fn a_vanished_file_is_a_miss() {
        let dir = tempfile::tempdir().unwrap();
        let mut lru = DiskLru::open(dir.path(), 100).unwrap();
        lru.put("aa", &[1; 10]).unwrap();
        fs::remove_file(dir.path().join("aa.img")).unwrap();
        assert_eq!(lru.get("aa").unwrap(), None);
        assert_eq!((lru.len(), lru.total_bytes()), (0, 0));
    }
}
