//! Runs the real Streamlink. Ignored by default (needs Streamlink and a network).
//!
//! `STREAMLINK_PATH` overrides the program. `LIVE_CHANNEL` names a channel that is live now,
//! `LIVE_VIDEO` a Twitch video id and `LIVE_CLIP` a Twitch clip slug that can be played.
//! Run: `cargo test -p htpc-resolver --test live -- --ignored --nocapture`

use htpc_resolver::{ResolveError, Site, StreamResolver, StreamlinkResolver, Target, TokioRunner};

fn resolver() -> StreamlinkResolver<TokioRunner> {
    let r = StreamlinkResolver::new(TokioRunner);
    match std::env::var("STREAMLINK_PATH") {
        Ok(path) if !path.trim().is_empty() => r.with_program(path),
        _ => r,
    }
}

/// The environment variable `name`, or a panic that says to set it.
fn env(name: &str) -> String {
    std::env::var(name).unwrap_or_else(|_| panic!("set {name}"))
}

#[tokio::test]
#[ignore = "needs Streamlink and a network"]
async fn live_a_channel_that_does_not_exist_is_offline() {
    let got = resolver()
        .resolve(
            &Target::channel(Site::Twitch, "zz_no_such_channel_9981"),
            "best",
        )
        .await;
    assert_eq!(got, Err(ResolveError::Offline));
}

#[tokio::test]
#[ignore = "needs Streamlink, a network and LIVE_CHANNEL"]
async fn live_a_live_channel_resolves() {
    let channel = env("LIVE_CHANNEL");
    let url = resolver()
        .resolve(&Target::channel(Site::Twitch, channel), "best")
        .await
        .expect("resolves");
    println!("resolved: {} chars", url.0.len());
    assert!(url.0.starts_with("https://"));
}

#[tokio::test]
#[ignore = "needs Streamlink and a network"]
async fn live_a_video_that_does_not_exist_is_offline() {
    let got = resolver()
        .resolve(&Target::TwitchVideo("999999999999".into()), "best")
        .await;
    assert_eq!(got, Err(ResolveError::Offline));
}

#[tokio::test]
#[ignore = "needs Streamlink, a network and LIVE_VIDEO"]
async fn live_a_video_resolves_to_a_playlist() {
    let url = resolver()
        .resolve(&Target::TwitchVideo(env("LIVE_VIDEO")), "720p60,720p,best")
        .await
        .expect("resolves");
    println!("resolved: {}…", &url.0[..url.0.len().min(80)]);
    assert!(url.0.starts_with("https://") && url.0.contains(".m3u8"));
}

#[tokio::test]
#[ignore = "needs Streamlink, a network and LIVE_CLIP"]
async fn live_a_clip_resolves_to_an_mp4() {
    let url = resolver()
        .resolve(&Target::TwitchClip(env("LIVE_CLIP")), "best")
        .await
        .expect("resolves");
    println!("resolved: {}…", &url.0[..url.0.len().min(80)]);
    assert!(url.0.starts_with("https://") && url.0.contains(".mp4"));
}

#[tokio::test]
#[ignore = "needs a network"]
async fn live_a_missing_program_is_not_installed() {
    let got = StreamlinkResolver::new(TokioRunner)
        .with_program("definitely-not-a-program-9981")
        .resolve(&Target::channel(Site::Twitch, "x"), "best")
        .await;
    assert_eq!(got, Err(ResolveError::NotInstalled));
}
