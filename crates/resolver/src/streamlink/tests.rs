use super::*;
use crate::FakeRunner;

const PLAYLIST: &str = "https://usher.example/api/channel/hls/somechannel.m3u8?sig=abc";

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
    let url = resolver.resolve("somechannel", "720p").await.unwrap();
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
    resolver.resolve("a_b", "best").await.unwrap();
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
            .resolve("x", "best")
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
            resolver.resolve(bad, "best").await,
            Err(ResolveError::NotFound),
            "{bad:?}"
        );
    }
    assert!(runner.calls().is_empty());
}
