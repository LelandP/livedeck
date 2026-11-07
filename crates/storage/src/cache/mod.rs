//! Thumbnail cache: disk LRU with a size cap, fetch via `reqwest`, decode via `image`.
//!
//! [`ImageCache::get`] returns decoded RGBA pixels. A disk hit skips the network. A miss downloads
//! the image, checks that it decodes (so junk is never cached) and stores the original bytes, not
//! the bigger RGBA buffer. A cached file that no longer decodes is deleted and fetched again.
//! The result is UI-agnostic: the UI turns it into a `slint::Image`.
//!
//! Disk IO and decoding run on `spawn_blocking`, so callers on the tokio runtime never block.
//! Two concurrent misses for the same URL both download it, which is harmless (last write wins).

mod lru;

use std::path::Path;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use crate::{Error, Result};
use lru::DiskLru;

/// Largest accepted download. Twitch thumbnails and box art are well under 1 MiB.
pub const MAX_IMAGE_BYTES: u64 = 8 * 1024 * 1024;
const REQUEST_TIMEOUT: Duration = Duration::from_secs(10);

/// A decoded image: `width * height` RGBA8 pixels, row-major.
#[derive(Clone, PartialEq, Eq)]
pub struct DecodedImage {
    pub width: u32,
    pub height: u32,
    pub rgba: Vec<u8>,
}

impl std::fmt::Debug for DecodedImage {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "DecodedImage({}x{})", self.width, self.height)
    }
}

/// Largest decoded size, in pixels on each side. Cards are at most 640 px wide (the hero), so a
/// bigger picture only costs memory and time on its way to the window: a row of full-HD previews
/// made the whole UI sluggish. Smaller images are left as they are.
pub const MAX_DECODED_SIDE: u32 = 640;

/// Decodes JPEG, PNG or WebP bytes, scaled down to fit [`MAX_DECODED_SIDE`].
pub fn decode(bytes: &[u8]) -> Result<DecodedImage> {
    let mut img = image::load_from_memory(bytes).map_err(|e| Error::Decode(e.to_string()))?;
    if img.width() > MAX_DECODED_SIDE || img.height() > MAX_DECODED_SIDE {
        img = img.thumbnail(MAX_DECODED_SIDE, MAX_DECODED_SIDE);
    }
    let rgba = img.to_rgba8();
    Ok(DecodedImage {
        width: rgba.width(),
        height: rgba.height(),
        rgba: rgba.into_raw(),
    })
}

/// Usage numbers, for logs and tests.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CacheStats {
    pub entries: usize,
    pub total_bytes: u64,
    pub max_bytes: u64,
}

/// Cheap to clone: clones share the same index and HTTP client.
#[derive(Debug, Clone)]
pub struct ImageCache {
    lru: Arc<Mutex<DiskLru>>,
    http: reqwest::Client,
}

impl ImageCache {
    /// Opens the cache in `dir` (see `Paths::image_cache_dir`) with the default HTTP client.
    /// Blocking: indexes the folder and evicts down to `max_bytes`.
    pub fn open(dir: &Path, max_bytes: u64) -> Result<Self> {
        let http = reqwest::Client::builder()
            .timeout(REQUEST_TIMEOUT)
            .user_agent(concat!("livedeck/", env!("CARGO_PKG_VERSION")))
            .build()
            .map_err(Error::Network)?;
        Self::with_client(dir, max_bytes, http)
    }

    /// Like [`ImageCache::open`] with a custom client (tests use one without a proxy).
    pub fn with_client(dir: &Path, max_bytes: u64, http: reqwest::Client) -> Result<Self> {
        let lru = DiskLru::open(dir, max_bytes)?;
        Ok(Self {
            lru: Arc::new(Mutex::new(lru)),
            http,
        })
    }

    /// The image at `url`, from disk if cached, else downloaded and cached.
    pub async fn get(&self, url: &str) -> Result<DecodedImage> {
        let key = cache_key(url);
        let k = key.clone();
        if let Some(bytes) = self.blocking(move |lru| lru.get(&k)).await? {
            match unblock(move || decode(&bytes)).await {
                Ok(img) => return Ok(img),
                Err(err) => {
                    tracing::warn!(%url, %err, "cached image is corrupt, fetching it again");
                    let k = key.clone();
                    self.blocking(move |lru| lru.remove(&k)).await?;
                }
            }
        }
        let bytes = self.download(url).await?;
        let (img, bytes) = unblock(move || decode(&bytes).map(|img| (img, bytes))).await?;
        if !self.blocking(move |lru| lru.put(&key, &bytes)).await? {
            tracing::debug!(%url, "image larger than the whole cache, not cached");
        }
        Ok(img)
    }

    /// Makes sure `url` is on disk without returning pixels (thumbnail prefetch).
    pub async fn prefetch(&self, url: &str) -> Result<()> {
        let key = cache_key(url);
        if !self.blocking(move |lru| Ok(lru.contains(&key))).await? {
            self.get(url).await?;
        }
        Ok(())
    }

    pub fn stats(&self) -> CacheStats {
        let lru = self.lock();
        CacheStats {
            entries: lru.len(),
            total_bytes: lru.total_bytes(),
            max_bytes: lru.max_bytes(),
        }
    }

    async fn download(&self, url: &str) -> Result<Vec<u8>> {
        let resp = self.http.get(url).send().await.map_err(Error::Network)?;
        let status = resp.status();
        if !status.is_success() {
            return Err(Error::HttpStatus(status.as_u16()));
        }
        if let Some(len) = resp.content_length().filter(|&l| l > MAX_IMAGE_BYTES) {
            return Err(Error::TooLarge(len));
        }
        let bytes = resp.bytes().await.map_err(Error::Network)?;
        if bytes.len() as u64 > MAX_IMAGE_BYTES {
            return Err(Error::TooLarge(bytes.len() as u64));
        }
        Ok(bytes.to_vec())
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, DiskLru> {
        // A panic while holding the lock leaves the index usable (worst case slightly off).
        self.lru.lock().unwrap_or_else(|p| p.into_inner())
    }

    /// Runs `f` on the index on the blocking pool.
    async fn blocking<T: Send + 'static>(
        &self,
        f: impl FnOnce(&mut DiskLru) -> Result<T> + Send + 'static,
    ) -> Result<T> {
        let this = self.clone();
        unblock(move || f(&mut this.lock())).await
    }
}

async fn unblock<T: Send + 'static>(f: impl FnOnce() -> Result<T> + Send + 'static) -> Result<T> {
    tokio::task::spawn_blocking(f)
        .await
        .unwrap_or_else(|join| Err(Error::Decode(format!("worker task failed: {join}"))))
}

/// A stable file name for `url`: two FNV-1a 64-bit hashes with different offsets, as 32 hex
/// digits. Stable across Rust versions, unlike `DefaultHasher`.
pub(crate) fn cache_key(url: &str) -> String {
    fn fnv1a(offset: u64, bytes: &[u8]) -> u64 {
        bytes.iter().fold(offset, |h, &b| {
            (h ^ u64::from(b)).wrapping_mul(0x0000_0100_0000_01b3)
        })
    }
    let a = fnv1a(0xcbf2_9ce4_8422_2325, url.as_bytes());
    let b = fnv1a(0x8422_2325_cbf2_9ce4, url.as_bytes());
    format!("{a:016x}{b:016x}")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cache_keys_are_stable_hex_and_distinct() {
        let k = cache_key("https://static-cdn.jtvnw.net/a.jpg");
        assert_eq!(k.len(), 32);
        assert!(k.bytes().all(|b| b.is_ascii_hexdigit()));
        assert_eq!(k, cache_key("https://static-cdn.jtvnw.net/a.jpg"));
        assert_ne!(k, cache_key("https://static-cdn.jtvnw.net/b.jpg"));
    }
}
