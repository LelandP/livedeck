//! Proof of concept for VOD playback: a channel's latest past broadcast, found through GQL,
//! resolved by Streamlink and played by libmpv in its own window, with a scripted seek test.
//!
//! ```text
//! cargo run -p htpc-player --features mpv --example vod_probe -- [login] [start] [quality]
//! ```
//!
//! `login` defaults to `chatterbox`, `start` (where to begin, `1:12:40` or seconds, like a resume
//! position) to `0`, `quality` to `best`. `HTPC_STREAMLINK` names the Streamlink program; without
//! it the portable copy in the repo's `deps\` folder is used, then `streamlink` from `PATH`.
//!
//! After 15 s of playback the probe seeks +10 s, -10 s, +1 h and back to the start position, and
//! prints how long each seek took to show a frame again. The window then stays open: mpv's own
//! keys work (Space pauses, arrows seek 5 s / 1 min, q quits).

use std::path::PathBuf;
use std::process::Command;
use std::time::{Duration, Instant};

use libmpv2::events::Event;
use libmpv2::Mpv;
use serde_json::{json, Value};

const GQL_URL: &str = "https://gql.twitch.tv/gql";
/// The twitch.tv website's public client ID, as in `crates/twitch/src/gql.rs`.
const WEB_CLIENT_ID: &str = "kimne78kx3ncx6brgo4mv6wki5h1ko";
const LATEST_VIDEO_QUERY: &str = "query($login: String!) { user(login: $login) { displayName \
    videos(first: 1, type: ARCHIVE, sort: TIME) { edges { node { \
    id title lengthSeconds createdAt game { name } } } } } }";

struct Video {
    id: String,
    title: String,
    length: u64,
    created: String,
    game: String,
    channel: String,
}

fn main() {
    let mut args = std::env::args().skip(1);
    let login = args.next().unwrap_or_else(|| "chatterbox".into());
    let start = args.next().map_or(0, |s| parse_time(&s));
    let quality = args.next().unwrap_or_else(|| "best".into());

    let t = Instant::now();
    let video = latest_video(&login);
    println!(
        "{} · {}\n  video {} · {} · {} · streamed {}  (GQL {} ms)",
        video.channel,
        video.title,
        video.id,
        video.game,
        hms(video.length),
        video.created,
        t.elapsed().as_millis()
    );

    let t = Instant::now();
    let url = resolve(&video.id, &quality);
    println!(
        "  Streamlink: {} ms\n  {}…",
        t.elapsed().as_millis(),
        &url[..url.len().min(100)]
    );

    play(&url, &video, start);
}

/// The channel's newest past broadcast (GQL, anonymous).
fn latest_video(login: &str) -> Video {
    let body = json!({ "query": LATEST_VIDEO_QUERY, "variables": { "login": login } });
    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .expect("tokio runtime");
    let data: Value = rt.block_on(async {
        reqwest::Client::new()
            .post(GQL_URL)
            .header("Client-Id", WEB_CLIENT_ID)
            .json(&body)
            .send()
            .await
            .expect("GQL request")
            .json()
            .await
            .expect("GQL answer is JSON")
    });
    let user = &data["data"]["user"];
    if user.is_null() {
        panic!("no Twitch user {login:?}: {data}");
    }
    let node = &user["videos"]["edges"][0]["node"];
    if node.is_null() {
        panic!("{login} has no past broadcasts: {data}");
    }
    let s = |v: &Value| v.as_str().unwrap_or_default().to_owned();
    Video {
        id: s(&node["id"]),
        title: s(&node["title"]),
        length: node["lengthSeconds"].as_u64().unwrap_or(0),
        created: s(&node["createdAt"]),
        game: s(&node["game"]["name"]),
        channel: s(&user["displayName"]),
    }
}

/// `streamlink --stream-url https://www.twitch.tv/videos/<id> <quality>,best`, as the app's
/// resolver does for live channels.
fn resolve(id: &str, quality: &str) -> String {
    let out = Command::new(streamlink())
        .args([
            "--stream-url",
            &format!("https://www.twitch.tv/videos/{id}"),
            &format!("{quality},best"),
        ])
        .output()
        .expect("Streamlink runs (set HTPC_STREAMLINK)");
    let text = String::from_utf8_lossy(&out.stdout).trim().to_owned();
    if !out.status.success() || !text.starts_with("http") {
        panic!(
            "Streamlink could not resolve video {id}: {text} {}",
            String::from_utf8_lossy(&out.stderr)
        );
    }
    text
}

fn streamlink() -> PathBuf {
    if let Some(p) = std::env::var_os("HTPC_STREAMLINK") {
        return p.into();
    }
    let bundled = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../deps/streamlink/x/streamlink-8.6.3-1-py315-x86_64/bin/streamlink.exe");
    if bundled.exists() {
        bundled
    } else {
        "streamlink".into()
    }
}

/// Plays `url` from `start` in mpv's own window, then runs the seek test.
fn play(url: &str, video: &Video, start: u64) {
    let mpv = Mpv::with_initializer(|init| {
        init.set_option("vo", "gpu")?;
        init.set_option("hwdec", "auto-safe")?;
        init.set_option("force-window", "yes")?;
        init.set_option("keep-open", "yes")?;
        init.set_option("osc", "yes")?;
        init.set_option("input-default-bindings", "yes")?;
        init.set_option("input-vo-keyboard", "yes")?;
        init.set_option("ytdl", false)?;
        init.set_option("title", format!("vod_probe · {}", video.title).as_str())?;
        init.set_option("geometry", "1280x720")?;
        Ok(())
    })
    .expect("libmpv starts");

    let t = Instant::now();
    let opts = format!("start={start}");
    mpv.command("loadfile", &[url, "replace", "-1", opts.as_str()])
        .expect("loadfile");
    println!("loading at {} …", hms(start));

    // The scripted seeks: (seconds of playback before it, what to seek to, label).
    let plan: Vec<(f64, String, String)> = vec![
        (15.0, "+10".into(), "+10 s".into()),
        (8.0, "-10".into(), "-10 s".into()),
        (8.0, "+3600".into(), "+1 h".into()),
        (8.0, start.to_string(), format!("back to {}", hms(start))),
    ];
    let mut step = 0;
    let mut first_frame = false;
    let mut seek_started: Option<(Instant, String)> = None;
    let mut since_restart = Instant::now();
    let mut last_print = Instant::now();

    loop {
        while let Some(event) = mpv.wait_event(0.05) {
            match event {
                Ok(Event::PlaybackRestart) => {
                    if !first_frame {
                        first_frame = true;
                        println!("first frame after {} ms", t.elapsed().as_millis());
                    } else if let Some((at, label)) = seek_started.take() {
                        println!(
                            "  seek {label}: picture back after {} ms, now at {}",
                            at.elapsed().as_millis(),
                            pos(&mpv)
                        );
                    } else {
                        // A seek of the viewer's own (keys or the seek bar).
                        println!("  picture back at {} after a seek in the window", pos(&mpv));
                    }
                    since_restart = Instant::now();
                }
                Ok(Event::EndFile(r)) => println!("end of file (reason {r})"),
                Ok(Event::Shutdown) => {
                    println!("window closed");
                    return;
                }
                Ok(_) => {}
                Err(e) => println!("mpv error: {e}"),
            }
        }
        if first_frame && seek_started.is_none() && step < plan.len() {
            let (wait, target, label) = &plan[step];
            if since_restart.elapsed() >= Duration::from_secs_f64(*wait) {
                let absolute = !target.starts_with(['+', '-']);
                let mode = if absolute { "absolute" } else { "relative" };
                println!("seeking {label} from {} …", pos(&mpv));
                let _ = mpv.command("seek", &[target.as_str(), mode]);
                seek_started = Some((Instant::now(), label.clone()));
                step += 1;
                if step == plan.len() {
                    println!("(seek test done after this one; the window stays open, q quits)");
                }
            }
        }
        if first_frame && last_print.elapsed() >= Duration::from_secs(5) {
            last_print = Instant::now();
            let num = |n| mpv.get_property::<f64>(n).unwrap_or(-1.0);
            println!(
                "    {} / {}  buffer {:.1} s  {}x{}  {}",
                pos(&mpv),
                hms(num("duration").max(0.0) as u64),
                num("demuxer-cache-duration"),
                mpv.get_property::<i64>("width").unwrap_or(0),
                mpv.get_property::<i64>("height").unwrap_or(0),
                mpv.get_property::<String>("hwdec-current")
                    .unwrap_or_default(),
            );
        }
    }
}

fn pos(mpv: &Mpv) -> String {
    hms(mpv.get_property::<f64>("time-pos").unwrap_or(0.0).max(0.0) as u64)
}

fn hms(s: u64) -> String {
    format!("{}:{:02}:{:02}", s / 3600, s % 3600 / 60, s % 60)
}

/// `1:12:40`, `12:40` or `4360` to seconds.
fn parse_time(text: &str) -> u64 {
    text.split(':')
        .try_fold(0u64, |acc, part| part.parse::<u64>().map(|n| acc * 60 + n))
        .unwrap_or_else(|_| panic!("not a time: {text:?}"))
}
