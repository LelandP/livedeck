//! The speech models of the captions (captions-plan.md, phase 4): which files each quality needs,
//! and downloading them into the data folder the first time they are used.
//!
//! - [`files`] is the catalogue: the voice-activity model every quality needs and the Whisper model
//!   of the quality, each with its address, size and SHA-256. Addresses are pinned to a commit of
//!   the Hugging Face repository, and the hash is checked anyway.
//! - [`Models`] finds, checks and downloads them. A file is streamed into `<name>.part`, hashed as
//!   it arrives, and renamed once the hash is right. A file that is there with the right size is
//!   hashed once, and the answer is remembered for the run. A download that stops (a lost
//!   connection, a cancel, the app closing) leaves its `.part` file, and the next one asks the
//!   server for the rest. One download runs at a time.
//! - `HTPC_CAPTIONS_MODEL` and `HTPC_CAPTIONS_VAD_MODEL` (both set, both files there) replace the
//!   catalogue, for development: nothing is checked or downloaded then.

// Without the `captions` feature nothing makes a Whisper engine, so nothing here is used.
#![cfg_attr(not(feature = "captions"), allow(dead_code))]

use std::collections::HashSet;
use std::fs::OpenOptions;
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::time::Duration;

use htpc_core::CaptionsQuality;
use sha2::{Digest, Sha256};

/// The Whisper model's environment variable (a ggml file such as `ggml-base.en-q5_1.bin`).
pub const ENV_MODEL: &str = "HTPC_CAPTIONS_MODEL";
/// The voice-activity model's (a ggml Silero file such as `ggml-silero-v5.1.2.bin`).
pub const ENV_VAD_MODEL: &str = "HTPC_CAPTIONS_VAD_MODEL";

/// A model file to download.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ModelFile {
    /// The file name in the data folder.
    pub name: &'static str,
    pub url: &'static str,
    pub size: u64,
    /// Lowercase hex.
    pub sha256: &'static str,
}

/// Silero voice-activity detection, for every quality: silence and music never reach Whisper.
pub const VAD: ModelFile = ModelFile {
    name: "ggml-silero-v5.1.2.bin",
    url: "https://huggingface.co/ggml-org/whisper-vad/resolve/9ffd54a1e1ee413ddf265af9913beaf518d1639b/ggml-silero-v5.1.2.bin",
    size: 885_098,
    sha256: "29940d98d42b91fbd05ce489f3ecf7c72f0a42f027e4875919a28fb4c04ea2cf",
};

/// *Fast*: whisper tiny, English.
pub const FAST: ModelFile = ModelFile {
    name: "ggml-tiny.en.bin",
    url: "https://huggingface.co/ggerganov/whisper.cpp/resolve/5359861c739e955e79d9a303bcbc70fb988958b1/ggml-tiny.en.bin",
    size: 77_704_715,
    sha256: "921e4cf8686fdd993dcd081a5da5b6c365bfde1162e72b08d75ac75289920b1f",
};

/// *Standard*: whisper base, English, quantised.
pub const STANDARD: ModelFile = ModelFile {
    name: "ggml-base.en-q5_1.bin",
    url: "https://huggingface.co/ggerganov/whisper.cpp/resolve/5359861c739e955e79d9a303bcbc70fb988958b1/ggml-base.en-q5_1.bin",
    size: 59_721_011,
    sha256: "4baf70dd0d7c4247ba2b81fafd9c01005ac77c2f9ef064e00dcf195d0e2fdd2f",
};

/// *Best*: whisper small, multilingual, quantised.
pub const BEST: ModelFile = ModelFile {
    name: "ggml-small-q5_1.bin",
    url: "https://huggingface.co/ggerganov/whisper.cpp/resolve/5359861c739e955e79d9a303bcbc70fb988958b1/ggml-small-q5_1.bin",
    size: 190_085_487,
    sha256: "ae85e4a935d7a567bd102fe55afc16bb595bdb618e11b2fc7591bc08120411bb",
};

/// The Whisper model of `quality`.
pub fn model_of(quality: CaptionsQuality) -> &'static ModelFile {
    match quality {
        CaptionsQuality::Fast => &FAST,
        CaptionsQuality::Standard => &STANDARD,
        CaptionsQuality::Best => &BEST,
    }
}

/// Every file `quality` needs: its model, then the voice-activity model.
pub fn files(quality: CaptionsQuality) -> [&'static ModelFile; 2] {
    [model_of(quality), &VAD]
}

/// Why a download failed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DownloadError {
    /// No connection, or it dropped. The part that arrived is kept for the next try.
    Network(String),
    /// The server said no (a status code).
    Status(u16),
    /// The file arrived whole but is not the one expected: it was deleted.
    Corrupt,
    /// The data folder could not be written.
    Disk(String),
}

impl std::fmt::Display for DownloadError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            DownloadError::Network(e) => write!(f, "network error: {e}"),
            DownloadError::Status(code) => write!(f, "the server answered {code}"),
            DownloadError::Corrupt => write!(f, "the file did not match its checksum"),
            DownloadError::Disk(e) => write!(f, "could not write the file: {e}"),
        }
    }
}

impl std::error::Error for DownloadError {}

/// How a call ended without an error.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Outcome {
    /// Every file is there and right.
    Ready,
    /// Stopped on request. What arrived is kept.
    Cancelled,
}

/// No bytes for this long: the connection is dead.
const STALL: Duration = Duration::from_secs(30);

/// Finds, checks and downloads the models in `dir`.
pub struct Models {
    dir: PathBuf,
    client: reqwest::Client,
    /// The development override (both files named and there).
    fixed: Option<(PathBuf, PathBuf)>,
    /// Replaces the catalogue's addresses: `<base>/<file name>`. Tests only.
    base_url: Option<String>,
    /// Which files a quality needs. Tests swap it for small ones.
    catalogue: fn(CaptionsQuality) -> [&'static ModelFile; 2],
    /// Files whose hash was checked in this run.
    verified: Mutex<HashSet<&'static str>>,
    /// Held while a download runs: one at a time.
    gate: tokio::sync::Mutex<()>,
}

impl Models {
    /// Models in `dir`, replaced by the environment's two files when they are named and there.
    pub fn new(dir: PathBuf) -> Self {
        let client = reqwest::Client::builder()
            .connect_timeout(Duration::from_secs(15))
            .build()
            .unwrap_or_default();
        Self {
            dir,
            client,
            fixed: None,
            base_url: None,
            catalogue: files,
            verified: Mutex::new(HashSet::new()),
            gate: tokio::sync::Mutex::new(()),
        }
    }

    /// Takes the two environment variables, if both name a file.
    pub fn with_env_override(mut self) -> Self {
        let named = |name: &str| crate::env_var(name).map(PathBuf::from);
        if let (Some(model), Some(vad)) = (named(ENV_MODEL), named(ENV_VAD_MODEL)) {
            if model.is_file() && vad.is_file() {
                tracing::info!(model = %model.display(), "captions use the model files from the environment");
                self.fixed = Some((model, vad));
            } else {
                tracing::warn!(
                    "{ENV_MODEL} or {ENV_VAD_MODEL} names no file: using downloaded models"
                );
            }
        }
        self
    }

    /// Downloads from `<base>/<file name>` instead of the catalogue's addresses.
    #[cfg(test)]
    pub(crate) fn with_base_url(mut self, base: String) -> Self {
        self.base_url = Some(base);
        self
    }

    /// Takes catalogue for the files of each quality.
    #[cfg(test)]
    pub(crate) fn with_catalogue(
        mut self,
        catalogue: fn(CaptionsQuality) -> [&'static ModelFile; 2],
    ) -> Self {
        self.catalogue = catalogue;
        self
    }

    /// The Whisper model and the voice-activity model of `quality`, as paths. They exist once
    /// [`Models::ensure`] has returned `Ready`.
    pub fn paths(&self, quality: CaptionsQuality) -> (PathBuf, PathBuf) {
        match &self.fixed {
            Some((model, vad)) => (model.clone(), vad.clone()),
            None => {
                let [model, vad] = (self.catalogue)(quality);
                (self.dir.join(model.name), self.dir.join(vad.name))
            }
        }
    }

    /// Makes sure the files of `quality` are in the data folder, downloading what is missing or
    /// wrong. `progress` gets 0 when a download starts and then each percent of the missing bytes
    /// that have arrived (it is not called when everything is there). `cancelled` is asked
    /// before every chunk.
    pub async fn ensure(
        &self,
        quality: CaptionsQuality,
        progress: &(dyn Fn(u8) + Sync),
        cancelled: &(dyn Fn() -> bool + Sync),
    ) -> Result<Outcome, DownloadError> {
        if self.fixed.is_some() {
            return Ok(Outcome::Ready);
        }
        self.ensure_files(&(self.catalogue)(quality), progress, cancelled)
            .await
    }

    async fn ensure_files(
        &self,
        wanted: &[&'static ModelFile],
        progress: &(dyn Fn(u8) + Sync),
        cancelled: &(dyn Fn() -> bool + Sync),
    ) -> Result<Outcome, DownloadError> {
        // One at a time. Whoever waits here finds the files there when the first is done.
        let _gate = self.gate.lock().await;
        let mut missing = Vec::new();
        for file in wanted {
            if !self.is_there(file).await {
                missing.push(*file);
            }
        }
        if missing.is_empty() {
            return Ok(Outcome::Ready);
        }
        let total: u64 = missing.iter().map(|f| f.size).sum();
        let mut before = 0;
        let mut last = None;
        let mut report = |done: u64| {
            let percent = (done.min(total) * 100 / total.max(1)) as u8;
            if last != Some(percent) {
                last = Some(percent);
                progress(percent);
            }
        };
        report(0);
        for file in missing {
            let url = match &self.base_url {
                Some(base) => format!("{base}/{}", file.name),
                None => file.url.to_owned(),
            };
            let done = fetch(&self.client, &url, file, &self.dir, cancelled, &mut |n| {
                report(before + n)
            })
            .await?;
            if done == Outcome::Cancelled {
                return Ok(Outcome::Cancelled);
            }
            before += file.size;
            self.verified.lock().unwrap().insert(file.name);
        }
        Ok(Outcome::Ready)
    }

    /// `file` is in the data folder and is the right one. A file of the right size is hashed once
    /// per run; a wrong one is deleted.
    async fn is_there(&self, file: &'static ModelFile) -> bool {
        if self.verified.lock().unwrap().contains(file.name) {
            return true;
        }
        let path = self.dir.join(file.name);
        match std::fs::metadata(&path) {
            Ok(m) if m.len() == file.size => {}
            Ok(_) => {
                let _ = std::fs::remove_file(&path);
                return false;
            }
            Err(_) => return false,
        }
        let p = path.clone();
        let hash = tokio::task::spawn_blocking(move || hash_file(&p)).await;
        if matches!(hash, Ok(Ok(h)) if h == file.sha256) {
            self.verified.lock().unwrap().insert(file.name);
            true
        } else {
            tracing::warn!(
                file = file.name,
                "a caption model on disk is wrong: downloading it again"
            );
            let _ = std::fs::remove_file(&path);
            false
        }
    }
}

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

/// The SHA-256 of a file, as lowercase hex.
fn hash_file(path: &Path) -> std::io::Result<String> {
    Ok(hex(&hash_start(path)?.finalize()))
}

/// A hasher that has taken in all of `path`.
fn hash_start(path: &Path) -> std::io::Result<Sha256> {
    let mut file = std::fs::File::open(path)?;
    let mut hasher = Sha256::new();
    let mut buf = vec![0u8; 256 * 1024];
    loop {
        let n = file.read(&mut buf)?;
        if n == 0 {
            return Ok(hasher);
        }
        hasher.update(&buf[..n]);
    }
}

/// Downloads `file` from `url` into `dir` (through `<name>.part`), checks it and renames it.
/// `got` is told the bytes of the file that are there so far.
async fn fetch(
    client: &reqwest::Client,
    url: &str,
    file: &ModelFile,
    dir: &Path,
    cancelled: &(dyn Fn() -> bool + Sync),
    got: &mut (dyn FnMut(u64) + Send),
) -> Result<Outcome, DownloadError> {
    let disk = |e: std::io::Error| DownloadError::Disk(e.to_string());
    std::fs::create_dir_all(dir).map_err(disk)?;
    let dest = dir.join(file.name);
    let part = dir.join(format!("{}.part", file.name));

    // What an earlier try left. More than the file holds means it is not this file's.
    let mut have = std::fs::metadata(&part).map_or(0, |m| m.len());
    if have > file.size {
        let _ = std::fs::remove_file(&part);
        have = 0;
    }

    let mut response = None;
    if have < file.size {
        let mut request = client.get(url);
        if have > 0 {
            request = request.header(reqwest::header::RANGE, format!("bytes={have}-"));
        }
        let r = request
            .send()
            .await
            .map_err(|e| DownloadError::Network(e.to_string()))?;
        match r.status().as_u16() {
            206 if have > 0 => response = Some(r),
            // The server sent all of it: start again from the top.
            200 => {
                have = 0;
                response = Some(r);
            }
            // The range is past the end: what is there is no use.
            416 => {
                let _ = std::fs::remove_file(&part);
                return Err(DownloadError::Network(
                    "the partial file was refused".into(),
                ));
            }
            code => return Err(DownloadError::Status(code)),
        }
    }

    // Hash what is already there, then the rest as it arrives.
    let mut hasher = if have > 0 {
        let p = part.clone();
        tokio::task::spawn_blocking(move || hash_start(&p))
            .await
            .map_err(|e| DownloadError::Disk(e.to_string()))?
            .map_err(disk)?
    } else {
        Sha256::new()
    };
    let mut out = if have > 0 {
        OpenOptions::new().append(true).open(&part).map_err(disk)?
    } else {
        std::fs::File::create(&part).map_err(disk)?
    };
    got(have);

    if let Some(mut r) = response {
        loop {
            if cancelled() {
                return Ok(Outcome::Cancelled);
            }
            let chunk = match tokio::time::timeout(STALL, r.chunk()).await {
                Err(_) => return Err(DownloadError::Network("no data for 30 seconds".into())),
                Ok(Err(e)) => return Err(DownloadError::Network(e.to_string())),
                Ok(Ok(chunk)) => chunk,
            };
            let Some(chunk) = chunk else { break };
            have += chunk.len() as u64;
            if have > file.size {
                let _ = std::fs::remove_file(&part);
                return Err(DownloadError::Corrupt);
            }
            hasher.update(&chunk);
            out.write_all(&chunk).map_err(disk)?;
            got(have);
        }
    }
    out.flush().map_err(disk)?;
    drop(out);

    if have < file.size {
        return Err(DownloadError::Network(
            "the connection ended before the file did".into(),
        ));
    }
    if hex(&hasher.finalize()) != file.sha256 {
        let _ = std::fs::remove_file(&part);
        return Err(DownloadError::Corrupt);
    }
    // Windows does not rename over a file.
    let _ = std::fs::remove_file(&dest);
    std::fs::rename(&part, &dest).map_err(disk)?;
    Ok(Outcome::Ready)
}

/// Small models for tests elsewhere in the app: three qualities and a voice-activity file.
#[cfg(test)]
pub(crate) mod testing {
    use std::sync::OnceLock;

    use super::*;

    /// A model file made for a test: `payload` and a catalogue entry for it.
    pub(crate) fn test_file(name: &'static str, payload: &[u8]) -> &'static ModelFile {
        let sha: &'static str = Box::leak(hex(&Sha256::digest(payload)).into_boxed_str());
        Box::leak(Box::new(ModelFile {
            name,
            url: "unused",
            size: payload.len() as u64,
            sha256: sha,
        }))
    }

    /// Bytes that are not all alike.
    pub(crate) fn payload(len: usize, seed: u8) -> Vec<u8> {
        (0..len)
            .map(|i| (i as u8).wrapping_mul(31).wrapping_add(seed))
            .collect()
    }

    /// The bytes served for each test model.
    pub(crate) fn body(name: &str) -> Vec<u8> {
        match name {
            "tiny.bin" => payload(30_000, 1),
            "base.bin" => payload(40_000, 2),
            "small.bin" => payload(50_000, 3),
            _ => payload(10_000, 4),
        }
    }

    /// A server with every test model, each after delay.
    pub(crate) async fn server(delay: std::time::Duration) -> wiremock::MockServer {
        use wiremock::matchers::{method, path};
        use wiremock::{Mock, MockServer, ResponseTemplate};

        let server = MockServer::start().await;
        for name in ["tiny.bin", "base.bin", "small.bin", "vad.bin"] {
            Mock::given(method("GET"))
                .and(path(format!("/{name}")))
                .respond_with(
                    ResponseTemplate::new(200)
                        .set_body_bytes(body(name))
                        .set_delay(delay),
                )
                .mount(&server)
                .await;
        }
        server
    }

    /// Models in dir that download from server.
    pub(crate) fn models(dir: &Path, server: &wiremock::MockServer) -> Models {
        Models::new(dir.to_path_buf())
            .with_base_url(server.uri())
            .with_catalogue(files)
    }

    /// The catalogue for [`Models::with_catalogue`].
    pub(crate) fn files(quality: CaptionsQuality) -> [&'static ModelFile; 2] {
        static FILES: OnceLock<[&'static ModelFile; 4]> = OnceLock::new();
        let all = FILES.get_or_init(|| {
            ["tiny.bin", "base.bin", "small.bin", "vad.bin"].map(|n| test_file(n, &body(n)))
        });
        let model = match quality {
            CaptionsQuality::Fast => all[0],
            CaptionsQuality::Standard => all[1],
            CaptionsQuality::Best => all[2],
        };
        [model, all[3]]
    }
}

#[cfg(test)]
mod tests {
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::Arc;

    use wiremock::matchers::{method, path};
    use wiremock::{Mock, MockServer, Request, Respond, ResponseTemplate};

    use super::testing::{payload, test_file};
    use super::*;

    // Commits of the two repositories that hold these files (checked 2026-11-13: they serve the
    // same bytes as `main`).
    const WHISPER_CPP: &str = "https://huggingface.co/ggerganov/whisper.cpp/resolve/5359861c739e955e79d9a303bcbc70fb988958b1";
    const WHISPER_VAD: &str = "https://huggingface.co/ggml-org/whisper-vad/resolve/9ffd54a1e1ee413ddf265af9913beaf518d1639b";

    // ---- the catalogue ----

    #[test]
    fn every_file_has_a_size_a_hash_and_a_pinned_address() {
        for file in [&VAD, &FAST, &STANDARD, &BEST] {
            assert!(file.size > 0, "{}", file.name);
            assert_eq!(file.sha256.len(), 64, "{}", file.name);
            assert!(
                file.sha256
                    .chars()
                    .all(|c| c.is_ascii_digit() || ('a'..='f').contains(&c)),
                "{}: lowercase hex",
                file.name
            );
            assert!(file.url.starts_with("https://huggingface.co/"));
            assert!(
                file.url.ends_with(&format!("/{}", file.name)),
                "{}",
                file.url
            );
            assert!(!file.url.contains("/main/"), "{} is not pinned", file.url);
        }
        assert!(FAST.url.starts_with(WHISPER_CPP));
        assert!(STANDARD.url.starts_with(WHISPER_CPP));
        assert!(BEST.url.starts_with(WHISPER_CPP));
        assert!(VAD.url.starts_with(WHISPER_VAD));
    }

    #[test]
    fn the_values_are_the_ones_measured() {
        assert_eq!(
            (FAST.size, STANDARD.size, BEST.size),
            (77_704_715, 59_721_011, 190_085_487)
        );
        assert_eq!(VAD.size, 885_098);
        assert_eq!(STANDARD.name, "ggml-base.en-q5_1.bin");
        assert!(STANDARD.sha256.starts_with("4baf70dd"));
    }

    #[test]
    fn every_quality_needs_its_model_and_the_vad() {
        for q in CaptionsQuality::ALL {
            let [model, vad] = files(q);
            assert_eq!(model, model_of(q));
            assert_eq!(vad, &VAD);
        }
        assert_eq!(
            model_of(htpc_core::DEFAULT_CAPTIONS_QUALITY),
            &STANDARD,
            "the default is Standard"
        );
    }

    // ---- the download ----

    /// Serves `body`: the whole of it, or, for a request with `Range: bytes=N-`, from N (206).
    /// The first `cut_first` requests without a range get only that many bytes (a dropped
    /// connection, as the client sees it). `ignore_range` always sends all of it.
    struct Serve {
        body: Vec<u8>,
        cut_first: Option<usize>,
        ignore_range: bool,
        calls: AtomicUsize,
    }

    impl Serve {
        fn new(body: Vec<u8>) -> Self {
            Self {
                body,
                cut_first: None,
                ignore_range: false,
                calls: AtomicUsize::new(0),
            }
        }
    }

    impl Respond for Serve {
        fn respond(&self, request: &Request) -> ResponseTemplate {
            let call = self.calls.fetch_add(1, Ordering::SeqCst);
            let from = request
                .headers
                .get("range")
                .and_then(|v| v.to_str().ok())
                .and_then(|v| v.strip_prefix("bytes="))
                .and_then(|v| v.strip_suffix('-'))
                .and_then(|v| v.parse::<usize>().ok());
            match from {
                Some(n) if !self.ignore_range => {
                    ResponseTemplate::new(206).set_body_bytes(self.body[n..].to_vec())
                }
                _ => match self.cut_first {
                    Some(cut) if call == 0 => {
                        ResponseTemplate::new(200).set_body_bytes(self.body[..cut].to_vec())
                    }
                    _ => ResponseTemplate::new(200).set_body_bytes(self.body.clone()),
                },
            }
        }
    }

    async fn server_with(name: &str, serve: Serve) -> MockServer {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path(format!("/{name}")))
            .respond_with(serve)
            .mount(&server)
            .await;
        server
    }

    fn models_in(dir: &Path, server: &MockServer) -> Models {
        Models::new(dir.to_path_buf()).with_base_url(server.uri())
    }

    async fn run(
        m: &Models,
        file: &'static ModelFile,
        cancel: &(dyn Fn() -> bool + Sync),
    ) -> (Result<Outcome, DownloadError>, Vec<u8>) {
        let seen = Mutex::new(Vec::new());
        let r = m
            .ensure_files(&[file], &|p| seen.lock().unwrap().push(p), cancel)
            .await;
        let seen = seen.into_inner().unwrap();
        (r, seen)
    }

    #[tokio::test]
    async fn a_download_is_checked_renamed_and_reported() {
        let body = payload(300_000, 1);
        let file = test_file("ok.bin", &body);
        let server = server_with("ok.bin", Serve::new(body.clone())).await;
        let dir = tempfile::tempdir().unwrap();
        let m = models_in(dir.path(), &server);

        let (r, seen) = run(&m, file, &|| false).await;
        assert_eq!(r, Ok(Outcome::Ready));
        assert_eq!(std::fs::read(dir.path().join("ok.bin")).unwrap(), body);
        assert!(!dir.path().join("ok.bin.part").exists());
        assert_eq!(seen.first(), Some(&0));
        assert_eq!(seen.last(), Some(&100));
        assert!(
            seen.windows(2).all(|w| w[0] < w[1]),
            "once per percent: {seen:?}"
        );
    }

    #[tokio::test]
    async fn a_file_that_is_there_is_not_downloaded_again() {
        let body = payload(50_000, 2);
        let file = test_file("there.bin", &body);
        let server = server_with("there.bin", Serve::new(body.clone())).await;
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("there.bin"), &body).unwrap();
        let m = models_in(dir.path(), &server);

        let (r, seen) = run(&m, file, &|| false).await;
        assert_eq!(r, Ok(Outcome::Ready));
        assert!(seen.is_empty(), "nothing to report: {seen:?}");
        assert!(server.received_requests().await.unwrap().is_empty());
        // The hash was checked once and remembered: damaging the file now changes nothing.
        std::fs::write(dir.path().join("there.bin"), vec![0u8; body.len()]).unwrap();
        assert_eq!(run(&m, file, &|| false).await.0, Ok(Outcome::Ready));
        assert!(server.received_requests().await.unwrap().is_empty());
    }

    #[tokio::test]
    async fn a_wrong_file_on_disk_is_replaced() {
        let body = payload(50_000, 3);
        let file = test_file("wrong.bin", &body);
        let server = server_with("wrong.bin", Serve::new(body.clone())).await;
        let dir = tempfile::tempdir().unwrap();
        // The right size, the wrong bytes.
        std::fs::write(dir.path().join("wrong.bin"), vec![7u8; body.len()]).unwrap();
        let m = models_in(dir.path(), &server);

        assert_eq!(run(&m, file, &|| false).await.0, Ok(Outcome::Ready));
        assert_eq!(std::fs::read(dir.path().join("wrong.bin")).unwrap(), body);
    }

    #[tokio::test]
    async fn a_hash_mismatch_deletes_the_file_and_fails() {
        let body = payload(80_000, 4);
        let file = test_file("bad.bin", &body);
        // The server sends other bytes of the same length.
        let server = server_with("bad.bin", Serve::new(payload(80_000, 99))).await;
        let dir = tempfile::tempdir().unwrap();
        let m = models_in(dir.path(), &server);

        assert_eq!(
            run(&m, file, &|| false).await.0,
            Err(DownloadError::Corrupt)
        );
        assert!(!dir.path().join("bad.bin").exists());
        assert!(!dir.path().join("bad.bin.part").exists());
    }

    #[tokio::test]
    async fn an_error_status_fails_and_leaves_nothing() {
        let file = test_file("gone.bin", b"abc");
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .respond_with(ResponseTemplate::new(404))
            .mount(&server)
            .await;
        let dir = tempfile::tempdir().unwrap();
        let m = models_in(dir.path(), &server);

        assert_eq!(
            run(&m, file, &|| false).await.0,
            Err(DownloadError::Status(404))
        );
        assert!(!dir.path().join("gone.bin").exists());
    }

    #[tokio::test]
    async fn a_dropped_connection_keeps_the_part_and_the_next_try_asks_for_the_rest() {
        let body = payload(400_000, 5);
        let file = test_file("cut.bin", &body);
        let mut serve = Serve::new(body.clone());
        serve.cut_first = Some(150_000);
        let server = server_with("cut.bin", serve).await;
        let dir = tempfile::tempdir().unwrap();
        let m = models_in(dir.path(), &server);

        let (first, _) = run(&m, file, &|| false).await;
        assert!(matches!(first, Err(DownloadError::Network(_))), "{first:?}");
        assert!(!dir.path().join("cut.bin").exists());
        assert_eq!(
            std::fs::metadata(dir.path().join("cut.bin.part"))
                .unwrap()
                .len(),
            150_000
        );

        let (second, seen) = run(&m, file, &|| false).await;
        assert_eq!(second, Ok(Outcome::Ready));
        assert_eq!(std::fs::read(dir.path().join("cut.bin")).unwrap(), body);
        assert!(!dir.path().join("cut.bin.part").exists());
        // It started where the part ended: 150000 of 400000 is 37 percent.
        assert!(seen[1] >= 37, "{seen:?}");
        let requests = server.received_requests().await.unwrap();
        assert_eq!(
            requests[1].headers.get("range").unwrap().to_str().unwrap(),
            "bytes=150000-"
        );
    }

    #[tokio::test]
    async fn a_server_that_ignores_the_range_starts_over() {
        let body = payload(200_000, 6);
        let file = test_file("full.bin", &body);
        let mut serve = Serve::new(body.clone());
        serve.ignore_range = true;
        let server = server_with("full.bin", serve).await;
        let dir = tempfile::tempdir().unwrap();
        // An earlier try left a part (not even this file's bytes).
        std::fs::write(dir.path().join("full.bin.part"), vec![1u8; 70_000]).unwrap();
        let m = models_in(dir.path(), &server);

        assert_eq!(run(&m, file, &|| false).await.0, Ok(Outcome::Ready));
        assert_eq!(std::fs::read(dir.path().join("full.bin")).unwrap(), body);
    }

    #[tokio::test]
    async fn a_cancel_stops_without_a_file_and_the_next_try_finishes() {
        let body = payload(300_000, 7);
        let file = test_file("cancel.bin", &body);
        let server = server_with("cancel.bin", Serve::new(body.clone())).await;
        let dir = tempfile::tempdir().unwrap();
        let m = models_in(dir.path(), &server);

        let (r, _) = run(&m, file, &|| true).await;
        assert_eq!(r, Ok(Outcome::Cancelled));
        assert!(!dir.path().join("cancel.bin").exists());

        // Cancelled once the first bytes are in: the part stays and is continued.
        let asked = AtomicUsize::new(0);
        let (r, _) = run(&m, file, &|| asked.fetch_add(1, Ordering::SeqCst) >= 1).await;
        assert_eq!(r, Ok(Outcome::Cancelled));
        assert!(!dir.path().join("cancel.bin").exists());
        assert!(dir.path().join("cancel.bin.part").exists());

        assert_eq!(run(&m, file, &|| false).await.0, Ok(Outcome::Ready));
        assert_eq!(std::fs::read(dir.path().join("cancel.bin")).unwrap(), body);
    }

    #[tokio::test]
    async fn two_calls_download_once() {
        let body = payload(300_000, 8);
        let file = test_file("once.bin", &body);
        let server = server_with("once.bin", Serve::new(body.clone())).await;
        let dir = tempfile::tempdir().unwrap();
        let m = Arc::new(models_in(dir.path(), &server));

        let (a, b) = tokio::join!(run(&m, file, &|| false), run(&m, file, &|| false));
        assert_eq!(a.0, Ok(Outcome::Ready));
        assert_eq!(b.0, Ok(Outcome::Ready));
        assert_eq!(server.received_requests().await.unwrap().len(), 1);
    }

    /// The real Standard model from Hugging Face (60 MB) into `HTPC_MODELS_TEST_DIR`. By hand:
    /// `cargo test -p livedeck real_standard -- --ignored --nocapture`.
    #[tokio::test]
    #[ignore = "downloads 60 MB"]
    async fn real_standard_download() {
        let dir = std::env::var("HTPC_MODELS_TEST_DIR").expect("HTPC_MODELS_TEST_DIR");
        let m = Models::new(PathBuf::from(&dir));
        let last = std::sync::atomic::AtomicU8::new(255);
        let r = m
            .ensure(
                CaptionsQuality::Standard,
                &|p| {
                    if p % 10 == 0 && last.swap(p, Ordering::SeqCst) != p {
                        println!("{p}%");
                    }
                },
                &|| false,
            )
            .await;
        assert_eq!(r, Ok(Outcome::Ready));
        let (model, vad) = m.paths(CaptionsQuality::Standard);
        assert_eq!(std::fs::metadata(model).unwrap().len(), STANDARD.size);
        assert_eq!(std::fs::metadata(vad).unwrap().len(), VAD.size);
        // A second call finds both and downloads nothing.
        let again = m
            .ensure(
                CaptionsQuality::Standard,
                &|_| panic!("downloaded twice"),
                &|| false,
            )
            .await;
        assert_eq!(again, Ok(Outcome::Ready));
    }

    #[test]
    fn without_the_environment_files_models_come_from_the_data_folder() {
        let dir = tempfile::tempdir().unwrap();
        let m = Models::new(dir.path().to_path_buf());
        let (model, vad) = m.paths(CaptionsQuality::Fast);
        assert_eq!(model, dir.path().join("ggml-tiny.en.bin"));
        assert_eq!(vad, dir.path().join("ggml-silero-v5.1.2.bin"));
    }
}
