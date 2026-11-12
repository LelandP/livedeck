//! Checks the player's video controls on the real `MpvPlayer`, without a window: a load from a
//! start position, health with the position and length, absolute seeks forward and back, a speed
//! change, a pause, and a live load afterwards (which must clear the start position and speed).
//!
//! ```text
//! cargo run -p htpc-player --features mpv --example video_probe -- [url] [start_s] [watch_s]
//! ```
//!
//! `url` defaults to a 10-minute `lavfi` test picture with a tone, written to a file first as
//! `--mock` does. A Twitch video's playlist (from `streamlink --stream-url`) works too. With
//! `watch_s`, the probe only plays from `start_s` for that long, printing health and the audio's
//! numbers (to watch across a muted part). No window is drawn, so mpv decodes without showing
//! anything; the numbers are what counts.

use std::sync::mpsc::{channel, Receiver};
use std::time::{Duration, Instant};

use htpc_player::{LoadOptions, MpvPlayer, Player, PlayerEvent, PlayerOption};

/// A generated video with a known length: 600 s of picture and sound.
const MOCK: &str = "av://lavfi:testsrc2=size=384x216:rate=15:duration=600,format=yuv420p[out0];\
    sine=frequency=440:sample_rate=8000:duration=600,volume=0.12[out1]";

fn main() {
    let mut args = std::env::args().skip(1);
    let url = args.next().unwrap_or_else(|| {
        // A plain `lavfi` source cannot seek backwards, so it is written to a file first, as
        // `--mock` does.
        let path = std::env::temp_dir().join("video_probe_mock.mkv");
        let t = Instant::now();
        htpc_player::encode_file(MOCK, &path).expect("the mock video encodes");
        let size = std::fs::metadata(&path).map_or(0, |m| m.len());
        println!(
            "encoded {} ({} kB) in {} ms",
            path.display(),
            size / 1024,
            t.elapsed().as_millis()
        );
        path.to_string_lossy().into_owned()
    });
    let start_s: u64 = args
        .next()
        .map_or(30, |s| s.parse().expect("start in seconds"));
    let watch_s: Option<u64> = args.next().map(|s| s.parse().expect("watch in seconds"));

    let (tx, rx) = channel();
    let (player, _output) = MpvPlayer::new(move |e| {
        let _ = tx.send((Instant::now(), e));
    })
    .expect("libmpv starts");

    let t = Instant::now();
    player.load(&url, LoadOptions::video(start_s * 1000));
    println!("loading at {start_s} s (waits ~2 s for a render context that never comes)");
    wait_for(&rx, "first frame", |e| matches!(e, PlayerEvent::Playing));
    println!("  first frame after {} ms", t.elapsed().as_millis());
    if let Some(watch_s) = watch_s {
        // Just watch, with the audio's numbers: across a muted part, say.
        player.set_stats(true);
        report(&rx, watch_s);
        return;
    }
    report(&rx, 2);

    for (to_s, label) in [
        (start_s + 120, "+2 min"),
        (5, "back near the start"),
        (590, "near the end"),
    ] {
        player.seek(to_s * 1000);
        println!("seek {label} ({to_s} s)");
        report(&rx, 2);
    }

    player.seek(200_000);
    player.set_option(PlayerOption::Speed(2000));
    println!("seek to 200 s, speed 2x: the position should move 2 s per report");
    report(&rx, 3);

    player.set_paused(true);
    println!("paused: no health should come");
    report(&rx, 2);
    player.seek(300_000);
    player.set_paused(false);
    println!("seeked to 300 s while paused, then resumed");
    report(&rx, 2);

    player.load(&url, LoadOptions::default());
    println!("loaded as a live stream: no position, normal speed, from the beginning");
    report(&rx, 3);
}

/// Waits up to 20 s for an event that `want` accepts, printing the others.
fn wait_for(
    rx: &Receiver<(Instant, PlayerEvent)>,
    what: &str,
    want: impl Fn(&PlayerEvent) -> bool,
) {
    let until = Instant::now() + Duration::from_secs(20);
    while let Some(left) = until.checked_duration_since(Instant::now()) {
        match rx.recv_timeout(left) {
            Ok((_, e)) if want(&e) => return,
            Ok((_, e)) => println!("  (event {e:?})"),
            Err(_) => break,
        }
    }
    panic!("no {what} within 20 s");
}

/// Prints every event for `seconds` seconds.
fn report(rx: &Receiver<(Instant, PlayerEvent)>, seconds: u64) {
    let until = Instant::now() + Duration::from_secs(seconds) + Duration::from_millis(300);
    while let Some(left) = until.checked_duration_since(Instant::now()) {
        match rx.recv_timeout(left) {
            Ok((_, PlayerEvent::Health(h))) => println!(
                "  health: pos {} / length {}  buffer {:.1} s{}",
                h.pos_ms
                    .map_or("-".into(), |ms| format!("{:.1} s", ms as f64 / 1000.0)),
                h.length_ms
                    .map_or("-".into(), |ms| format!("{:.1} s", ms as f64 / 1000.0)),
                f64::from(h.buffer_ms) / 1000.0,
                if h.stalled { "  STALLED" } else { "" },
            ),
            Ok((_, PlayerEvent::Stats(s))) => println!(
                "  stats: audio {} at {} kbit/s, video {}x{}",
                s.audio_codec.as_deref().unwrap_or("-"),
                s.audio_bitrate.map_or(0, |b| b / 1000),
                s.width.unwrap_or(0),
                s.height.unwrap_or(0),
            ),
            Ok((_, e)) => println!("  event {e:?}"),
            Err(_) => break,
        }
    }
}
