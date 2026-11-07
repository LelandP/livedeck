//! Kick client behind the mockable `KickApi` trait.
//!
//! Kick's official API wants a token for every call, and a token needs the app's client secret,
//! which an open-source app cannot keep. Signed out (the only way LiveDeck uses Kick), this crate
//! talks to the website's own unofficial API, as the Twitch crate does with GQL. It can break
//! without notice.
//!
//! This crate does not depend on `htpc-core`. The app maps its types to domain types.

/// The Pusher application the Kick website reads chat from, as of 2026-11-06. Kick hands out the
/// current key at `POST web.kick.com/api/v1/realtime/channels/<id>/chat/connection`; this one is
/// the fallback when that call fails.
pub const PUSHER_KEY: &str = "34bf7a0ff419a2a775b9";
pub const PUSHER_CLUSTER: &str = "us2";

/// The Pusher WebSocket address for `key` in `cluster`.
pub fn pusher_url(key: &str, cluster: &str) -> String {
    format!(
        "wss://ws-{cluster}.pusher.com/app/{key}?protocol=7&client=js&version=8.4.0&flash=false"
    )
}
