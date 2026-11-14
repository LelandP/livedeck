//! Plan 14: does a quality change land at the old picture's moment, and never ahead of it?
//!
//! Resolves a live Twitch channel at a few qualities with Streamlink, plays the first in a
//! headless `MpvPlayer` (the app's own player, nothing shown or heard), and changes the quality
//! `swaps` times, every `every` seconds, as the app does: a second headless `MpvPlayer` takes over
//! from the first (`Player::take_over`), and the old one stops at the new one's `Playing`. At each
//! swap it prints the jump, the new picture minus the old one on the stream clock
//! (`PlayerHealth::stream_ms`), at the swap and half a second later: negative is a short repeat,
//! positive a skip forward. Then how long the change took, and the new one's buffer. The summary
//! takes the jump at the swap (half a second later where that is unknown, as in `plain`).
//!
//! `plain` swaps as before plan 14, at the new one's first frame near the live edge, to compare.
//! `normal` loads with the normal latency, and `delay` (ms) adds the captions' live delay.
//!
//! `clock` instead checks that two renditions share a clock, which taking over relies on: it taps
//! the audio of both (`AudioTap`, on each one's own clock) for 25 s and finds the offset at which
//! their loudness lines up best. On one clock it is 0 give or take a few ms. A channel written
//! `kick:<slug>` is looked up on Kick.
//!
//! ```text
//! cargo run -p htpc-player --features mpv --example quality_swap_probe -- <channel> [swaps] [every-s] [sync|plain] [low|normal] [delay-ms]
//! cargo run -p htpc-player --features mpv --example quality_swap_probe -- clock <channel> [quality] [quality]
//! ```

use std::path::PathBuf;
use std::process::Command;
use std::sync::mpsc::{channel, Receiver};
use std::time::{Duration, Instant};

use htpc_player::{Latency, LoadOptions, MpvPlayer, Player, PlayerEvent, PlayerOption};

/// The qualities it goes through, in turn (Streamlink fallback lists, as the app asks).
const QUALITIES: [(&str, &str); 5] = [
    ("720p60", "720p60,720p,720p30,best"),
    ("160p", "160p30,160p,worst"),
    ("480p", "480p30,480p,480p60,worst"),
    ("1080p60", "1080p60,1080p,best"),
    ("360p", "360p30,360p,360p60,worst"),
];

fn main() {
    if let Ok(filter) = std::env::var("HTPC_SWAP_LOG") {
        tracing_subscriber::fmt().with_env_filter(filter).init();
    }
    let usage =
        "usage: quality_swap_probe <channel> [swaps] [every-s] [sync|plain] [low|normal] [delay-ms]";
    let mut args = std::env::args().skip(1);
    let login = args.next().expect(usage);
    if login == "clock" {
        let login = args
            .next()
            .expect("usage: quality_swap_probe clock <channel> [q] [q]");
        let a = args.next().unwrap_or_else(|| "160p30,160p,worst".into());
        let b = args.next().unwrap_or_else(|| "1080p60,1080p,best".into());
        clock_check(&login, &a, &b);
        return;
    }
    let swaps: usize = args.next().map_or(8, |s| s.parse().expect("swaps"));
    let every = Duration::from_secs(args.next().map_or(12, |s| s.parse().expect("every")));
    let sync = args.next().is_none_or(|m| m != "plain");
    let latency = match args.next().as_deref() {
        Some("normal") => Latency::Normal,
        _ => Latency::Low,
    };
    let delay: Option<u32> = args.next().map(|s| s.parse().expect("delay"));

    let urls: Vec<(&str, String)> = QUALITIES
        .iter()
        .map(|(name, list)| (*name, resolve(&login, list)))
        .collect();
    let (tx, rx) = channel();
    let players: Vec<MpvPlayer> = (0..2)
        .map(|i| {
            let tx = tx.clone();
            MpvPlayer::headless(move |e| {
                let _ = tx.send((i, e));
            })
            .expect("libmpv")
        })
        .collect();
    let clocks: Vec<_> = players
        .iter()
        .map(|p| p.clock().expect("an mpv player has a clock"))
        .collect();
    // Both publish all along, so the jump can be read at any moment.
    let _watches: Vec<_> = clocks.iter().map(|c| c.watch()).collect();
    for p in &players {
        p.set_option(PlayerOption::LiveDelay(delay));
    }
    let options = LoadOptions {
        latency,
        start_ms: None,
    };
    println!(
        "{login}: {} swaps every {} s, {}, {latency:?} latency, delay {delay:?}",
        swaps,
        every.as_secs(),
        if sync {
            "taking over (plan 14)"
        } else {
            "plain"
        },
    );

    let mut active = 0;
    players[0].load(&urls[0].1, options);
    wait_for_playing(&rx, 0, Duration::from_secs(30)).expect("the first stream plays");
    settle(&rx, every);

    let mut jumps = Vec::new();
    println!("swap  to        took   jump at swap   +0.5 s   buffer");
    for k in 1..=swaps {
        let spare = 1 - active;
        let (name, url) = &urls[k % urls.len()];
        let started = Instant::now();
        if sync {
            players[spare].take_over(clocks[active].clone());
        }
        players[spare].load(url, options);
        if wait_for_playing(&rx, spare, Duration::from_secs(30)).is_none() {
            println!("{k:>4}  {name:<8}  the new quality did not start");
            players[spare].stop();
            continue;
        }
        let took = started.elapsed();
        let jump = diff(clocks[spare].now_ms(), clocks[active].now_ms());
        std::thread::sleep(Duration::from_millis(500));
        let later = diff(clocks[spare].now_ms(), clocks[active].now_ms());
        players[active].stop();
        active = spare;
        let buffer = clocks[active].read().map(|r| r.ahead_ms);
        let ms = |v: Option<i64>| v.map_or_else(|| "-".into(), |v| format!("{v:+} ms"));
        println!(
            "{k:>4}  {name:<8}  {:>4.1} s  {:>12}  {:>8}  {:>6}",
            took.as_secs_f64(),
            ms(jump),
            ms(later),
            buffer.map_or_else(|| "-".into(), |b| format!("{:.1} s", f64::from(b) / 1000.0)),
        );
        // A plain swap publishes its first reading only after its `Playing`.
        jumps.extend(jump.or(later));
        settle(&rx, every);
    }

    if jumps.is_empty() {
        println!("no swap was measured");
        return;
    }
    let lo = jumps.iter().min().copied().unwrap_or(0);
    let hi = jumps.iter().max().copied().unwrap_or(0);
    let forward = jumps.iter().filter(|j| **j > 0).count();
    println!(
        "{} swaps: jump {lo:+} to {hi:+} ms; {forward} forward; never forward and under 0.5 s back: {}",
        jumps.len(),
        forward == 0 && lo > -500,
    );
}

/// `a` − `b` in milliseconds, if both are known.
fn diff(a: Option<u64>, b: Option<u64>) -> Option<i64> {
    let (a, b) = (i64::try_from(a?).ok()?, i64::try_from(b?).ok()?);
    Some(a - b)
}

/// Waits for player `which` to report `Playing`, printing what else happens.
fn wait_for_playing(
    rx: &Receiver<(usize, PlayerEvent)>,
    which: usize,
    within: Duration,
) -> Option<()> {
    let until = Instant::now() + within;
    while let Ok((i, event)) = rx.recv_timeout(until.saturating_duration_since(Instant::now())) {
        match event {
            PlayerEvent::Playing if i == which => return Some(()),
            PlayerEvent::Failed(why) => println!("player {i} failed: {why}"),
            PlayerEvent::Ended => println!("player {i} ended"),
            _ => {}
        }
    }
    None
}

/// Lets `for_` pass, printing failures meanwhile.
fn settle(rx: &Receiver<(usize, PlayerEvent)>, for_: Duration) {
    let until = Instant::now() + for_;
    while let Ok((i, event)) = rx.recv_timeout(until.saturating_duration_since(Instant::now())) {
        match event {
            PlayerEvent::Failed(why) => println!("player {i} failed: {why}"),
            PlayerEvent::Ended => println!("player {i} ended"),
            _ => {}
        }
    }
}

/// Taps renditions `qa` and `qb` of `login` for 25 s and prints the clock offset between them
/// (b − a) at which their loudness, in 1 ms steps, correlates best.
fn clock_check(login: &str, qa: &str, qb: &str) {
    use htpc_player::{AudioTap, TapEvent, ROTATE_AFTER, TAP_RATE};
    let urls = [resolve(login, qa), resolve(login, qb)];
    let (tx, rx) = channel();
    let _taps: Vec<AudioTap> = urls
        .iter()
        .enumerate()
        .map(|(i, url)| {
            let tx = tx.clone();
            AudioTap::start_rotating(
                url,
                Box::new(move |e| {
                    let _ = tx.send((i, e));
                }),
                ROTATE_AFTER,
            )
            .expect("tap")
        })
        .collect();
    // Loudness per millisecond on each tap's clock.
    let per_ms = (TAP_RATE / 1000) as usize;
    let mut env: [Vec<(i64, f32)>; 2] = [Vec::new(), Vec::new()];
    let until = Instant::now() + Duration::from_secs(25);
    while let Ok((i, event)) = rx.recv_timeout(until.saturating_duration_since(Instant::now())) {
        if let TapEvent::Audio(chunk) = event {
            let start_ms = (chunk.start_s * 1000.0).round() as i64;
            for (k, block) in chunk.samples.chunks(per_ms).enumerate() {
                let loud = block.iter().map(|s| s.abs()).sum::<f32>() / block.len() as f32;
                env[i].push((start_ms + k as i64, loud));
            }
        } else {
            println!("tap {i}: {event:?}");
        }
    }
    // Dense, from each tap's first millisecond (NaN where it has none).
    let dense = |v: &[(i64, f32)]| {
        let first = v.first().map_or(0, |p| p.0);
        let last = v.last().map_or(0, |p| p.0);
        let mut out = vec![f32::NAN; usize::try_from(last - first + 1).unwrap_or(0)];
        for (t, loud) in v {
            out[usize::try_from(t - first).unwrap_or(0)] = *loud;
        }
        (first, out)
    };
    let (a, b) = (dense(&env[0]), dense(&env[1]));
    let at = |(first, v): &(i64, Vec<f32>), t: i64| {
        usize::try_from(t - first)
            .ok()
            .and_then(|i| v.get(i).copied())
            .filter(|x| !x.is_nan())
    };
    let span = |v: &[(i64, f32)]| (v.first().map(|p| p.0), v.last().map(|p| p.0));
    let (Some(a0), Some(a1)) = span(&env[0]) else {
        println!("no audio from {qa}");
        return;
    };
    let (Some(b0), Some(b1)) = span(&env[1]) else {
        println!("no audio from {qb}");
        return;
    };
    let s = |ms: i64| ms as f64 / 1000.0;
    println!(
        "{qa}: {:.2} to {:.2} s; {qb}: {:.2} to {:.2} s",
        s(a0),
        s(a1),
        s(b0),
        s(b1)
    );
    // An 8 s window of b in the middle of where both have audio, against a shifted by each lag.
    const LAG: i64 = 3_000;
    let mid = (a0.max(b0) + a1.min(b1)) / 2;
    let window: Vec<i64> = (mid - 4_000..mid + 4_000).collect();
    let pearson = |lag: i64| -> Option<f64> {
        let pairs: Vec<(f64, f64)> = window
            .iter()
            .filter_map(|t| Some((f64::from(at(&a, t - lag)?), f64::from(at(&b, *t)?))))
            .collect();
        if pairs.len() < 4_000 {
            return None;
        }
        let n = pairs.len() as f64;
        let (ma, mb) = pairs
            .iter()
            .fold((0.0, 0.0), |(x, y), (p, q)| (x + p, y + q));
        let (ma, mb) = (ma / n, mb / n);
        let (mut sab, mut saa, mut sbb) = (0.0, 0.0, 0.0);
        for (p, q) in &pairs {
            sab += (p - ma) * (q - mb);
            saa += (p - ma) * (p - ma);
            sbb += (q - mb) * (q - mb);
        }
        (saa > 0.0 && sbb > 0.0).then(|| sab / (saa * sbb).sqrt())
    };
    let best = (-LAG..=LAG)
        .filter_map(|lag| pearson(lag).map(|r| (lag, r)))
        .max_by(|x, y| x.1.total_cmp(&y.1));
    match best {
        Some((lag, r)) => println!(
            "best match: {qb} is {lag:+} ms against {qa} on their clocks (correlation {r:.3}); one clock: {}",
            lag.abs() <= 40 && r > 0.5
        ),
        None => println!("the two taps do not overlap enough to compare"),
    }
}

fn resolve(login: &str, quality: &str) -> String {
    let page = match login.strip_prefix("kick:") {
        Some(slug) => format!("https://kick.com/{slug}"),
        None => format!("https://twitch.tv/{login}"),
    };
    let out = Command::new(streamlink())
        .args(["--stream-url", &page, quality])
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
