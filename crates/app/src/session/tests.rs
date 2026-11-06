//! Session tests: the login poll in every state, token storage, restore, refresh and sign-out.
//! Twitch is a scripted `FakeAuth` and the keyring an in-memory fake.

use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant as StdInstant};

use htpc_storage::{BackendResult, SecretBackend, TokenStore, Tokens};
use htpc_twitch_api::{DeviceGrant, FakeAuth, FakeAuthCall, PollOutcome, Refresher};

use super::*;

/// An in-memory secret, shared between clones so a "restart" sees it.
#[derive(Clone, Default)]
struct Mem(Arc<Mutex<Option<String>>>);

impl SecretBackend for Mem {
    fn get(&self) -> BackendResult<Option<String>> {
        Ok(self.0.lock().unwrap().clone())
    }

    fn set(&self, secret: &str) -> BackendResult<()> {
        *self.0.lock().unwrap() = Some(secret.to_owned());
        Ok(())
    }

    fn delete(&self) -> BackendResult<()> {
        *self.0.lock().unwrap() = None;
        Ok(())
    }

    fn name(&self) -> &'static str {
        "mem"
    }
}

type Events = Arc<Mutex<Vec<Event>>>;
type Applied = Arc<Mutex<Vec<Option<String>>>>;

struct H {
    session: Arc<Session>,
    auth: FakeAuth,
    events: Events,
    applied: Applied,
    store: Arc<TokenStore>,
    /// The in-memory keyring behind `store`, so a test can "restart" over the same secrets.
    keyring: Mem,
    viewer: Viewer,
}

fn grant(expires_in: Duration) -> DeviceGrant {
    DeviceGrant {
        device_code: "dev".into(),
        user_code: "ABCD-EFGH".into(),
        verification_uri: "https://www.twitch.tv/activate?device-code=ABCDEFGH".into(),
        expires_in,
        interval: Duration::from_millis(5),
    }
}

fn store_over(keyring: &Mem) -> Arc<TokenStore> {
    Arc::new(TokenStore::with_backends(
        Box::new(keyring.clone()),
        Box::new(Mem::default()),
    ))
}

fn harness_with(auth: FakeAuth, keyring: &Mem, timing: Timing) -> H {
    let events = Events::default();
    let applied = Applied::default();
    let viewer = Viewer::default();
    let store = store_over(keyring);
    let notify: Notify = {
        let events = Arc::clone(&events);
        Arc::new(move |e| events.lock().unwrap().push(e))
    };
    let apply: Apply = {
        let applied = Arc::clone(&applied);
        Arc::new(move |c: Option<Credentials>| {
            applied.lock().unwrap().push(c.map(|c| c.access_token));
        })
    };
    let session = Session::new(
        AppAuth::Fake(auth.clone()),
        "client",
        Some(Arc::clone(&store)),
        apply,
        notify,
        Arc::clone(&viewer),
    )
    .with_timing(timing);
    H {
        session,
        auth,
        events,
        applied,
        store,
        keyring: keyring.clone(),
        viewer,
    }
}

fn harness(auth: FakeAuth) -> H {
    harness_with(auth, &Mem::default(), Timing::default())
}

impl H {
    fn events(&self) -> Vec<Event> {
        self.events.lock().unwrap().clone()
    }

    fn last_applied(&self) -> Option<String> {
        self.applied.lock().unwrap().last().cloned().flatten()
    }

    async fn wait(&self, what: &str, pred: impl Fn(&[Event]) -> bool) {
        for _ in 0..600 {
            if pred(&self.events.lock().unwrap()) {
                return;
            }
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
        panic!("timed out waiting for {what}, saw {:?}", self.events());
    }

    fn polls(&self) -> usize {
        let polls = |c: &FakeAuthCall| matches!(c, FakeAuthCall::Poll(_));
        self.auth.calls().iter().filter(|c| polls(c)).count()
    }
}

fn failed(events: &[Event]) -> Option<LoginProblem> {
    events.iter().find_map(|e| match e {
        Event::LoginFailed(p) => Some(p.clone()),
        _ => None,
    })
}

fn signed_in(events: &[Event]) -> bool {
    events.iter().any(|e| matches!(e, Event::SignedIn(_)))
}

fn stored(user_id: Option<&str>, refresh: Option<&str>, expires_at: Option<u64>) -> Tokens {
    Tokens {
        access_token: "old-access".into(),
        refresh_token: refresh.map(str::to_owned),
        expires_at,
        scopes: vec!["user:read:follows".into()],
        login: Some("nightowl_dev".into()),
        user_id: user_id.map(str::to_owned),
    }
}

// ---- the login poll ---------------------------------------------------------------------------

#[tokio::test]
async fn a_login_shows_the_code_then_signs_in_when_approved() {
    let auth = FakeAuth::new()
        .with_grant(grant(Duration::from_secs(30)))
        .then_poll(Ok(PollOutcome::Pending))
        .then_poll(Ok(PollOutcome::Granted(FakeAuth::token("a"))));
    let h = harness(auth);
    h.session.start_login();
    h.wait("sign-in", signed_in).await;

    let events = h.events();
    let code = DeviceCode {
        user_code: "ABCD-EFGH".into(),
        verification_uri: "https://www.twitch.tv/activate?device-code=ABCDEFGH".into(),
    };
    assert_eq!(events[0], Event::DeviceCodeIssued(code));
    let user = User {
        login: "nightowl_dev".into(),
        can_chat: true,
    };
    assert_eq!(events[1], Event::SignedIn(user));
    assert_eq!(h.polls(), 2);
    // The API got the new token and the provider the user id.
    assert_eq!(h.last_applied().as_deref(), Some("a-access"));
    assert_eq!(h.viewer.lock().unwrap().as_deref(), Some("1000"));
    // And it is kept for the next start, with who it belongs to.
    let kept = h.store.load().unwrap().expect("tokens saved");
    assert_eq!(kept.access_token, "a-access");
    assert_eq!(kept.refresh_token.as_deref(), Some("a-refresh"));
    assert_eq!(kept.login.as_deref(), Some("nightowl_dev"));
    assert_eq!(kept.user_id.as_deref(), Some("1000"));
    assert!(kept.expires_at.is_some());
}

#[tokio::test]
async fn slow_down_widens_the_polling_interval() {
    let auth = FakeAuth::new()
        .with_grant(grant(Duration::from_secs(30)))
        .then_poll(Ok(PollOutcome::SlowDown))
        .then_poll(Ok(PollOutcome::Granted(FakeAuth::token("a"))));
    let timing = Timing {
        slow_down_step: Duration::from_millis(60),
    };
    let h = harness_with(auth, &Mem::default(), timing);
    let started = StdInstant::now();
    h.session.start_login();
    h.wait("sign-in", signed_in).await;
    // 5 ms before the first poll, then 5 + 60 ms before the second.
    assert!(started.elapsed() >= Duration::from_millis(65));
}

#[tokio::test]
async fn each_end_of_a_login_is_reported() {
    let cases = [
        (PollOutcome::Expired, LoginProblem::Expired),
        (PollOutcome::Denied, LoginProblem::Denied),
    ];
    for (outcome, problem) in cases {
        let auth = FakeAuth::new()
            .with_grant(grant(Duration::from_secs(30)))
            .then_poll(Ok(outcome));
        let h = harness(auth);
        h.session.start_login();
        h.wait("problem", |e| failed(e).is_some()).await;
        assert_eq!(failed(&h.events()), Some(problem));
        assert_eq!(h.last_applied(), None, "nothing was installed");
        assert!(h.store.load().unwrap().is_none());
    }
}

#[tokio::test]
async fn a_code_that_runs_out_is_expired_even_without_an_answer() {
    let auth = FakeAuth::new().with_grant(grant(Duration::from_millis(40)));
    let h = harness(auth);
    h.session.start_login();
    h.wait("expiry", |e| failed(e).is_some()).await;
    assert_eq!(failed(&h.events()), Some(LoginProblem::Expired));
}

#[tokio::test]
async fn a_hiccup_does_not_end_the_login() {
    let outage = Error::Server {
        status: 503,
        message: "down".into(),
    };
    let auth = FakeAuth::new()
        .with_grant(grant(Duration::from_secs(30)))
        .then_poll(Err(outage))
        .then_poll(Ok(PollOutcome::Granted(FakeAuth::token("a"))));
    let h = harness(auth);
    h.session.start_login();
    h.wait("sign-in", signed_in).await;
    assert_eq!(h.polls(), 2);
}

#[tokio::test]
async fn a_hard_failure_ends_the_login_with_its_message() {
    let auth = FakeAuth::new()
        .with_grant(grant(Duration::from_secs(30)))
        .then_poll(Err(Error::AuthRejected("invalid client".into())));
    let h = harness(auth);
    h.session.start_login();
    h.wait("problem", |e| failed(e).is_some()).await;
    let why = LoginProblem::Failed("invalid client".into());
    assert_eq!(failed(&h.events()), Some(why));
}

#[tokio::test]
async fn no_client_id_says_so_on_the_sign_in_screen() {
    let events = Events::default();
    let notify: Notify = {
        let events = Arc::clone(&events);
        Arc::new(move |e| events.lock().unwrap().push(e))
    };
    let apply: Apply = Arc::new(|_| {});
    let auth = AppAuth::Unavailable(NO_CLIENT_ID.to_owned());
    let session = Session::new(auth, "", None, apply, notify, Viewer::default());
    session.start_login();
    for _ in 0..200 {
        if !events.lock().unwrap().is_empty() {
            break;
        }
        tokio::time::sleep(Duration::from_millis(5)).await;
    }
    let event = events.lock().unwrap()[0].clone();
    let why = LoginProblem::Failed(NO_CLIENT_ID.into());
    assert_eq!(event, Event::LoginFailed(why));
}

#[tokio::test]
async fn cancelling_stops_the_polling() {
    let h = harness(FakeAuth::new().with_grant(grant(Duration::from_secs(30))));
    h.session.start_login();
    h.wait("code", |e| !e.is_empty()).await;
    h.session.cancel_login();
    tokio::time::sleep(Duration::from_millis(20)).await;
    let before = h.polls();
    tokio::time::sleep(Duration::from_millis(60)).await;
    assert_eq!(h.polls(), before, "no polls after the cancel");
    assert_eq!(failed(&h.events()), None, "a cancel is not a failure");
}

// ---- restore, persistence and resume ----------------------------------------------------------

/// A harness whose keyring already holds `tokens`, as after a restart.
fn restarted(tokens: &Tokens, auth: FakeAuth) -> H {
    let keyring = Mem::default();
    store_over(&keyring).save(tokens).unwrap();
    harness_with(auth, &keyring, Timing::default())
}

#[tokio::test]
async fn a_stored_login_is_restored_without_the_network() {
    let tokens = stored(Some("1000"), Some("r"), Some(now_secs() + 3600));
    let h = restarted(&tokens, FakeAuth::new());
    let user = h.session.restore().expect("restored");
    assert_eq!(user.login, "nightowl_dev");
    assert!(h.session.is_signed_in());
    assert_eq!(h.last_applied().as_deref(), Some("old-access"));
    assert_eq!(h.viewer.lock().unwrap().as_deref(), Some("1000"));
    assert!(h.auth.calls().is_empty(), "no request at startup");
}

#[tokio::test]
async fn a_token_from_before_chat_sending_cannot_chat_and_a_new_login_can() {
    // `stored` was granted `user:read:follows` only, like every sign-in before chat sending.
    let old = stored(Some("1000"), Some("r"), Some(now_secs() + 3600));
    let h = restarted(&old, FakeAuth::new());
    let user = h.session.restore().expect("restored");
    assert!(!user.can_chat, "needs a new sign-in for chat");

    let mut chatty = old.clone();
    chatty.scopes.push("user:write:chat".into());
    let h = restarted(&chatty, FakeAuth::new());
    assert!(h.session.restore().expect("restored").can_chat);
}

#[tokio::test]
async fn a_login_asks_for_every_scope_and_can_chat() {
    let auth = FakeAuth::new()
        .with_grant(grant(Duration::from_secs(30)))
        .then_poll(Ok(PollOutcome::Granted(FakeAuth::token("a"))));
    let h = harness(auth);
    h.session.start_login();
    h.wait("sign-in", signed_in).await;
    let user = h.events().into_iter().find_map(|e| match e {
        Event::SignedIn(u) => Some(u),
        _ => None,
    });
    assert!(user.expect("signed in").can_chat);
    let kept = h.store.load().unwrap().unwrap();
    assert!(kept.scopes.iter().any(|s| s == "user:write:chat"));
}

#[tokio::test]
async fn a_login_survives_a_restart() {
    let keyring = Mem::default();
    let auth = FakeAuth::new()
        .with_grant(grant(Duration::from_secs(30)))
        .then_poll(Ok(PollOutcome::Granted(FakeAuth::token("a"))));
    let first = harness_with(auth, &keyring, Timing::default());
    first.session.start_login();
    first.wait("sign-in", signed_in).await;

    let second = harness_with(FakeAuth::new(), &keyring, Timing::default());
    let user = second.session.restore().expect("restored after a restart");
    assert_eq!(user.login, "nightowl_dev");
    assert_eq!(second.last_applied().as_deref(), Some("a-access"));
}

#[tokio::test]
async fn nothing_stored_or_an_old_format_means_signed_out() {
    let empty = harness(FakeAuth::new());
    assert!(empty.session.restore().is_none());
    assert!(!empty.session.is_signed_in());

    // Tokens written before the user id was kept cannot ask for follows: sign in again.
    let old = stored(None, Some("r"), None);
    let h = restarted(&old, FakeAuth::new());
    assert!(h.session.restore().is_none());
    assert_eq!(h.last_applied(), None);
}

#[tokio::test]
async fn resume_leaves_a_fresh_token_alone() {
    let tokens = stored(Some("1000"), Some("r"), Some(now_secs() + 3600));
    let h = restarted(&tokens, FakeAuth::new());
    h.session.restore();
    assert!(h.session.resume().await);
    assert!(h.auth.calls().is_empty());
}

#[tokio::test]
async fn resume_renews_an_expired_token_silently() {
    let tokens = stored(Some("1000"), Some("old-refresh"), Some(now_secs() - 10));
    let auth = FakeAuth::new().then_refresh(Ok(FakeAuth::token("new")));
    let h = restarted(&tokens, auth);
    h.session.restore();
    assert!(h.session.resume().await);

    assert_eq!(h.last_applied().as_deref(), Some("new-access"));
    let kept = h.store.load().unwrap().unwrap();
    assert_eq!(kept.access_token, "new-access");
    assert_eq!(kept.refresh_token.as_deref(), Some("new-refresh"));
    assert!(h.events().is_empty(), "the user is not told: it was silent");
    let refreshed = FakeAuthCall::Refresh("old-refresh".into());
    assert!(h.auth.calls().contains(&refreshed));
}

#[tokio::test]
async fn resume_renews_a_token_that_is_about_to_expire() {
    let soon = now_secs() + EXPIRY_MARGIN.as_secs() - 5;
    let tokens = stored(Some("1000"), Some("r"), Some(soon));
    let auth = FakeAuth::new().then_refresh(Ok(FakeAuth::token("new")));
    let h = restarted(&tokens, auth);
    h.session.restore();
    h.session.resume().await;
    assert_eq!(h.last_applied().as_deref(), Some("new-access"));
}

#[tokio::test]
async fn a_dead_refresh_token_signs_the_user_out() {
    let tokens = stored(Some("1000"), Some("revoked"), Some(now_secs() - 10));
    let h = restarted(&tokens, FakeAuth::new());
    h.session.restore();
    assert!(!h.session.resume().await, "the session ended");

    assert!(!h.session.is_signed_in());
    assert_eq!(h.events(), vec![Event::SignedOut]);
    assert_eq!(h.last_applied(), None, "the API is anonymous again");
    assert!(h.store.load().unwrap().is_none(), "the tokens are gone");
    assert_eq!(*h.viewer.lock().unwrap(), None);
}

#[tokio::test]
async fn a_refresh_outage_keeps_the_session() {
    let outage = Error::Server {
        status: 503,
        message: "down".into(),
    };
    let tokens = stored(Some("1000"), Some("r"), Some(now_secs() - 10));
    let h = restarted(&tokens, FakeAuth::new().then_refresh(Err(outage)));
    h.session.restore();
    assert!(h.session.resume().await, "not ended: Twitch is just down");
    assert!(h.session.is_signed_in());
    assert!(h.events().is_empty());
    assert!(h.store.load().unwrap().is_some());
}

// ---- refresh on 401 ---------------------------------------------------------------------------

use htpc_twitch_api::{
    FollowedChannel, Game, Page, PageRequest, RefreshingApi, StreamInfo, TwitchApi, UserInfo,
};

type ApiResult<T> = htpc_twitch_api::Result<T>;

/// A `TwitchApi` that answers 401 to the first `rejects` category requests, then an empty page.
/// With `accepts` set it behaves like Twitch instead: 401 until the session has installed that
/// token, whatever the order of the calls.
#[derive(Clone)]
struct Flaky {
    rejects: Arc<Mutex<u32>>,
    calls: Arc<Mutex<u32>>,
    accepts: Option<(Applied, String)>,
}

impl Flaky {
    fn rejecting(n: u32) -> Self {
        Self {
            rejects: Arc::new(Mutex::new(n)),
            calls: Arc::default(),
            accepts: None,
        }
    }

    /// Refuses every request until `token` is the one the session handed to the API.
    fn accepting_only(applied: &Applied, token: &str) -> Self {
        Self {
            accepts: Some((Arc::clone(applied), token.to_owned())),
            ..Self::rejecting(0)
        }
    }

    fn calls(&self) -> u32 {
        *self.calls.lock().unwrap()
    }
}

impl TwitchApi for Flaky {
    async fn top_categories(&self, _: PageRequest) -> ApiResult<Page<Game>> {
        *self.calls.lock().unwrap() += 1;
        if let Some((applied, good)) = &self.accepts {
            let current = applied.lock().unwrap().last().cloned().flatten();
            if current.as_deref() != Some(good.as_str()) {
                return Err(Error::Unauthorized("Invalid OAuth token".into()));
            }
        }
        let mut left = self.rejects.lock().unwrap();
        if *left > 0 {
            *left -= 1;
            return Err(Error::Unauthorized("Invalid OAuth token".into()));
        }
        Ok(Page {
            items: Vec::new(),
            cursor: None,
        })
    }

    async fn top_streams(&self, _: PageRequest) -> ApiResult<Page<StreamInfo>> {
        unreachable!()
    }

    async fn followed_streams(&self, _: &str, _: PageRequest) -> ApiResult<Page<StreamInfo>> {
        unreachable!()
    }

    async fn followed_channels(&self, _: &str, _: PageRequest) -> ApiResult<Page<FollowedChannel>> {
        unreachable!()
    }

    async fn streams_by_category(&self, _: &str, _: PageRequest) -> ApiResult<Page<StreamInfo>> {
        unreachable!()
    }

    async fn user(&self, _: Option<&str>) -> ApiResult<Option<UserInfo>> {
        unreachable!()
    }

    async fn search_channels(
        &self,
        _: &str,
        _: bool,
        _: PageRequest,
    ) -> ApiResult<Page<htpc_twitch_api::ChannelResult>> {
        unreachable!()
    }

    async fn search_categories(&self, _: &str, _: PageRequest) -> ApiResult<Page<Game>> {
        unreachable!()
    }

    async fn streams_by_login(&self, _: &[String]) -> ApiResult<Vec<StreamInfo>> {
        unreachable!()
    }
}

/// Signed in with a token the server will refuse, and a refresh token to repair it with.
fn logged_in(auth: FakeAuth) -> H {
    let tokens = stored(Some("1000"), Some("old-refresh"), Some(now_secs() + 3600));
    let h = restarted(&tokens, auth);
    h.session.restore();
    h
}

#[tokio::test]
async fn a_401_refreshes_the_token_and_repeats_the_request() {
    let h = logged_in(FakeAuth::new().then_refresh(Ok(FakeAuth::token("new"))));
    let flaky = Flaky::rejecting(1);
    let api = RefreshingApi::new(flaky.clone(), Arc::clone(&h.session));

    api.top_categories(PageRequest::default()).await.unwrap();
    assert_eq!(flaky.calls(), 2, "the request ran again after the refresh");
    assert_eq!(h.last_applied().as_deref(), Some("new-access"));
    assert!(h.events().is_empty(), "silent: the user is not told");
}

#[tokio::test]
async fn a_second_401_is_not_retried_for_ever() {
    let h = logged_in(FakeAuth::new().then_refresh(Ok(FakeAuth::token("new"))));
    let flaky = Flaky::rejecting(5);
    let api = RefreshingApi::new(flaky.clone(), Arc::clone(&h.session));

    let err = api
        .top_categories(PageRequest::default())
        .await
        .unwrap_err();
    assert!(matches!(err, Error::Unauthorized(_)), "{err:?}");
    assert_eq!(flaky.calls(), 2, "one repeat, no more");
}

#[tokio::test]
async fn a_401_with_a_dead_refresh_token_signs_out_and_fails_the_request() {
    let h = logged_in(FakeAuth::new());
    let flaky = Flaky::rejecting(5);
    let api = RefreshingApi::new(flaky.clone(), Arc::clone(&h.session));

    let err = api
        .top_categories(PageRequest::default())
        .await
        .unwrap_err();
    assert!(matches!(err, Error::Unauthorized(_)), "{err:?}");
    assert_eq!(flaky.calls(), 1, "nothing to repeat with");
    assert_eq!(h.events(), vec![Event::SignedOut]);
    assert!(!h.session.is_signed_in());
}

fn refresh_calls(h: &H) -> usize {
    let refresh = |c: &FakeAuthCall| matches!(c, FakeAuthCall::Refresh(_));
    h.auth.calls().iter().filter(|c| refresh(c)).count()
}

#[tokio::test]
async fn parallel_401s_refresh_once() {
    // Refresh tokens are single use: a second refresh would be refused and sign the user out.
    let h = logged_in(FakeAuth::new().then_refresh(Ok(FakeAuth::token("new"))));
    let flaky = Flaky::accepting_only(&h.applied, "new-access");
    let api = Arc::new(RefreshingApi::new(flaky.clone(), Arc::clone(&h.session)));

    let tasks: Vec<_> = (0..3)
        .map(|_| {
            let api = Arc::clone(&api);
            tokio::spawn(async move { api.top_categories(PageRequest::default()).await })
        })
        .collect();
    for task in tasks {
        task.await.unwrap().unwrap();
    }
    assert_eq!(refresh_calls(&h), 1);
    assert!(h.session.is_signed_in());
    assert_eq!(h.last_applied().as_deref(), Some("new-access"));
}

#[tokio::test]
async fn the_generation_moves_with_every_credential_change() {
    let h = logged_in(FakeAuth::new().then_refresh(Ok(FakeAuth::token("new"))));
    let before = h.session.generation();
    assert!(h.session.refresh(before).await);
    assert!(h.session.generation() > before);
    // A caller holding the old generation does not refresh again: it just retries.
    assert!(h.session.refresh(before).await);
    assert_eq!(refresh_calls(&h), 1);
}

// ---- sign-out and environment tokens ------------------------------------------------------------

#[tokio::test]
async fn sign_out_forgets_everywhere_and_revokes() {
    let h = logged_in(FakeAuth::new());
    h.session.sign_out().await;

    assert!(!h.session.is_signed_in());
    assert_eq!(h.events(), vec![Event::SignedOut]);
    assert_eq!(h.last_applied(), None);
    assert_eq!(*h.viewer.lock().unwrap(), None);
    assert!(h.store.load().unwrap().is_none(), "gone from the keyring");
    let revoked = FakeAuthCall::Revoke("old-access".into());
    assert!(h.auth.calls().contains(&revoked));

    // And it stays gone after a restart.
    let again = harness_with(FakeAuth::new(), &h.keyring, Timing::default());
    assert!(again.session.restore().is_none());
}

#[tokio::test]
async fn signing_out_while_signed_out_still_reports() {
    let h = harness(FakeAuth::new());
    h.session.sign_out().await;
    assert_eq!(h.events(), vec![Event::SignedOut]);
    assert!(h.auth.calls().is_empty(), "nothing to revoke");
}

#[tokio::test]
async fn a_token_from_the_environment_is_used_but_never_stored() {
    let h = harness(FakeAuth::new());
    let user = h.session.adopt("env-token".into()).await.expect("a user");
    assert_eq!(user.login, "nightowl_dev");
    assert_eq!(h.last_applied().as_deref(), Some("env-token"));
    assert_eq!(h.viewer.lock().unwrap().as_deref(), Some("1000"));
    assert!(h.store.load().unwrap().is_none(), "not written down");
}

#[test]
fn the_session_runs_only_its_own_commands() {
    assert!(Session::handles(&Command::StartLogin));
    assert!(Session::handles(&Command::CancelLogin));
    assert!(Session::handles(&Command::SignOut));
    assert!(!Session::handles(&Command::LeaveChat));
    assert!(!Session::handles(&Command::StopPlayback));
}
