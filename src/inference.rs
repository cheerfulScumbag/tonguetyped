//! Single GGUF inference module, built on the `transcribe-cpp` Rust binding for
//! [transcribe.cpp](https://github.com/handy-computer/transcribe.cpp). One
//! library, one GGUF model format, and one code path handles every backend:
//! CPU, Vulkan, CUDA, ROCm, and Metal (whichever the current build was
//! compiled with - see the `gpu-vulkan`/`gpu-cuda`/`gpu-rocm`/`gpu-metal`
//! Cargo features). `InferenceEngine::load` always requests an explicit
//! backend, in a fixed priority order, never `transcribe_cpp::Backend::Auto` -
//! an accelerator whose feature wasn't compiled in simply isn't natively
//! satisfiable and `Model::load_with` returns `Error::Backend`, so the same
//! unconditional priority list works correctly on every build without any
//! `cfg` gating of its own. Every attempt - accelerator or CPU - loads the
//! exact same GGUF file (`InferenceEngine::new`'s `model_path`): fallback
//! never substitutes a different model.
//!
//! `config.model.preferred_backend` (see `BackendPreference`) can pin `load`
//! to exactly one backend instead. A pinned backend is the only one tried:
//! if it can't be used, `load` fails with an error naming it rather than
//! quietly running on a different backend.

use anyhow::Context;
use std::path::{Path, PathBuf};

/// Cooperative cancellation handle for an in-flight `transcribe`, re-exported so
/// the coordinator can reach it without depending on `transcribe_cpp` directly.
pub use transcribe_cpp::CancelToken;

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize, PartialEq, Eq)]
pub struct BackendInfo {
    pub backend: String,
    pub device: String,
}

/// Backends `InferenceEngine::load` requests, in priority order, before the
/// unconditional final `Cpu` entry. Never includes `Backend::Auto`: an
/// explicit request is the only way to know (via a clean `Error::Backend`,
/// rather than silent native rerouting) whether a given backend was actually
/// used, which is what makes the CPU fallback deterministic and testable.
const ACCELERATOR_PRIORITY: &[(transcribe_cpp::Backend, &str)] = &[
    (transcribe_cpp::Backend::Cuda, "cuda"),
    (transcribe_cpp::Backend::Rocm, "rocm"),
    (transcribe_cpp::Backend::Vulkan, "vulkan"),
    (transcribe_cpp::Backend::Metal, "metal"),
];

/// Every value `config.model.preferred_backend` accepts, in the order the
/// setup wizard and dashboard list them. `"auto"` is the priority-order
/// fallback chain above; every other entry pins `InferenceEngine::load` to
/// that one backend.
pub const BACKEND_PREFERENCES: &[&str] = &["auto", "cpu", "vulkan", "cuda", "rocm", "metal"];

/// Human-readable label for a `BACKEND_PREFERENCES` value, shared by the
/// setup wizard and dashboard.
pub fn backend_preference_label(name: &str) -> &'static str {
    match name {
        "auto" => "Auto (tries CUDA, ROCm, Vulkan, Metal, then CPU)",
        "cpu" => "CPU",
        "vulkan" => "Vulkan",
        "cuda" => "CUDA (NVIDIA)",
        "rocm" => "ROCm (AMD)",
        "metal" => "Metal (Apple)",
        _ => "Unknown backend",
    }
}

/// A parsed `config.model.preferred_backend` value.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BackendPreference {
    /// Try `ACCELERATOR_PRIORITY` in order, then CPU.
    Auto,
    /// Try only this backend, never falling back to another one.
    Only(transcribe_cpp::Backend),
}

impl BackendPreference {
    pub fn parse(value: &str) -> anyhow::Result<Self> {
        if value == "auto" {
            return Ok(BackendPreference::Auto);
        }
        selectable_backend(value)
            .map(|backend| BackendPreference::Only(backend.backend))
            .ok_or_else(|| {
                anyhow::anyhow!(
                    "unsupported inference backend '{value}' (expected one of: {})",
                    BACKEND_PREFERENCES.join(", ")
                )
            })
    }
}

/// A non-`auto` backend choice, with whether this build compiled it in.
struct SelectableBackend {
    name: &'static str,
    backend: transcribe_cpp::Backend,
    /// The Cargo feature that compiles this backend in, `None` for CPU.
    feature: Option<&'static str>,
    compiled: bool,
}

const SELECTABLE_BACKENDS: &[SelectableBackend] = &[
    SelectableBackend {
        name: "cpu",
        backend: transcribe_cpp::Backend::Cpu,
        feature: None,
        compiled: true,
    },
    SelectableBackend {
        name: "vulkan",
        backend: transcribe_cpp::Backend::Vulkan,
        feature: Some("gpu-vulkan"),
        compiled: cfg!(feature = "gpu-vulkan"),
    },
    SelectableBackend {
        name: "cuda",
        backend: transcribe_cpp::Backend::Cuda,
        feature: Some("gpu-cuda"),
        compiled: cfg!(feature = "gpu-cuda"),
    },
    SelectableBackend {
        name: "rocm",
        backend: transcribe_cpp::Backend::Rocm,
        feature: Some("gpu-rocm"),
        compiled: cfg!(feature = "gpu-rocm"),
    },
    SelectableBackend {
        name: "metal",
        backend: transcribe_cpp::Backend::Metal,
        feature: Some("gpu-metal"),
        compiled: cfg!(feature = "gpu-metal"),
    },
];

fn selectable_backend(name: &str) -> Option<&'static SelectableBackend> {
    SELECTABLE_BACKENDS.iter().find(|entry| entry.name == name)
}

fn selectable_backend_for(backend: transcribe_cpp::Backend) -> Option<&'static SelectableBackend> {
    SELECTABLE_BACKENDS
        .iter()
        .find(|entry| entry.backend == backend)
}

/// Why a pinned backend can't be used on this build and host, or `None` when
/// it can. Separates "not compiled in" (fixable by rebuilding with a feature)
/// from "compiled in, but no usable device or driver" (a host problem).
fn backend_unavailable_reason(entry: &SelectableBackend) -> Option<String> {
    if !entry.compiled {
        return Some(format!(
            "this build was compiled without the `{}` feature",
            entry.feature.unwrap_or_default()
        ));
    }
    if entry.feature.is_some() && !transcribe_cpp::backend_available(entry.backend) {
        return Some(format!(
            "no usable {} device or driver was found on this host",
            entry.name
        ));
    }
    None
}

/// One row of the backend picker shown by the setup wizard and dashboard.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BackendChoice {
    /// The `config.model.preferred_backend` value.
    pub name: &'static str,
    /// Why this choice can't work here, `None` when it can.
    pub unavailable: Option<String>,
}

/// Every `BACKEND_PREFERENCES` entry with whether it can work on this build
/// and host. Blocks on the first call the same way `backend_info` does
/// (device enumeration), so keep it off hot paths.
pub fn backend_choices() -> Vec<BackendChoice> {
    BACKEND_PREFERENCES
        .iter()
        .map(|&name| BackendChoice {
            name,
            unavailable: selectable_backend(name).and_then(backend_unavailable_reason),
        })
        .collect()
}

static CAPABILITY_PROBE: std::sync::OnceLock<BackendInfo> = std::sync::OnceLock::new();

/// Hardware-capability probe: which backend this build and host would use for
/// a model that loads cleanly, without actually loading one. Blocks on the
/// first call (enumerating non-CPU devices is a real, non-free native call -
/// tens of milliseconds), then serves a memoized result instantly, since
/// backend availability cannot change over a process's lifetime. Use
/// `cached_backend_info` instead from a hot path that cannot afford that
/// first-call cost.
pub fn backend_info() -> BackendInfo {
    CAPABILITY_PROBE.get_or_init(detect_capability).clone()
}

/// Non-blocking counterpart to `backend_info`: serves the memoized result
/// once warm, or a `"detecting"` placeholder before the first `backend_info`
/// call completes. `Coordinator::new` warms the real probe in the background,
/// so the placeholder should only ever surface very early in process startup.
pub fn cached_backend_info() -> BackendInfo {
    CAPABILITY_PROBE
        .get()
        .cloned()
        .unwrap_or_else(|| BackendInfo {
            backend: "detecting".to_string(),
            device: "unknown".to_string(),
        })
}

/// The backend a preference would use when no model has actually loaded yet,
/// for reporting: a pinned preference reports that backend by name (it is
/// never substituted by another), while `auto` reports the hardware-capability
/// probe. Blocks on the first call for `auto`, exactly like `backend_info`.
pub fn preferred_backend_info(preference: BackendPreference) -> BackendInfo {
    match preference {
        BackendPreference::Only(backend) => pinned_backend_info(backend),
        BackendPreference::Auto => backend_info(),
    }
}

/// Non-blocking counterpart to `preferred_backend_info`, for hot paths.
pub fn cached_preferred_backend_info(preference: BackendPreference) -> BackendInfo {
    match preference {
        BackendPreference::Only(backend) => pinned_backend_info(backend),
        BackendPreference::Auto => cached_backend_info(),
    }
}

fn pinned_backend_info(backend: transcribe_cpp::Backend) -> BackendInfo {
    BackendInfo {
        backend: format!(
            "transcribe.cpp/{}",
            selectable_backend_for(backend).map_or("unknown", |entry| entry.name)
        ),
        device: "not loaded".to_string(),
    }
}

/// Whether one backend kind can be used on this build and host.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize, PartialEq, Eq)]
pub struct BackendAvailability {
    pub kind: String,
    pub available: bool,
}

/// Every backend `InferenceEngine::load` can request, in its priority order
/// (`ACCELERATOR_PRIORITY`, then CPU), each marked available or not. A backend
/// is unavailable when its Cargo feature wasn't compiled in or the host has no
/// device for it. Blocks like `backend_info`'s first call (a native device
/// probe), so keep it off hot paths; `doctor` is the intended caller.
pub fn backend_availability() -> Vec<BackendAvailability> {
    ACCELERATOR_PRIORITY
        .iter()
        .copied()
        .chain(std::iter::once((transcribe_cpp::Backend::Cpu, "cpu")))
        .map(|(backend, kind)| BackendAvailability {
            kind: kind.to_string(),
            available: transcribe_cpp::backend_available(backend),
        })
        .collect()
}

fn detect_capability() -> BackendInfo {
    for (backend, kind) in ACCELERATOR_PRIORITY {
        if transcribe_cpp::backend_available(*backend) {
            return BackendInfo {
                backend: format!("transcribe.cpp/{kind}"),
                device: device_description(kind).unwrap_or_else(|| "unknown".to_string()),
            };
        }
    }
    BackendInfo {
        backend: "transcribe.cpp/cpu".to_string(),
        device: "CPU".to_string(),
    }
}

fn device_description(kind: &str) -> Option<String> {
    transcribe_cpp::devices()
        .into_iter()
        .find(|device| device.kind == kind)
        .map(|device| non_empty(device.description).unwrap_or(device.name))
}

/// Below this much free device memory, another process is plausibly
/// contending for the accelerator and a benchmark result on it should not be
/// trusted as a clean baseline. The whisper-small GGUF model plus its
/// KV/compute buffers need on the order of a few hundred MB; 1 GiB is a
/// conservative floor.
const MIN_FREE_ACCELERATOR_BYTES: u64 = 1024 * 1024 * 1024;

/// Flags whichever accelerator this build and host would use (per
/// `backend_info`) if it currently has suspiciously little free device
/// memory, so a benchmark run isn't reported as a clean baseline. `None` on a
/// CPU-only build or host, or when the device has headroom.
pub fn accelerator_competing_load_warning() -> Option<String> {
    for (backend, kind) in ACCELERATOR_PRIORITY {
        if !transcribe_cpp::backend_available(*backend) {
            continue;
        }
        let device = transcribe_cpp::devices()
            .into_iter()
            .find(|device| device.kind == *kind)?;
        if device.memory_total == 0 || device.memory_free >= MIN_FREE_ACCELERATOR_BYTES {
            return None;
        }
        const GIB: f64 = 1024.0 * 1024.0 * 1024.0;
        return Some(format!(
            "pre-run transcribe.cpp/{kind} device '{}' had {:.2} GiB free of {:.2} GiB total; do not use this result as a release threshold",
            device.description,
            device.memory_free as f64 / GIB,
            device.memory_total as f64 / GIB,
        ));
    }
    None
}

fn non_empty(value: String) -> Option<String> {
    (!value.is_empty()).then_some(value)
}

struct LoadedSession {
    session: transcribe_cpp::Session,
    backend: BackendInfo,
    decode_support: DecodeSupport,
    /// Whether `DecodeSupport::warn_unsupported` already logged for this
    /// load, so a dropped option is reported once rather than per dictation.
    warned_unsupported: bool,
}

/// Which of `TranscriptionConfig`'s decode options the loaded model can take,
/// probed once when it loads. `run_options` drops an option the model cannot
/// take rather than letting it fail every run.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct DecodeSupport {
    /// Accepts the Whisper run extension (initial prompt, temperature and
    /// thresholds).
    pub whisper_options: bool,
    /// Can translate into English (multilingual Whisper, not `.en`).
    pub translate: bool,
}

impl DecodeSupport {
    fn probe(model: &transcribe_cpp::Model) -> Self {
        DecodeSupport {
            whisper_options: model.supports(transcribe_cpp::Feature::InitialPrompt),
            translate: model.capabilities().supports_translate,
        }
    }

    /// Logs each configured decode option this model will ignore. Returns
    /// whether anything was logged.
    fn warn_unsupported(self, config: &crate::config::TranscriptionConfig) -> bool {
        let mut warned = false;
        if config.translate && !self.translate {
            tracing::warn!(
                "transcription.translate is set but this model cannot translate; transcribing instead"
            );
            warned = true;
        }
        let whisper_set = !config.initial_prompt.trim().is_empty()
            || crate::config::DECODE_KNOBS
                .iter()
                .any(|knob| (knob.get)(config).is_some());
        if whisper_set && !self.whisper_options {
            tracing::warn!(
                "this model takes no Whisper decode options; ignoring the vocabulary prompt and decode overrides"
            );
            warned = true;
        }
        warned
    }
}

/// The per-run options for `config`. An untouched config produces exactly
/// `RunOptions::default()` plus the language hint, so the decoder sees the
/// same inputs it did before these options existed; the Whisper extension is
/// attached only when at least one of its fields is set.
pub(crate) fn run_options(
    config: &crate::config::TranscriptionConfig,
    support: DecodeSupport,
) -> transcribe_cpp::RunOptions {
    let language = &config.language;
    let task = if config.translate && support.translate {
        transcribe_cpp::Task::Translate
    } else {
        transcribe_cpp::Task::Transcribe
    };
    let prompt = config.initial_prompt.trim();
    let whisper = transcribe_cpp::WhisperRunOptions {
        initial_prompt: (!prompt.is_empty()).then(|| prompt.to_string()),
        temperature: config.temperature.map(|value| value as f32),
        temperature_inc: config.temperature_increment.map(|value| value as f32),
        compression_ratio_thold: config.compression_ratio_threshold.map(|value| value as f32),
        logprob_thold: config.logprob_threshold.map(|value| value as f32),
        no_speech_thold: config.no_speech_threshold.map(|value| value as f32),
        ..Default::default()
    };
    let family = (support.whisper_options && whisper != Default::default())
        .then_some(transcribe_cpp::RunExtension::Whisper(whisper));
    transcribe_cpp::RunOptions {
        task,
        language: (language != "auto").then(|| language.to_string()),
        family,
        ..Default::default()
    }
}

/// The rate every buffer reaching `InferenceEngine::transcribe` is already at -
/// the live capture path resamples to it and the file path does the same - so
/// `transcription.chunk_seconds` converts to a sample count with it.
const DECODE_SAMPLE_RATE: usize = 16_000;

/// The shortest window the chunk splitter analyses when hunting for a quiet
/// boundary, in samples (10 ms at 16 kHz).
const CHUNK_PROBE_SAMPLES: usize = 160;

/// The decode window in samples for `config` on a model with `long_form`, or
/// `None` when neither chunking nor a hard cap bounds a call. Chunking bounds
/// every call to `chunk_seconds`; a hard-capped family bounds every call to
/// its cap regardless, so the file path cannot hand it an over-length buffer
/// with chunking off. When both apply the smaller window wins.
fn decode_window_samples(
    config: &crate::config::TranscriptionConfig,
    long_form: crate::catalog::LongForm,
) -> Option<usize> {
    let chunk = (config.chunk_seconds > 0)
        .then(|| (config.chunk_seconds as usize).saturating_mul(DECODE_SAMPLE_RATE));
    let cap = match long_form {
        crate::catalog::LongForm::HardCap { cap_seconds } => {
            Some((cap_seconds as usize).saturating_mul(DECODE_SAMPLE_RATE))
        }
        _ => None,
    };
    match (chunk, cap) {
        (Some(chunk), Some(cap)) => Some(chunk.min(cap)),
        (Some(chunk), None) => Some(chunk),
        (None, cap) => cap,
    }
}

/// Contiguous sample ranges covering `samples`, each at most `window` long. A
/// limit that falls before the end is nudged back to the quietest
/// `CHUNK_PROBE_SAMPLES` window in the preceding tenth of the window (at least
/// one probe long), so a split lands in a pause rather than through the middle
/// of a word. `window == 0`, or audio no longer than `window`, yields a single
/// range covering everything.
pub(crate) fn split_chunks(samples: &[f32], window: usize) -> Vec<std::ops::Range<usize>> {
    if window == 0 || samples.len() <= window {
        return std::iter::once(0..samples.len()).collect();
    }
    let search = (window / 10).max(CHUNK_PROBE_SAMPLES);
    let mut ranges = Vec::new();
    let mut start = 0;
    while start < samples.len() {
        let limit = start + window;
        if limit >= samples.len() {
            ranges.push(start..samples.len());
            break;
        }
        let low = limit
            .saturating_sub(search)
            .max(start + CHUNK_PROBE_SAMPLES);
        let split = quietest_boundary(samples, low, limit);
        ranges.push(start..split);
        start = split;
    }
    ranges
}

/// The offset in `[low, high]` whose following `CHUNK_PROBE_SAMPLES` window has
/// the least energy. Returns `high` when the range is too short to probe.
fn quietest_boundary(samples: &[f32], low: usize, high: usize) -> usize {
    if high <= low + CHUNK_PROBE_SAMPLES {
        return high;
    }
    let mut best_at = high;
    let mut best_energy = f32::INFINITY;
    let mut at = low;
    while at + CHUNK_PROBE_SAMPLES <= high {
        let energy: f32 = samples[at..at + CHUNK_PROBE_SAMPLES]
            .iter()
            .map(|sample| sample * sample)
            .sum();
        if energy < best_energy {
            best_energy = energy;
            best_at = at;
        }
        at += CHUNK_PROBE_SAMPLES;
    }
    best_at
}

/// One decode's text plus whether the model's generation budget cut it short
/// before end-of-stream. `truncated` is true only for a run the binding reports
/// as [`transcribe_cpp::Error::OutputTruncated`], so a caller can tell a
/// partial transcript from a complete one and cue the user accordingly.
#[derive(Debug, Default, PartialEq, Eq)]
pub struct Transcription {
    pub text: String,
    pub truncated: bool,
}

/// Maps a `Session::run` result to the [`Transcription`] `transcribe` returns.
/// The binding reports a decode that hit its generation budget before
/// end-of-stream as [`transcribe_cpp::Error::OutputTruncated`] carrying the
/// partial transcript, not as `Ok`. That is not a hard failure: the partial
/// text is surfaced with `truncated` set and the truncation logged, so a model
/// with a soft window (moonshine) yields the short transcript the catalog
/// documents instead of losing the dictation. Every other error stays a
/// failure.
fn run_result(
    result: transcribe_cpp::Result<transcribe_cpp::Transcript>,
) -> anyhow::Result<Transcription> {
    match result {
        Ok(transcript) => Ok(Transcription {
            text: transcript.text,
            truncated: false,
        }),
        Err(transcribe_cpp::Error::OutputTruncated { partial, .. }) => {
            let text = partial
                .map(|transcript| transcript.text)
                .unwrap_or_default();
            tracing::warn!(
                "transcription reached the model's decode limit and was truncated; keeping the {}-character partial transcript",
                text.chars().count()
            );
            Ok(Transcription {
                text,
                truncated: true,
            })
        }
        Err(error) => Err(anyhow::Error::new(error).context("transcription failed")),
    }
}

pub struct InferenceEngine {
    session: Option<LoadedSession>,
    model_path: PathBuf,
    preference: BackendPreference,
    cancel: CancelToken,
    /// The loaded model's input-length contract, recovered from the catalog by
    /// `model_path`. A hard-capped family (canary, cohere) is always decoded
    /// in windows at most its cap, so no call can exceed the limit even when
    /// `transcription.chunk_seconds` is `0`.
    long_form: crate::catalog::LongForm,
}

impl InferenceEngine {
    pub fn new(model_path: PathBuf, preference: BackendPreference) -> Self {
        Self::new_with_token(model_path, preference, CancelToken::new())
    }

    /// Build an engine that installs `cancel` on its session, so an in-flight
    /// `transcribe` can be aborted from another thread. The coordinator creates
    /// one token per run and calls `set_cancel_token` before inference starts.
    pub fn new_with_token(
        model_path: PathBuf,
        preference: BackendPreference,
        cancel: CancelToken,
    ) -> Self {
        let long_form = crate::catalog::entry_for_path(&model_path)
            .map_or(crate::catalog::LongForm::Unbounded, |entry| {
                entry.info().long_form
            });
        InferenceEngine {
            session: None,
            model_path,
            preference,
            cancel,
            long_form,
        }
    }

    /// Install `cancel` on the loaded session (if any) and retain it so it is
    /// reinstalled if the engine is reused. Cancellation is polled by the native
    /// library between decode steps and makes `transcribe` return `Error::Aborted`.
    pub fn set_cancel_token(&mut self, cancel: &CancelToken) {
        self.cancel = cancel.clone();
        if let Some(loaded) = self.session.as_mut() {
            loaded.session.set_cancel_token(cancel);
        }
    }

    pub fn load(&mut self) -> anyhow::Result<()> {
        if self.session.is_some() {
            return Ok(());
        }
        if !self.model_path.exists() {
            anyhow::bail!("model file not found: {}", self.model_path.display());
        }

        let session_options = transcribe_cpp::SessionOptions {
            n_threads: Self::cpu_threads(),
            ..Default::default()
        };

        if let BackendPreference::Only(backend) = self.preference {
            let entry = selectable_backend_for(backend)
                .expect("BackendPreference::parse only produces selectable backends");
            if let Some(reason) = backend_unavailable_reason(entry) {
                anyhow::bail!(
                    "configured inference backend '{}' is unavailable: {reason}",
                    entry.name
                );
            }
        }

        let mut last_error: Option<anyhow::Error> = None;
        for backend in self.backend_candidates() {
            let model = match transcribe_cpp::Model::load_with(
                &self.model_path,
                &transcribe_cpp::ModelOptions {
                    backend,
                    device: None,
                },
            ) {
                Ok(model) => model,
                Err(error) => {
                    if self.preference == BackendPreference::Auto
                        && backend != transcribe_cpp::Backend::Cpu
                    {
                        tracing::warn!(
                            "inference backend {backend:?} unavailable, trying next: {error:#}"
                        );
                    }
                    last_error = Some(error.into());
                    continue;
                }
            };
            match model.session_with(&session_options) {
                Ok(mut session) => {
                    session.set_cancel_token(&self.cancel);
                    self.session = Some(LoadedSession {
                        backend: describe_loaded_backend(&model),
                        decode_support: DecodeSupport::probe(&model),
                        warned_unsupported: false,
                        session,
                    });
                    return Ok(());
                }
                Err(error) => {
                    last_error = Some(error.into());
                }
            }
        }
        let error = last_error.unwrap_or_else(|| anyhow::anyhow!("no inference backend available"));
        match self.preference {
            BackendPreference::Auto => Err(error.context("failed to create inference session")),
            BackendPreference::Only(backend) => Err(error.context(format!(
                "configured inference backend '{}' failed to load the model",
                selectable_backend_for(backend).map_or("unknown", |entry| entry.name)
            ))),
        }
    }

    /// Every backend this engine will try, in order. `Auto` is the priority
    /// list ending with the unconditional, always-available explicit CPU
    /// backend; a pinned preference is that one backend alone.
    fn backend_candidates(&self) -> Vec<transcribe_cpp::Backend> {
        match self.preference {
            BackendPreference::Auto => ACCELERATOR_PRIORITY
                .iter()
                .map(|(backend, _)| *backend)
                .chain(std::iter::once(transcribe_cpp::Backend::Cpu))
                .collect(),
            BackendPreference::Only(backend) => vec![backend],
        }
    }

    pub fn unload(&mut self) {
        self.session = None;
    }

    /// Transcribe one buffer of 16 kHz mono audio, splitting it into bounded
    /// chunks first when it is longer than `transcription.chunk_seconds` or
    /// the active hard-capped model's limit. This is the one decode path
    /// shared by live dictation (`coordinator`) and the file command
    /// (`crate::transcribe`): both hand their audio to this method, which owns
    /// every `Session::run` call. Each chunk is decoded by the same loaded
    /// session and the non-empty transcripts are joined with a single space, so
    /// a long recording never sends one unbounded buffer through the decoder.
    pub fn transcribe(
        &mut self,
        audio: &[f32],
        config: &crate::config::TranscriptionConfig,
    ) -> anyhow::Result<Transcription> {
        let window = decode_window_samples(config, self.long_form);
        let loaded = self.session.as_mut().context("engine not loaded")?;
        if !loaded.warned_unsupported {
            loaded.warned_unsupported = loaded.decode_support.warn_unsupported(config);
        }
        let options = run_options(config, loaded.decode_support);
        let session = &mut loaded.session;
        match window {
            Some(window) if audio.len() > window => {
                let mut parts: Vec<String> = Vec::new();
                let mut truncated = false;
                for range in split_chunks(audio, window) {
                    let decoded = run_result(session.run(&audio[range], &options))?;
                    truncated |= decoded.truncated;
                    let text = decoded.text.trim();
                    if !text.is_empty() {
                        parts.push(text.to_string());
                    }
                }
                Ok(Transcription {
                    text: parts.join(" "),
                    truncated,
                })
            }
            _ => run_result(session.run(audio, &options)),
        }
    }

    /// The GGUF file backing the loaded engine - always the exact path passed
    /// to `new`, on every backend: fallback never substitutes a different
    /// model file or family.
    pub fn active_model_path(&self) -> Option<&Path> {
        self.session.is_some().then_some(self.model_path.as_path())
    }

    /// The backend/device that actually backed the loaded engine - the real
    /// outcome of `load`'s accelerator-then-CPU-fallback chain, as opposed to
    /// `backend_info()`'s hardware-capability probe.
    pub fn active_backend_info(&self) -> Option<BackendInfo> {
        self.session.as_ref().map(|loaded| loaded.backend.clone())
    }

    pub fn cpu_threads() -> i32 {
        cpu_thread_count(num_cpus::get_physical(), num_cpus::get())
    }

    pub fn models_dir() -> anyhow::Result<PathBuf> {
        directories::BaseDirs::new()
            .map(|b| b.data_dir().join("tonguetyped").join("models"))
            .ok_or_else(|| anyhow::anyhow!("cannot determine the user data directory"))
    }
}

/// Builds `BackendInfo` from a successfully loaded model's own resolved
/// backend/device - the ground truth for which backend actually ended up
/// bound, rather than which one was requested. Prefers `Device::kind` over
/// `Model::backend()` for the category label: the latter was observed in
/// practice to return device-indexed strings like `"Vulkan0"` (and `"CPU"`
/// uppercase for the CPU backend) rather than the clean, lowercase vendor
/// vocabulary (`"cpu"`, `"vulkan"`, `"cuda"`, `"rocm"`, `"metal"`, ...)
/// `Device::kind` documents and `detect_capability` already uses, so this
/// keeps both backend-info sources speaking the same vocabulary.
fn describe_loaded_backend(model: &transcribe_cpp::Model) -> BackendInfo {
    match model.device() {
        Ok(device) => BackendInfo {
            backend: format!("transcribe.cpp/{}", device.kind),
            device: non_empty(device.description).unwrap_or(device.name),
        },
        Err(_) => BackendInfo {
            backend: format!("transcribe.cpp/{}", model.backend().to_lowercase()),
            device: "unknown".to_string(),
        },
    }
}

fn cpu_thread_count(physical_cores: usize, logical_cpus: usize) -> i32 {
    physical_cores.min(logical_cpus) as i32
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_models_dir_returns_path() {
        let dir = InferenceEngine::models_dir().unwrap();
        assert!(dir.to_str().is_some());
    }

    #[test]
    fn cpu_thread_count_does_not_exceed_available_logical_cpus() {
        assert_eq!(cpu_thread_count(8, 16), 8);
        assert_eq!(cpu_thread_count(8, 4), 4);
    }

    const WHISPER: DecodeSupport = DecodeSupport {
        whisper_options: true,
        translate: true,
    };

    #[test]
    fn default_decode_config_produces_the_pre_existing_run_options() {
        // Exactly what `transcribe` built before the decode options existed:
        // the language hint on top of the binding's defaults, no extension.
        let config = crate::config::TranscriptionConfig::default();
        assert_eq!(
            run_options(&config, WHISPER),
            transcribe_cpp::RunOptions {
                language: None,
                ..Default::default()
            }
        );

        let english = crate::config::TranscriptionConfig {
            language: "en".to_string(),
            ..Default::default()
        };
        assert_eq!(
            run_options(&english, WHISPER),
            transcribe_cpp::RunOptions {
                language: Some("en".to_string()),
                ..Default::default()
            }
        );
    }

    #[test]
    fn whitespace_only_prompt_attaches_no_extension() {
        let config = crate::config::TranscriptionConfig {
            initial_prompt: "  \t ".to_string(),
            ..Default::default()
        };
        assert_eq!(run_options(&config, WHISPER).family, None);
    }

    #[test]
    fn decode_config_maps_onto_the_binding_options() {
        let config = crate::config::TranscriptionConfig {
            language: "de".to_string(),
            initial_prompt: "  TongueTyped, Wayland, KWin ".to_string(),
            translate: true,
            temperature: Some(0.1),
            temperature_increment: Some(0.0),
            compression_ratio_threshold: Some(f64::INFINITY),
            logprob_threshold: Some(f64::NEG_INFINITY),
            no_speech_threshold: Some(0.8),
            ..Default::default()
        };
        assert_eq!(
            run_options(&config, WHISPER),
            transcribe_cpp::RunOptions {
                task: transcribe_cpp::Task::Translate,
                language: Some("de".to_string()),
                family: Some(transcribe_cpp::RunExtension::Whisper(
                    transcribe_cpp::WhisperRunOptions {
                        initial_prompt: Some("TongueTyped, Wayland, KWin".to_string()),
                        temperature: Some(0.1),
                        temperature_inc: Some(0.0),
                        compression_ratio_thold: Some(f32::INFINITY),
                        logprob_thold: Some(f32::NEG_INFINITY),
                        no_speech_thold: Some(0.8),
                        ..Default::default()
                    }
                )),
                ..Default::default()
            }
        );
    }

    #[test]
    fn a_single_knob_attaches_only_that_override() {
        let config = crate::config::TranscriptionConfig {
            no_speech_threshold: Some(0.3),
            ..Default::default()
        };
        assert_eq!(
            run_options(&config, WHISPER).family,
            Some(transcribe_cpp::RunExtension::Whisper(
                transcribe_cpp::WhisperRunOptions {
                    no_speech_thold: Some(0.3),
                    ..Default::default()
                }
            ))
        );
    }

    #[test]
    fn options_the_model_cannot_take_are_dropped_instead_of_failing_the_run() {
        let config = crate::config::TranscriptionConfig {
            initial_prompt: "TongueTyped".to_string(),
            translate: true,
            temperature: Some(0.4),
            ..Default::default()
        };
        // An English-only Whisper model: no translation, prompt still applies.
        let english_only = DecodeSupport {
            whisper_options: true,
            translate: false,
        };
        let options = run_options(&config, english_only);
        assert_eq!(options.task, transcribe_cpp::Task::Transcribe);
        assert!(options.family.is_some());

        // A family without the Whisper extension gets none of it.
        let other_family = DecodeSupport {
            whisper_options: false,
            translate: false,
        };
        assert_eq!(
            run_options(&config, other_family),
            transcribe_cpp::RunOptions::default()
        );
    }

    #[test]
    fn a_truncated_run_keeps_the_partial_transcript_instead_of_failing() {
        let result = run_result(Err(transcribe_cpp::Error::OutputTruncated {
            message: "max_length reached".to_string(),
            partial: Some(Box::new(transcribe_cpp::Transcript {
                text: "the first half of the dictation".to_string(),
                ..Default::default()
            })),
        }))
        .unwrap();
        assert!(result.truncated);
        assert_eq!(result.text, "the first half of the dictation");
    }

    #[test]
    fn a_successful_run_returns_its_transcript_text_untruncated() {
        let result = run_result(Ok(transcribe_cpp::Transcript {
            text: "hello world".to_string(),
            ..Default::default()
        }))
        .unwrap();
        assert!(!result.truncated);
        assert_eq!(result.text, "hello world");
    }

    #[test]
    fn any_other_run_error_stays_a_failure() {
        let result = run_result(Err(transcribe_cpp::Error::OutOfMemory(
            "out of host memory".to_string(),
        )));
        assert!(result.is_err());
    }

    fn engine(preference: BackendPreference) -> InferenceEngine {
        InferenceEngine::new(PathBuf::from("irrelevant.gguf"), preference)
    }

    #[test]
    fn backend_candidates_end_with_explicit_cpu() {
        assert_eq!(
            engine(BackendPreference::Auto).backend_candidates().last(),
            Some(&transcribe_cpp::Backend::Cpu)
        );
    }

    #[test]
    fn backend_availability_lists_every_backend_in_load_order() {
        let availability = backend_availability();
        let kinds: Vec<&str> = availability.iter().map(|b| b.kind.as_str()).collect();
        assert_eq!(kinds, ["cuda", "rocm", "vulkan", "metal", "cpu"]);
        // CPU is the unconditional fallback, compiled into every build.
        assert!(availability.last().unwrap().available);
    }

    #[test]
    fn backend_candidates_never_include_auto() {
        assert!(engine(BackendPreference::Auto)
            .backend_candidates()
            .iter()
            .all(|backend| *backend != transcribe_cpp::Backend::Auto));
    }

    #[test]
    fn pinned_backend_is_the_only_candidate() {
        for name in BACKEND_PREFERENCES.iter().filter(|name| **name != "auto") {
            let preference = BackendPreference::parse(name).unwrap();
            let BackendPreference::Only(backend) = preference else {
                panic!("{name} parsed as auto");
            };
            assert_eq!(engine(preference).backend_candidates(), vec![backend]);
        }
    }

    #[test]
    fn parse_accepts_every_listed_preference_and_rejects_others() {
        assert_eq!(
            BackendPreference::parse("auto").unwrap(),
            BackendPreference::Auto
        );
        assert_eq!(
            BackendPreference::parse("cpu").unwrap(),
            BackendPreference::Only(transcribe_cpp::Backend::Cpu)
        );
        for name in BACKEND_PREFERENCES {
            BackendPreference::parse(name).unwrap();
        }
        assert!(BackendPreference::parse("cdua").is_err());
        assert!(BackendPreference::parse("CPU").is_err());
    }

    #[test]
    fn backend_choices_list_every_preference_with_cpu_and_auto_always_usable() {
        let choices = backend_choices();
        let names: Vec<&str> = choices.iter().map(|choice| choice.name).collect();
        assert_eq!(names, BACKEND_PREFERENCES);
        for choice in &choices {
            if matches!(choice.name, "auto" | "cpu") {
                assert_eq!(choice.unavailable, None, "{} should be usable", choice.name);
            }
        }
    }

    #[cfg(not(feature = "gpu-metal"))]
    #[test]
    fn pinned_backend_missing_from_the_build_fails_without_falling_back() {
        let model = std::env::temp_dir().join(format!(
            "tonguetyped-pinned-backend-{}.gguf",
            std::process::id()
        ));
        std::fs::write(&model, b"not a real model").unwrap();
        let mut engine = InferenceEngine::new(
            model.clone(),
            BackendPreference::Only(transcribe_cpp::Backend::Metal),
        );
        let error = engine.load().unwrap_err().to_string();
        std::fs::remove_file(&model).unwrap();
        assert!(error.contains("configured inference backend 'metal' is unavailable"));
        assert!(error.contains("`gpu-metal` feature"));
        assert!(engine.active_backend_info().is_none());
    }

    #[test]
    fn load_reports_a_missing_model_file_without_trying_any_backend() {
        let mut engine = InferenceEngine::new(
            PathBuf::from("/nonexistent/tonguetyped-inference-test/missing.gguf"),
            BackendPreference::Auto,
        );
        let error = engine.load().unwrap_err();
        assert!(error.to_string().contains("model file not found"));
        assert!(engine.active_model_path().is_none());
        assert!(engine.active_backend_info().is_none());
    }

    #[test]
    fn unloaded_engine_reports_no_active_model_or_backend() {
        let engine = engine(BackendPreference::Auto);
        assert!(engine.active_model_path().is_none());
        assert!(engine.active_backend_info().is_none());
    }

    #[test]
    fn pinned_preference_reports_its_own_backend_before_loading() {
        for (name, expected) in [
            ("cpu", "transcribe.cpp/cpu"),
            ("vulkan", "transcribe.cpp/vulkan"),
        ] {
            let preference = BackendPreference::parse(name).unwrap();
            for info in [
                preferred_backend_info(preference),
                cached_preferred_backend_info(preference),
            ] {
                assert_eq!(info.backend, expected);
                assert_eq!(info.device, "not loaded");
            }
        }
    }

    fn config_with_chunk_seconds(seconds: u64) -> crate::config::TranscriptionConfig {
        crate::config::TranscriptionConfig {
            chunk_seconds: seconds,
            ..Default::default()
        }
    }

    #[test]
    fn decode_window_is_the_configured_seconds_and_none_when_unbounded_and_disabled() {
        use crate::catalog::LongForm;
        assert_eq!(
            decode_window_samples(&config_with_chunk_seconds(30), LongForm::Unbounded),
            Some(30 * 16_000)
        );
        assert_eq!(
            decode_window_samples(&config_with_chunk_seconds(0), LongForm::Unbounded),
            None
        );
    }

    #[test]
    fn decode_window_is_narrowed_to_a_hard_cap_and_never_none() {
        use crate::catalog::LongForm;
        let cap = LongForm::HardCap { cap_seconds: 400 };
        // Chunking on but configured wider than the cap: the cap wins.
        assert_eq!(
            decode_window_samples(&config_with_chunk_seconds(600), cap),
            Some(400 * 16_000)
        );
        // Chunking off: the cap still bounds every call.
        assert_eq!(
            decode_window_samples(&config_with_chunk_seconds(0), cap),
            Some(400 * 16_000)
        );
        // Chunking narrower than the cap: chunking wins.
        assert_eq!(
            decode_window_samples(&config_with_chunk_seconds(30), cap),
            Some(30 * 16_000)
        );
    }

    #[test]
    fn engine_reads_a_models_hard_cap_from_its_catalog_path() {
        // Callers build the engine from `catalog::model_path`, so it recovers
        // the family's input-length contract without threading it through.
        let entry = crate::catalog::find("canary-1b-v2-f16").unwrap();
        let engine = InferenceEngine::new(PathBuf::from(entry.filename), BackendPreference::Auto);
        assert_eq!(engine.long_form, entry.info().long_form);

        let unknown =
            InferenceEngine::new(PathBuf::from("irrelevant.gguf"), BackendPreference::Auto);
        assert_eq!(unknown.long_form, crate::catalog::LongForm::Unbounded);
    }

    #[test]
    fn hard_capped_model_chunks_a_whole_file_even_when_chunking_is_off() {
        // chunk_seconds = 0 must not hand a hard-capped model one over-length
        // buffer: the engine still splits a whole file into capped windows.
        let entry = crate::catalog::find("canary-1b-v2-f16").unwrap();
        let crate::catalog::LongForm::HardCap { cap_seconds } = entry.info().long_form else {
            panic!("canary-1b-v2-f16 should be hard-capped");
        };
        let window = decode_window_samples(&config_with_chunk_seconds(0), entry.info().long_form)
            .expect("a hard-capped model always has a bounded window");
        assert_eq!(window, cap_seconds as usize * 16_000);

        let samples = vec![0.0_f32; window + 16_000];
        let ranges = split_chunks(&samples, window);
        assert!(ranges.len() > 1, "an over-cap file must be chunked");
        assert_eq!(ranges.last().unwrap().end, samples.len());
        for range in &ranges {
            assert!(
                range.end - range.start <= window,
                "chunk {range:?} exceeds the cap"
            );
        }
    }

    #[test]
    fn audio_no_longer_than_the_window_is_one_chunk() {
        let samples = vec![0.0_f32; 1_000];
        assert_eq!(
            split_chunks(&samples, 2_000),
            std::iter::once(0..1_000).collect::<Vec<_>>()
        );
        // A zero window means chunking is disabled: everything stays one range.
        assert_eq!(
            split_chunks(&samples, 0),
            std::iter::once(0..1_000).collect::<Vec<_>>()
        );
    }

    #[test]
    fn audio_past_the_window_splits_into_bounded_contiguous_chunks() {
        let window = 4_000;
        let samples = vec![1.0_f32; window * 5 + 123];
        let ranges = split_chunks(&samples, window);
        assert!(ranges.len() > 1, "long audio must be chunked");
        assert_eq!(ranges.first().unwrap().start, 0);
        assert_eq!(ranges.last().unwrap().end, samples.len());
        for (previous, range) in ranges.iter().zip(ranges.iter().skip(1)) {
            assert_eq!(previous.end, range.start, "chunks must be contiguous");
        }
        for range in &ranges {
            assert!(
                range.end - range.start <= window,
                "chunk {:?} exceeds the window",
                range
            );
        }
    }

    #[test]
    fn chunk_boundary_lands_in_the_quiet_gap_before_the_limit() {
        let window = 2_000;
        let mut samples = vec![1.0_f32; 5_000];
        // A pause straddling the first chunk's limit at sample 2_000.
        for sample in &mut samples[1_700..2_000] {
            *sample = 0.0;
        }
        let ranges = split_chunks(&samples, window);
        assert_eq!(ranges[0].start, 0);
        assert_eq!(
            ranges[0].end, 1_800,
            "the split should snap back into the quiet gap, not cut at the limit"
        );
        assert_eq!(ranges[1].start, 1_800);
    }
}
