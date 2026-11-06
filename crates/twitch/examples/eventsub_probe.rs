//! `eventsub_probe`: watches one channel for raids through the app's own EventSub client, with
//! LiveDeck's stored sign-in, and prints every raid.
//!
//! A research tool for the spikes in `plan/00-spikes.md` (07: does `channel.raid` accept a
//! viewer's token from our public client?), not part of the app.
//!
//! ```text
//! cargo run -p htpc-twitch-api --example eventsub_probe -- <login> [--client-id <id>]
//! ```
//!
//! Sign in to LiveDeck first. The protocol is logged: `RUST_LOG=debug` shows the welcome, the
//! subscription ("watching for raids") and every reconnect. Ctrl+C to stop.

use std::sync::Arc;

use htpc_twitch_api::{
    Credentials, EventSub, EventSubEvent, HelixClient, RaidWatch, Refresher, TokenSource, TwitchApi,
};

/// LiveDeck's built-in client id (`DEFAULT_CLIENT_ID` in the app's `main.rs`).
const APP_CLIENT_ID: &str = "b4d5txrh32mzg91jwrc0z79zk3myyd";

/// The stored token, as is. It cannot be renewed here: sign in to LiveDeck again if it expired.
struct Stored(Credentials);

impl Refresher for Stored {
    fn generation(&self) -> u64 {
        0
    }

    async fn refresh(&self, _seen: u64) -> bool {
        eprintln!("eventsub_probe: Twitch refused the token (expired?): open LiveDeck to renew it");
        false
    }
}

impl TokenSource for Stored {
    fn credentials(&self) -> Option<Credentials> {
        Some(self.0.clone())
    }
}

fn stored_token() -> Result<String, String> {
    let paths = htpc_storage::Paths::from_os().map_err(|e| e.to_string())?;
    let tokens = htpc_storage::TokenStore::new(paths.token_file())
        .load()
        .map_err(|e| e.to_string())?
        .ok_or("not signed in: sign in to LiveDeck first")?;
    Ok(tokens.access_token)
}

fn fail(message: impl std::fmt::Display) -> ! {
    eprintln!("eventsub_probe: {message}");
    std::process::exit(2);
}

#[tokio::main]
async fn main() {
    let filter = tracing_subscriber::EnvFilter::try_from_default_env()
        .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("htpc_twitch_api=debug"));
    tracing_subscriber::fmt().with_env_filter(filter).init();

    let mut args = pico_args::Arguments::from_env();
    let client_id: String = args
        .opt_value_from_str("--client-id")
        .unwrap_or_else(|e| fail(e))
        .unwrap_or_else(|| APP_CLIENT_ID.to_owned());
    let login: String = args
        .free_from_str()
        .unwrap_or_else(|_| fail("usage: eventsub_probe <login> [--client-id <id>]"));
    let creds = Credentials {
        client_id,
        access_token: stored_token().unwrap_or_else(|e| fail(e)),
    };

    let helix = HelixClient::new(Some(creds.clone())).unwrap_or_else(|e| fail(e));
    let user = match helix.user(Some(&login)).await {
        Ok(Some(user)) => user,
        Ok(None) => fail(format!("no channel called {login}")),
        Err(e) => fail(e),
    };
    println!("watching {} (user id {}) for raids", user.login, user.id);

    let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();
    let sub = EventSub::start(Arc::new(Stored(creds)), tx).unwrap_or_else(|e| fail(e));
    sub.watch_raids(&user.id);
    while let Some(event) = rx.recv().await {
        let EventSubEvent::Raid(raid) = event;
        println!(
            "RAID: {} -> {} ({}) with {} viewers",
            raid.from_login, raid.to_login, raid.to_name, raid.viewers
        );
    }
}
