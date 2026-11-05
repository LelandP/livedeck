//! `HelixClient` against a `wiremock` server: every endpoint, pagination, 401, 429 and 5xx.

use std::time::Duration;

use htpc_twitch_api::{Credentials, HelixClient, PageRequest, RetryPolicy, TwitchApi};
use wiremock::matchers::{header, method, path, query_param, query_param_is_missing};
use wiremock::{Mock, MockServer, ResponseTemplate};

fn fixture(name: &str) -> serde_json::Value {
    let path = format!("{}/tests/fixtures/{name}", env!("CARGO_MANIFEST_DIR"));
    let text = std::fs::read_to_string(&path).unwrap_or_else(|e| panic!("{path}: {e}"));
    serde_json::from_str(&text).unwrap()
}

fn ok(name: &str) -> ResponseTemplate {
    ResponseTemplate::new(200).set_body_json(fixture(name))
}

/// Fast retries so the tests do not sleep for long.
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

#[tokio::test]
async fn top_streams_sends_auth_and_parses() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/streams"))
        .and(header("Client-Id", "test-client"))
        .and(header("Authorization", "Bearer test-token"))
        .and(query_param("first", "2"))
        .and(query_param("type", "live"))
        .and(query_param_is_missing("after"))
        .respond_with(ok("streams_page1.json"))
        .expect(1)
        .mount(&server)
        .await;

    let page = client(&server)
        .top_streams(PageRequest::first(2))
        .await
        .unwrap();
    assert_eq!(page.items.len(), 2);
    let s = &page.items[0];
    assert_eq!(s.user_login, "grandmaster_k");
    assert_eq!(s.game_name, "VALORANT");
    assert_eq!(s.viewer_count, 48_100);
    assert!(s.thumbnail_url.contains("{width}x{height}"));
    assert_eq!(
        page.cursor.as_deref(),
        Some("eyJiIjpudWxsLCJhIjp7Ik9mZnNldCI6Mn19")
    );
}

#[tokio::test]
async fn pagination_follows_the_cursor_to_the_end() {
    let server = MockServer::start().await;
    let cursor = "eyJiIjpudWxsLCJhIjp7Ik9mZnNldCI6Mn19";
    Mock::given(path("/streams"))
        .and(query_param("after", cursor))
        .respond_with(ok("streams_page2.json"))
        .expect(1)
        .mount(&server)
        .await;
    Mock::given(path("/streams"))
        .and(query_param_is_missing("after"))
        .respond_with(ok("streams_page1.json"))
        .expect(1)
        .mount(&server)
        .await;

    let api = client(&server);
    let mut logins = Vec::new();
    let mut req = PageRequest::first(2);
    loop {
        let page = api.top_streams(req.clone()).await.unwrap();
        logins.extend(page.items.into_iter().map(|s| s.user_login));
        match page.cursor {
            Some(c) => req = PageRequest::after(2, c),
            None => break,
        }
    }
    assert_eq!(logins, ["grandmaster_k", "tidewatch", "brickby"]);
}

#[tokio::test]
async fn followed_streams_passes_user_id() {
    let server = MockServer::start().await;
    Mock::given(path("/streams/followed"))
        .and(query_param("user_id", "141981764"))
        .respond_with(ok("streams_page2.json"))
        .expect(1)
        .mount(&server)
        .await;
    let page = client(&server)
        .followed_streams("141981764", PageRequest::default())
        .await
        .unwrap();
    assert_eq!(page.items[0].user_login, "brickby");
    assert_eq!(
        page.cursor, None,
        "empty pagination object is the last page"
    );
}

#[tokio::test]
async fn top_categories_parses_games() {
    let server = MockServer::start().await;
    Mock::given(path("/games/top"))
        .and(query_param("first", "100"))
        .respond_with(ok("games_top.json"))
        .expect(1)
        .mount(&server)
        .await;
    // Page sizes above the Helix maximum are clamped.
    let page = client(&server)
        .top_categories(PageRequest::first(250))
        .await
        .unwrap();
    let names: Vec<_> = page.items.iter().map(|g| g.name.as_str()).collect();
    assert_eq!(names, ["Just Chatting", "VALORANT"]);
    assert_eq!(page.items[1].id, "516575");
    assert!(page.cursor.is_some());
}

#[tokio::test]
async fn streams_by_category_filters_on_game_id() {
    let server = MockServer::start().await;
    Mock::given(path("/streams"))
        .and(query_param("game_id", "27471"))
        .respond_with(ok("streams_page2.json"))
        .expect(1)
        .mount(&server)
        .await;
    let page = client(&server)
        .streams_by_category("27471", PageRequest::default())
        .await
        .unwrap();
    assert_eq!(page.items[0].game_name, "Minecraft");
}

#[tokio::test]
async fn user_by_login_and_token_owner() {
    let server = MockServer::start().await;
    Mock::given(path("/users"))
        .and(query_param("login", "nightowl_dev"))
        .respond_with(ok("users.json"))
        .expect(1)
        .mount(&server)
        .await;
    Mock::given(path("/users"))
        .and(query_param("login", "nobody"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({ "data": [] })))
        .expect(1)
        .mount(&server)
        .await;
    Mock::given(path("/users"))
        .and(query_param_is_missing("login"))
        .respond_with(ok("users.json"))
        .expect(1)
        .mount(&server)
        .await;

    let api = client(&server);
    let user = api.user(Some("nightowl_dev")).await.unwrap().unwrap();
    assert_eq!(
        (user.id.as_str(), user.login.as_str()),
        ("141981764", "nightowl_dev")
    );
    assert_eq!(api.user(Some("nobody")).await.unwrap(), None);
    assert_eq!(api.user(None).await.unwrap().unwrap().login, "nightowl_dev");
}

#[tokio::test]
async fn followed_channels_passes_user_id_and_pages() {
    let server = MockServer::start().await;
    Mock::given(path("/channels/followed"))
        .and(query_param("user_id", "141981764"))
        .and(query_param("first", "2"))
        .and(query_param_is_missing("after"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
            "total": 3,
            "data": [
                { "broadcaster_id": "1", "broadcaster_login": "a", "broadcaster_name": "A",
                  "followed_at": "2022-05-24T22:22:08Z" },
                { "broadcaster_id": "2", "broadcaster_login": "b", "broadcaster_name": "B",
                  "followed_at": "2022-05-24T22:22:08Z" }
            ],
            "pagination": { "cursor": "next" }
        })))
        .expect(1)
        .mount(&server)
        .await;

    let page = client(&server)
        .followed_channels("141981764", PageRequest::first(2))
        .await
        .unwrap();
    let logins: Vec<_> = page
        .items
        .iter()
        .map(|c| c.broadcaster_login.as_str())
        .collect();
    assert_eq!(logins, ["a", "b"]);
    assert_eq!(page.items[1].broadcaster_name, "B");
    assert_eq!(page.cursor.as_deref(), Some("next"));
}
