//! `GqlClient` against a `wiremock` server. Fixtures are trimmed copies of live responses.

use std::time::Duration;

use htpc_twitch_api::{Error, GqlClient, PageRequest, RetryPolicy, TwitchApi};
use serde_json::json;
use wiremock::matchers::{body_partial_json, body_string_contains, header, method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

fn fixture(name: &str) -> serde_json::Value {
    let path = format!("{}/tests/fixtures/{name}", env!("CARGO_MANIFEST_DIR"));
    serde_json::from_str(&std::fs::read_to_string(path).unwrap()).unwrap()
}

fn ok(name: &str) -> ResponseTemplate {
    ResponseTemplate::new(200).set_body_json(fixture(name))
}

fn client(server: &MockServer) -> GqlClient {
    let policy = RetryPolicy {
        max_retries: 2,
        base_backoff: Duration::from_millis(5),
        max_wait: Duration::from_millis(50),
    };
    GqlClient::with_endpoint(format!("{}/gql", server.uri()), "web-id", policy).unwrap()
}

#[tokio::test]
async fn top_streams_maps_nodes_and_cursor() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/gql"))
        .and(header("Client-Id", "web-id"))
        .and(body_string_contains("streams(first: $first)"))
        .and(body_partial_json(json!({ "variables": { "first": 20 } })))
        .respond_with(ok("gql_streams.json"))
        .expect(1)
        .mount(&server)
        .await;

    let page = client(&server)
        .top_streams(PageRequest::first(20))
        .await
        .unwrap();
    // The stream with a null broadcaster is skipped.
    assert_eq!(page.items.len(), 2);
    let s = &page.items[0];
    assert_eq!(
        (s.user_login.as_str(), s.user_name.as_str()),
        ("grandmaster_k", "Grandmaster_K")
    );
    assert_eq!(
        (s.game_id.as_str(), s.game_name.as_str()),
        ("516575", "VALORANT")
    );
    assert_eq!(s.viewer_count, 48_100);
    assert!(s.thumbnail_url.ends_with("-440x248.jpg"));
    // Nulls become empty strings, and a missing display name falls back to the login.
    let t = &page.items[1];
    assert_eq!((t.title.as_str(), t.game_name.as_str()), ("", ""));
    assert_eq!(t.user_name, "tidewatch");
    // hasNextPage: the last edge's cursor.
    assert_eq!(
        page.cursor.as_deref(),
        Some("eyJzIjoxMDAwMC4wLCJkIjpmYWxzZSwidCI6dHJ1ZX0=")
    );
}

#[tokio::test]
async fn page_size_is_capped_at_30() {
    let server = MockServer::start().await;
    Mock::given(body_partial_json(json!({ "variables": { "first": 30 } })))
        .respond_with(ok("gql_games.json"))
        .expect(1)
        .mount(&server)
        .await;
    let page = client(&server)
        .top_categories(PageRequest::first(100))
        .await
        .unwrap();
    let names: Vec<_> = page.items.iter().map(|g| g.name.as_str()).collect();
    assert_eq!(names, ["Just Chatting", "Minecraft"]);
    assert!(page.items[1].box_art_url.contains("27471"));
    assert_eq!(page.cursor, None, "hasNextPage false is the last page");
}

#[tokio::test]
async fn later_pages_need_integrity() {
    let server = MockServer::start().await;
    Mock::given(body_partial_json(
        json!({ "variables": { "after": "abc" } }),
    ))
    .and(body_string_contains("after: $after"))
    .respond_with(ok("gql_integrity.json"))
    .expect(1)
    .mount(&server)
    .await;
    let err = client(&server)
        .top_streams(PageRequest::after(20, "abc"))
        .await
        .unwrap_err();
    assert!(matches!(err, Error::IntegrityRequired), "got {err:?}");
}

#[tokio::test]
async fn streams_by_category_and_unknown_game() {
    let server = MockServer::start().await;
    let streams = fixture("gql_streams.json")["data"]["streams"].clone();
    Mock::given(body_partial_json(
        json!({ "variables": { "id": "516575" } }),
    ))
    .and(body_string_contains("game(id: $id)"))
    .respond_with(
        ResponseTemplate::new(200)
            .set_body_json(json!({ "data": { "game": { "streams": streams } } })),
    )
    .mount(&server)
    .await;
    Mock::given(body_partial_json(json!({ "variables": { "id": "0" } })))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({ "data": { "game": null } })))
        .mount(&server)
        .await;

    let api = client(&server);
    let page = api
        .streams_by_category("516575", PageRequest::default())
        .await
        .unwrap();
    assert_eq!(page.items[0].user_login, "grandmaster_k");
    let none = api
        .streams_by_category("0", PageRequest::default())
        .await
        .unwrap();
    assert!(none.items.is_empty() && none.cursor.is_none());
}

#[tokio::test]
async fn user_lookup_and_anonymous_limits() {
    let server = MockServer::start().await;
    Mock::given(body_partial_json(
        json!({ "variables": { "login": "twitch" } }),
    ))
    .respond_with(
        ResponseTemplate::new(200).set_body_json(json!({ "data": { "user": {
            "id": "12826", "login": "twitch", "displayName": "Twitch",
            "profileImageURL": "https://static-cdn.jtvnw.net/x-300x300.png"
        } } })),
    )
    .mount(&server)
    .await;
    Mock::given(body_partial_json(
        json!({ "variables": { "login": "nobody" } }),
    ))
    .respond_with(ResponseTemplate::new(200).set_body_json(json!({ "data": { "user": null } })))
    .mount(&server)
    .await;

    let api = client(&server);
    let u = api.user(Some("twitch")).await.unwrap().unwrap();
    assert_eq!(
        (u.id.as_str(), u.display_name.as_str()),
        ("12826", "Twitch")
    );
    assert_eq!(api.user(Some("nobody")).await.unwrap(), None);
    // No signed-in user: no "me" and no follows, and no request is sent for either.
    assert!(matches!(api.user(None).await, Err(Error::NotSignedIn)));
    let followed = api.followed_streams("1", PageRequest::default()).await;
    assert!(matches!(followed, Err(Error::NotSignedIn)));
}

#[tokio::test]
async fn search_needs_a_sign_in_and_sends_nothing() {
    let server = MockServer::start().await;
    let api = client(&server);
    let channels = api
        .search_channels("x", false, PageRequest::default())
        .await;
    assert!(matches!(channels, Err(Error::NotSignedIn)));
    let games = api.search_categories("x", PageRequest::default()).await;
    assert!(matches!(games, Err(Error::NotSignedIn)));
    let live = api.streams_by_login(&["x".into()]).await;
    assert!(matches!(live, Err(Error::NotSignedIn)));
    assert!(server.received_requests().await.unwrap().is_empty());
}

/// The recorded polls of one live train (plan 09's spike): a level up between two polls, then
/// the train gone.
#[tokio::test]
async fn hype_train_from_recorded_polls() {
    let server = MockServer::start().await;
    let api = client(&server);
    let mut got = Vec::new();
    for name in [
        "gql_hype_train_level1.json",
        "gql_hype_train_level2.json",
        "gql_hype_train_ended.json",
        "gql_hype_train_none.json",
    ] {
        server.reset().await;
        Mock::given(body_partial_json(
            json!({ "variables": { "login": "zackrawrr" } }),
        ))
        .and(body_string_contains("hypeTrain"))
        .respond_with(ok(&format!("spikes/{name}")))
        .expect(1)
        .mount(&server)
        .await;
        got.push(api.hype_train("zackrawrr").await.unwrap());
    }
    let one = got[0].as_ref().expect("a train at level 1");
    assert_eq!(
        (one.level, one.progress, one.goal, one.seconds_left),
        (1, 1501, 1600, 211)
    );
    assert_eq!(one.all_time_high, 62);
    let two = got[1].as_ref().expect("a train at level 2");
    assert_eq!(
        (two.level, two.progress, two.goal, two.seconds_left),
        (2, 401, 1800, 297)
    );
    assert_eq!(one.id, two.id, "the same train");
    assert_eq!(got[2], None, "ended");
    assert_eq!(got[3], None, "never ran");
}

#[tokio::test]
async fn an_ended_train_still_listed_counts_as_none() {
    let server = MockServer::start().await;
    Mock::given(path("/gql"))
        .respond_with(ResponseTemplate::new(200).set_body_json(
            json!({ "data": { "user": { "channel": {
                "hypeTrain": { "execution": {
                    "id": "t", "isActive": false, "endedAt": "2026-11-06T20:45:48Z",
                    "progress": { "level": { "value": 3 }, "goal": 2000, "progression": 10,
                        "remainingSeconds": 0 },
                    "allTimeHigh": null
                } }
            } } } }),
        ))
        .mount(&server)
        .await;
    assert_eq!(client(&server).hype_train("x").await.unwrap(), None);
}

#[tokio::test]
async fn hype_train_of_an_unknown_channel_is_none() {
    let server = MockServer::start().await;
    Mock::given(path("/gql"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({ "data": { "user": null } })))
        .mount(&server)
        .await;
    assert_eq!(client(&server).hype_train("nobody").await.unwrap(), None);
}

/// Hits the real gql.twitch.tv. Run by hand to check the hype train query still validates
/// (the answer is usually `None`: few channels have a train at any moment).
#[tokio::test]
#[ignore = "network: real Twitch GQL"]
async fn live_hype_train_query() {
    let api = GqlClient::new().unwrap();
    api.hype_train("xqc").await.unwrap();
}

/// Hits the real gql.twitch.tv. Run by hand to detect schema / client-ID changes:
/// `cargo test -p htpc-twitch-api --test gql -- --ignored live_`
#[tokio::test]
#[ignore = "network: real Twitch GQL"]
async fn live_anonymous_browse() {
    let api = GqlClient::new().unwrap();
    let streams = api.top_streams(PageRequest::first(5)).await.unwrap();
    assert!(!streams.items.is_empty(), "no live streams?");
    assert!(streams.items.iter().all(|s| !s.user_login.is_empty()));
    let games = api.top_categories(PageRequest::first(5)).await.unwrap();
    assert!(!games.items.is_empty());
    let in_game = api
        .streams_by_category(&games.items[0].id, PageRequest::first(3))
        .await
        .unwrap();
    assert!(!in_game.items.is_empty());
    assert_eq!(api.user(Some("twitch")).await.unwrap().unwrap().id, "12826");
}

#[tokio::test]
async fn graphql_errors_without_data_fail() {
    let server = MockServer::start().await;
    // Captured: the shape returned when a field no longer exists (schema change).
    Mock::given(path("/gql"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({ "errors": [
            { "message": "Cannot query field \"bogusField\" on type \"Query\".", "path": ["query"] }
        ] })))
        .mount(&server)
        .await;
    let err = client(&server)
        .top_categories(PageRequest::default())
        .await
        .unwrap_err();
    match err {
        Error::Graphql(msg) => assert!(msg.contains("bogusField")),
        other => panic!("expected Graphql, got {other:?}"),
    }
}

#[tokio::test]
async fn rejected_client_id_is_not_retried() {
    let server = MockServer::start().await;
    Mock::given(path("/gql"))
        .respond_with(ResponseTemplate::new(400).set_body_json(json!({
            "error": "Bad Request", "status": 400,
            "message": "The \"Client-ID\" header is missing from the request."
        })))
        .expect(1)
        .mount(&server)
        .await;
    let err = client(&server)
        .top_streams(PageRequest::default())
        .await
        .unwrap_err();
    assert!(matches!(err, Error::Api { status: 400, .. }), "got {err:?}");
}

#[tokio::test]
async fn server_errors_are_retried() {
    let server = MockServer::start().await;
    Mock::given(path("/gql"))
        .respond_with(ResponseTemplate::new(502))
        .up_to_n_times(2)
        .expect(2)
        .mount(&server)
        .await;
    Mock::given(path("/gql"))
        .respond_with(ok("gql_games.json"))
        .expect(1)
        .mount(&server)
        .await;
    let page = client(&server)
        .top_categories(PageRequest::default())
        .await
        .unwrap();
    assert_eq!(page.items.len(), 2);
}
