//! GPU inference backend built on the `transcribe-cpp` Rust binding for
//! [transcribe.cpp](https://github.com/handy-computer/transcribe.cpp). Only
//! compiled when the `gpu-vulkan` or `gpu-cuda` feature is enabled; the caller
//! in `inference.rs` falls back to the tested CPU path whenever this backend
//! is unavailable or fails to load. If both features are enabled, CUDA takes
//! priority over Vulkan.

use anyhow::Context;
use std::path::{Path, PathBuf};

#[cfg(feature = "gpu-cuda")]
pub const BACKEND: transcribe_cpp::Backend = transcribe_cpp::Backend::Cuda;
#[cfg(feature = "gpu-cuda")]
pub const BACKEND_NAME: &str = "transcribe.cpp/cuda";
#[cfg(feature = "gpu-cuda")]
const DEVICE_KIND: &str = "cuda";

#[cfg(all(feature = "gpu-vulkan", not(feature = "gpu-cuda")))]
pub const BACKEND: transcribe_cpp::Backend = transcribe_cpp::Backend::Vulkan;
#[cfg(all(feature = "gpu-vulkan", not(feature = "gpu-cuda")))]
pub const BACKEND_NAME: &str = "transcribe.cpp/vulkan";
#[cfg(all(feature = "gpu-vulkan", not(feature = "gpu-cuda")))]
const DEVICE_KIND: &str = "vulkan";

pub struct GpuEngine {
    session: transcribe_cpp::Session,
    model_path: PathBuf,
}

impl GpuEngine {
    pub fn load(gpu_model_id: &str) -> anyhow::Result<Self> {
        let model_path =
            crate::catalog::model_path(gpu_model_id).context("failed to resolve GPU model path")?;
        if !model_path.exists() {
            anyhow::bail!("GPU model file not found: {}", model_path.display());
        }

        let options = transcribe_cpp::ModelOptions {
            backend: BACKEND,
            device: None,
        };
        let model = transcribe_cpp::Model::load_with(&model_path, &options)
            .with_context(|| format!("failed to load {BACKEND_NAME} model"))?;

        let session_options = transcribe_cpp::SessionOptions {
            n_threads: super::InferenceEngine::cpu_threads(),
            ..Default::default()
        };
        let session = model
            .session_with(&session_options)
            .context("failed to create transcribe.cpp session")?;

        Ok(GpuEngine {
            session,
            model_path,
        })
    }

    pub fn model_path(&self) -> &Path {
        &self.model_path
    }

    pub fn transcribe(&mut self, audio: &[f32], language: &str) -> anyhow::Result<String> {
        let options = transcribe_cpp::RunOptions {
            language: (language != "auto").then(|| language.to_string()),
            ..Default::default()
        };
        let transcript = self
            .session
            .run(audio, &options)
            .context("GPU transcription failed")?;
        Ok(transcript.text)
    }
}

/// Below this much free device memory, another process is plausibly
/// contending for the GPU and a benchmark result on it should not be trusted
/// as a clean baseline. The whisper-small GGUF model plus its KV/compute
/// buffers need on the order of a few hundred MB; 1 GiB is a conservative
/// floor.
const MIN_FREE_BYTES: u64 = 1024 * 1024 * 1024;

/// Mirrors the CPU benchmark's `competing_load_warning`: flags a GPU that is
/// short on free memory right now, so a contended run isn't reported as a
/// clean baseline.
pub fn competing_load_warning() -> Option<String> {
    let device = transcribe_cpp::devices()
        .into_iter()
        .find(|device| device.kind == DEVICE_KIND)?;
    if device.memory_total == 0 || device.memory_free >= MIN_FREE_BYTES {
        return None;
    }
    const GIB: f64 = 1024.0 * 1024.0 * 1024.0;
    Some(format!(
        "pre-run {BACKEND_NAME} device '{}' had {:.2} GiB free of {:.2} GiB total; do not use this result as a release threshold",
        device.description,
        device.memory_free as f64 / GIB,
        device.memory_total as f64 / GIB,
    ))
}

static CACHED_BACKEND_INFO: std::sync::OnceLock<Option<super::BackendInfo>> =
    std::sync::OnceLock::new();

fn detect_backend_info() -> Option<super::BackendInfo> {
    if !transcribe_cpp::backend_available(BACKEND) {
        return None;
    }
    let device = transcribe_cpp::devices()
        .into_iter()
        .find(|device| device.kind == DEVICE_KIND);
    Some(super::BackendInfo {
        backend: BACKEND_NAME.to_string(),
        device: device
            .map(|d| d.description)
            .unwrap_or_else(|| "unknown".to_string()),
    })
}

/// Backend/device probe used for diagnostics (`backend_info`): checks device
/// registration without loading model weights. Blocks on the native call the
/// first time (measured well into the tens of milliseconds - unlike the CPU
/// path's device probe, enumerating Vulkan/CUDA devices is not free), then
/// serves the memoized result instantly since backend availability cannot
/// change over a process's lifetime. Use `cached_backend_info` instead from
/// any hot path that cannot afford that first-call cost.
pub fn probe_backend_info() -> Option<super::BackendInfo> {
    CACHED_BACKEND_INFO.get_or_init(detect_backend_info).clone()
}

/// Non-blocking counterpart to `probe_backend_info`: `Some` once the cache is
/// warm (from a prior `probe_backend_info` call, e.g. `Coordinator::new`'s
/// background warm-up, or `doctor`/daemon startup), `None` while detection
/// hasn't completed yet.
pub fn cached_backend_info_if_ready() -> Option<Option<super::BackendInfo>> {
    CACHED_BACKEND_INFO.get().cloned()
}
