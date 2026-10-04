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

use anyhow::Context;
use std::path::{Path, PathBuf};

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
}

impl InferenceEngine {
    pub fn new(model_path: PathBuf) -> Self {
        InferenceEngine {
            session: None,
            model_path,
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

        let mut last_error: Option<anyhow::Error> = None;
        for backend in Self::backend_candidates() {
            let model = match transcribe_cpp::Model::load_with(
                &self.model_path,
                &transcribe_cpp::ModelOptions {
                    backend,
                    device: None,
                },
            ) {
                Ok(model) => model,
                Err(error) => {
                    if backend != transcribe_cpp::Backend::Cpu {
                        tracing::warn!(
                            "inference backend {backend:?} unavailable, trying next: {error:#}"
                        );
                    }
                    last_error = Some(error.into());
                    continue;
                }
            };
            match model.session_with(&session_options) {
                Ok(session) => {
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
        Err(last_error.unwrap_or_else(|| anyhow::anyhow!("no inference backend available")))
            .context("failed to create inference session")
    }

    /// Every backend this engine will try, in priority order, ending with the
    /// unconditional, always-available explicit CPU backend.
    fn backend_candidates() -> impl Iterator<Item = transcribe_cpp::Backend> {
        ACCELERATOR_PRIORITY
            .iter()
            .map(|(backend, _)| *backend)
            .chain(std::iter::once(transcribe_cpp::Backend::Cpu))
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

    #[test]
    fn backend_candidates_end_with_explicit_cpu() {
        assert_eq!(
            InferenceEngine::backend_candidates().last(),
            Some(transcribe_cpp::Backend::Cpu)
        );
    }

    #[test]
    fn backend_candidates_never_include_auto() {
        assert!(InferenceEngine::backend_candidates()
            .all(|backend| backend != transcribe_cpp::Backend::Auto));
    }

    #[test]
    fn load_reports_a_missing_model_file_without_trying_any_backend() {
        let mut engine = InferenceEngine::new(PathBuf::from(
            "/nonexistent/tonguetyped-inference-test/missing.gguf",
        ));
        let error = engine.load().unwrap_err();
        assert!(error.to_string().contains("model file not found"));
        assert!(engine.active_model_path().is_none());
        assert!(engine.active_backend_info().is_none());
    }

    #[test]
    fn unloaded_engine_reports_no_active_model_or_backend() {
        let engine = InferenceEngine::new(PathBuf::from("irrelevant.gguf"));
        assert!(engine.active_model_path().is_none());
        assert!(engine.active_backend_info().is_none());
    }
}
