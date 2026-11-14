//! Captions (captions-plan.md, phase 2): does the real `AudioTap` deliver audio on the main
//! player's clock?
//!
//! Resolves a live Twitch channel twice with Streamlink: the quality the player would show (720p60
//! by default) and the tap's (`TAP_QUALITY`, 160p30). Plays the first in a headless libmpv set up
//! like the app's main player (rebased timestamps, low latency, nothing shown or heard), taps the
//! second with `AudioTap`, and prints once a second:
//!
//! - `main`: the player's position on the stream clock (`time-pos` + `demuxer-start-time`, as
//!   `PlayerHealth::stream_ms`), and `edge`: the newest timestamp it has buffered.
//! - `tap`: the end of the newest tap chunk, and `tap-edge`: the tap minus the player's `edge`.
//!   Both read the live edge, so on one clock this stays within a segment or so (2 s).
//! - `lead`: how far the tap's audio is ahead of the picture: how long a caption has to be made.
//!
//! It also checks that the chunks join up (no gap or overlap), and with
//! `HTPC_TAP_LOG=htpc_player=debug` shows the tap's own clock checks (`audio-pts` against first
//! pts + samples / 16000). `rotate` (seconds)
//! moves the tap to a new file that often, to try the rotation. `out.wav` keeps the audio.
//!
//! ```text
//! cargo run -p htpc-player --features mpv --example audio_tap_probe -- <channel> [seconds] [quality] [rotate] [out.wav]
//! ```

use std::path::PathBuf;
use std::process::Command;
use std::sync::mpsc::channel;
use std::time::{Duration, Instant};

use htpc_player::{AudioTap, PcmChunk, TapEvent, ROTATE_AFTER, TAP_RATE};
use libmpv2::Mpv;

fn main() {
    if let Ok(filter) = std::env::var("HTPC_TAP_LOG") {
        tracing_subscriber::fmt().with_env_filter(filter).init();
    }
    let usage = "usage: audio_tap_probe <channel> [seconds] [quality] [rotate] [out.wav]";
    let mut args = std::env::args().skip(1);
    let login = args.next().expect(usage);
    let seconds: u64 = args.next().map_or(60, |s| s.parse().expect("seconds"));
    let quality = args.next().unwrap_or_else(|| "720p60,720p,best".into());
    let rotate = args.next().map_or(ROTATE_AFTER, |s| {
        Duration::from_secs(s.parse().expect("rotate"))
    });
    let out = args.next();

    let main_url = resolve(&login, &quality);
    let tap_url = resolve(&login, htpc_resolver::TAP_QUALITY);
    let main = Mpv::with_initializer(|init| {
        init.set_option("vo", "null")?;
        init.set_option("ao", "null")?;
        init.set_option("profile", "low-latency")?;
        init.set_option("ytdl", false)?;
        Ok(())
    })
    .expect("libmpv");
    main.command("loadfile", &[main_url.as_str(), "replace"])
        .expect("loadfile");

    let (tx, rx) = channel();
    let tap = AudioTap::start_rotating(
        &tap_url,
        Box::new(move |e| {
            let _ = tx.send(e);
        }),
        rotate,
    )
    .expect("tap");

    let started = Instant::now();
    let mut newest: Option<PcmChunk> = None;
    let mut audio: Vec<f32> = Vec::new();
    let (mut chunks, mut gaps, mut leads) = (0usize, Vec::new(), Vec::new());
    let mut edge_diffs = Vec::new();
    println!("    t      main      edge       tap  tap-edge  lead  chunks");
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
                    chunks += 1;
                    if out.is_some() {
                        audio.extend_from_slice(&chunk.samples);
                    }
                    newest = Some(chunk);
                }
                other => {
                    println!("tap: {other:?}");
                    break 'run;
                }
            }
        }
        while main.wait_event(0.0).is_some() {}
        let pos = main.get_property::<f64>("time-pos").ok();
        let start = main
            .get_property::<f64>("demuxer-start-time")
            .unwrap_or(0.0);
        let clock = pos.map(|p| p + start);
        let edge = main
            .get_property::<f64>("demuxer-cache-time")
            .ok()
            .map(|e| e + start);
        let tap_end = newest.as_ref().map(PcmChunk::end_s);
        let tap_edge = tap_end.zip(edge).map(|(t, e)| t - e);
        let lead = tap_end.zip(clock).map(|(t, c)| t - c);
        if second > 10 {
            leads.extend(lead);
            edge_diffs.extend(tap_edge);
        }
        let f = |v: Option<f64>, w: usize| v.map_or_else(|| "-".into(), |v| format!("{v:>w$.2}"));
        println!(
            "{second:>5}  {}  {}  {}  {}  {}  {chunks:>6}",
            f(clock, 8),
            f(edge, 8),
            f(tap_end, 8),
            f(tap_edge, 8),
            f(lead, 4),
        );
    }
    drop(tap);

    let range = |v: &[f64]| {
        let lo = v.iter().copied().fold(f64::INFINITY, f64::min);
        let hi = v.iter().copied().fold(f64::NEG_INFINITY, f64::max);
        format!("{lo:+.2} to {hi:+.2} s")
    };
    println!(
        "after the first 10 s: tap-edge {}, lead {}",
        range(&edge_diffs),
        range(&leads)
    );
    if gaps.is_empty() {
        println!("{chunks} chunks, all joined up");
    } else {
        println!("{chunks} chunks, gaps (at, size): {gaps:?}");
    }
    if let Some(path) = out {
        write_wav(&PathBuf::from(&path), &audio);
        println!(
            "wrote {:.1} s to {path}",
            audio.len() as f64 / f64::from(TAP_RATE)
        );
    }
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

/// 16-bit mono 16 kHz WAV.
fn write_wav(path: &PathBuf, samples: &[f32]) {
    let data = u32::try_from(samples.len() * 2).expect("small enough");
    let mut bytes = Vec::with_capacity(44 + samples.len() * 2);
    bytes.extend_from_slice(b"RIFF");
    bytes.extend_from_slice(&(36 + data).to_le_bytes());
    bytes.extend_from_slice(b"WAVEfmt \x10\0\0\0\x01\0\x01\0");
    bytes.extend_from_slice(&TAP_RATE.to_le_bytes());
    bytes.extend_from_slice(&(TAP_RATE * 2).to_le_bytes());
    bytes.extend_from_slice(b"\x02\0\x10\0data");
    bytes.extend_from_slice(&data.to_le_bytes());
    for s in samples {
        let v = (s * 32768.0).clamp(-32768.0, 32767.0) as i16;
        bytes.extend_from_slice(&v.to_le_bytes());
    }
    std::fs::write(path, bytes).expect("write the WAV");
}
