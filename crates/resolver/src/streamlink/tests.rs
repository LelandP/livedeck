use super::*;
use crate::{FakeRunner, Site};

const PLAYLIST: &str = "https://usher.example/api/channel/hls/somechannel.m3u8?sig=abc";

/// A clip's answer, as Streamlink 8.6.3 gives it: an MP4 file, not a playlist.
const CLIP_MP4: &str = "https://d1m7jfoe9zdc1j.cloudfront.net/abc/index.mp4?sig=def&token=ghi";

/// The live stream of a Twitch login.
fn twitch(login: &str) -> Target {
    Target::channel(Site::Twitch, login)
}

fn ok(stdout: &str) -> ProcessOutput {
    ProcessOutput {
        success: true,
        stdout: stdout.into(),
        stderr: String::new(),
    }
}

fn failed(stderr: &str) -> ProcessOutput {
    ProcessOutput {
        success: false,
        stdout: String::new(),
        stderr: stderr.into(),
    }
}

#[tokio::test]
async fn success_returns_the_url_and_passes_the_arguments() {
    let runner = FakeRunner::new(Ok(ok(&format!("{PLAYLIST}\n"))));
    let resolver = StreamlinkResolver::new(runner.clone());
    let url = resolver
        .resolve(&Target::channel(Site::Twitch, "somechannel"), "720p")
        .await
        .unwrap();
    assert_eq!(url, PlaylistUrl(PLAYLIST.into()));
    let calls = runner.calls();
    assert_eq!(calls.len(), 1);
    assert_eq!(calls[0].0, "streamlink");
    assert_eq!(
        calls[0].1,
        ["--stream-url", "https://twitch.tv/somechannel", "720p,best"]
    );
}

#[tokio::test]
async fn a_custom_program_is_used() {
    let runner = FakeRunner::new(Ok(ok(PLAYLIST)));
    let resolver = StreamlinkResolver::new(runner.clone()).with_program("C:/tools/sl.exe");
    resolver.resolve(&twitch("a_b"), "best").await.unwrap();
    assert_eq!(runner.calls()[0].0, "C:/tools/sl.exe");
}

#[test]
fn best_and_worst_have_no_fallback() {
    assert_eq!(quality_arg("best"), "best");
    assert_eq!(quality_arg(" Worst "), "worst");
    assert_eq!(quality_arg("1080p"), "1080p,best");
    assert_eq!(quality_arg(""), "best");
    assert_eq!(quality_arg("a b;c"), "best");
}

#[test]
fn fallback_lists_pass_through() {
    assert_eq!(
        quality_arg("480p30,480p,360p30,worst"),
        "480p30,480p,360p30,worst"
    );
    assert_eq!(quality_arg(" 720p60 , 720p "), "720p60,720p,best");
    assert_eq!(quality_arg("720p,,best"), "best", "an empty name");
    assert_eq!(quality_arg("720p,a b"), "best");
    assert_eq!(quality_arg(&vec!["160p"; MAX_NAMES + 1].join(",")), "best");
}

#[test]
fn audio_only_falls_back_to_video_when_a_stream_has_no_audio_rendition() {
    assert_eq!(quality_arg("audio_only"), "audio_only,best");
}

#[test]
fn no_playable_streams_means_offline() {
    let out = failed("error: No playable streams found on this URL: https://twitch.tv/x\n");
    assert_eq!(map_output(&out), Err(ResolveError::Offline));
}

#[test]
fn unknown_url_means_not_found() {
    let out = failed("error: No plugin can handle URL: https://example.com/\n");
    assert_eq!(map_output(&out), Err(ResolveError::NotFound));
}

#[test]
fn other_failures_carry_the_last_line() {
    let out = failed("[cli][info] Found matching plugin twitch\nerror: Unable to open URL\n\n");
    assert_eq!(
        map_output(&out),
        Err(ResolveError::Failed("error: Unable to open URL".into()))
    );
    assert_eq!(
        map_output(&failed("")),
        Err(ResolveError::Failed(
            "Streamlink failed without a message".into()
        ))
    );
}

#[test]
fn success_without_a_url_is_a_failure() {
    assert!(matches!(
        map_output(&ok("warning: nothing\n")),
        Err(ResolveError::Failed(_))
    ));
}

#[test]
fn the_url_is_found_among_other_output_lines() {
    let out = ok(&format!("[cli][info] hello\n{PLAYLIST}\n"));
    assert_eq!(map_output(&out), Ok(PlaylistUrl(PLAYLIST.into())));
}

#[tokio::test]
async fn runner_errors_map_to_resolve_errors() {
    let case = |err| async move {
        StreamlinkResolver::new(FakeRunner::new(Err(err)))
            .resolve(&twitch("x"), "best")
            .await
    };
    assert_eq!(
        case(RunError::NotFound).await,
        Err(ResolveError::NotInstalled)
    );
    assert_eq!(case(RunError::TimedOut).await, Err(ResolveError::TimedOut));
    assert_eq!(
        case(RunError::Io("boom".into())).await,
        Err(ResolveError::Failed("boom".into()))
    );
}

#[tokio::test]
async fn bad_channel_names_never_reach_the_program() {
    let runner = FakeRunner::new(Ok(ok(PLAYLIST)));
    let resolver = StreamlinkResolver::new(runner.clone());
    for bad in ["", "a b", "x/../y", "x?y=1", "--help", &"a".repeat(26)] {
        assert_eq!(
            resolver.resolve(&twitch(bad), "best").await,
            Err(ResolveError::NotFound),
            "{bad:?}"
        );
    }
    assert!(runner.calls().is_empty());
}

#[tokio::test]
async fn a_kick_channel_opens_its_kick_page() {
    let runner = FakeRunner::new(Ok(ok(PLAYLIST)));
    let resolver = StreamlinkResolver::new(runner.clone());
    resolver
        .resolve(&Target::channel(Site::Kick, "awa-xo"), "720p60")
        .await
        .unwrap();
    assert_eq!(
        runner.calls()[0].1,
        ["--stream-url", "https://kick.com/awa-xo", "720p60,best"]
    );
}

#[tokio::test]
async fn hyphens_are_kick_only_and_never_lead() {
    let runner = FakeRunner::new(Ok(ok(PLAYLIST)));
    let resolver = StreamlinkResolver::new(runner.clone());
    for (site, bad) in [
        (Site::Twitch, "awa-xo"),
        (Site::Kick, "-help"),
        (Site::Kick, "a b"),
        (Site::Kick, "x/../y"),
    ] {
        assert_eq!(
            resolver.resolve(&Target::channel(site, bad), "best").await,
            Err(ResolveError::NotFound),
            "{site:?} {bad:?}"
        );
    }
    assert!(runner.calls().is_empty());
}

#[tokio::test]
async fn a_video_opens_its_video_page_with_the_quality_list() {
    let runner = FakeRunner::new(Ok(ok(PLAYLIST)));
    let resolver = StreamlinkResolver::new(runner.clone());
    let url = resolver
        .resolve(
            &Target::TwitchVideo("2612345678".into()),
            "720p60,720p,worst",
        )
        .await;
    assert_eq!(url, Ok(PlaylistUrl(PLAYLIST.into())));
    assert_eq!(
        runner.calls()[0].1,
        [
            "--stream-url",
            "https://www.twitch.tv/videos/2612345678",
            "720p60,720p,worst"
        ]
    );
}

#[tokio::test]
async fn a_clip_opens_its_clip_page_and_resolves_to_an_mp4() {
    let runner = FakeRunner::new(Ok(ok(&format!("{CLIP_MP4}\n"))));
    let resolver = StreamlinkResolver::new(runner.clone());
    let url = resolver
        .resolve(
            &Target::TwitchClip("CleverTiredOtter-AbC123_xyz".into()),
            "1080p",
        )
        .await;
    assert_eq!(url, Ok(PlaylistUrl(CLIP_MP4.into())));
    assert_eq!(
        runner.calls()[0].1,
        [
            "--stream-url",
            "https://clips.twitch.tv/CleverTiredOtter-AbC123_xyz",
            "1080p,best"
        ]
    );
}

#[tokio::test]
async fn bad_video_ids_and_clip_slugs_never_reach_the_program() {
    let runner = FakeRunner::new(Ok(ok(PLAYLIST)));
    let resolver = StreamlinkResolver::new(runner.clone());
    let video = |id: &str| Target::TwitchVideo(id.into());
    let clip = |slug: &str| Target::TwitchClip(slug.into());
    for bad in [
        video(""),
        video("v2612345678"),
        video("12 34"),
        video("1234567890123"),
        video("-1"),
        video("１２３"),
        clip(""),
        clip("-help"),
        clip("a b"),
        clip("x/../y"),
        clip("x?y=1"),
        clip("slug.mp4"),
        clip(&"a".repeat(101)),
    ] {
        assert_eq!(
            resolver.resolve(&bad, "best").await,
            Err(ResolveError::NotFound),
            "{bad:?}"
        );
    }
    assert!(runner.calls().is_empty());
}

#[test]
fn the_longest_valid_ids_and_slugs_pass() {
    assert!(Target::TwitchVideo("9".repeat(12)).page_url().is_some());
    assert!(Target::TwitchVideo("1".into()).page_url().is_some());
    assert!(Target::TwitchClip("a".repeat(100)).page_url().is_some());
    assert!(Target::TwitchClip("_x-Y_9".into()).page_url().is_some());
}

#[test]
fn a_missing_video_or_clip_is_offline_as_streamlink_says_it() {
    // Streamlink 8.6.3's answer for a video id and a clip slug that do not exist (2026-11-12).
    for url in [
        "https://www.twitch.tv/videos/999999999999",
        "https://clips.twitch.tv/NoSuchClipSlugAtAll-zzz123",
    ] {
        let out = failed(&format!(
            "error: No playable streams found on this URL: {url}\n"
        ));
        assert_eq!(map_output(&out), Err(ResolveError::Offline), "{url}");
    }
}

#[test]
fn a_target_names_what_it_points_at() {
    assert_eq!(twitch("abc").name(), "abc");
    assert_eq!(Target::TwitchVideo("42".into()).name(), "42");
    assert_eq!(Target::TwitchClip("Slug".into()).name(), "Slug");
}
