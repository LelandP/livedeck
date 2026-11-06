//! `FakeAuth`: a scripted in-memory [`AuthApi`] for tests and `--mock`.
//!
//! Polls and refreshes are answered from queues. Once the poll queue is empty the answer is
//! `Pending` for ever, like a user who never approves. An empty refresh queue is a rejection.

use std::collections::VecDeque;
use std::sync::{Arc, Mutex, MutexGuard};
use std::time::Duration;

use crate::{AuthApi, DeviceGrant, Error, Identity, PollOutcome, Result, TokenGrant, SCOPES};

/// Every scope the app asks for, as the fake grants them.
fn all_scopes() -> Vec<String> {
    SCOPES.iter().map(|s| (*s).to_owned()).collect()
}

/// A request the fake received, for assertions.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum FakeAuthCall {
    RequestDeviceCode,
    Poll(String),
    Refresh(String),
    Validate(String),
    Revoke(String),
}

/// Clones share their queues and call log, so a test keeps one clone for assertions.
#[derive(Debug, Clone)]
pub struct FakeAuth {
    grant: DeviceGrant,
    identity: Identity,
    polls: Arc<Mutex<VecDeque<Result<PollOutcome>>>>,
    refreshes: Arc<Mutex<VecDeque<Result<TokenGrant>>>>,
    calls: Arc<Mutex<Vec<FakeAuthCall>>>,
    /// Answer `Granted` instead of `Pending` once the poll queue is empty.
    auto_approve: bool,
}

fn lock<T>(m: &Mutex<T>) -> MutexGuard<'_, T> {
    m.lock().unwrap_or_else(|e| e.into_inner())
}

impl Default for FakeAuth {
    fn default() -> Self {
        Self::new()
    }
}

impl FakeAuth {
    /// The mock sign-in's code and user, nothing scripted.
    pub fn new() -> Self {
        Self {
            grant: DeviceGrant {
                device_code: "mock-device-code".into(),
                user_code: "KQRT-7ZMP".into(),
                verification_uri: "https://www.twitch.tv/activate?device-code=KQRT7ZMP".into(),
                expires_in: Duration::from_secs(900),
                interval: Duration::from_secs(2),
            },
            identity: Identity {
                user_id: "1000".into(),
                login: "nightowl_dev".into(),
                scopes: all_scopes(),
            },
            polls: Arc::default(),
            refreshes: Arc::default(),
            calls: Arc::default(),
            auto_approve: false,
        }
    }

    /// A user who approves on the second poll, every time, as `--mock` shows it.
    pub fn approving() -> Self {
        let mut auth = Self::new().then_poll(Ok(PollOutcome::Pending));
        auth.auto_approve = true;
        auth
    }

    /// A token grant with a refresh token and a four hour lifetime.
    pub fn token(name: &str) -> TokenGrant {
        TokenGrant {
            access_token: format!("{name}-access"),
            refresh_token: Some(format!("{name}-refresh")),
            expires_in: Some(Duration::from_secs(4 * 3600)),
            scopes: all_scopes(),
        }
    }

    pub fn with_grant(mut self, grant: DeviceGrant) -> Self {
        self.grant = grant;
        self
    }

    pub fn with_identity(mut self, identity: Identity) -> Self {
        self.identity = identity;
        self
    }

    /// Queues the answer to the next unanswered poll.
    pub fn then_poll(self, answer: Result<PollOutcome>) -> Self {
        lock(&self.polls).push_back(answer);
        self
    }

    /// Queues the answer to the next unanswered refresh.
    pub fn then_refresh(self, answer: Result<TokenGrant>) -> Self {
        lock(&self.refreshes).push_back(answer);
        self
    }

    pub fn calls(&self) -> Vec<FakeAuthCall> {
        lock(&self.calls).clone()
    }

    fn log(&self, call: FakeAuthCall) {
        lock(&self.calls).push(call);
    }
}

impl AuthApi for FakeAuth {
    async fn request_device_code(&self, _scopes: &[&str]) -> Result<DeviceGrant> {
        self.log(FakeAuthCall::RequestDeviceCode);
        Ok(self.grant.clone())
    }

    async fn poll_token(&self, device_code: &str, _scopes: &[&str]) -> Result<PollOutcome> {
        self.log(FakeAuthCall::Poll(device_code.to_owned()));
        let scripted = lock(&self.polls).pop_front();
        scripted.unwrap_or_else(|| {
            Ok(if self.auto_approve {
                PollOutcome::Granted(Self::token("mock"))
            } else {
                PollOutcome::Pending
            })
        })
    }

    async fn refresh(&self, refresh_token: &str) -> Result<TokenGrant> {
        self.log(FakeAuthCall::Refresh(refresh_token.to_owned()));
        lock(&self.refreshes)
            .pop_front()
            .unwrap_or_else(|| Err(Error::AuthRejected("Invalid refresh token".into())))
    }

    async fn validate(&self, access_token: &str) -> Result<Identity> {
        self.log(FakeAuthCall::Validate(access_token.to_owned()));
        Ok(self.identity.clone())
    }

    async fn revoke(&self, access_token: &str) -> Result<()> {
        self.log(FakeAuthCall::Revoke(access_token.to_owned()));
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn approving_waits_once_then_grants_every_time() {
        let auth = FakeAuth::approving();
        assert_eq!(
            auth.poll_token("d", &[]).await.unwrap(),
            PollOutcome::Pending
        );
        for _ in 0..2 {
            let answer = auth.poll_token("d", &[]).await.unwrap();
            assert!(matches!(answer, PollOutcome::Granted(_)));
        }
        assert_eq!(auth.calls().len(), 3);
    }

    #[tokio::test]
    async fn a_script_ends_in_pending_for_ever() {
        let auth = FakeAuth::new().then_poll(Ok(PollOutcome::SlowDown));
        assert_eq!(
            auth.poll_token("d", &[]).await.unwrap(),
            PollOutcome::SlowDown
        );
        for _ in 0..3 {
            assert_eq!(
                auth.poll_token("d", &[]).await.unwrap(),
                PollOutcome::Pending
            );
        }
    }

    #[tokio::test]
    async fn an_unscripted_refresh_is_rejected() {
        let auth = FakeAuth::new();
        assert!(matches!(
            auth.refresh("r").await,
            Err(Error::AuthRejected(_))
        ));
    }
}
