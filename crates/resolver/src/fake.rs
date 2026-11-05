//! Canned resolvers for tests and `--mock`.

use std::sync::{Arc, Mutex};
use std::time::Duration;

use crate::runner::{ProcessOutput, ProcessRunner, RunError};
use crate::{PlaylistUrl, ResolveError, StreamResolver};

/// The `(program, args)` of one call.
type Call = (String, Vec<String>);

/// A [`ProcessRunner`] that answers every call with the same result and records the calls.
#[derive(Clone)]
pub struct FakeRunner {
    result: Result<ProcessOutput, RunError>,
    calls: Arc<Mutex<Vec<Call>>>,
}

impl FakeRunner {
    pub fn new(result: Result<ProcessOutput, RunError>) -> Self {
        Self {
            result,
            calls: Arc::default(),
        }
    }

    /// The `(program, args)` of every call so far.
    pub fn calls(&self) -> Vec<Call> {
        self.calls.lock().expect("calls lock").clone()
    }
}

impl ProcessRunner for FakeRunner {
    async fn run(
        &self,
        program: &str,
        args: &[String],
        _timeout: Duration,
    ) -> Result<ProcessOutput, RunError> {
        self.calls
            .lock()
            .expect("calls lock")
            .push((program.to_owned(), args.to_vec()));
        self.result.clone()
    }
}

/// A [`StreamResolver`] that answers every channel with the same result.
#[derive(Debug, Clone)]
pub struct FakeResolver {
    pub result: Result<PlaylistUrl, ResolveError>,
}

impl FakeResolver {
    /// Resolves every channel to `url`.
    pub fn playing(url: &str) -> Self {
        Self {
            result: Ok(PlaylistUrl(url.to_owned())),
        }
    }

    pub fn failing(error: ResolveError) -> Self {
        Self { result: Err(error) }
    }
}

impl StreamResolver for FakeResolver {
    async fn resolve(&self, _channel: &str, _quality: &str) -> Result<PlaylistUrl, ResolveError> {
        self.result.clone()
    }
}
