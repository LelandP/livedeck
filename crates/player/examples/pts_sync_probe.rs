//! Captions spike (captions-plan.md, Phase 0): do two renditions of one Twitch stream (the
//! `audio_only` tap and the video the player shows) carry the same timestamps?
//!
//! Plays both at once with nothing shown or heard and `rebase-start-time=no`, so `time-pos` is the
//! stream's own timestamp, and prints both once a second. If the timestamps are shared, a caption
//! made from the tap at timestamp `t` shows when the player's `time-pos` reaches `t`.
//!
//! ```text
//! cargo run -p htpc-player --features mpv --example pts_sync_probe -- <audio-url> <video-url> [seconds]
//! ```

use std::time::{Duration, Instant};

use libmpv2::Mpv;

fn open(url: &str) -> Mpv {
    let mpv = Mpv::with_initializer(|init| {
        // Start at the newest segment instead of three back (`PTS_LIVE_EDGE=1`).
        if std::env::var_os("PTS_LIVE_EDGE").is_some() {
            init.set_option("demuxer-lavf-o", "live_start_index=-1")?;
        }
        init.set_option("vo", "null")?;
        init.set_option("ao", "null")?;
        init.set_option("rebase-start-time", "no")?;
        init.set_option("ytdl", "no")?;
        Ok(())
    })
    .expect("libmpv");
    mpv.command("loadfile", &[url, "replace"])
        .expect("loadfile");
    mpv
}

fn main() {
    let mut args = std::env::args().skip(1);
    let usage = "usage: pts_sync_probe <audio-url> <video-url> [seconds]";
    let audio = open(&args.next().expect(usage));
    let video = open(&args.next().expect(usage));
    let seconds: u64 = args.next().map_or(30, |s| s.parse().expect("seconds"));

    let started = Instant::now();
    while started.elapsed() < Duration::from_secs(seconds) {
        std::thread::sleep(Duration::from_secs(1));
        let a = audio.get_property::<f64>("time-pos").ok();
        let v = video.get_property::<f64>("time-pos").ok();
        let diff = a
            .zip(v)
            .map(|(a, v)| format!("{:+.2}", a - v))
            .unwrap_or_default();
        println!(
            "{:>3}s  audio {:>12}  video {:>12}  audio-video {diff}",
            started.elapsed().as_secs(),
            a.map(|a| format!("{a:.2}")).unwrap_or_else(|| "-".into()),
            v.map(|v| format!("{v:.2}")).unwrap_or_else(|| "-".into()),
        );
    }
}
