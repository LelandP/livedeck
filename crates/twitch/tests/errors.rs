//! `HelixClient` error handling: 401, 429 with backoff, 5xx retries, other 4xx and bad JSON.

use std::time::{Duration, SystemTime, UNIX_EPOCH};

use htpc_twitch_api::{Credentials, Error, HelixClient, PageRequest, RetryPolicy, TwitchApi};
use wiremock::matchers::path;
use wiremock::{Mock, MockServer, ResponseTemplate};

fn fixture(name: &str) -> serde_json::Value {
    let path = format!("{}/tests/fixtures/{name}", env!("CARGO_MANIFEST_DIR"));
    serde_json::from_str(&std::fs::read_to_string(path).unwrap()).unwrap()
}

fn policy() -> RetryPolicy {
    RetryPolicy {
        max_retries: 2,
        base_backoff: Duration::from_millis(5),
        max_wait: Duration::from_millis(50),
    }
}

fn client(server: &MockServer) -> HelixClient {
    let creds = Credentials {
        client_id: "test-client".into(),
        access_token: "test-token".into(),
    };
    HelixClient::with_base_url(server.uri(), Some(creds), policy()).unwrap()
}

fn unix_now() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_secs()
}

fn rate_limited() -> ResponseTemplate {
    ResponseTemplate::new(429)
        .insert_header("Ratelimit-Limit", "800")
        .insert_header("Ratelimit-Remaining", "0")
        .insert_header("Ratelimit-Reset", (unix_now() + 1).to_string())
        .set_body_json(serde_json::json!({
            "error": "Too Many Requests", "status": 429, "message": "slow down"
        }))
}

#[tokio::test]
async fn unauthorized_is_not_retried() {
    let server = MockServer::start().await;
    Mock::given(path("/streams"))
        .respond_with(ResponseTemplate::new(401).set_body_json(fixture("error_401.json")))
        .expect(1)
        .mount(&server)
        .await;
    let err = client(&server)
        .top_streams(PageRequest::default())
        .await
        .unwrap_err();
    match err {
        Error::Unauthorized(msg) => assert_eq!(msg, "Invalid OAuth token"),
        other => panic!("expected Unauthorized, got {other:?}"),
    }
}

#[tokio::test]
async fn rate_limit_backs_off_then_succeeds() {
    let server = MockServer::start().await;
    Mock::given(path("/games/top"))
        .respond_with(rate_limited())
        .up_to_n_times(1)
        .expect(1)
        .mount(&server)
        .await;
    Mock::given(path("/games/top"))
        .respond_with(
            ResponseTemplate::new(200)
                .insert_header("Ratelimit-Limit", "800")
                .insert_header("Ratelimit-Remaining", "799")
                .insert_header("Ratelimit-Reset", unix_now().to_string())
                .set_body_json(fixture("games_top.json")),
        )
        .expect(1)
        .mount(&server)
        .await;

    let api = client(&server);
    let page = api.top_categories(PageRequest::default()).await.unwrap();
    assert_eq!(page.items.len(), 2);
    let bucket = api.rate_limiter().bucket().expect("headers observed");
    assert_eq!((bucket.limit, bucket.remaining), (800, 799));
}

#[tokio::test]
async fn rate_limit_gives_up_after_retries() {
    let server = MockServer::start().await;
    // 1 attempt + 2 retries.
    Mock::given(path("/streams"))
        .respond_with(rate_limited())
        .expect(3)
        .mount(&server)
        .await;
    let err = client(&server)
        .top_streams(PageRequest::default())
        .await
        .unwrap_err();
    assert!(matches!(err, Error::RateLimited), "got {err:?}");
}

#[tokio::test]
async fn server_errors_are_retried() {
    let server = MockServer::start().await;
    Mock::given(path("/streams"))
        .respond_with(ResponseTemplate::new(503))
        .up_to_n_times(2)
        .expect(2)
        .mount(&server)
        .await;
    Mock::given(path("/streams"))
        .respond_with(ResponseTemplate::new(200).set_body_json(fixture("streams_page2.json")))
        .expect(1)
        .mount(&server)
        .await;
    let page = client(&server)
        .top_streams(PageRequest::default())
        .await
        .unwrap();
    assert_eq!(page.items[0].user_login, "brickby");
}

#[tokio::test]
async fn server_errors_give_up_after_retries() {
    let server = MockServer::start().await;
    Mock::given(path("/streams"))
        .respond_with(ResponseTemplate::new(500).set_body_string("boom"))
        .expect(3)
        .mount(&server)
        .await;
    let err = client(&server)
        .top_streams(PageRequest::default())
        .await
        .unwrap_err();
    match err {
        Error::Server { status, message } => assert_eq!((status, message.as_str()), (500, "boom")),
        other => panic!("expected Server, got {other:?}"),
    }
}

#[tokio::test]
async fn other_client_errors_are_not_retried() {
    let server = MockServer::start().await;
    Mock::given(path("/streams/followed"))
        .respond_with(ResponseTemplate::new(400).set_body_json(serde_json::json!({
            "error": "Bad Request", "status": 400, "message": "Missing user_id"
        })))
        .expect(1)
        .mount(&server)
        .await;
    let err = client(&server)
        .followed_streams("", PageRequest::default())
        .await
        .unwrap_err();
    assert!(matches!(err, Error::Api { status: 400, .. }), "got {err:?}");
}

#[tokio::test]
async fn malformed_json_is_a_decode_error() {
    let server = MockServer::start().await;
    Mock::given(path("/streams"))
        .respond_with(ResponseTemplate::new(200).set_body_string("{\"data\": 5}"))
        .mount(&server)
        .await;
    let err = client(&server)
        .top_streams(PageRequest::default())
        .await
        .unwrap_err();
    assert!(matches!(err, Error::Decode(_)), "got {err:?}");
}

#[tokio::test]
async fn missing_credentials_send_nothing() {
    let server = MockServer::start().await;
    Mock::given(path("/streams"))
        .respond_with(ResponseTemplate::new(200))
        .expect(0)
        .mount(&server)
        .await;
    let api = HelixClient::with_base_url(server.uri(), None, policy()).unwrap();
    let err = api.top_streams(PageRequest::default()).await.unwrap_err();
    assert!(matches!(err, Error::NotSignedIn));
}
