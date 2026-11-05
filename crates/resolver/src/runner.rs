//! Runs an external program. A trait so tests can feed canned Streamlink output.

use std::future::Future;
use std::process::Stdio;
use std::time::Duration;

/// What a finished process left behind.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct ProcessOutput {
    pub success: bool,
    pub stdout: String,
    pub stderr: String,
}

/// Why a process did not finish.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RunError {
    /// The program does not exist.
    NotFound,
    /// It was still running after the timeout and has been killed.
    TimedOut,
    /// Any other failure to start or read it.
    Io(String),
}

pub trait ProcessRunner: Send + Sync + 'static {
    fn run(
        &self,
        program: &str,
        args: &[String],
        timeout: Duration,
    ) -> impl Future<Output = Result<ProcessOutput, RunError>> + Send;
}

/// Runs real processes with tokio.
#[derive(Debug, Clone, Copy, Default)]
pub struct TokioRunner;

impl ProcessRunner for TokioRunner {
    async fn run(
        &self,
        program: &str,
        args: &[String],
        timeout: Duration,
    ) -> Result<ProcessOutput, RunError> {
        let mut cmd = tokio::process::Command::new(program);
        cmd.args(args)
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            // Dropping the future on timeout kills the child.
            .kill_on_drop(true);
        #[cfg(windows)]
        cmd.creation_flags(0x0800_0000); // CREATE_NO_WINDOW

        let child = cmd.spawn().map_err(|e| match e.kind() {
            std::io::ErrorKind::NotFound => RunError::NotFound,
            _ => RunError::Io(e.to_string()),
        })?;
        match tokio::time::timeout(timeout, child.wait_with_output()).await {
            Err(_) => Err(RunError::TimedOut),
            Ok(Err(e)) => Err(RunError::Io(e.to_string())),
            Ok(Ok(out)) => Ok(ProcessOutput {
                success: out.status.success(),
                stdout: String::from_utf8_lossy(&out.stdout).into_owned(),
                stderr: String::from_utf8_lossy(&out.stderr).into_owned(),
            }),
        }
    }
}
