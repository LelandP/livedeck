//! Captions (captions-plan.md, phase 3b): does `PlayerOption::LiveDelay` keep the picture far
//! enough behind the audio tap, and does turning it off and on work without a reload?
//!
//! Resolves a live Twitch channel twice with Streamlink (the picture at 720p60 and the tap's
//! 160p30), plays the first in a headless `MpvPlayer` (the app's own player and its delay, with
//! nothing shown or heard) and taps the second with `AudioTap`. Then:
//!
//! 1. loads with the delay on (`delay` ms, 12500 by default, as the app asks): the load starts that far back;
//! 2. at `off` seconds turns the delay off: the player jumps ahead to near the live edge;
//! 3. at `on` seconds turns it on again: the picture is held until the buffer is long enough.
//!
//! Once a second it prints the picture's place on the stream clock (`PlayerHealth::stream_ms`),
//! its buffer, the newest tap audio and the lead (tap minus picture: the time a caption has to be
//! made in), and at the end the lead's range while the delay was on and settled.
//!
//! ```text
//! cargo run -p htpc-player --features mpv --example caption_delay_probe -- <channel> [seconds] [delay] [off] [on]
//! ```

use std::path::PathBuf;
use std::process::Command;
use std::sync::mpsc::channel;
use std::time::{Duration, Instant};

use htpc_player::{
    AudioTap, LoadOptions, MpvPlayer, PcmChunk, Player, PlayerEvent, PlayerOption, TapEvent,
    ROTATE_AFTER,
};

enum Seen {
    Tap(TapEvent),
    Player(PlayerEvent),
}

fn main() {
    if let Ok(filter) = std::env::var("HTPC_TAP_LOG") {
        tracing_subscriber::fmt().with_env_filter(filter).init();
    }
    let usage = "usage: caption_delay_probe <channel> [seconds] [delay-ms] [off-s] [on-s]";
    let mut args = std::env::args().skip(1);
    let login = args.next().expect(usage);
    let seconds: u64 = args.next().map_or(180, |s| s.parse().expect("seconds"));
    let delay: u32 = args.next().map_or(12_500, |s| s.parse().expect("delay"));
    let off_at: u64 = args.next().map_or(90, |s| s.parse().expect("off"));
    let on_at: u64 = args.next().map_or(120, |s| s.parse().expect("on"));

    let main_url = resolve(&login, "720p60,720p,best");
    let tap_url = resolve(&login, htpc_resolver::TAP_QUALITY);
    let (tx, rx) = channel();
    let to_main = tx.clone();
    let player = MpvPlayer::headless(move |e| {
        let _ = to_main.send(Seen::Player(e));
    })
    .expect("libmpv");
    let _tap = AudioTap::start_rotating(
        &tap_url,
        Box::new(move |e| {
            let _ = tx.send(Seen::Tap(e));
        }),
        ROTATE_AFTER,
    )
    .expect("tap");
    player.set_option(PlayerOption::LiveDelay(Some(delay)));
    let started = Instant::now();
    player.load(&main_url, LoadOptions::default());

    let mut tap_end: Option<f64> = None;
    let mut stream_s: Option<f64> = None;
    let mut buffer_s: Option<f64> = None;
    let mut on = true;
    let mut on_since = Some(0u64);
    let mut leads = Vec::new();
    println!("     t   picture   buffer       tap    lead  delay");
    for second in 1..=seconds {
        let due = started + Duration::from_secs(second);
        while let Ok(seen) = rx.recv_timeout(due.saturating_duration_since(Instant::now())) {
            match seen {
                Seen::Tap(TapEvent::Audio(chunk)) => tap_end = Some(PcmChunk::end_s(&chunk)),
                Seen::Tap(other) => println!("tap: {other:?}"),
                Seen::Player(PlayerEvent::Health(h)) => {
                    stream_s = h.stream_ms.map(|ms| ms as f64 / 1000.0);
                    buffer_s = Some(f64::from(h.buffer_ms) / 1000.0);
                }
                Seen::Player(event) => {
                    println!("{:6.2}  player: {event:?}", started.elapsed().as_secs_f64())
                }
            }
        }
        if second == off_at {
            on = false;
            on_since = None;
            player.set_option(PlayerOption::LiveDelay(None));
            println!("--- delay off");
        }
        if second == on_at {
            on = true;
            on_since = Some(second);
            player.set_option(PlayerOption::LiveDelay(Some(delay)));
            println!("--- delay on");
        }
        let lead = tap_end.zip(stream_s).map(|(t, p)| t - p);
        // Settled: 15 s after the delay came on.
        if on_since.is_some_and(|since| second > since + 15) {
            leads.extend(lead);
        }
        let f = |v: Option<f64>, w: usize| v.map_or_else(|| "-".into(), |v| format!("{v:>w$.2}"));
        println!(
            "{second:>6}  {}  {}  {}  {}  {}",
            f(stream_s, 8),
            f(buffer_s, 7),
            f(tap_end, 8),
            f(lead, 6),
            if on { "on" } else { "off" }
        );
    }
    let lo = leads.iter().copied().fold(f64::INFINITY, f64::min);
    let hi = leads.iter().copied().fold(f64::NEG_INFINITY, f64::max);
    println!(
        "lead with the delay on and settled: {lo:.2} to {hi:.2} s ({} readings)",
        leads.len()
    );
    drop(player);
    // Let the tap's thread close mpv and delete its file.
    std::thread::sleep(Duration::from_millis(500));
}

fn resolve(login: &str, quality: &str) -> String {
    let out = Command::new(streamlink())
        .args([
            "--stream-url",
            &format!("https://twitch.tv/{login}"),
            quality,
        ])
        .output()
        .expect("Streamlink runs (set HTPC_STREAMLINK)");
    let text = String::from_utf8_lossy(&out.stdout).trim().to_owned();
    assert!(
        out.status.success() && text.starts_with("http"),
        "Streamlink could not resolve {login} ({quality}): {text}"
    );
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
