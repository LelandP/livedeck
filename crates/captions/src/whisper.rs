//! whisper.cpp through `whisper-rs`: Silero VAD for the speech spans, the model for the text and
//! its word times.

use std::path::PathBuf;

pub use whisper_rs::DtwModelPreset;
use whisper_rs::{
    DtwMode, DtwParameters, FullParams, SamplingStrategy, WhisperContext, WhisperContextParameters,
    WhisperSegment, WhisperState, WhisperVadContext, WhisperVadContextParams, WhisperVadParams,
};

use crate::chunker::{ChunkerConfig, Span, SpeechDetector};
use crate::worker::{CaptionWorker, RawSegment, SegmentEngine};
use crate::{Caption, Transcriber, Word};

#[derive(Debug, Clone)]
pub struct WhisperConfig {
    /// A ggml Whisper model, such as `ggml-base.en-q5_1.bin`.
    pub model_path: PathBuf,
    /// A ggml Silero VAD model, such as `ggml-silero-v5.1.2.bin`.
    pub vad_model_path: PathBuf,
    /// Threads for inference. 4 matches the Steam Deck's cores.
    pub threads: i32,
    /// Run the model on the GPU. Only does anything in a `vulkan` build.
    pub use_gpu: bool,
    /// Spoken language code (`"en"`), or `"auto"` to detect it (multilingual models only).
    pub language: String,
    pub chunker: ChunkerConfig,
    /// Word times from the model's alignment heads (DTW), for the preset matching the model.
    /// `None` (the default): whisper's plain token times. On `tests/fixtures/jfk.wav` with
    /// base.en q5_1 (2026-11-14) the token times were within 0.05 s of where each word's sound
    /// starts for most words, and DTW's 0.1 to 0.5 s later, so DTW is left off.
    pub dtw: Option<DtwModelPreset>,
    /// Seconds of audio that may wait for the model before the oldest is dropped
    /// ([`DEFAULT_BACKLOG_S`](crate::DEFAULT_BACKLOG_S)).
    pub backlog_s: f64,
}

impl WhisperConfig {
    pub fn new(model_path: impl Into<PathBuf>, vad_model_path: impl Into<PathBuf>) -> Self {
        Self {
            model_path: model_path.into(),
            vad_model_path: vad_model_path.into(),
            threads: 4,
            use_gpu: false,
            language: "en".into(),
            chunker: ChunkerConfig::default(),
            dtw: None,
            backlog_s: crate::DEFAULT_BACKLOG_S,
        }
    }
}

/// Live captions with whisper.cpp. Loads the models once, then runs on its own thread.
pub struct WhisperTranscriber {
    worker: CaptionWorker,
}

impl WhisperTranscriber {
    /// Load both models and start the thread. Fails with the reason if either file is missing or
    /// not a valid model.
    pub fn new(cfg: WhisperConfig) -> Result<Self, String> {
        install_logging();
        let chunker = cfg.chunker.clone();
        let backlog_s = cfg.backlog_s;
        let worker = CaptionWorker::spawn(move || load(&cfg), chunker)?;
        worker.set_backlog_s(backlog_s);
        Ok(Self { worker })
    }

    /// Seconds of audio dropped because inference fell behind.
    pub fn dropped_s(&self) -> f64 {
        self.worker.dropped_s()
    }

    /// Seconds of audio waiting for the model.
    pub fn queued_s(&self) -> f64 {
        self.worker.queued_s()
    }
}

impl Transcriber for WhisperTranscriber {
    fn push(&mut self, start_s: f64, samples: &[f32]) {
        self.worker.push(start_s, samples);
    }

    fn poll(&mut self) -> Vec<Caption> {
        self.worker.poll()
    }

    fn reset(&mut self) {
        self.worker.reset();
    }

    fn set_backlog_s(&mut self, backlog_s: f64) {
        self.worker.set_backlog_s(backlog_s);
    }

    fn set_playhead_s(&mut self, playhead_s: f64) {
        Transcriber::set_playhead_s(&mut self.worker, playhead_s);
    }
}

/// Sends whisper.cpp's and ggml's own log lines to `tracing` (target `whisper`) instead of
/// stderr: errors as warnings, warnings at debug, the rest (several lines per VAD call) at trace.
/// Done once, before the first model loads; later calls do nothing.
pub fn install_logging() {
    static ONCE: std::sync::Once = std::sync::Once::new();
    ONCE.call_once(|| {
        // SAFETY: `log_line` matches `ggml_log_callback` and lives for the whole program; the
        // user data is unused. Set before any model is loaded, so no log call races with it.
        unsafe {
            whisper_rs::whisper_rs_sys::whisper_log_set(Some(log_line), std::ptr::null_mut());
            whisper_rs::whisper_rs_sys::ggml_log_set(Some(log_line), std::ptr::null_mut());
        }
    });
}

unsafe extern "C" fn log_line(
    level: whisper_rs::whisper_rs_sys::ggml_log_level,
    text: *const std::ffi::c_char,
    _user_data: *mut std::ffi::c_void,
) {
    use whisper_rs::whisper_rs_sys as sys;
    if text.is_null() {
        return;
    }
    // SAFETY: whisper.cpp passes a NUL-terminated string that lives for the call.
    let text = unsafe { std::ffi::CStr::from_ptr(text) }.to_string_lossy();
    let text = text.trim_end();
    if text.is_empty() {
        return;
    }
    match level {
        sys::ggml_log_level_GGML_LOG_LEVEL_ERROR => tracing::warn!(target: "whisper", "{text}"),
        sys::ggml_log_level_GGML_LOG_LEVEL_WARN => tracing::debug!(target: "whisper", "{text}"),
        _ => tracing::trace!(target: "whisper", "{text}"),
    }
}

fn load(cfg: &WhisperConfig) -> Result<(SileroDetector, WhisperEngine), String> {
    let path_str = |p: &PathBuf| p.to_str().map(String::from).ok_or("non-UTF-8 model path");
    for p in [&cfg.model_path, &cfg.vad_model_path] {
        if !p.is_file() {
            return Err(format!("model file not found: {}", p.display()));
        }
    }

    let mut vad_ctx = WhisperVadContextParams::new();
    vad_ctx.set_n_threads(cfg.threads.clamp(1, 2));
    vad_ctx.set_use_gpu(false);
    let vad = WhisperVadContext::new(&path_str(&cfg.vad_model_path)?, vad_ctx)
        .map_err(|e| format!("loading the VAD model: {e}"))?;

    let mut ctx_params = WhisperContextParameters::new();
    ctx_params.use_gpu(cfg.use_gpu);
    if let Some(preset) = cfg.dtw.clone() {
        ctx_params.dtw_parameters(DtwParameters {
            mode: DtwMode::ModelPreset {
                model_preset: preset,
            },
            ..DtwParameters::default()
        });
    }
    let ctx = WhisperContext::new_with_params(&cfg.model_path, ctx_params)
        .map_err(|e| format!("loading the Whisper model: {e}"))?;
    let state = ctx
        .create_state()
        .map_err(|e| format!("creating the Whisper state: {e}"))?;

    let mut vad_params = WhisperVadParams::new();
    // Our chunker does the cutting and merging; the VAD only reports speech.
    vad_params.set_max_speech_duration(f32::MAX);
    let eot = ctx.token_eot();
    Ok((
        SileroDetector { vad, vad_params },
        WhisperEngine {
            _ctx: ctx,
            state,
            threads: cfg.threads,
            language: cfg.language.clone(),
            dtw: cfg.dtw.is_some(),
            eot,
        },
    ))
}

struct SileroDetector {
    vad: WhisperVadContext,
    vad_params: WhisperVadParams,
}

impl SpeechDetector for SileroDetector {
    fn detect(&mut self, samples: &[f32]) -> Vec<Span> {
        // Silero needs a few windows of audio to say anything.
        if samples.len() < 1024 {
            return Vec::new();
        }
        match self.vad.segments_from_samples(self.vad_params, samples) {
            // whisper.cpp reports centiseconds.
            Ok(segs) => segs
                .map(|s| Span {
                    start_s: f64::from(s.start) / 100.0,
                    end_s: f64::from(s.end) / 100.0,
                })
                .collect(),
            Err(e) => {
                tracing::warn!("captions: VAD failed: {e}");
                Vec::new()
            }
        }
    }
}

struct WhisperEngine {
    // The state holds what it needs of the context; kept so the model outlives it.
    _ctx: WhisperContext,
    state: WhisperState,
    threads: i32,
    language: String,
    /// Word times from DTW (`t_dtw`) rather than the token times (`t0`).
    dtw: bool,
    /// Token ids from this one up are special (timestamps, end of text): no words.
    eot: whisper_rs::WhisperTokenId,
}

impl SegmentEngine for WhisperEngine {
    fn transcribe(&mut self, samples: &[f32]) -> Result<Vec<RawSegment>, String> {
        let mut params = FullParams::new(SamplingStrategy::Greedy { best_of: 1 });
        params.set_n_threads(self.threads);
        params.set_language(Some(self.language.as_str()));
        // Each chunk stands alone. The audio context is never shortened: base.en loops on
        // repeated text when it is.
        params.set_no_context(true);
        params.set_suppress_nst(true);
        params.set_print_special(false);
        params.set_print_progress(false);
        params.set_print_realtime(false);
        params.set_print_timestamps(false);
        // A time for every token, so words can show as they are spoken.
        params.set_token_timestamps(true);

        self.state
            .full(params, samples)
            .map_err(|e| format!("whisper: {e}"))?;

        let mut out = Vec::new();
        for seg in self.state.as_iter() {
            let text = seg.to_str_lossy().map_err(|e| e.to_string())?.into_owned();
            out.push(RawSegment {
                // Centiseconds from the start of the chunk.
                start_s: seg.start_timestamp() as f64 / 100.0,
                end_s: seg.end_timestamp() as f64 / 100.0,
                words: words_of(&seg, self.eot, self.dtw),
                text,
            });
        }
        Ok(out)
    }
}

/// The segment's words: its text tokens joined, a new word at each token that starts with a
/// space, timed by the word's first token. Special tokens are skipped.
fn words_of(seg: &WhisperSegment<'_>, eot: whisper_rs::WhisperTokenId, dtw: bool) -> Vec<Word> {
    let mut words: Vec<Word> = Vec::new();
    for i in 0..seg.n_tokens() {
        let Some(token) = seg.get_token(i) else {
            continue;
        };
        let data = token.token_data();
        if data.id >= eot {
            continue;
        }
        let Ok(text) = token.to_str_lossy() else {
            continue;
        };
        // `t_dtw` is -1 where DTW gave none.
        let cs = if dtw && data.t_dtw >= 0 {
            data.t_dtw
        } else {
            data.t0
        };
        let at = cs as f64 / 100.0;
        match words.last_mut() {
            Some(word) if !text.starts_with(' ') => word.text.push_str(&text),
            _ => words.push(Word::new(at, text.trim_start())),
        }
    }
    words.retain(|w| !w.text.trim().is_empty());
    words
}
