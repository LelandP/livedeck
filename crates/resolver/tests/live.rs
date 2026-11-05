//! Runs the real Streamlink. Ignored by default (needs Streamlink and a network).
//!
//! `STREAMLINK_PATH` overrides the program. `LIVE_CHANNEL` names a channel that is live now.
//! Run: `cargo test -p htpc-resolver --test live -- --ignored --nocapture`

use htpc_resolver::{ResolveError, StreamResolver, StreamlinkResolver, TokioRunner};

fn resolver() -> StreamlinkResolver<TokioRunner> {
    let r = StreamlinkResolver::new(TokioRunner);
    match std::env::var("STREAMLINK_PATH") {
        Ok(path) if !path.trim().is_empty() => r.with_program(path),
        _ => r,
    }
}

#[tokio::test]
#[ignore = "needs Streamlink and a network"]
async fn live_a_channel_that_does_not_exist_is_offline() {
    let got = resolver().resolve("zz_no_such_channel_9981", "best").await;
    assert_eq!(got, Err(ResolveError::Offline));
}

#[tokio::test]
#[ignore = "needs Streamlink, a network and LIVE_CHANNEL"]
async fn live_a_live_channel_resolves() {
    let Ok(channel) = std::env::var("LIVE_CHANNEL") else {
        panic!("set LIVE_CHANNEL to a channel that is live");
    };
    let url = resolver()
        .resolve(&channel, "best")
        .await
        .expect("resolves");
    println!("resolved: {} chars", url.0.len());
    assert!(url.0.starts_with("https://"));
}

#[tokio::test]
#[ignore = "needs a network"]
async fn live_a_missing_program_is_not_installed() {
    let got = StreamlinkResolver::new(TokioRunner)
        .with_program("definitely-not-a-program-9981")
        .resolve("x", "best")
        .await;
    assert_eq!(got, Err(ResolveError::NotInstalled));
}
