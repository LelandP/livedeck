//! `kick_probe`: records the raw chat frames of one Kick chatroom, one JSON frame per line.
//!
//! A research tool for the spikes in `plan/11-kick-support.md`, not part of the app. The saved
//! lines become the chat mapping fixtures.
//!
//! ```text
//! cargo run -p htpc-kick-api --example kick_probe -- <chatroom id> [--secs 60] [--out frames.jsonl]
//! ```
//!
//! The chatroom id is `chatroom.id` in `https://kick.com/api/v2/channels/<slug>`.

use std::io::Write;
use std::time::Duration;

use async_tungstenite::tungstenite::Message;
use futures_util::StreamExt;

fn fail(message: impl std::fmt::Display) -> ! {
    eprintln!("kick_probe: {message}");
    std::process::exit(2);
}

#[tokio::main]
async fn main() {
    let mut args = pico_args::Arguments::from_env();
    let secs: u64 = args
        .opt_value_from_str("--secs")
        .unwrap_or_else(|e| fail(e))
        .unwrap_or(60);
    let out: Option<String> = args.opt_value_from_str("--out").unwrap_or_else(|e| fail(e));
    let room: String = args
        .free_from_str()
        .unwrap_or_else(|_| fail("usage: kick_probe <chatroom id> [--secs 60] [--out file]"));

    let url = htpc_kick_api::pusher_url(htpc_kick_api::PUSHER_KEY, htpc_kick_api::PUSHER_CLUSTER);
    let (mut ws, _) = async_tungstenite::tokio::connect_async(url)
        .await
        .unwrap_or_else(|e| fail(e));
    for channel in [format!("chatrooms.{room}.v2"), format!("chatroom_{room}")] {
        let sub = serde_json::json!({
            "event": "pusher:subscribe",
            "data": { "auth": "", "channel": channel },
        });
        ws.send(Message::text(sub.to_string()))
            .await
            .unwrap_or_else(|e| fail(e));
    }

    let mut file = out.map(|p| std::fs::File::create(p).unwrap_or_else(|e| fail(e)));
    let deadline = tokio::time::Instant::now() + Duration::from_secs(secs);
    let mut frames = 0usize;
    loop {
        let next = tokio::time::timeout_at(deadline, ws.next()).await;
        let Ok(Some(item)) = next else { break };
        match item {
            Ok(Message::Text(text)) => {
                frames += 1;
                let text = text.as_str();
                match &mut file {
                    Some(f) => writeln!(f, "{text}").unwrap_or_else(|e| fail(e)),
                    None => println!("{text}"),
                }
            }
            Ok(Message::Close(frame)) => fail(format!("closed: {frame:?}")),
            Ok(_) => {}
            Err(e) => fail(e),
        }
    }
    eprintln!("kick_probe: {frames} frames in {secs} s");
}
