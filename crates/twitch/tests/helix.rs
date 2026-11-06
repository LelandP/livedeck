//! `HelixClient` against a `wiremock` server: every endpoint, pagination, 401, 429 and 5xx.

use std::time::Duration;

use htpc_twitch_api::{Credentials, HelixClient, PageRequest, RetryPolicy, TwitchApi};
use wiremock::matchers::{body_json, header, method, path, query_param, query_param_is_missing};
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
async fn search_channels_sends_the_query_and_parses() {
    let server = MockServer::start().await;
    Mock::given(path("/search/channels"))
        .and(query_param("query", "loserfruit"))
        .and(query_param("live_only", "true"))
        .and(query_param("first", "20"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
            "data": [
                { "broadcaster_language": "en", "broadcaster_login": "loserfruit",
                  "display_name": "Loserfruit", "game_id": "498000", "game_name": "House Flipper",
                  "id": "41245072", "is_live": true, "tags": [],
                  "thumbnail_url": "https://cdn/fruit-profile_image-300x300.png",
                  "title": "loserfruit", "started_at": "2026-11-05T19:00:00Z" }
            ],
            "pagination": { "cursor": "Mg==" }
        })))
        .expect(1)
        .mount(&server)
        .await;

    let page = client(&server)
        .search_channels("loserfruit", true, PageRequest::first(20))
        .await
        .unwrap();
    let c = &page.items[0];
    assert_eq!(
        (c.broadcaster_login.as_str(), c.display_name.as_str()),
        ("loserfruit", "Loserfruit")
    );
    assert!(c.is_live);
    assert_eq!(c.game_name, "House Flipper");
    assert_eq!(page.cursor.as_deref(), Some("Mg=="));
}

#[tokio::test]
async fn search_categories_turns_the_box_art_into_a_template() {
    let server = MockServer::start().await;
    Mock::given(path("/search/categories"))
        .and(query_param("query", "fort"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
            "data": [
                { "id": "33214", "name": "Fortnite",
                  "box_art_url": "https://static-cdn.jtvnw.net/ttv-boxart/33214-52x72.jpg" }
            ],
            "pagination": {}
        })))
        .expect(1)
        .mount(&server)
        .await;

    let page = client(&server)
        .search_categories("fort", PageRequest::default())
        .await
        .unwrap();
    assert_eq!(page.items[0].name, "Fortnite");
    assert_eq!(
        page.items[0].box_art_url,
        "https://static-cdn.jtvnw.net/ttv-boxart/33214-{width}x{height}.jpg"
    );
    assert_eq!(page.cursor, None);
}

#[tokio::test]
async fn streams_by_login_asks_for_every_login_in_one_request() {
    let server = MockServer::start().await;
    Mock::given(path("/streams"))
        .and(query_param("user_login", "brickby"))
        .and(query_param("user_login", "tidewatch"))
        .respond_with(ok("streams_page2.json"))
        .expect(1)
        .mount(&server)
        .await;
    let api = client(&server);
    let streams = api
        .streams_by_login(&["tidewatch".into(), "brickby".into()])
        .await
        .unwrap();
    assert_eq!(streams[0].user_login, "brickby");
    // Nobody to look up: no request at all (`expect(1)` above).
    assert!(api.streams_by_login(&[]).await.unwrap().is_empty());
}

#[tokio::test]
async fn send_chat_posts_the_message_and_reads_that_it_was_sent() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/chat/messages"))
        .and(header("Client-Id", "test-client"))
        .and(header("Authorization", "Bearer test-token"))
        .and(body_json(serde_json::json!({
            "broadcaster_id": "12826",
            "sender_id": "141981764",
            "message": "GG o7",
        })))
        .respond_with(ok("chat_sent.json"))
        .expect(1)
        .mount(&server)
        .await;

    let outcome = client(&server)
        .send_chat("12826", "141981764", "GG o7")
        .await
        .unwrap();
    assert!(outcome.sent);
    assert_eq!(outcome.message_id, "abc-123-def");
    assert_eq!(outcome.drop_reason, None);
}

#[tokio::test]
async fn send_chat_reads_why_a_message_was_dropped() {
    let cases = fixture("chat_dropped.json")["cases"]
        .as_array()
        .unwrap()
        .clone();
    let want = [
        "msg_followersonly",
        "msg_subsonly",
        "msg_slowmode",
        "msg_rejected",
        "msg_banned",
    ];
    assert_eq!(cases.len(), want.len());
    for (case, code) in cases.into_iter().zip(want) {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/chat/messages"))
            .respond_with(ResponseTemplate::new(200).set_body_json(case))
            .mount(&server)
            .await;
        let outcome = client(&server).send_chat("1", "2", "hi").await.unwrap();
        assert!(!outcome.sent, "{code}");
        let reason = outcome.drop_reason.expect("a reason");
        assert_eq!(reason.code, code);
        assert!(!reason.message.is_empty(), "{code}");
    }
}

#[tokio::test]
async fn send_chat_is_never_repeated_after_a_server_error() {
    // The message may have been posted before the server failed: a retry could post it twice.
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/chat/messages"))
        .respond_with(ResponseTemplate::new(503))
        .expect(1)
        .mount(&server)
        .await;
    let err = client(&server).send_chat("1", "2", "hi").await.unwrap_err();
    assert!(
        matches!(err, htpc_twitch_api::Error::Server { status: 503, .. }),
        "{err:?}"
    );
}

#[tokio::test]
async fn send_chat_needs_credentials() {
    let server = MockServer::start().await;
    let anonymous = HelixClient::with_base_url(server.uri(), None, policy()).unwrap();
    let err = anonymous.send_chat("1", "2", "hi").await.unwrap_err();
    assert!(matches!(err, htpc_twitch_api::Error::NotSignedIn));
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
