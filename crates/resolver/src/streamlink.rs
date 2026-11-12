//! `StreamlinkResolver`: `streamlink --stream-url <page> <quality>,best`, where the page is a
//! channel (`https://twitch.tv/<login>`), a video (`https://www.twitch.tv/videos/<id>`) or a clip
//! (`https://clips.twitch.tv/<slug>`).

use std::time::Duration;

use crate::runner::{ProcessOutput, ProcessRunner, RunError};
use crate::{PlaylistUrl, ResolveError, StreamResolver, Target};

/// Streamlink fetches a token and a playlist, so a slow network needs a few seconds.
pub const DEFAULT_TIMEOUT: Duration = Duration::from_secs(15);

pub struct StreamlinkResolver<R> {
    runner: R,
    program: String,
    timeout: Duration,
}

impl<R: ProcessRunner> StreamlinkResolver<R> {
    pub fn new(runner: R) -> Self {
        Self {
            runner,
            program: "streamlink".to_owned(),
            timeout: DEFAULT_TIMEOUT,
        }
    }

    /// Uses this program instead of `streamlink` from `PATH` (a bundled copy, a dev venv).
    pub fn with_program(mut self, program: impl Into<String>) -> Self {
        self.program = program.into();
        self
    }

    pub fn with_timeout(mut self, timeout: Duration) -> Self {
        self.timeout = timeout;
        self
    }
}

/// The most names a fallback list may have (a 1080p list has 16).
const MAX_NAMES: usize = 24;

/// `quality` is a rendition name (`best`, `worst`, `720p60`-like) or a comma-separated fallback
/// list of them, which Streamlink tries in order. Anything else becomes `best`. A list that does
/// not end in `best` or `worst` gets `best` added, so a stream without any of the names still
/// plays instead of failing.
fn quality_arg(quality: &str) -> String {
    let q = quality.trim().to_ascii_lowercase();
    let names: Vec<&str> = q.split(',').map(str::trim).collect();
    let known = names.len() <= MAX_NAMES
        && names.iter().all(|n| {
            !n.is_empty()
                && n.len() <= 12
                && n.chars().all(|c| c.is_ascii_alphanumeric() || c == '_')
        });
    if !known {
        return "best".to_owned();
    }
    let list = names.join(",");
    match names.last() {
        Some(&("best" | "worst")) => list,
        _ => format!("{list},best"),
    }
}

/// Turns the result of a finished Streamlink run into a playlist or an error.
pub fn map_output(out: &ProcessOutput) -> Result<PlaylistUrl, ResolveError> {
    if out.success {
        let url = out
            .stdout
            .lines()
            .map(str::trim)
            .find(|l| l.starts_with("http://") || l.starts_with("https://"));
        return match url {
            Some(url) => Ok(PlaylistUrl(url.to_owned())),
            None => Err(ResolveError::Failed("no playlist URL in the output".into())),
        };
    }
    let text = format!("{}\n{}", out.stderr, out.stdout);
    let lower = text.to_ascii_lowercase();
    if lower.contains("no playable streams") {
        return Err(ResolveError::Offline);
    }
    if lower.contains("no plugin can handle") {
        return Err(ResolveError::NotFound);
    }
    let last = text
        .lines()
        .map(str::trim)
        .rfind(|l| !l.is_empty())
        .unwrap_or("Streamlink failed without a message");
    Err(ResolveError::Failed(last.to_owned()))
}

impl<R: ProcessRunner> StreamResolver for StreamlinkResolver<R> {
    async fn resolve(&self, target: &Target, quality: &str) -> Result<PlaylistUrl, ResolveError> {
        let Some(page) = target.page_url() else {
            return Err(ResolveError::NotFound);
        };
        let args = vec!["--stream-url".to_owned(), page, quality_arg(quality)];
        tracing::debug!(target = target.name(), ?args, "resolving the stream");
        match self.runner.run(&self.program, &args, self.timeout).await {
            Ok(out) => map_output(&out),
            Err(RunError::NotFound) => Err(ResolveError::NotInstalled),
            Err(RunError::TimedOut) => Err(ResolveError::TimedOut),
            Err(RunError::Io(msg)) => Err(ResolveError::Failed(msg)),
        }
    }
}

#[cfg(test)]
mod tests;
