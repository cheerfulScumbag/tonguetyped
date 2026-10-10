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
}

pub struct InferenceEngine {
    session: Option<LoadedSession>,
    model_path: PathBuf,
    preference: BackendPreference,
    cancel: CancelToken,
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
        InferenceEngine {
            session: None,
            model_path,
            preference,
            cancel,
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

    pub fn transcribe(&mut self, audio: &[f32], language: &str) -> anyhow::Result<String> {
        let loaded = self.session.as_mut().context("engine not loaded")?;
        let options = transcribe_cpp::RunOptions {
            language: (language != "auto").then(|| language.to_string()),
            ..Default::default()
        };
        let transcript = loaded
            .session
            .run(audio, &options)
            .context("transcription failed")?;
        Ok(transcript.text)
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
}
