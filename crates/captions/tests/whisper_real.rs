//! Needs the `whisper` feature, a Whisper model and a Silero VAD model:
//!
//! ```text
//! $env:CAPTIONS_MODEL = "...\ggml-base.en-q5_1.bin"
//! $env:CAPTIONS_VAD_MODEL = "...\ggml-silero-v5.1.2.bin"
//! cargo test -p htpc-captions --features whisper --release -- --ignored --nocapture
//! ```
//!
//! Each caption's words are printed with their times. `CAPTIONS_DTW=base.en` (the model's size)
//! times them with DTW instead of the token times (later than the sound, measured: see
//! `WhisperConfig::dtw`).
#![cfg(feature = "whisper")]

use std::path::PathBuf;
use std::time::{Duration, Instant};

use htpc_captions::{Transcriber, WhisperConfig, WhisperTranscriber};

/// 16 kHz mono 16-bit PCM WAV (the JFK sample from whisper.cpp, public domain).
fn jfk() -> Vec<f32> {
    let bytes = include_bytes!("fixtures/jfk.wav");
    // Walk the RIFF chunks to "data" (the file has a LIST chunk before it).
    let mut pos = 12;
    let data = loop {
        let id = &bytes[pos..pos + 4];
        let len = u32::from_le_bytes(bytes[pos + 4..pos + 8].try_into().unwrap()) as usize;
        if id == b"data" {
            break &bytes[pos + 8..(pos + 8 + len).min(bytes.len())];
        }
        pos += 8 + len + (len & 1);
    };
    data.chunks(2)
        .map(|b| f32::from(i16::from_le_bytes([b[0], b[1]])) / 32768.0)
        .collect()
}

fn config() -> WhisperConfig {
    let model = std::env::var("CAPTIONS_MODEL").expect("set CAPTIONS_MODEL");
    let vad = std::env::var("CAPTIONS_VAD_MODEL").expect("set CAPTIONS_VAD_MODEL");
    let mut cfg = WhisperConfig::new(PathBuf::from(model), PathBuf::from(vad));
    // `CAPTIONS_DTW=base.en` (or tiny.en) times the words with DTW, for a model of that size.
    cfg.dtw = match std::env::var("CAPTIONS_DTW").as_deref() {
        Ok("tiny.en") => Some(htpc_captions::DtwModelPreset::TinyEn),
        Ok("base.en") => Some(htpc_captions::DtwModelPreset::BaseEn),
        Ok("small") => Some(htpc_captions::DtwModelPreset::Small),
        _ => None,
    };
    cfg
}

/// Push `audio` as 0.5 s chunks from `t0`, then wait for captions.
fn run(t: &mut WhisperTranscriber, t0: f64, audio: &[f32], wait: Duration) -> String {
    for (i, part) in audio.chunks(8000).enumerate() {
        t.push(t0 + i as f64 * 0.5, part);
    }
    let start = Instant::now();
    let mut text = String::new();
    let mut quiet_since = Instant::now();
    while start.elapsed() < wait {
        let got = t.poll();
        if !got.is_empty() {
            for c in &got {
                println!(
                    "[{:.2} - {:.2}] at {:.1?}: {}",
                    c.start_s,
                    c.end_s,
                    start.elapsed(),
                    c.text
                );
                let words: Vec<String> = c
                    .words
                    .iter()
                    .map(|w| format!("{:.2} {}", w.start_s, w.text))
                    .collect();
                println!("    {}", words.join(" | "));
                assert!(!c.words.is_empty(), "every caption has its words");
                assert!(c.words.windows(2).all(|p| p[0].start_s <= p[1].start_s));
                assert!(c
                    .words
                    .iter()
                    .all(|w| w.start_s >= c.start_s && w.start_s <= c.end_s));
                text.push_str(&c.text);
                text.push(' ');
            }
            quiet_since = Instant::now();
        } else if !text.is_empty() && quiet_since.elapsed() > Duration::from_secs(10) {
            break;
        }
        std::thread::sleep(Duration::from_millis(50));
    }
    text.to_lowercase()
}

#[test]
#[ignore = "needs CAPTIONS_MODEL and CAPTIONS_VAD_MODEL"]
fn jfk_clip_gives_the_expected_words() {
    let mut t = WhisperTranscriber::new(config()).expect("models load");
    let mut audio = vec![0.0f32; 16_000 * 2];
    audio.extend(jfk());
    audio.extend(vec![0.0f32; 16_000 * 3]);
    let started = Instant::now();
    let text = run(&mut t, 1000.0, &audio, Duration::from_secs(60));
    println!("took {:?}: {text}", started.elapsed());
    for word in ["fellow americans", "ask not", "your country"] {
        assert!(text.contains(word), "missing {word:?} in {text:?}");
    }
}

#[test]
#[ignore = "needs CAPTIONS_MODEL and CAPTIONS_VAD_MODEL"]
fn silence_and_noise_give_nothing() {
    let mut t = WhisperTranscriber::new(config()).expect("models load");
    // 20 s of digital silence, then 10 s of faint hiss.
    let mut audio = vec![0.0f32; 16_000 * 20];
    let mut x = 12345u32;
    audio.extend((0..16_000 * 10).map(|_| {
        x = x.wrapping_mul(1_664_525).wrapping_add(1_013_904_223);
        ((x >> 16) as f32 / 65_535.0 - 0.5) * 0.004
    }));
    let text = run(&mut t, 0.0, &audio, Duration::from_secs(8));
    assert!(text.trim().is_empty(), "got {text:?}");
}

#[test]
#[ignore = "needs CAPTIONS_MODEL and CAPTIONS_VAD_MODEL"]
fn a_missing_model_is_an_error() {
    let cfg = WhisperConfig::new("does-not-exist.bin", "nor-this.bin");
    assert!(WhisperTranscriber::new(cfg).is_err());
}
