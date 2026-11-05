//! `HttpAuth` against a `wiremock` server: the device-code request, every poll state, refresh
//! and validate.

use std::time::Duration;

use htpc_twitch_api::{AuthApi, Error, HttpAuth, PollOutcome, SCOPE_READ_FOLLOWS};
use serde_json::json;
use wiremock::matchers::{body_string_contains, header, method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

fn client(server: &MockServer) -> HttpAuth {
    HttpAuth::with_base_url(server.uri(), "test-client").unwrap()
}

fn bad_request(message: &str) -> ResponseTemplate {
    ResponseTemplate::new(400).set_body_json(json!({ "status": 400, "message": message }))
}

async fn poll_answering(template: ResponseTemplate) -> Result<PollOutcome, Error> {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/token"))
        .respond_with(template)
        .mount(&server)
        .await;
    client(&server)
        .poll_token("dev-code", &[SCOPE_READ_FOLLOWS])
        .await
}

#[tokio::test]
async fn device_code_request_sends_the_client_id_and_scopes() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/device"))
        .and(body_string_contains("client_id=test-client"))
        .and(body_string_contains("scopes=user%3Aread%3Afollows"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "device_code": "dev-code",
            "expires_in": 1800,
            "interval": 5,
            "user_code": "ABCDEFGH",
            "verification_uri": "https://www.twitch.tv/activate?device-code=ABCDEFGH",
        })))
        .expect(1)
        .mount(&server)
        .await;

    let grant = client(&server)
        .request_device_code(&[SCOPE_READ_FOLLOWS])
        .await
        .unwrap();
    assert_eq!(grant.device_code, "dev-code");
    assert_eq!(grant.user_code, "ABCDEFGH");
    assert!(grant.verification_uri.contains("device-code=ABCDEFGH"));
    assert_eq!(grant.expires_in, Duration::from_secs(1800));
    assert_eq!(grant.interval, Duration::from_secs(5));
}

#[tokio::test]
async fn a_missing_interval_falls_back_to_five_seconds() {
    let server = MockServer::start().await;
    Mock::given(path("/device"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "device_code": "d", "expires_in": 60, "user_code": "U",
            "verification_uri": "https://example.test/activate",
        })))
        .mount(&server)
        .await;
    let grant = client(&server).request_device_code(&[]).await.unwrap();
    assert_eq!(grant.interval, Duration::from_secs(5));
}

#[tokio::test]
async fn a_rejected_client_id_is_an_api_error() {
    let server = MockServer::start().await;
    Mock::given(path("/device"))
        .respond_with(bad_request("invalid client"))
        .mount(&server)
        .await;
    let err = client(&server).request_device_code(&[]).await.unwrap_err();
    assert!(matches!(err, Error::Api { status: 400, .. }), "{err:?}");
}

#[tokio::test]
async fn polling_sends_the_device_grant_type() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/token"))
        .and(body_string_contains("device_code=dev-code"))
        .and(body_string_contains("client_id=test-client"))
        .and(body_string_contains(
            "grant_type=urn%3Aietf%3Aparams%3Aoauth%3Agrant-type%3Adevice_code",
        ))
        .respond_with(bad_request("authorization_pending"))
        .expect(1)
        .mount(&server)
        .await;
    let outcome = client(&server)
        .poll_token("dev-code", &[SCOPE_READ_FOLLOWS])
        .await
        .unwrap();
    assert_eq!(outcome, PollOutcome::Pending);
}

#[tokio::test]
async fn poll_pending() {
    let outcome = poll_answering(bad_request("authorization_pending")).await;
    assert_eq!(outcome.unwrap(), PollOutcome::Pending);
}

#[tokio::test]
async fn poll_slow_down() {
    let outcome = poll_answering(bad_request("slow_down")).await;
    assert_eq!(outcome.unwrap(), PollOutcome::SlowDown);
}

#[tokio::test]
async fn poll_expired() {
    let outcome = poll_answering(bad_request("invalid device code")).await;
    assert_eq!(outcome.unwrap(), PollOutcome::Expired);
}

#[tokio::test]
async fn poll_denied() {
    let outcome = poll_answering(bad_request("access_denied")).await;
    assert_eq!(outcome.unwrap(), PollOutcome::Denied);
}

#[tokio::test]
async fn poll_granted_carries_the_tokens() {
    let outcome = poll_answering(ResponseTemplate::new(200).set_body_json(json!({
        "access_token": "access",
        "refresh_token": "refresh",
        "expires_in": 14124,
        "scope": ["user:read:follows"],
        "token_type": "bearer",
    })))
    .await
    .unwrap();
    let PollOutcome::Granted(grant) = outcome else {
        panic!("expected a grant, got {outcome:?}");
    };
    assert_eq!(grant.access_token, "access");
    assert_eq!(grant.refresh_token.as_deref(), Some("refresh"));
    assert_eq!(grant.expires_in, Some(Duration::from_secs(14124)));
    assert_eq!(grant.scopes, ["user:read:follows"]);
}

#[tokio::test]
async fn an_unknown_poll_failure_is_an_error_not_a_state() {
    let err = poll_answering(bad_request("invalid client"))
        .await
        .unwrap_err();
    assert!(matches!(err, Error::Api { status: 400, .. }), "{err:?}");
    let err = poll_answering(ResponseTemplate::new(503))
        .await
        .unwrap_err();
    assert!(matches!(err, Error::Server { status: 503, .. }), "{err:?}");
}

#[tokio::test]
async fn refresh_returns_the_next_token_pair() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/token"))
        .and(body_string_contains("grant_type=refresh_token"))
        .and(body_string_contains("refresh_token=old-refresh"))
        .and(body_string_contains("client_id=test-client"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "access_token": "new-access",
            "refresh_token": "new-refresh",
            "scope": ["user:read:follows"],
            "token_type": "bearer",
        })))
        .expect(1)
        .mount(&server)
        .await;
    let grant = client(&server).refresh("old-refresh").await.unwrap();
    assert_eq!(grant.access_token, "new-access");
    assert_eq!(grant.refresh_token.as_deref(), Some("new-refresh"));
    assert_eq!(grant.expires_in, None);
}

#[tokio::test]
async fn a_dead_refresh_token_is_auth_rejected() {
    let server = MockServer::start().await;
    Mock::given(path("/token"))
        .respond_with(bad_request("Invalid refresh token"))
        .mount(&server)
        .await;
    let err = client(&server).refresh("dead").await.unwrap_err();
    assert!(
        matches!(err, Error::AuthRejected(ref m) if m.contains("refresh")),
        "{err:?}"
    );
}

#[tokio::test]
async fn a_refresh_outage_is_not_auth_rejected() {
    let server = MockServer::start().await;
    Mock::given(path("/token"))
        .respond_with(ResponseTemplate::new(500))
        .mount(&server)
        .await;
    let err = client(&server).refresh("r").await.unwrap_err();
    assert!(matches!(err, Error::Server { .. }), "{err:?}");
}

#[tokio::test]
async fn validate_uses_the_oauth_header_and_names_the_user() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/validate"))
        .and(header("Authorization", "OAuth the-token"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "client_id": "test-client",
            "login": "nightowl_dev",
            "scopes": ["user:read:follows"],
            "user_id": "141981764",
            "expires_in": 5520838,
        })))
        .expect(1)
        .mount(&server)
        .await;
    let who = client(&server).validate("the-token").await.unwrap();
    assert_eq!(
        (who.user_id.as_str(), who.login.as_str()),
        ("141981764", "nightowl_dev")
    );
    assert_eq!(who.scopes, ["user:read:follows"]);
}

#[tokio::test]
async fn validate_on_a_bad_token_is_unauthorized() {
    let server = MockServer::start().await;
    Mock::given(path("/validate"))
        .respond_with(
            ResponseTemplate::new(401)
                .set_body_json(json!({ "status": 401, "message": "invalid access token" })),
        )
        .mount(&server)
        .await;
    let err = client(&server).validate("bad").await.unwrap_err();
    assert!(matches!(err, Error::Unauthorized(_)), "{err:?}");
}

#[tokio::test]
async fn revoke_sends_the_token_and_forgives_a_dead_one() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/revoke"))
        .and(body_string_contains("token=the-token"))
        .and(body_string_contains("client_id=test-client"))
        .respond_with(ResponseTemplate::new(200))
        .expect(1)
        .mount(&server)
        .await;
    client(&server).revoke("the-token").await.unwrap();

    let dead = MockServer::start().await;
    Mock::given(path("/revoke"))
        .respond_with(bad_request("Invalid token"))
        .mount(&dead)
        .await;
    client(&dead).revoke("gone").await.unwrap();

    let down = MockServer::start().await;
    Mock::given(path("/revoke"))
        .respond_with(ResponseTemplate::new(500))
        .mount(&down)
        .await;
    assert!(client(&down).revoke("t").await.is_err());
}
