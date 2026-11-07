//! `gql_probe`: sends one raw query to Twitch's internal GQL endpoint and prints the JSON.
//!
//! A research tool for the spikes in `plan/00-spikes.md`, not part of the app. It uses the
//! website's public client id, like `GqlClient`, so it shows what an anonymous viewer can read.
//!
//! ```text
//! cargo run -p htpc-twitch-api --example gql_probe -- [options] <query | @file | --hash <sha256> --op <name>>
//!
//!   --vars '<json>'      variables (default {})
//!   --hash <sha256>      send a persisted query instead of query text (needs --op)
//!   --op <name>          operationName (needed by persisted queries)
//!   --user               add "Authorization: OAuth <token>" with LiveDeck's stored token
//!   --client-id <id>     override the Client-Id header (default: the website's)
//!   --every <secs>       poll and print only when the response changes (Ctrl+C to stop)
//!   --save <file>        append every raw response to <file>, one JSON per line
//! ```
//!
//! Example: `gql_probe --vars '{"l":"xqc"}' 'query($l: String!) { user(login: $l) { channel {
//! activePredictionEvents { id title status } } } }'`

use std::io::Write;
use std::time::Duration;

use htpc_twitch_api::{GQL_URL, WEB_CLIENT_ID};
use serde_json::{json, Value};

struct Args {
    body: Value,
    user: bool,
    client_id: String,
    every: Option<u64>,
    save: Option<String>,
}

fn parse() -> Result<Args, String> {
    let mut a = pico_args::Arguments::from_env();
    if a.contains(["-h", "--help"]) {
        return Err("see the module docs in crates/twitch/examples/gql_probe.rs".into());
    }
    let vars: Value = match a
        .opt_value_from_str::<_, String>("--vars")
        .map_err(|e| e.to_string())?
    {
        Some(s) => serde_json::from_str(&s).map_err(|e| format!("--vars: {e}"))?,
        None => json!({}),
    };
    let hash: Option<String> = a.opt_value_from_str("--hash").map_err(|e| e.to_string())?;
    let op: Option<String> = a.opt_value_from_str("--op").map_err(|e| e.to_string())?;
    let user = a.contains("--user");
    let client_id = a
        .opt_value_from_str("--client-id")
        .map_err(|e| e.to_string())?
        .unwrap_or_else(|| WEB_CLIENT_ID.to_owned());
    let every = a.opt_value_from_str("--every").map_err(|e| e.to_string())?;
    let save = a.opt_value_from_str("--save").map_err(|e| e.to_string())?;
    let rest: Vec<String> = a
        .finish()
        .into_iter()
        .map(|s| s.to_string_lossy().into_owned())
        .collect();

    let mut body = json!({ "variables": vars });
    if let Some(op) = op {
        body["operationName"] = json!(op);
    }
    match (hash, rest.as_slice()) {
        (Some(h), []) => {
            if body.get("operationName").is_none() {
                return Err("--hash needs --op".into());
            }
            body["extensions"] = json!({ "persistedQuery": { "version": 1, "sha256Hash": h } });
        }
        (None, [q]) => {
            let text = match q.strip_prefix('@') {
                Some(path) => std::fs::read_to_string(path).map_err(|e| format!("{path}: {e}"))?,
                None => q.clone(),
            };
            body["query"] = json!(text);
        }
        _ => return Err("give exactly one of: a query (or @file), or --hash with --op".into()),
    }
    Ok(Args {
        body,
        user,
        client_id,
        every,
        save,
    })
}

/// LiveDeck's stored access token (OS keyring, then the fallback file).
fn stored_token() -> Result<String, String> {
    let paths = htpc_storage::Paths::from_os().map_err(|e| e.to_string())?;
    let tokens = htpc_storage::TokenStore::new(paths.token_file())
        .load()
        .map_err(|e| e.to_string())?
        .ok_or("not signed in: sign in to LiveDeck first")?;
    Ok(tokens.access_token)
}

#[tokio::main]
async fn main() {
    let args = match parse() {
        Ok(a) => a,
        Err(e) => {
            eprintln!("gql_probe: {e}");
            std::process::exit(2);
        }
    };
    let token = if args.user {
        match stored_token() {
            Ok(t) => Some(t),
            Err(e) => {
                eprintln!("gql_probe: --user: {e}");
                std::process::exit(2);
            }
        }
    } else {
        None
    };

    let http = reqwest::Client::new();
    let mut last: Option<Value> = None;
    loop {
        let mut req = http
            .post(GQL_URL)
            .header("Client-Id", &args.client_id)
            .json(&args.body);
        if let Some(t) = &token {
            req = req.header("Authorization", format!("OAuth {t}"));
        }
        let (status, text) = match req.send().await {
            Ok(r) => (r.status(), r.text().await.unwrap_or_default()),
            Err(e) => {
                eprintln!("gql_probe: {e}");
                std::process::exit(1);
            }
        };
        if let Some(path) = &args.save {
            if let Ok(mut f) = std::fs::OpenOptions::new()
                .create(true)
                .append(true)
                .open(path)
            {
                let _ = writeln!(f, "{}", text.trim_end());
            }
        }
        let mut value: Value = serde_json::from_str(&text).unwrap_or(Value::String(text));
        // `extensions` carries a per-request id and timing, which would count as a change.
        if let Some(o) = value.as_object_mut() {
            o.remove("extensions");
        }
        if last.as_ref() != Some(&value) {
            println!("--- t={} HTTP {status}", unix_now());
            println!(
                "{}",
                serde_json::to_string_pretty(&value).unwrap_or_default()
            );
            last = Some(value);
        }
        match args.every {
            Some(s) => tokio::time::sleep(Duration::from_secs(s.max(1))).await,
            None => break,
        }
    }
}

/// Seconds since the Unix epoch, enough to line up polls with a browser HAR.
fn unix_now() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_secs())
}
