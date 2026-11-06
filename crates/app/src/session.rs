//! The signed-in session: device-code login, token storage, refresh and
//! sign-out. It owns the polling loop and the tokens, and tells the store what happened with
//! `Event`s (`DeviceCodeIssued`, `SignedIn`, `LoginFailed`, `SignedOut`).
//!
//! The HTTP clients never see the `TokenStore` and the reducer never sees a token: the session
//! pushes new credentials into the API through `apply`, and reports through `notify`.
//!
//! Refresh tokens are single use, so everything that renews goes through `refresh_gate`: a Home
//! load sends three requests at once and all three can get a 401. The first renews, the others
//! see that the credentials' generation moved on and just retry.

use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, MutexGuard};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use htpc_core::{Command, DeviceCode, Event, LoginProblem, User};
use htpc_storage::{TokenStore, Tokens};
use htpc_twitch_api::{
    AuthApi, Credentials, DeviceGrant, Error, FakeAuth, HttpAuth, Identity, PollOutcome, Refresher,
    TokenGrant, TokenSource, SCOPES, SCOPE_WRITE_CHAT,
};
use tokio::task::JoinHandle;
use tokio::time::Instant;

/// Renew an access token this long before it runs out, so a request never starts with one that
/// is about to expire.
const EXPIRY_MARGIN: Duration = Duration::from_secs(120);

/// What the sign-in screen says when no client ID is configured.
/// Shown when there is no client ID at all. The app ships one, so this only happens in a fork
/// that blanked it. The how-to is in the README, so the on-screen text stays short.
pub const NO_CLIENT_ID: &str =
    "Sign-in is not set up in this build (no Twitch client ID). See the README.";

/// The OAuth server the session talks to: the real one, the scripted fake of `--mock`, or none
/// (no client ID configured, so every login explains that).
pub enum AppAuth {
    Real(HttpAuth),
    Fake(FakeAuth),
    /// Cannot log in, with the reason to show.
    Unavailable(String),
}

impl AuthApi for AppAuth {
    async fn request_device_code(&self, scopes: &[&str]) -> htpc_twitch_api::Result<DeviceGrant> {
        match self {
            AppAuth::Real(a) => a.request_device_code(scopes).await,
            AppAuth::Fake(a) => a.request_device_code(scopes).await,
            AppAuth::Unavailable(why) => Err(Error::AuthRejected(why.clone())),
        }
    }

    async fn poll_token(
        &self,
        device_code: &str,
        scopes: &[&str],
    ) -> htpc_twitch_api::Result<PollOutcome> {
        match self {
            AppAuth::Real(a) => a.poll_token(device_code, scopes).await,
            AppAuth::Fake(a) => a.poll_token(device_code, scopes).await,
            AppAuth::Unavailable(why) => Err(Error::AuthRejected(why.clone())),
        }
    }

    async fn refresh(&self, refresh_token: &str) -> htpc_twitch_api::Result<TokenGrant> {
        match self {
            AppAuth::Real(a) => a.refresh(refresh_token).await,
            AppAuth::Fake(a) => a.refresh(refresh_token).await,
            AppAuth::Unavailable(why) => Err(Error::AuthRejected(why.clone())),
        }
    }

    async fn validate(&self, access_token: &str) -> htpc_twitch_api::Result<Identity> {
        match self {
            AppAuth::Real(a) => a.validate(access_token).await,
            AppAuth::Fake(a) => a.validate(access_token).await,
            AppAuth::Unavailable(why) => Err(Error::AuthRejected(why.clone())),
        }
    }

    async fn revoke(&self, access_token: &str) -> htpc_twitch_api::Result<()> {
        match self {
            AppAuth::Real(a) => a.revoke(access_token).await,
            AppAuth::Fake(a) => a.revoke(access_token).await,
            AppAuth::Unavailable(_) => Ok(()),
        }
    }
}

/// Waits the session does that tests shorten.
#[derive(Debug, Clone, Copy)]
pub struct Timing {
    /// Added to the polling interval each time Twitch answers `slow_down`.
    pub slow_down_step: Duration,
}

impl Default for Timing {
    fn default() -> Self {
        Self {
            slow_down_step: Duration::from_secs(5),
        }
    }
}

/// Reports an `Event` to the store.
pub type Notify = Arc<dyn Fn(Event) + Send + Sync>;
/// Hands new credentials (or none) to the API client.
pub type Apply = Arc<dyn Fn(Option<Credentials>) + Send + Sync>;

/// The signed-in user's Helix id, shared with the provider (it asks for the follows of this id).
pub type Viewer = Arc<Mutex<Option<String>>>;

#[derive(Default)]
struct Inner {
    tokens: Option<Tokens>,
}

pub struct Session {
    auth: AppAuth,
    client_id: String,
    apply: Apply,
    notify: Notify,
    store: Option<Arc<TokenStore>>,
    timing: Timing,
    viewer: Viewer,
    /// Changes whenever the credentials do (sign-in, refresh, sign-out).
    generation: AtomicU64,
    inner: Mutex<Inner>,
    refresh_gate: tokio::sync::Mutex<()>,
    login: Mutex<Option<JoinHandle<()>>>,
}

#[cfg(test)]
mod tests;

fn lock<T>(m: &Mutex<T>) -> MutexGuard<'_, T> {
    m.lock().unwrap_or_else(|e| e.into_inner())
}

fn now_secs() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| d.as_secs())
}

/// The text of a failed login step, as the sign-in screen shows it.
fn problem_text(err: &Error) -> String {
    match err {
        Error::AuthRejected(message) => message.clone(),
        other => other.to_string(),
    }
}

/// The user as the store sees them: who, and whether the token's scopes let them write in chat.
/// Tokens from before chat sending were granted `user:read:follows` only.
fn user_of(login: &str, scopes: &[String]) -> User {
    User {
        login: login.to_owned(),
        can_chat: scopes.iter().any(|s| s == SCOPE_WRITE_CHAT),
    }
}

/// Failures that say nothing about the login itself: keep polling until the code runs out.
fn is_transient(err: &Error) -> bool {
    matches!(
        err,
        Error::Network(_) | Error::Server { .. } | Error::RateLimited
    )
}

impl Session {
    /// `store` is `None` when the storage folders are unusable: logins then last for this run.
    pub fn new(
        auth: AppAuth,
        client_id: impl Into<String>,
        store: Option<Arc<TokenStore>>,
        apply: Apply,
        notify: Notify,
        viewer: Viewer,
    ) -> Arc<Self> {
        Arc::new(Self {
            auth,
            client_id: client_id.into(),
            apply,
            notify,
            store,
            timing: Timing::default(),
            viewer,
            generation: AtomicU64::new(0),
            inner: Mutex::new(Inner::default()),
            refresh_gate: tokio::sync::Mutex::new(()),
            login: Mutex::new(None),
        })
    }

    #[cfg(test)]
    pub fn with_timing(mut self: Arc<Self>, timing: Timing) -> Arc<Self> {
        if let Some(me) = Arc::get_mut(&mut self) {
            me.timing = timing;
        }
        self
    }

    pub fn is_signed_in(&self) -> bool {
        lock(&self.inner).tokens.is_some()
    }

    fn bump(&self) {
        self.generation.fetch_add(1, Ordering::SeqCst);
    }

    /// Puts `tokens` in charge: the API starts using them, and they are kept for the next start.
    async fn install(&self, tokens: Tokens) {
        self.install_with(tokens, true).await;
    }

    /// `persist` is false for a token that must not be written down (one from the environment).
    async fn install_with(&self, tokens: Tokens, persist: bool) {
        let creds = Credentials {
            client_id: self.client_id.clone(),
            access_token: tokens.access_token.clone(),
        };
        *lock(&self.viewer) = tokens.user_id.clone();
        lock(&self.inner).tokens = Some(tokens.clone());
        self.bump();
        (self.apply)(Some(creds));
        if persist {
            self.persist(tokens).await;
        }
    }

    /// Takes a user token handed in through the environment (`TWITCH_ACCESS_TOKEN`, a developer
    /// affordance): asks whose it is so Following works, and never stores it. `None` if it is
    /// not a user token (an app token browses fine, it just has no account).
    pub async fn adopt(&self, access_token: String) -> Option<User> {
        let who = match self.auth.validate(&access_token).await {
            Ok(who) => who,
            Err(err) => {
                tracing::info!(%err, "the token from the environment has no account");
                return None;
            }
        };
        let user = user_of(&who.login, &who.scopes);
        let tokens = Tokens {
            access_token,
            refresh_token: None,
            expires_at: None,
            scopes: who.scopes,
            login: Some(who.login),
            user_id: Some(who.user_id),
        };
        self.install_with(tokens, false).await;
        Some(user)
    }

    /// Writes the tokens to the keyring or the private file, off the async threads (both can
    /// block). A failure is logged and the session carries on in memory: the user just has to
    /// sign in again after a restart.
    async fn persist(&self, tokens: Tokens) {
        let Some(store) = self.store.clone() else {
            return;
        };
        let saved = tokio::task::spawn_blocking(move || store.save(&tokens)).await;
        match saved {
            Ok(Ok(location)) => tracing::debug!(?location, "tokens saved"),
            Ok(Err(err)) => {
                tracing::warn!(%err, "could not save the tokens, they last for this run");
            }
            Err(err) => tracing::warn!(%err, "the token save was cut short"),
        }
    }

    /// Drops the tokens everywhere. Returns what was there, to revoke it.
    async fn forget(&self) -> Option<Tokens> {
        let old = lock(&self.inner).tokens.take();
        *lock(&self.viewer) = None;
        self.bump();
        (self.apply)(None);
        if let Some(store) = self.store.clone() {
            let cleared = tokio::task::spawn_blocking(move || store.clear()).await;
            match cleared {
                Ok(Ok(())) => {}
                Ok(Err(err)) => tracing::warn!(%err, "could not remove the stored tokens"),
                Err(err) => tracing::warn!(%err, "the token removal was cut short"),
            }
        }
        old
    }

    fn tokens_from(&self, grant: &TokenGrant, who: &Identity, previous: Option<&Tokens>) -> Tokens {
        Tokens {
            access_token: grant.access_token.clone(),
            // Twitch sends a new refresh token with every refresh. Keep the old one only if a
            // response ever leaves it out.
            refresh_token: grant
                .refresh_token
                .clone()
                .or_else(|| previous.and_then(|p| p.refresh_token.clone())),
            expires_at: grant.expires_in.map(|d| now_secs() + d.as_secs()),
            scopes: if grant.scopes.is_empty() {
                who.scopes.clone()
            } else {
                grant.scopes.clone()
            },
            login: Some(who.login.clone()),
            user_id: Some(who.user_id.clone()),
        }
    }

    /// Restores the stored login at startup. Returns the user to show at once, without waiting
    /// for the network: an expired token is renewed in the background by `resume`.
    pub fn restore(&self) -> Option<User> {
        // Without a client ID the stored tokens cannot be used (Helix wants both).
        if self.client_id.is_empty() {
            return None;
        }
        let store = self.store.as_ref()?;
        let tokens = match store.load() {
            Ok(Some(tokens)) => tokens,
            Ok(None) => return None,
            Err(err) => {
                tracing::warn!(%err, "could not read the stored tokens, starting signed out");
                return None;
            }
        };
        let login = tokens.login.clone()?;
        // Tokens from before the user id was stored cannot ask for follows. Sign in again.
        tokens.user_id.as_ref()?;
        tracing::info!(%login, "restored the stored sign-in");
        let user = user_of(&login, &tokens.scopes);
        // Not written back: they were just read from there.
        let creds = Credentials {
            client_id: self.client_id.clone(),
            access_token: tokens.access_token.clone(),
        };
        *lock(&self.viewer) = tokens.user_id.clone();
        lock(&self.inner).tokens = Some(tokens);
        self.bump();
        (self.apply)(Some(creds));
        Some(user)
    }

    /// Renews the access token if it is about to expire. Called at startup and when the machine
    /// wakes from sleep, where an old token is the likely state. Returns false only if the
    /// session ended because Twitch refused the refresh token.
    pub async fn resume(&self) -> bool {
        let due = {
            let inner = lock(&self.inner);
            match &inner.tokens {
                None => return true,
                Some(t) => t.is_expired(now_secs() + EXPIRY_MARGIN.as_secs()),
            }
        };
        if !due {
            return true;
        }
        let seen = self.generation.load(Ordering::SeqCst);
        self.refresh_inner(seen).await != Refreshed::Ended
    }
}

/// How a refresh attempt went.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Refreshed {
    /// New credentials are in place (this call renewed them, or another one just had).
    Renewed,
    /// Nothing changed, e.g. the network is down. The session lives on.
    Unchanged,
    /// Twitch refused the refresh token. The session was ended.
    Ended,
}

impl Session {
    /// Renews the access token, unless it already moved past generation `seen`. One at a time:
    /// the refresh token is single use, so two parallel refreshes would kill each other.
    async fn refresh_inner(&self, seen: u64) -> Refreshed {
        let _gate = self.refresh_gate.lock().await;
        if self.generation.load(Ordering::SeqCst) != seen {
            // Somebody renewed (or signed in or out) while this call waited for the gate.
            return if self.is_signed_in() {
                Refreshed::Renewed
            } else {
                Refreshed::Unchanged
            };
        }
        let Some(old) = lock(&self.inner).tokens.clone() else {
            return Refreshed::Unchanged;
        };
        let Some(refresh) = old.refresh_token.clone() else {
            // No way to renew. The request will fail with 401 and the user signs in again.
            tracing::warn!("the access token expired and there is no refresh token");
            return Refreshed::Unchanged;
        };
        let renewed = async {
            let grant = self.auth.refresh(&refresh).await?;
            let who = self.auth.validate(&grant.access_token).await?;
            Ok::<_, Error>((grant, who))
        };
        match renewed.await {
            Ok((grant, who)) => {
                let tokens = self.tokens_from(&grant, &who, Some(&old));
                tracing::info!("access token refreshed");
                self.install(tokens).await;
                Refreshed::Renewed
            }
            Err(Error::AuthRejected(why)) => {
                // The refresh token is dead (revoked on twitch.tv, or unused for too long).
                tracing::warn!(%why, "refresh token refused, signing out");
                self.forget().await;
                (self.notify)(Event::SignedOut);
                Refreshed::Ended
            }
            Err(err) => {
                tracing::warn!(%err, "could not refresh the access token, will try again");
                Refreshed::Unchanged
            }
        }
    }

    /// Starts a device-code login, replacing one that is still polling.
    pub fn start_login(self: &Arc<Self>) {
        let me = Arc::clone(self);
        let handle = tokio::spawn(async move { me.run_login().await });
        if let Some(old) = lock(&self.login).replace(handle) {
            old.abort();
        }
    }

    /// Stops polling for an approval that nobody is waiting for any more.
    pub fn cancel_login(&self) {
        if let Some(handle) = lock(&self.login).take() {
            handle.abort();
        }
    }

    async fn run_login(&self) {
        // Every scope at once, so a later feature needs no new sign-in.
        let grant = match self.auth.request_device_code(SCOPES).await {
            Ok(grant) => grant,
            Err(err) => {
                tracing::warn!(%err, "could not start the login");
                (self.notify)(Event::LoginFailed(LoginProblem::Failed(problem_text(&err))));
                return;
            }
        };
        (self.notify)(Event::DeviceCodeIssued(DeviceCode {
            user_code: grant.user_code.clone(),
            verification_uri: grant.verification_uri.clone(),
        }));
        match self.poll(&grant).await {
            Ok(tokens) => self.finish_login(tokens).await,
            Err(problem) => (self.notify)(Event::LoginFailed(problem)),
        }
    }

    /// Polls at the server's interval until the user approves, refuses, or the code expires.
    async fn poll(&self, grant: &DeviceGrant) -> Result<TokenGrant, LoginProblem> {
        let deadline = Instant::now() + grant.expires_in;
        let mut interval = grant.interval.max(Duration::from_millis(1));
        loop {
            tokio::time::sleep(interval).await;
            if Instant::now() >= deadline {
                return Err(LoginProblem::Expired);
            }
            let answer = self.auth.poll_token(&grant.device_code, SCOPES).await;
            match answer {
                Ok(PollOutcome::Pending) => {}
                Ok(PollOutcome::SlowDown) => interval += self.timing.slow_down_step,
                Ok(PollOutcome::Expired) => return Err(LoginProblem::Expired),
                Ok(PollOutcome::Denied) => return Err(LoginProblem::Denied),
                Ok(PollOutcome::Granted(tokens)) => return Ok(tokens),
                // A dropped connection must not end a login the user is about to approve.
                Err(err) if is_transient(&err) => {
                    tracing::debug!(%err, "poll failed, trying again");
                }
                Err(err) => return Err(LoginProblem::Failed(problem_text(&err))),
            }
        }
    }
}

impl Session {
    /// Asks who the new token belongs to, stores it and tells the store.
    async fn finish_login(&self, grant: TokenGrant) {
        let who = match self.auth.validate(&grant.access_token).await {
            Ok(who) => who,
            Err(err) => {
                tracing::warn!(%err, "could not look up the new account");
                (self.notify)(Event::LoginFailed(LoginProblem::Failed(problem_text(&err))));
                return;
            }
        };
        let tokens = self.tokens_from(&grant, &who, None);
        let user = user_of(&who.login, &tokens.scopes);
        tracing::info!(login = %user.login, "signed in");
        self.install(tokens).await;
        (self.notify)(Event::SignedIn(user));
    }

    /// Signs out: the credentials are gone at once, the revoke is best effort.
    pub async fn sign_out(&self) {
        if let Some(tokens) = self.forget().await {
            if let Err(err) = self.auth.revoke(&tokens.access_token).await {
                tracing::debug!(%err, "could not revoke the token (it is forgotten anyway)");
            }
        }
        (self.notify)(Event::SignedOut);
    }

    /// True for the commands the session runs.
    pub fn handles(cmd: &Command) -> bool {
        matches!(
            cmd,
            Command::StartLogin | Command::CancelLogin | Command::SignOut
        )
    }

    pub async fn execute(self: &Arc<Self>, cmd: Command) {
        match cmd {
            Command::StartLogin => self.start_login(),
            Command::CancelLogin => self.cancel_login(),
            Command::SignOut => self.sign_out().await,
            _ => {}
        }
    }
}

impl TokenSource for Session {
    fn credentials(&self) -> Option<Credentials> {
        lock(&self.inner).tokens.as_ref().map(|t| Credentials {
            client_id: self.client_id.clone(),
            access_token: t.access_token.clone(),
        })
    }
}

impl Refresher for Session {
    fn generation(&self) -> u64 {
        self.generation.load(Ordering::SeqCst)
    }

    async fn refresh(&self, seen: u64) -> bool {
        self.refresh_inner(seen).await == Refreshed::Renewed
    }
}
