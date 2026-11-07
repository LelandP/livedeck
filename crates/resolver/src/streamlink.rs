//! `StreamlinkResolver`: `streamlink --stream-url https://twitch.tv/<channel> <quality>,best`.

use std::time::Duration;

use crate::runner::{ProcessOutput, ProcessRunner, RunError};
use crate::{PlaylistUrl, ResolveError, Site, StreamResolver};

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

/// Twitch channel logins are letters, digits and underscores, Kick slugs also take `-`. Checking
/// here keeps anything else out of the URL that is handed to the program.
fn valid_channel(site: Site, channel: &str) -> bool {
    let (max, extra) = match site {
        Site::Twitch => (25, '_'),
        Site::Kick => (50, '-'),
    };
    !channel.is_empty()
        && channel.len() <= max
        && !channel.starts_with('-')
        && channel
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '_' || c == extra)
}

/// The channel's page, which Streamlink picks its plugin by.
fn page_url(site: Site, channel: &str) -> String {
    match site {
        Site::Twitch => format!("https://twitch.tv/{channel}"),
        Site::Kick => format!("https://kick.com/{channel}"),
    }
}

/// Quality names are `best`, `worst` or `720p60`-like. Anything else falls back to `best`.
fn quality_arg(quality: &str) -> String {
    let q = quality.trim().to_ascii_lowercase();
    let known =
        !q.is_empty() && q.len() <= 12 && q.chars().all(|c| c.is_ascii_alphanumeric() || c == '_');
    match q.as_str() {
        _ if !known => "best".to_owned(),
        "best" | "worst" => q,
        // A missing variant falls back to the best one instead of failing.
        _ => format!("{q},best"),
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
    async fn resolve(
        &self,
        site: Site,
        channel: &str,
        quality: &str,
    ) -> Result<PlaylistUrl, ResolveError> {
        if !valid_channel(site, channel) {
            return Err(ResolveError::NotFound);
        }
        let args = vec![
            "--stream-url".to_owned(),
            page_url(site, channel),
            quality_arg(quality),
        ];
        tracing::debug!(channel, ?args, "resolving the stream");
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
