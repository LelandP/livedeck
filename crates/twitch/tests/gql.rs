//! `GqlClient` against a `wiremock` server. Fixtures are trimmed copies of live responses.

use std::time::Duration;

use htpc_twitch_api::{
    Error, GqlClient, MutedSegment, PageRequest, RetryPolicy, TwitchApi, VideoChapter, VideoType,
};
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

/// Recorded 2026-11-11 from chatterbox (offline), trimmed to two items per row.
#[tokio::test]
async fn channel_videos_maps_every_row() {
    let server = MockServer::start().await;
    Mock::given(body_partial_json(
        json!({ "variables": { "login": "chatterbox" } }),
    ))
    .and(body_string_contains(
        "archives: videos(first: 30, type: ARCHIVE",
    ))
    .and(body_string_contains("clipsAll: clips(first: 30"))
    .respond_with(ok("gql_channel_videos.json"))
    .expect(1)
    .mount(&server)
    .await;

    let page = client(&server)
        .channel_videos("chatterbox")
        .await
        .unwrap()
        .expect("a known channel");
    assert_eq!(
        (page.user.login.as_str(), page.user.display_name.as_str()),
        ("chatterbox", "Chatterbox")
    );
    assert_eq!(page.user.id, "56990029");
    assert!(page.user.profile_image_url.contains("150x150"));
    assert_eq!(page.live, None, "offline");
    assert!(page
        .last_broadcast_at
        .as_deref()
        .unwrap()
        .starts_with("2026-11-11T"));

    assert_eq!(page.archives.len(), 2);
    let v = &page.archives[0];
    assert_eq!(v.id, "2925092481");
    assert_eq!(v.kind, VideoType::Archive);
    assert_eq!(v.length_seconds, 67_565);
    assert_eq!(v.created_at, "2026-11-10T18:28:22Z");
    assert_eq!(v.game_name, "Grand Theft Auto V");
    assert!(v.thumbnail_url.ends_with("-440x248.jpg"));
    assert!(!v.recording && !v.sub_only);
    assert_eq!(
        (v.owner_login.as_str(), v.owner_name.as_str()),
        ("chatterbox", "Chatterbox")
    );
    assert!(
        v.muted.is_empty(),
        "a null mutedSegmentConnection is no muted parts"
    );
    assert_eq!(
        page.archives[1].muted,
        [MutedSegment {
            offset: 5400,
            duration: 200
        }]
    );

    assert_eq!(page.highlights.len(), 2);
    assert!(page
        .highlights
        .iter()
        .all(|h| h.kind == VideoType::Highlight));
    assert_eq!(page.clips_week.len(), 2);
    assert_eq!(page.clips_all_time.len(), 2);
    let c = &page.clips_all_time[0];
    assert!(!c.slug.is_empty() && !c.title.is_empty());
    assert!(c.duration_seconds > 0 && c.view_count > 0);
    assert!(!c.curator_name.is_empty());
    assert!(c.thumbnail_url.starts_with("https://"));
}

/// Recorded 2026-11-11 from xqc while live: the stream is on the page, its archive recording.
#[tokio::test]
async fn channel_videos_of_a_live_channel() {
    let server = MockServer::start().await;
    Mock::given(path("/gql"))
        .respond_with(ok("gql_channel_videos_live.json"))
        .mount(&server)
        .await;
    let page = client(&server)
        .channel_videos("xqc")
        .await
        .unwrap()
        .unwrap();
    let live = page.live.expect("live");
    assert_eq!(live.user_login, "xqc");
    assert_eq!(live.user_id, page.user.id);
    assert_eq!(live.game_name, "Grand Theft Auto V");
    assert!(live.viewer_count > 0);
    assert!(live.thumbnail_url.ends_with("-440x248.jpg"));
    assert!(
        page.archives[0].recording,
        "the live broadcast is still recording"
    );
    assert!(page.highlights.is_empty());
}

#[tokio::test]
async fn channel_videos_of_an_unknown_channel_is_none() {
    let server = MockServer::start().await;
    Mock::given(path("/gql"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({ "data": { "user": null } })))
        .mount(&server)
        .await;
    assert_eq!(
        client(&server).channel_videos("nobody").await.unwrap(),
        None
    );
}

/// Nulls Twitch sends for hidden or missing parts: a sub-only video, a row that is `null`, a
/// node without an owner, a stream without a game.
#[tokio::test]
async fn channel_videos_tolerates_nulls_and_marks_sub_only() {
    let server = MockServer::start().await;
    Mock::given(path("/gql"))
        .respond_with(
            ResponseTemplate::new(200).set_body_json(json!({ "data": { "user": {
            "id": "7", "login": "locked", "displayName": null, "profileImageURL": null,
            "lastBroadcast": { "startedAt": null },
            "stream": { "createdAt": "2026-11-11T10:00:00Z", "title": null, "viewersCount": 3,
                "game": null, "previewImageURL": null },
            "archives": { "edges": [
                { "node": { "id": "1", "title": "subs only", "lengthSeconds": 60,
                    "createdAt": "2026-11-11T10:00:00Z", "viewCount": 1, "status": "RECORDED",
                    "previewThumbnailURL": null, "game": null,
                    "resourceRestriction": { "type": "SUB" }, "muteInfo": null, "owner": null } },
                { "node": null }
            ] },
            "highlights": null, "clipsWeek": { "edges": [] }, "clipsAll": null
        } } })),
        )
        .mount(&server)
        .await;
    let page = client(&server)
        .channel_videos("locked")
        .await
        .unwrap()
        .unwrap();
    assert_eq!(page.user.display_name, "locked", "falls back to the login");
    assert_eq!(page.last_broadcast_at, None);
    let live = page.live.unwrap();
    assert_eq!((live.title.as_str(), live.game_name.as_str()), ("", ""));
    assert_eq!(page.archives.len(), 1, "a null node is skipped");
    let v = &page.archives[0];
    assert!(v.sub_only);
    assert_eq!(v.thumbnail_url, "");
    assert_eq!(v.owner_login, "");
    assert!(page.highlights.is_empty() && page.clips_all_time.is_empty());
}

/// Recorded 2026-11-11: four chapters and one muted part.
#[tokio::test]
async fn video_details_maps_chapters_and_muted_parts() {
    let server = MockServer::start().await;
    Mock::given(body_partial_json(
        json!({ "variables": { "id": "2923341186" } }),
    ))
    .and(body_string_contains("VIDEO_CHAPTER_MARKERS"))
    .respond_with(ok("gql_video_details.json"))
    .expect(1)
    .mount(&server)
    .await;
    let d = client(&server)
        .video_details("2923341186")
        .await
        .unwrap()
        .unwrap();
    assert_eq!(d.id, "2923341186");
    assert_eq!(d.length_seconds, 68_034);
    let chapters: Vec<_> = d
        .chapters
        .iter()
        .map(|c| (c.position_ms, c.name.as_str()))
        .collect();
    assert_eq!(
        chapters,
        [
            (0, "Grand Theft Auto V"),
            (638_000, "Scam Line"),
            (7_590_000, "Counter-Strike"),
            (67_111_000, "Just Chatting")
        ]
    );
    assert_eq!(d.chapters[1].duration_ms, 6_952_000);
    assert_eq!(
        d.muted,
        [MutedSegment {
            offset: 5400,
            duration: 200
        }]
    );
    assert!(d.seek_previews_url.ends_with("-info.json"));
}

#[tokio::test]
async fn video_details_without_markers_is_one_chapter() {
    let server = MockServer::start().await;
    Mock::given(path("/gql"))
        .respond_with(
            ResponseTemplate::new(200).set_body_json(json!({ "data": { "video": {
            "id": "5", "lengthSeconds": 120, "seekPreviewsURL": null,
            "game": { "name": "Chess" }, "muteInfo": null, "moments": { "edges": [] }
        } } })),
        )
        .mount(&server)
        .await;
    let d = client(&server).video_details("5").await.unwrap().unwrap();
    assert_eq!(
        d.chapters,
        [VideoChapter {
            position_ms: 0,
            duration_ms: 120_000,
            name: "Chess".into()
        }]
    );
    assert!(d.muted.is_empty());
    assert_eq!(d.seek_previews_url, "");

    server.reset().await;
    Mock::given(path("/gql"))
        .respond_with(
            ResponseTemplate::new(200).set_body_json(json!({ "data": { "video": null } })),
        )
        .mount(&server)
        .await;
    assert_eq!(client(&server).video_details("0").await.unwrap(), None);
}

/// Recorded 2026-11-11 from xqc while live.
#[tokio::test]
async fn live_archive_while_recording() {
    let server = MockServer::start().await;
    Mock::given(body_partial_json(
        json!({ "variables": { "login": "xqc" } }),
    ))
    .respond_with(ok("gql_live_archive.json"))
    .mount(&server)
    .await;
    let a = client(&server).live_archive("xqc").await.unwrap().unwrap();
    assert_eq!(a.video_id, "2926188923");
    assert_eq!(a.stream_started_at, "2026-11-11T21:55:14Z");
}

/// Offline, or live but the newest archive is an old one (the channel keeps no past
/// broadcasts): no archive of this stream.
#[tokio::test]
async fn live_archive_needs_a_live_recording() {
    let server = MockServer::start().await;
    let api = client(&server);
    let answer = |stream: serde_json::Value, status: &str| {
        json!({ "data": { "user": {
            "stream": stream,
            "videos": { "edges": [ { "node": { "id": "9", "status": status } } ] }
        } } })
    };
    for (body, why) in [
        (answer(json!(null), "RECORDING"), "offline"),
        (
            answer(json!({ "createdAt": "2026-11-11T10:00:00Z" }), "RECORDED"),
            "live, no recording",
        ),
        (json!({ "data": { "user": null } }), "no such channel"),
    ] {
        server.reset().await;
        Mock::given(path("/gql"))
            .respond_with(ResponseTemplate::new(200).set_body_json(body))
            .mount(&server)
            .await;
        assert_eq!(api.live_archive("x").await.unwrap(), None, "{why}");
    }
}

/// Recorded 2026-11-12 from a past broadcast of summit1g at offset 3600 (59 lines starting at
/// 3555), trimmed to six, with the chatters renamed.
#[tokio::test]
async fn chat_replay_maps_lines_and_emotes() {
    let server = MockServer::start().await;
    Mock::given(body_partial_json(
        json!({ "variables": { "id": "2926143952", "offset": 3600 } }),
    ))
    .and(body_string_contains("contentOffsetSeconds: $offset"))
    .respond_with(ok("gql_chat_replay.json"))
    .expect(1)
    .mount(&server)
    .await;
    let lines = client(&server)
        .chat_replay("2926143952", 3600)
        .await
        .unwrap()
        .expect("a replay");
    assert_eq!(lines.len(), 6);
    // The window starts before the offset, and two lines share a second.
    let offsets: Vec<u32> = lines.iter().map(|l| l.offset_seconds).collect();
    assert_eq!(offsets, [3555, 3555, 3578, 3582, 3623, 3696]);

    let first = &lines[0];
    assert_eq!(first.id, "e7218915-b79e-43fb-ad0e-fac9288c4ccf");
    assert_eq!(
        (first.login.as_str(), first.name.as_str()),
        ("chatter_one", "Chatter_one")
    );
    assert_eq!(first.color, "#008000");
    assert_eq!(first.fragments.len(), 1);
    assert_eq!(first.fragments[0].text, "Kreygasm");
    assert_eq!(first.fragments[0].emote_id.as_deref(), Some("41"));

    // Text and an emote in one line.
    let mixed = &lines[2].fragments;
    assert_eq!(mixed[0].text, "oh nice ");
    assert_eq!(mixed[0].emote_id, None);
    assert_eq!(
        mixed[1].emote_id.as_deref(),
        Some("emotesv2_bd95ea08beb4412db4e5cc0f605b4a72")
    );
    // A chatter who never picked a colour.
    assert_eq!(lines[3].color, "");
    // Emotes and text alternate.
    let pieces: Vec<bool> = lines[5]
        .fragments
        .iter()
        .map(|f| f.emote_id.is_some())
        .collect();
    assert_eq!(
        pieces,
        [true, false, true, false, true, false, true, false, true]
    );
}

/// An offset past the end, or a video without chat, answers `comments: null` next to a "service
/// error" (checked live 2026-11-12): no replay, not a failure. No such video is the same.
#[tokio::test]
async fn chat_replay_without_chat_is_none() {
    let server = MockServer::start().await;
    let api = client(&server);
    for (body, why) in [
        (
            json!({ "data": { "video": { "comments": null } },
                    "errors": [{ "message": "service error", "path": ["video", "comments"] }] }),
            "past the end",
        ),
        (json!({ "data": { "video": null } }), "no such video"),
        (
            json!({ "data": { "video": { "comments": { "edges": [] } } } }),
            "no lines",
        ),
    ] {
        server.reset().await;
        Mock::given(path("/gql"))
            .respond_with(ResponseTemplate::new(200).set_body_json(body))
            .mount(&server)
            .await;
        let got = api.chat_replay("1", 99_999).await.unwrap();
        assert!(got.is_none_or(|lines| lines.is_empty()), "{why}");
    }
}

/// The cursor and the page size are not asked for, and the integrity check is what Twitch
/// answers if it starts refusing viewers: a failure, so the player can say there is no replay.
#[tokio::test]
async fn chat_replay_refused_by_the_integrity_check_fails() {
    let server = MockServer::start().await;
    Mock::given(path("/gql"))
        .respond_with(ok("gql_integrity.json"))
        .expect(1)
        .mount(&server)
        .await;
    let err = client(&server).chat_replay("1", 0).await.unwrap_err();
    assert!(matches!(err, Error::IntegrityRequired), "got {err:?}");
}

#[tokio::test]
async fn chat_replay_skips_empty_lines_and_tolerates_nulls() {
    let server = MockServer::start().await;
    let node = |id: &str, at: u32, commenter: serde_json::Value, message: serde_json::Value| {
        json!({ "node": { "id": id, "contentOffsetSeconds": at,
                          "commenter": commenter, "message": message } })
    };
    let said =
        |text: &str| json!({ "userColor": null, "fragments": [{ "text": text, "emote": null }] });
    let body = json!({ "data": { "video": { "comments": { "edges": [
        node("late", 30, json!(null), said("from a deleted account")),
        node("early", 10, json!({ "login": "a", "displayName": null }), said("hi")),
        node("blank", 11, json!({ "login": "b" }), json!({ "fragments": [] })),
        node("nomessage", 12, json!({ "login": "c" }), json!(null)),
        node("", 13, json!({ "login": "d" }), said("no id")),
        json!({ "node": null }),
    ] } } } });
    Mock::given(path("/gql"))
        .respond_with(ResponseTemplate::new(200).set_body_json(body))
        .mount(&server)
        .await;
    let lines = client(&server).chat_replay("1", 0).await.unwrap().unwrap();
    // Sorted by time, and only the lines that say something.
    let ids: Vec<&str> = lines.iter().map(|l| l.id.as_str()).collect();
    assert_eq!(ids, ["early", "late"]);
    assert_eq!(
        lines[0].name, "a",
        "no display name falls back to the login"
    );
    assert_eq!((lines[1].login.as_str(), lines[1].name.as_str()), ("", ""));
}

/// Hits the real gql.twitch.tv. Run by hand to check the chat replay query still validates and
/// that it still answers signed out: `cargo test -p htpc-twitch-api --test gql -- --ignored live_`
#[tokio::test]
#[ignore = "network: real Twitch GQL"]
async fn live_chat_replay() {
    let api = GqlClient::new().unwrap();
    let page = api
        .channel_videos("summit1g")
        .await
        .unwrap()
        .expect("summit1g exists");
    // A finished broadcast with a long chat, a few minutes in.
    let video = page
        .archives
        .iter()
        .find(|v| !v.recording && v.length_seconds > 1_200 && !v.sub_only)
        .expect("a past broadcast");
    let lines = api
        .chat_replay(&video.id, 600)
        .await
        .unwrap()
        .expect("a replay");
    assert!(lines.len() > 10, "{} lines", lines.len());
    assert!(lines
        .windows(2)
        .all(|w| w[0].offset_seconds <= w[1].offset_seconds));
    assert!(lines
        .iter()
        .all(|l| !l.id.is_empty() && !l.fragments.is_empty()));
    // The window starts a little before the offset and carries on past it.
    assert!(lines[0].offset_seconds <= 600);
    assert!(lines.last().unwrap().offset_seconds >= 600);
    // Nothing past the end of the video.
    let past = api
        .chat_replay(&video.id, video.length_seconds + 5_000)
        .await;
    assert!(past.is_err() || past.unwrap().is_none_or(|l| l.is_empty()));
    assert_eq!(
        api.chat_replay("1", 0).await.unwrap(),
        None,
        "no such video"
    );
}

/// Hits the real gql.twitch.tv. Run by hand to check the channel page and video queries still
/// validate: `cargo test -p htpc-twitch-api --test gql -- --ignored live_`
#[tokio::test]
#[ignore = "network: real Twitch GQL"]
async fn live_channel_videos_and_details() {
    let api = GqlClient::new().unwrap();
    let page = api
        .channel_videos("twitch")
        .await
        .unwrap()
        .expect("twitch exists");
    assert_eq!(page.user.id, "12826");
    let any = page
        .archives
        .iter()
        .chain(&page.highlights)
        .next()
        .expect("twitch has videos");
    let details = api
        .video_details(&any.id)
        .await
        .unwrap()
        .expect("the video");
    assert!(!details.chapters.is_empty());
    assert_eq!(api.channel_videos("nosuchuser_zz_123").await.unwrap(), None);
    // Usually not live: the query only has to validate.
    api.live_archive("twitch").await.unwrap();
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
