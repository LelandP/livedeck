//! Captions (captions-plan.md, phase 3b): are live captions ready before the picture gets to
//! them? The app's own parts without its window: a headless `MpvPlayer` with the caption delay
//! (`PlayerOption::LiveDelay`), the audio tap and the real whisper engine.
//!
//! Resolves a live Twitch channel twice with Streamlink (the picture at 720p60 and the tap's
//! 160p30), plays the picture headless with the delay, feeds the tap's audio to whisper and, for
//! every caption, prints how late it is: where the picture is when the caption is made, minus the
//! caption's start (above 0 its first words show late; below, it waited that long). Every 10 s it
//! prints the lead (the newest tap audio minus the picture), and at the end a summary.
//!
//! ```text
//! cargo run --release -p livedeck --features mpv,captions --example caption_timing_probe -- <channel> [seconds] [delay-ms, 0 = off]
//! ```
//!
//! The models come from `HTPC_CAPTIONS_MODEL` and `HTPC_CAPTIONS_VAD_MODEL`.

use std::path::PathBuf;
use std::process::Command;
use std::sync::mpsc::channel;
use std::time::{Duration, Instant};

use htpc_captions::{Transcriber, WhisperConfig, WhisperTranscriber};
use htpc_player::{
    AudioTap, LoadOptions, MpvPlayer, Player, PlayerEvent, PlayerOption, TapEvent, ROTATE_AFTER,
};

enum Seen {
    Tap(TapEvent),
    Player(PlayerEvent),
}

fn main() {
    if let Ok(filter) = std::env::var("RUST_LOG") {
        tracing_subscriber::fmt().with_env_filter(filter).init();
    }
    let usage = "usage: caption_timing_probe <channel> [seconds] [delay-ms]";
    let mut args = std::env::args().skip(1);
    let login = args.next().expect(usage);
    let seconds: u64 = args.next().map_or(300, |s| s.parse().expect("seconds"));
    let delay: u32 = args.next().map_or(12_500, |s| s.parse().expect("delay"));
    let model = std::env::var("HTPC_CAPTIONS_MODEL").expect("set HTPC_CAPTIONS_MODEL");
    let vad = std::env::var("HTPC_CAPTIONS_VAD_MODEL").expect("set HTPC_CAPTIONS_VAD_MODEL");

    let mut engine =
        WhisperTranscriber::new(WhisperConfig::new(model, vad)).expect("the models load");
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
    if delay > 0 {
        player.set_option(PlayerOption::LiveDelay(Some(delay)));
    }
    let started = Instant::now();
    player.load(&main_url, LoadOptions::default());

    // The picture: its last report and when it came, and whether it is held.
    let mut picture: Option<(f64, Instant)> = None;
    let mut held = false;
    let mut tap_end: Option<f64> = None;
    let mut lates = Vec::new();
    let mut words_late = 0usize;
    let mut words = 0usize;
    let mut leads = Vec::new();
    let mut next_lead = started + Duration::from_secs(10);
    println!("delay {delay} ms");
    while started.elapsed() < Duration::from_secs(seconds) {
        while let Ok(seen) = rx.recv_timeout(Duration::from_millis(50)) {
            match seen {
                Seen::Tap(TapEvent::Audio(chunk)) => {
                    tap_end = Some(chunk.end_s());
                    engine.push(chunk.start_s, &chunk.samples);
                }
                Seen::Tap(other) => println!("tap: {other:?}"),
                Seen::Player(PlayerEvent::Health(h)) => {
                    held = false;
                    picture = h.stream_ms.map(|ms| (ms as f64 / 1000.0, Instant::now()));
                }
                Seen::Player(PlayerEvent::Held(h)) => {
                    // The picture stands where it was: keep that place, not the time since.
                    if h {
                        picture = picture_now(picture, false).map(|p| (p, Instant::now()));
                    } else if let Some((p, _)) = picture {
                        picture = Some((p, Instant::now()));
                    }
                    held = h;
                    println!("{:7.1}  held {h}", started.elapsed().as_secs_f64());
                }
                Seen::Player(other) => {
                    println!("{:7.1}  player: {other:?}", started.elapsed().as_secs_f64())
                }
            }
        }
        let now = picture_now(picture, held);
        for c in engine.poll() {
            let Some(now) = now else { continue };
            let late = now - c.start_s;
            let late_words = c.words.iter().filter(|w| w.start_s < now).count();
            words += c.words.len();
            words_late += late_words;
            // Skip the first 20 s: the delay is still being built, the engine warming up.
            if started.elapsed() > Duration::from_secs(20) {
                lates.push(late);
            }
            println!(
                "{:7.1}  caption {:.2}..{:.2} late {late:+.2} s, {late_words}/{} words late: {}",
                started.elapsed().as_secs_f64(),
                c.start_s,
                c.end_s,
                c.words.len(),
                c.text
            );
        }
        if Instant::now() >= next_lead {
            next_lead += Duration::from_secs(10);
            if let Some(lead) = tap_end.zip(now).map(|(t, p)| t - p) {
                if started.elapsed() > Duration::from_secs(20) {
                    leads.push(lead);
                }
                println!(
                    "{:7.1}  lead {lead:.2} s, engine queue {:.1} s, dropped {:.1} s",
                    started.elapsed().as_secs_f64(),
                    engine.queued_s(),
                    engine.dropped_s()
                );
            }
        }
    }
    let range = |v: &[f64]| {
        let lo = v.iter().copied().fold(f64::INFINITY, f64::min);
        let hi = v.iter().copied().fold(f64::NEG_INFINITY, f64::max);
        format!("{lo:+.2} to {hi:+.2} s")
    };
    let mut sorted = lates.clone();
    sorted.sort_by(f64::total_cmp);
    let median = sorted.get(sorted.len() / 2).copied().unwrap_or(f64::NAN);
    println!(
        "after 20 s: {} captions, lateness {} (median {median:+.2} s), {} late (> 0); \
         {words_late}/{words} words already due when made; lead {}",
        lates.len(),
        range(&lates),
        lates.iter().filter(|l| **l > 0.0).count(),
        range(&leads)
    );
    drop(player);
    std::thread::sleep(Duration::from_millis(500));
}

/// Where the picture is now: its last report moved on by the time since, unless it is held.
fn picture_now(picture: Option<(f64, Instant)>, held: bool) -> Option<f64> {
    picture.map(|(p, at)| {
        if held {
            p
        } else {
            p + at.elapsed().as_secs_f64()
        }
    })
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
