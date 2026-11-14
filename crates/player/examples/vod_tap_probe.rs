//! Captions (captions-plan.md, phase 5): does the audio tap follow a video or clip, stay on the
//! main player's clock, and read no more than the lookahead ahead of the playhead?
//!
//! Resolves a Twitch video (digits) or clip (a slug) twice with Streamlink: a quality the player
//! would show and the tap's (`TAP_QUALITY`, 160p30). Plays the first in a headless libmpv set up
//! like the app's main player (rebased timestamps, nothing shown or heard) from `start`, taps the
//! second with `AudioTap` (`TapOptions`: the start and the pace), and prints once a second:
//!
//! - `main`: the player's place on the stream clock (`time-pos` + `demuxer-start-time`, as
//!   `PlayerHealth::stream_ms`), which is also given to the tap as the playhead.
//! - `tap`: the end of the newest tap chunk, and `ahead`: tap minus main. It should stay at or
//!   under the lookahead (30 s), and above 25 s once the tap has caught up.
//!
//! `seeks` are `at:to` pairs (seconds since the start, then a position in the video): at that
//! moment the main player seeks there and a new tap starts at `to`, as the app does. For each, and
//! for the first start, it prints how long the first audio took and how far its clock was from
//! `to` + the main player's start time (`first-clock`: a packet or two is normal). `speed` plays
//! the main player faster (the playhead moves faster, the tap must keep ahead). `HTPC_TAP_LOG=
//! htpc_player=debug` shows the tap's pacing.
//!
//! ```text
//! cargo run -p htpc-player --features mpv --example vod_tap_probe -- <id|slug> [seconds] [start] [speed] [at:to,...] [quality]
//! ```

use std::path::PathBuf;
use std::process::Command;
use std::sync::mpsc::{channel, Receiver};
use std::time::{Duration, Instant};

use htpc_player::{AudioTap, PcmChunk, TapEvent, TapOptions, TapPace, ROTATE_AFTER};
use libmpv2::Mpv;

fn main() {
    if let Ok(filter) = std::env::var("HTPC_TAP_LOG") {
        tracing_subscriber::fmt().with_env_filter(filter).init();
    }
    let usage = "usage: vod_tap_probe <id|slug> [seconds] [start] [speed] [at:to,...] [quality]";
    let mut args = std::env::args().skip(1);
    let what = args.next().expect(usage);
    let seconds: u64 = args.next().map_or(60, |s| s.parse().expect("seconds"));
    let start: f64 = args.next().map_or(0.0, |s| s.parse().expect("start"));
    let speed: f64 = args.next().map_or(1.0, |s| s.parse().expect("speed"));
    let seeks: Vec<(u64, f64)> = args
        .next()
        .filter(|s| s != "-")
        .map(|s| {
            s.split(',')
                .map(|p| {
                    let (at, to) = p.split_once(':').expect("at:to");
                    (at.parse().expect("at"), to.parse().expect("to"))
                })
                .collect()
        })
        .unwrap_or_default();
    let clip = !what.chars().all(|c| c.is_ascii_digit());
    let page = if clip {
        format!("https://clips.twitch.tv/{what}")
    } else {
        format!("https://www.twitch.tv/videos/{what}")
    };
    let quality = args.next().unwrap_or_else(|| {
        if clip {
            "720p0,best".into()
        } else {
            "720p30,480p30,best".into()
        }
    });

    let main_url = resolve(&page, &quality);
    let tap_url = resolve(&page, htpc_resolver::TAP_QUALITY);
    let main = Mpv::with_initializer(|init| {
        init.set_option("vo", "null")?;
        init.set_option("ao", "null")?;
        init.set_option("start", format!("{start:.3}").as_str())?;
        init.set_option("speed", format!("{speed}").as_str())?;
        init.set_option("ytdl", false)?;
        Ok(())
    })
    .expect("libmpv");
    main.command("loadfile", &[main_url.as_str(), "replace"])
        .expect("loadfile");

    let pace = TapPace::default();
    let started = Instant::now();
    let (mut rx, mut tap) = new_tap(&tap_url, start, &pace);
    let mut tap_started = Instant::now();
    let mut expect_first: Option<f64> = Some(start);
    let mut newest: Option<PcmChunk> = None;
    let (mut chunks, mut gaps, mut aheads) = (0usize, Vec::new(), Vec::new());
    let mut pending: Vec<(u64, f64)> = seeks;
    pending.reverse();
    println!("    t      main       tap   ahead  chunks");
    'run: for second in 1..=seconds {
        let due = started + Duration::from_secs(second);
        while let Ok(event) = rx.recv_timeout(due.saturating_duration_since(Instant::now())) {
            match event {
                TapEvent::Audio(chunk) => {
                    if let Some(prev) = &newest {
                        let gap = chunk.start_s - prev.end_s();
                        if gap.abs() > 0.001 {
                            gaps.push((prev.end_s(), gap));
                        }
                    }
                    if let Some(to) = expect_first.take() {
                        let start_time = main
                            .get_property::<f64>("demuxer-start-time")
                            .unwrap_or(0.0);
                        println!(
                            "first audio {:.2} s after the tap started: clock {:.2} (to {to:.2} + start time {start_time:.2} = {:.2}), first-clock {:+.2} s",
                            tap_started.elapsed().as_secs_f64(),
                            chunk.start_s,
                            to + start_time,
                            chunk.start_s - (to + start_time),
                        );
                    }
                    chunks += 1;
                    newest = Some(chunk);
                }
                other => {
                    println!("tap: {other:?}");
                    break 'run;
                }
            }
        }
        while main.wait_event(0.0).is_some() {}
        let start_time = main
            .get_property::<f64>("demuxer-start-time")
            .unwrap_or(0.0);
        let clock = main
            .get_property::<f64>("time-pos")
            .ok()
            .map(|p| p + start_time);
        if let Some(c) = clock {
            pace.set_playhead_ms((c * 1000.0) as u64);
        }
        let tap_end = newest.as_ref().map(PcmChunk::end_s);
        let ahead = tap_end.zip(clock).map(|(t, c)| t - c);
        if second > 15 && pending.is_empty() {
            aheads.extend(ahead);
        }
        let f = |v: Option<f64>, w: usize| v.map_or_else(|| "-".into(), |v| format!("{v:>w$.2}"));
        println!(
            "{second:>5}  {}  {}  {}  {chunks:>6}",
            f(clock, 9),
            f(tap_end, 9),
            f(ahead, 6),
        );
        if pending.last().is_some_and(|(at, _)| *at <= second) {
            let (_, to) = pending.pop().expect("checked");
            println!("-- seek to {to:.2}: main seeks, a new tap starts there");
            main.command("seek", &[format!("{to:.3}").as_str(), "absolute"])
                .expect("seek");
            drop(tap);
            pace.clear();
            (rx, tap) = new_tap(&tap_url, to, &pace);
            tap_started = Instant::now();
            expect_first = Some(to);
            newest = None;
        }
    }
    drop(tap);
    if !aheads.is_empty() {
        let lo = aheads.iter().copied().fold(f64::INFINITY, f64::min);
        let hi = aheads.iter().copied().fold(f64::NEG_INFINITY, f64::max);
        println!(
            "after 15 s with no seek due, the tap was {lo:.2} to {hi:.2} s ahead of the picture"
        );
    }
    if gaps.is_empty() {
        println!("{chunks} chunks, all joined up");
    } else {
        println!("{chunks} chunks, gaps (at, size): {gaps:?}");
    }
    // Let the tap's thread close mpv and delete its file.
    std::thread::sleep(Duration::from_millis(500));
}

fn new_tap(url: &str, from_s: f64, pace: &TapPace) -> (Receiver<TapEvent>, AudioTap) {
    let (tx, rx) = channel();
    // `HTPC_ROTATE` (seconds of audio per file) tries the rotation, which a video resumes at the
    // newest audio instead of at its start.
    let rotate = std::env::var("HTPC_ROTATE").ok().map_or(ROTATE_AFTER, |s| {
        Duration::from_secs(s.parse().expect("rotate"))
    });
    let tap = AudioTap::start_rotating_with(
        url,
        TapOptions {
            from_ms: Some((from_s * 1000.0) as u64),
            pace: Some(pace.clone()),
        },
        Box::new(move |e| {
            let _ = tx.send(e);
        }),
        rotate,
    )
    .expect("tap");
    (rx, tap)
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
