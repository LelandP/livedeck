//! Checks against the real Twitch servers. Ignored by default (they need the network and a busy
//! live channel). Run with:
//!
//! ```text
//! $env:LIVEDECK_CHAT_CHANNEL = "some_busy_live_channel"
//! cargo test -p htpc-chat --test live -- --ignored --nocapture
//! ```

use std::time::Duration;

use htpc_chat::{ChatClient, ChatEvent, HttpBadges, IrcChat};
use htpc_core::ChatStatus;
use tokio::sync::mpsc::unbounded_channel;
use tokio::time::timeout;

#[tokio::test]
#[ignore = "needs the network"]
async fn live_badge_sets_are_served() {
    let badges = HttpBadges::new().unwrap();
    let global = badges.global().await.expect("global badges");
    println!("{} global badges", global.len());
    assert!(
        global.contains_key(&("moderator".to_owned(), "1".to_owned())),
        "the moderator badge is among the global ones"
    );
    let channel = badges.channel("twitch").await.expect("channel badges");
    println!("{} badges of one channel", channel.len());
}
#[tokio::test]
#[ignore = "needs the network and a live channel"]
async fn live_chat_connects_and_delivers_lines() {
    let channel = std::env::var("LIVEDECK_CHAT_CHANNEL").unwrap_or_else(|_| "twitch".into());
    let chat = IrcChat::new();
    let (tx, mut rx) = unbounded_channel();
    let _session = chat.join(&channel, tx);

    let mut went_live = false;
    let mut lines = Vec::new();
    let waited = timeout(Duration::from_secs(60), async {
        while let Some(event) = rx.recv().await {
            println!("{event:?}");
            match event {
                ChatEvent::Status(ChatStatus::Live) => went_live = true,
                ChatEvent::Messages(batch) => lines.extend(batch),
                _ => {}
            }
            if went_live && lines.len() >= 5 {
                break;
            }
        }
    })
    .await;
    assert!(went_live, "never went live in {channel}");
    assert!(
        waited.is_ok() && lines.len() >= 5,
        "only {} lines from {channel} (is it live and busy?)",
        lines.len()
    );
    let with_badges = lines.iter().filter(|l| !l.badges.is_empty()).count();
    let with_emotes = lines.iter().filter(|l| l.image_urls().count() > 0).count();
    println!(
        "{} lines, {with_badges} with badges, {with_emotes} with images",
        lines.len()
    );
    for line in lines.iter().take(8) {
        println!("{}: {}  {:?}", line.user, line.text(), line.badges);
    }
}
