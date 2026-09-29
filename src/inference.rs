use anyhow::Context;
use std::path::{Path, PathBuf};
use transcribe_rs::whisper_cpp::{WhisperEngine, WhisperInferenceParams, WhisperLoadParams};

#[cfg(any(feature = "gpu-vulkan", feature = "gpu-cuda"))]
mod gpu;

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize, PartialEq, Eq)]
pub struct BackendInfo {
    pub backend: String,
    pub device: String,
}

pub fn backend_info() -> BackendInfo {
    #[cfg(any(feature = "gpu-vulkan", feature = "gpu-cuda"))]
    if let Some(info) = gpu::probe_backend_info() {
        return info;
    }

    // The CPU path always loads with use_gpu: false (see `load`), so this is
    // never anything but the CPU backend/device.
    cpu_backend_info()
}

fn cpu_backend_info() -> BackendInfo {
    BackendInfo {
        backend: "whisper.cpp/cpu".to_string(),
        device: "CPU".to_string(),
    }
}

/// Like `backend_info`, but never blocks on a not-yet-warmed GPU device
/// probe - returns a "detecting" placeholder instead. Use this from hot
/// paths (per-dictation latency reporting) where a native GPU enumeration
/// call costing on the order of 100ms would itself distort the very
/// stop-to-idle latency being measured. `Coordinator::new` warms the real
/// probe in the background, so the placeholder should only ever surface for
/// a dictation that finishes within that narrow startup window.
#[cfg(any(feature = "gpu-vulkan", feature = "gpu-cuda"))]
pub fn cached_backend_info() -> BackendInfo {
    match gpu::cached_backend_info_if_ready() {
        Some(Some(info)) => info,
        Some(None) => cpu_backend_info(),
        None => BackendInfo {
            backend: "detecting".to_string(),
            device: "unknown".to_string(),
        },
    }
}

#[cfg(not(any(feature = "gpu-vulkan", feature = "gpu-cuda")))]
pub fn cached_backend_info() -> BackendInfo {
    cpu_backend_info()
}

/// A GPU-benchmark counterpart to a CPU competing-load check: flags a GPU
/// backend build whose device is currently short on free memory, so a
/// contended run isn't reported as a clean baseline. `None` when no GPU
/// feature is compiled in, or the device has enough headroom.
#[cfg(any(feature = "gpu-vulkan", feature = "gpu-cuda"))]
pub fn gpu_competing_load_warning() -> Option<String> {
    gpu::competing_load_warning()
}

enum Backend {
    Cpu(WhisperEngine),
    #[cfg(any(feature = "gpu-vulkan", feature = "gpu-cuda"))]
    Gpu(gpu::GpuEngine),
}

pub struct InferenceEngine {
    engine: Option<Backend>,
    model_path: PathBuf,
}

impl InferenceEngine {
    pub fn new(model_path: PathBuf) -> Self {
        InferenceEngine {
            engine: None,
            model_path,
        }
    }

    pub fn load(&mut self) -> anyhow::Result<()> {
        if self.engine.is_some() {
            return Ok(());
        }

        #[cfg(any(feature = "gpu-vulkan", feature = "gpu-cuda"))]
        match gpu::GpuEngine::load() {
            Ok(engine) => {
                self.engine = Some(Backend::Gpu(engine));
                return Ok(());
            }
            Err(err) => {
                tracing::warn!(
                    "GPU inference backend ({}) unavailable, falling back to CPU: {err:#}",
                    gpu::BACKEND_NAME
                );
            }
        }

        if !self.model_path.exists() {
            anyhow::bail!("model file not found: {}", self.model_path.display());
        }

        // Force use_gpu: false. whisper-rs otherwise auto-detects any GPU
        // backend (Vulkan/CUDA/Metal) it was linked against and prefers it,
        // which would make this "CPU fallback" only ever tested as CPU by
        // accident of which libraries happened to be present at build time -
        // exactly the ambiguity a tested fallback must not have. See the
        // slice 2 GPU benchmark writeup for the crash this caused before the
        // fix (whisper.cpp's own opportunistic Vulkan use hit an
        // out-of-device-memory abort under contended VRAM).
        let params = WhisperLoadParams {
            use_gpu: false,
            ..Default::default()
        };
        let engine = WhisperEngine::load_with_params(&self.model_path, params)
            .context("failed to create WhisperEngine")?;

        self.engine = Some(Backend::Cpu(engine));
        Ok(())
    }

    pub fn unload(&mut self) {
        self.engine = None;
    }

    pub fn transcribe(&mut self, audio: &[f32], language: &str) -> anyhow::Result<String> {
        match self.engine.as_mut().context("engine not loaded")? {
            Backend::Cpu(engine) => {
                let options = WhisperInferenceParams {
                    language: (language != "auto").then(|| language.to_string()),
                    n_threads: Self::cpu_threads(),
                    ..Default::default()
                };
                let result = engine
                    .transcribe_with(audio, &options)
                    .context("transcription failed")?;
                Ok(result.text)
            }
            #[cfg(any(feature = "gpu-vulkan", feature = "gpu-cuda"))]
            Backend::Gpu(engine) => engine.transcribe(audio, language),
        }
    }

    /// The model file actually backing the loaded engine: the GPU GGUF model
    /// when a GPU backend loaded, otherwise the CPU model passed to `new`.
    /// Benchmark tooling should hash and report this path, not the CLI's
    /// input model argument, since a GPU build may silently be running a
    /// different file than the one requested.
    pub fn active_model_path(&self) -> Option<&Path> {
        match self.engine.as_ref()? {
            Backend::Cpu(_) => Some(&self.model_path),
            #[cfg(any(feature = "gpu-vulkan", feature = "gpu-cuda"))]
            Backend::Gpu(engine) => Some(engine.model_path()),
        }
    }

    /// The backend/device that actually backed the loaded engine, as opposed
    /// to `backend_info()`'s hardware-capability probe: this reflects `load`'s
    /// real GPU-then-CPU-fallback outcome (e.g. `None` for the GPU model file,
    /// which forces the CPU fallback even on GPU-capable hardware). Blocks on
    /// an unwarmed GPU device probe like `backend_info` does; use this from
    /// callers that already tolerate that cost (e.g. `doctor`). Per-dictation
    /// latency reporting, which cannot, uses `cached_active_backend_info`
    /// instead.
    pub fn active_backend_info(&self) -> Option<BackendInfo> {
        match self.engine.as_ref()? {
            Backend::Cpu(_) => Some(cpu_backend_info()),
            #[cfg(any(feature = "gpu-vulkan", feature = "gpu-cuda"))]
            Backend::Gpu(_) => Some(gpu::probe_backend_info().unwrap_or_else(|| BackendInfo {
                backend: gpu::BACKEND_NAME.to_string(),
                device: "unknown".to_string(),
            })),
        }
    }

    /// Non-blocking counterpart to `active_backend_info`: safe to call from
    /// the stop-to-idle hot path, like `cached_backend_info`.
    pub fn cached_active_backend_info(&self) -> Option<BackendInfo> {
        match self.engine.as_ref()? {
            Backend::Cpu(_) => Some(cpu_backend_info()),
            #[cfg(any(feature = "gpu-vulkan", feature = "gpu-cuda"))]
            Backend::Gpu(_) => Some(
                gpu::cached_backend_info_if_ready()
                    .flatten()
                    .unwrap_or_else(|| BackendInfo {
                        backend: gpu::BACKEND_NAME.to_string(),
                        device: "unknown".to_string(),
                    }),
            ),
        }
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
}
