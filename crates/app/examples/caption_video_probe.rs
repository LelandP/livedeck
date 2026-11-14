//! Captions (captions-plan.md, phase 5): are a video's captions ready before the picture gets to
//! them, and how soon after a seek? The app's own parts without its window: a headless
//! `MpvPlayer` playing a Twitch video or clip, the audio tap (`TapOptions`: its start and its
//! pace) and the real whisper engine with the backlog a video gets.
//!
//! Resolves the video twice with Streamlink (the picture at 720p30 and the tap's 160p30), plays
//! the picture from `start`, feeds the tap's audio to whisper and, for every caption, prints how
//! late it is: where the picture is when the caption is made, minus the caption's start (above 0
//! its first words show late; below, it waited that long). `seeks` are `at:to` pairs (seconds
//! since the start, then a position in the video): the picture seeks there and a new tap starts,
//! the engine is reset, as the app does. Every 10 s it prints how far ahead of the picture the tap
//! is and the engine's queue; at the end a summary, per place (the start and each seek).
//!
//! ```text
//! cargo run --release -p livedeck --features mpv,captions --example caption_video_probe -- <id|slug> [seconds] [start] [speed] [at:to,...]
//! ```
//!
//! The models come from `HTPC_CAPTIONS_MODEL` and `HTPC_CAPTIONS_VAD_MODEL`.

use std::path::PathBuf;
use std::process::Command;
use std::sync::mpsc::{channel, Sender};
use std::time::{Duration, Instant};

use htpc_captions::{Transcriber, WhisperConfig, WhisperTranscriber};
use htpc_player::{
    AudioTap, LoadOptions, MpvPlayer, Player, PlayerEvent, PlayerOption, TapEvent, TapOptions,
    TapPace,
};

enum Seen {
    Tap(u32, TapEvent),
    Player(PlayerEvent),
}

/// One place in the video the picture played from, and what came of it.
struct Place {
    /// Where it began, in the player's own terms (seconds), and when (since the start).
    to: f64,
    at: Instant,
    /// Lateness of each caption made here.
    lates: Vec<f64>,
    /// When its first caption was made, and how late that was.
    first: Option<(f64, f64)>,
}

fn main() {
    if let Ok(filter) = std::env::var("RUST_LOG") {
        tracing_subscriber::fmt().with_env_filter(filter).init();
    }
    let usage = "usage: caption_video_probe <id|slug> [seconds] [start] [speed] [at:to,...]";
    let mut args = std::env::args().skip(1);
    let what = args.next().expect(usage);
    let seconds: u64 = args.next().map_or(120, |s| s.parse().expect("seconds"));
    let start: f64 = args.next().map_or(0.0, |s| s.parse().expect("start"));
    let speed: f64 = args.next().map_or(1.0, |s| s.parse().expect("speed"));
    let mut seeks: Vec<(u64, f64)> = args
        .next()
        .map(|s| {
            s.split(',')
                .map(|p| {
                    let (at, to) = p.split_once(':').expect("at:to");
                    (at.parse().expect("at"), to.parse().expect("to"))
                })
                .collect()
        })
        .unwrap_or_default();
    seeks.reverse();
    let model = std::env::var("HTPC_CAPTIONS_MODEL").expect("set HTPC_CAPTIONS_MODEL");
    let vad = std::env::var("HTPC_CAPTIONS_VAD_MODEL").expect("set HTPC_CAPTIONS_VAD_MODEL");

    let clip = !what.chars().all(|c| c.is_ascii_digit());
    let page = if clip {
        format!("https://clips.twitch.tv/{what}")
    } else {
        format!("https://www.twitch.tv/videos/{what}")
    };
    let mut engine =
        WhisperTranscriber::new(WhisperConfig::new(model, vad)).expect("the models load");
    // What the app gives a video's audio (`VIDEO_BACKLOG_S` in `captions.rs`).
    engine.set_backlog_s(45.0);
    let main_url = resolve(
        &page,
        if clip {
            "720p0,best"
        } else {
            "720p30,480p30,best"
        },
    );
    let tap_url = resolve(&page, htpc_resolver::TAP_QUALITY);
    let (tx, rx) = channel();
    let to_main = tx.clone();
    let player = MpvPlayer::headless(move |e| {
        let _ = to_main.send(Seen::Player(e));
    })
    .expect("libmpv");
    player.set_option(PlayerOption::Speed((speed * 1000.0) as u32));
    let started = Instant::now();
    player.load(&main_url, LoadOptions::video((start * 1000.0) as u64));

    let pace = TapPace::default();
    let mut generation = 0u32;
    let mut tap = Some(new_tap(&tap_url, start, &pace, &tx, generation));
    let mut places = vec![Place {
        to: start,
        at: started,
        lates: Vec::new(),
        first: None,
    }];
    // The picture's last report (its clock, seconds) and when it came.
    let mut picture: Option<(f64, Instant)> = None;
    let mut tap_end: Option<f64> = None;
    let mut next_lead = started + Duration::from_secs(10);
    let mut aheads = Vec::new();
    println!("start {start} s, speed {speed}");
    while started.elapsed() < Duration::from_secs(seconds) {
        while let Ok(seen) = rx.recv_timeout(Duration::from_millis(20)) {
            match seen {
                Seen::Tap(g, _) if g != generation => {}
                Seen::Tap(_, TapEvent::Audio(chunk)) => {
                    if tap_end.is_none() {
                        engine.reset();
                    }
                    tap_end = Some(chunk.end_s());
                    engine.push(chunk.start_s, &chunk.samples);
                }
                Seen::Tap(_, other) => println!("tap: {other:?}"),
                Seen::Player(PlayerEvent::Health(h)) => {
                    picture = h.stream_ms.map(|ms| (ms as f64 / 1000.0, Instant::now()));
                }
                Seen::Player(other) => {
                    println!("{:7.1}  player: {other:?}", started.elapsed().as_secs_f64());
                }
            }
        }
        let now = picture.map(|(p, at)| p + at.elapsed().as_secs_f64() * speed);
        // The reducer's clock, as the app hands it to the tap's pace.
        match now {
            Some(ms) => pace.set_playhead_ms((ms * 1000.0) as u64),
            None => pace.clear(),
        }
        // And the engine skips what the picture has passed (`PLAYHEAD_SLACK_MS` in `captions.rs`),
        // unless `NO_SKIP` is set (to compare).
        if std::env::var_os("NO_SKIP").is_none() {
            engine.set_playhead_s(now.map_or(f64::NEG_INFINITY, |p| p - 1.0));
        }
        for c in engine.poll() {
            let Some(now) = now else { continue };
            let late = now - c.start_s;
            let place = places.last_mut().expect("a place");
            place.lates.push(late);
            if place.first.is_none() {
                place.first = Some((place.at.elapsed().as_secs_f64(), late));
            }
            let late_words = c.words.iter().filter(|w| w.start_s < now).count();
            println!(
                "{:7.1}  caption {:.2}..{:.2} late {late:+.2} s, {late_words}/{} words late: {}",
                started.elapsed().as_secs_f64(),
                c.start_s,
                c.end_s,
                c.words.len(),
                c.text
            );
        }
        if seeks
            .last()
            .is_some_and(|(at, _)| started.elapsed() >= Duration::from_secs(*at))
        {
            let (_, to) = seeks.pop().expect("checked");
            println!(
                "{:7.1}  -- seek to {to:.1} s: the picture jumps, the tap starts there, the engine resets",
                started.elapsed().as_secs_f64()
            );
            player.seek((to * 1000.0) as u64);
            drop(tap.take());
            generation += 1;
            // What the reducer does: no clock until the first report that has landed.
            picture = None;
            pace.clear();
            engine.reset();
            tap_end = None;
            tap = Some(new_tap(&tap_url, to, &pace, &tx, generation));
            places.push(Place {
                to,
                at: Instant::now(),
                lates: Vec::new(),
                first: None,
            });
        }
        if Instant::now() >= next_lead {
            next_lead += Duration::from_secs(10);
            if let Some(ahead) = tap_end.zip(now).map(|(t, p)| t - p) {
                if started.elapsed() > Duration::from_secs(20) {
                    aheads.push(ahead);
                }
                println!(
                    "{:7.1}  tap {ahead:.1} s ahead of the picture, engine queue {:.1} s, dropped {:.1} s",
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
    println!("tap ahead of the picture, after 20 s: {}", range(&aheads));
    for p in &places {
        let mut sorted = p.lates.clone();
        sorted.sort_by(f64::total_cmp);
        let median = sorted.get(sorted.len() / 2).copied().unwrap_or(f64::NAN);
        println!(
            "from {:.0} s: {} captions, lateness {} (median {median:+.2} s), {} late (> 0); first caption {}",
            p.to,
            p.lates.len(),
            if p.lates.is_empty() {
                "none".into()
            } else {
                range(&p.lates)
            },
            p.lates.iter().filter(|l| **l > 0.0).count(),
            p.first.map_or("none".into(), |(after, late)| format!(
                "{after:.1} s after, late {late:+.2} s"
            )),
        );
    }
    drop(tap);
    drop(player);
    std::thread::sleep(Duration::from_millis(500));
}

fn new_tap(url: &str, from_s: f64, pace: &TapPace, tx: &Sender<Seen>, generation: u32) -> AudioTap {
    let tx = tx.clone();
    AudioTap::start_with(
        url,
        TapOptions {
            from_ms: Some((from_s * 1000.0) as u64),
            pace: Some(pace.clone()),
        },
        Box::new(move |e| {
            let _ = tx.send(Seen::Tap(generation, e));
        }),
    )
    .expect("tap")
}

fn resolve(page: &str, quality: &str) -> String {
    let out = Command::new(streamlink())
        .args(["--stream-url", page, quality])
        .output()
        .expect("Streamlink runs (set HTPC_STREAMLINK)");
    let text = String::from_utf8_lossy(&out.stdout).trim().to_owned();
    assert!(
        out.status.success() && text.starts_with("http"),
        "Streamlink could not resolve {page} ({quality}): {text}"
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
