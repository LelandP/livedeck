//! Against the real kick.com. Ignored by default: run with
//! `cargo test -p htpc-kick-api --test live -- --ignored --nocapture`.

use htpc_kick_api::{KickApi, KickWebClient};

#[tokio::test]
#[ignore = "needs the network and kick.com"]
async fn top_live_answers() {
    let kick = KickWebClient::new().unwrap();
    let page = kick.top_livestreams(1).await;
    println!(
        "top_livestreams: {:?}",
        page.as_ref().map(|p| p.streams.len())
    );
    let channel = kick.channel("xqc").await;
    println!(
        "channel: {:?}",
        channel.as_ref().map(|c| c.as_ref().map(|c| c.chatroom_id))
    );
    let found = kick.search_channels("xqc").await;
    println!("search: {:?}", found.as_ref().map(Vec::len));
    let app = kick.chat_app(668).await;
    println!("chat app: {app:?}");
    page.unwrap();
    channel.unwrap();
    found.unwrap();
    app.unwrap();
}
