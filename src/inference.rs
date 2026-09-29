use anyhow::Context;
use std::path::PathBuf;
use transcribe_rs::whisper_cpp::{WhisperEngine, WhisperInferenceParams};

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize, PartialEq, Eq)]
pub struct BackendInfo {
    pub backend: String,
    pub device: String,
}

pub fn backend_info() -> BackendInfo {
    let devices = transcribe_rs::whisper_cpp::gpu::list_gpu_devices();
    match devices.into_iter().max_by_key(|device| {
        let kind = match device.kind {
            transcribe_rs::whisper_cpp::gpu::GpuKind::Dedicated => 1,
            transcribe_rs::whisper_cpp::gpu::GpuKind::Integrated => 0,
        };
        (kind, device.total_vram)
    }) {
        Some(device) => BackendInfo {
            backend: "whisper.cpp/gpu".to_string(),
            device: device.name,
        },
        None => BackendInfo {
            backend: "whisper.cpp/cpu".to_string(),
            device: "CPU".to_string(),
        },
    }
}

pub struct InferenceEngine {
    engine: Option<WhisperEngine>,
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

        if !self.model_path.exists() {
            anyhow::bail!("model file not found: {}", self.model_path.display());
        }

        let engine =
            WhisperEngine::load(&self.model_path).context("failed to create WhisperEngine")?;

        self.engine = Some(engine);
        Ok(())
    }

    pub fn unload(&mut self) {
        self.engine = None;
    }

    pub fn transcribe(&mut self, audio: &[f32], language: &str) -> anyhow::Result<String> {
        let engine = self.engine.as_mut().context("engine not loaded")?;

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
