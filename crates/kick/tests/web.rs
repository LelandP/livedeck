//! `KickWebClient` against recorded Kick answers (`tests/fixtures`, trimmed from real ones on
//! 2026-11-06).

use htpc_kick_api::{Error, KickApi, KickWebClient, PusherApp};
use wiremock::matchers::{body_partial_json, method, path, query_param};
use wiremock::{Mock, MockServer, ResponseTemplate};

fn fixture(name: &str) -> String {
    let path = format!("{}/tests/fixtures/{name}", env!("CARGO_MANIFEST_DIR"));
    std::fs::read_to_string(&path).unwrap_or_else(|e| panic!("{path}: {e}"))
}

fn json(body: String) -> ResponseTemplate {
    ResponseTemplate::new(200).set_body_raw(body, "application/json")
}

async fn client(server: &MockServer) -> KickWebClient {
    KickWebClient::with_base_urls(server.uri(), format!("{}/api", server.uri())).unwrap()
}

#[tokio::test]
async fn top_livestreams_maps_a_real_page_and_its_next_page() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/stream/livestreams/en"))
        .and(query_param("page", "1"))
        .and(query_param("sort", "desc"))
        .respond_with(json(fixture("livestreams_page1.json")))
        .mount(&server)
        .await;
    let page = client(&server).await.top_livestreams(1).await.unwrap();
    assert_eq!(page.streams.len(), 4);
    assert_eq!(page.next_page, Some(2));
    let first = &page.streams[0];
    assert_eq!(first.slug, "sxb");
    assert_eq!(first.name, "SXB");
    assert_eq!(first.channel_id, 17_883_767);
    assert_eq!(first.viewers, 46_034);
    assert_eq!(first.category, "Just Chatting");
    assert!(first.title.contains("SXBThone"));
    assert!(
        first
            .thumbnail
            .as_deref()
            .is_some_and(|u| u.starts_with("https://images.kick.com/") && u.contains("/360.webp")),
        "the 480 px preview, not the 1280 px one"
    );
    // Viewers come first, like Twitch's list.
    assert!(page
        .streams
        .windows(2)
        .all(|w| w[0].viewers >= w[1].viewers));
}

#[tokio::test]
async fn the_last_page_has_no_next_page() {
    let server = MockServer::start().await;
    Mock::given(path("/stream/livestreams/en"))
        .respond_with(json(
            r#"{"current_page":7,"data":[],"next_page_url":null}"#.to_owned(),
        ))
        .mount(&server)
        .await;
    let page = client(&server).await.top_livestreams(7).await.unwrap();
    assert!(page.streams.is_empty());
    assert_eq!(page.next_page, None);
}

#[tokio::test]
async fn a_live_channel_has_its_chatroom_and_stream() {
    let server = MockServer::start().await;
    Mock::given(path("/api/v2/channels/xqc"))
        .respond_with(json(fixture("channel_live.json")))
        .mount(&server)
        .await;
    let channel = client(&server).await.channel("xqc").await.unwrap().unwrap();
    assert_eq!((channel.id, channel.chatroom_id), (668, 668));
    assert_eq!(channel.name, "xQc");
    let live = channel.live.expect("live");
    assert_eq!(live.slug, "xqc");
    assert_eq!(live.viewers, 8_769);
    assert_eq!(live.category, "Grand Theft Auto V (GTA)");
    assert!(live
        .thumbnail
        .is_some_and(|u| u.starts_with("https://stream.kick.com/thumbnails/")));
}

#[tokio::test]
async fn an_unknown_channel_is_none_and_bad_slugs_are_never_asked() {
    let server = MockServer::start().await;
    Mock::given(path("/api/v2/channels/nobody"))
        .respond_with(ResponseTemplate::new(404))
        .expect(1)
        .mount(&server)
        .await;
    let kick = client(&server).await;
    assert_eq!(kick.channel("nobody").await, Ok(None));
    assert_eq!(kick.channel("../x").await, Ok(None));
    assert_eq!(kick.channel("").await, Ok(None));
}

#[tokio::test]
async fn search_finds_channels_and_says_which_are_live() {
    let server = MockServer::start().await;
    Mock::given(path("/api/search"))
        .and(query_param("searched_word", "xqc"))
        .respond_with(json(fixture("search.json")))
        .mount(&server)
        .await;
    let hits = client(&server).await.search_channels("xqc").await.unwrap();
    assert_eq!(hits.len(), 4);
    assert_eq!(
        (
            hits[0].slug.as_str(),
            hits[0].name.as_str(),
            hits[0].is_live
        ),
        ("xqc", "xQc", true)
    );
    assert!(!hits[1].is_live);
}

#[tokio::test]
async fn the_chat_app_comes_from_kicks_realtime_answer() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/api/v1/realtime/channels/668/chat/connection"))
        .and(body_partial_json(serde_json::json!({
            "capabilities": { "accepted_providers": [{ "provider": "pusher" }] }
        })))
        .respond_with(json(
            r#"{"data":{"connections":[{"credentials":{"app_key":"k1","cluster":"eu"},"provider":"pusher"}],"mode":"websocket"},"message":"success"}"#.to_owned(),
        ))
        .mount(&server)
        .await;
    let app = client(&server).await.chat_app(668).await.unwrap();
    assert_eq!(
        app,
        PusherApp {
            key: "k1".into(),
            cluster: "eu".into()
        }
    );
}

#[tokio::test]
async fn an_answer_without_pusher_is_an_error() {
    let server = MockServer::start().await;
    Mock::given(path("/api/v1/realtime/channels/1/chat/connection"))
        .respond_with(json(
            r#"{"data":{"connections":[{"credentials":{"url":"wss://x"},"provider":"centrifugo"}]}}"#.to_owned(),
        ))
        .mount(&server)
        .await;
    assert!(matches!(
        client(&server).await.chat_app(1).await,
        Err(Error::Decode(_))
    ));
}

#[tokio::test]
async fn refusals_and_rate_limits_are_told_apart() {
    let server = MockServer::start().await;
    Mock::given(path("/stream/livestreams/en"))
        .respond_with(ResponseTemplate::new(403))
        .mount(&server)
        .await;
    Mock::given(path("/api/search"))
        .respond_with(ResponseTemplate::new(429))
        .mount(&server)
        .await;
    Mock::given(path("/api/v2/channels/x"))
        .respond_with(ResponseTemplate::new(503))
        .mount(&server)
        .await;
    let kick = client(&server).await;
    assert_eq!(kick.top_livestreams(1).await, Err(Error::Blocked));
    assert_eq!(kick.search_channels("a").await, Err(Error::RateLimited));
    assert_eq!(kick.channel("x").await, Err(Error::Status(503)));
}

#[tokio::test]
async fn a_changed_answer_is_a_decode_error() {
    let server = MockServer::start().await;
    Mock::given(path("/stream/livestreams/en"))
        .respond_with(json(r#"{"data":"not a list"}"#.to_owned()))
        .mount(&server)
        .await;
    assert!(matches!(
        client(&server).await.top_livestreams(1).await,
        Err(Error::Decode(_))
    ));
}
