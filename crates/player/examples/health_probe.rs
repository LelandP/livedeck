//! Checks on a live stream that mpv's health properties are reliable (plan.md, phase 8), with the
//! network slowed and cut on a schedule.
//!
//! mpv reaches the stream through a local CONNECT proxy (`http-proxy`) whose download speed
//! follows the schedule, so a run shows what `paused-for-cache`, `demuxer-cache-duration` and
//! `cache-speed` do when the network gets slow, goes away and comes back. Nothing is shown or
//! heard (`vo=null`, `ao=null`).
//!
//! ```text
//! cargo run -p htpc-player --features mpv --example health_probe -- <playlist-url> [low|normal] [schedule]
//! ```
//!
//! The schedule is `seconds:rate` steps, a rate being `full`, `0` or a speed in kB/s, for example
//! the default `20:full,25:150,15:0,40:full`. Each line printed is one second.

use std::io::{Read, Write};
use std::net::{Shutdown, TcpListener, TcpStream};
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, Instant};

use libmpv2::Mpv;

/// The app's latency options (`mpv.rs`): `(option, low, normal)`.
const LATENCY_OPTIONS: &[(&str, &str, &str)] = &[
    ("audio-buffer", "0", "0.2"),
    ("vd-lavc-threads", "1", "0"),
    ("cache-pause", "no", "yes"),
    ("cache-pause-wait", "1", "3"),
    ("demuxer-lavf-o", "fflags=+nobuffer", ""),
    ("demuxer-lavf-probe-info", "nostreams", "auto"),
    ("demuxer-lavf-analyzeduration", "0.1", "0"),
    ("demuxer-readahead-secs", "1", "10"),
    ("video-latency-hacks", "yes", "no"),
    ("stream-buffer-size", "4k", "128k"),
];

/// Bytes per second the proxy lets through. `u64::MAX` is no limit, 0 nothing at all.
static RATE: AtomicU64 = AtomicU64::new(u64::MAX);

fn main() {
    let mut args = std::env::args().skip(1);
    let url = args
        .next()
        .expect("usage: health_probe <playlist-url> [low|normal] [schedule]");
    let low = args.next().as_deref() != Some("normal");
    let schedule = parse_schedule(
        &args
            .next()
            .unwrap_or_else(|| "20:full,25:150,15:0,40:full".into()),
    );

    let proxy = TcpListener::bind("127.0.0.1:0").expect("proxy port");
    let proxy_addr = proxy.local_addr().expect("proxy address");
    std::thread::spawn(move || {
        for client in proxy.incoming().flatten() {
            std::thread::spawn(move || tunnel(client));
        }
    });

    let mpv = Mpv::with_initializer(|init| {
        init.set_option("vo", "null")?;
        init.set_option("ao", "null")?;
        init.set_option("idle", "yes")?;
        init.set_option("profile", "low-latency")?;
        init.set_option("ytdl", false)?;
        init.set_option("http-proxy", format!("http://{proxy_addr}").as_str())?;
        Ok(())
    })
    .expect("libmpv starts");
    for (name, l, n) in LATENCY_OPTIONS {
        let _ = mpv.set_property(name, if low { *l } else { *n });
    }
    mpv.command("loadfile", &[url.as_str(), "replace"])
        .expect("loadfile");

    println!("latency {}", if low { "low" } else { "normal" });
    println!("   t  net      stall  buffer  speed(kB/s)  buffering  drops  behind");
    let start = Instant::now();
    let total: u64 = schedule.iter().map(|(s, _)| s).sum();
    for second in 0..total {
        let rate = rate_at(&schedule, second);
        RATE.store(rate, Ordering::SeqCst);
        // Drain events so the core keeps going.
        while let Some(event) = mpv.wait_event(0.0) {
            if let Err(e) = event {
                println!("     event error: {e}");
            }
        }
        let flag = |name| mpv.get_property::<bool>(name).ok();
        let num = |name| {
            mpv.get_property::<f64>(name)
                .ok()
                .or_else(|| mpv.get_property::<i64>(name).ok().map(|v| v as f64))
        };
        let behind = match (num("demuxer-cache-time"), num("time-pos")) {
            (Some(end), Some(pos)) => format!("{:.1}", end - pos),
            _ => "-".into(),
        };
        println!(
            "{:4}  {:<7}  {:<5}  {:>6}  {:>11}  {:>9}  {:>5}  {:>6}",
            second,
            describe_rate(rate),
            flag("paused-for-cache").map_or("-".into(), |b| b.to_string()),
            num("demuxer-cache-duration").map_or("-".into(), |v| format!("{v:.2}")),
            num("cache-speed").map_or("-".into(), |v| format!("{:.0}", v / 1000.0)),
            num("cache-buffering-state").map_or("-".into(), |v| format!("{v:.0}")),
            num("frame-drop-count").map_or("-".into(), |v| format!("{v:.0}")),
            behind,
        );
        let next = start + Duration::from_secs(second + 1);
        std::thread::sleep(next.saturating_duration_since(Instant::now()));
    }
}

fn parse_schedule(text: &str) -> Vec<(u64, u64)> {
    text.split(',')
        .map(|step| {
            let (secs, rate) = step.split_once(':').expect("seconds:rate");
            let rate = match rate {
                "full" => u64::MAX,
                kb => kb.parse::<u64>().expect("a rate in kB/s") * 1000,
            };
            (secs.parse().expect("seconds"), rate)
        })
        .collect()
}

fn rate_at(schedule: &[(u64, u64)], second: u64) -> u64 {
    let mut end = 0;
    for (secs, rate) in schedule {
        end += secs;
        if second < end {
            return *rate;
        }
    }
    u64::MAX
}

fn describe_rate(rate: u64) -> String {
    match rate {
        u64::MAX => "full".into(),
        0 => "cut".into(),
        r => format!("{}k", r / 1000),
    }
}

/// One proxied connection: read the `CONNECT host:port` request, open the tunnel, and pipe both
/// ways. The download side keeps to `RATE`.
fn tunnel(mut client: TcpStream) {
    let mut head = Vec::new();
    let mut byte = [0u8; 1];
    while !head.ends_with(b"\r\n\r\n") {
        match client.read(&mut byte) {
            Ok(1) => head.push(byte[0]),
            _ => return,
        }
    }
    let head = String::from_utf8_lossy(&head);
    let Some(target) = head
        .lines()
        .next()
        .and_then(|l| l.strip_prefix("CONNECT "))
        .and_then(|l| l.split_whitespace().next())
    else {
        let _ = client.write_all(b"HTTP/1.1 405 CONNECT only\r\n\r\n");
        return;
    };
    let Ok(server) = TcpStream::connect(target) else {
        let _ = client.write_all(b"HTTP/1.1 502 Bad Gateway\r\n\r\n");
        return;
    };
    if client
        .write_all(b"HTTP/1.1 200 Connection established\r\n\r\n")
        .is_err()
    {
        return;
    }
    let (Ok(up_from), Ok(up_to)) = (client.try_clone(), server.try_clone()) else {
        return;
    };
    let up = std::thread::spawn(move || pipe(up_from, up_to, false));
    pipe(server, client, true);
    let _ = up.join();
}

fn pipe(mut from: TcpStream, mut to: TcpStream, throttled: bool) {
    let mut buf = [0u8; 4096];
    loop {
        if throttled {
            while RATE.load(Ordering::SeqCst) == 0 {
                std::thread::sleep(Duration::from_millis(100));
            }
        }
        let n = match from.read(&mut buf) {
            Ok(0) | Err(_) => break,
            Ok(n) => n,
        };
        if to.write_all(&buf[..n]).is_err() {
            break;
        }
        if throttled {
            let rate = RATE.load(Ordering::SeqCst);
            if rate != u64::MAX && rate > 0 {
                std::thread::sleep(Duration::from_secs_f64(n as f64 / rate as f64));
            }
        }
    }
    let _ = to.shutdown(Shutdown::Write);
}
