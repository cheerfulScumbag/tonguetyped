use anyhow::Context;
use std::path::PathBuf;
use transcribe_rs::whisper_cpp::{WhisperEngine, WhisperInferenceParams, WhisperLoadParams};

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

        let engine = WhisperEngine::load_with_params(
            &self.model_path,
            WhisperLoadParams {
                use_gpu: false,
                flash_attn: false,
                ..Default::default()
            },
        )
        .context("failed to create WhisperEngine")?;

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
        cpu_thread_count(num_cpus::get_physical())
    }

    pub fn models_dir() -> anyhow::Result<PathBuf> {
        directories::BaseDirs::new()
            .map(|b| b.data_dir().join("tonguetyped").join("models"))
            .ok_or_else(|| anyhow::anyhow!("cannot determine the user data directory"))
    }
}

fn cpu_thread_count(physical_cores: usize) -> i32 {
    i32::try_from(physical_cores.max(1)).unwrap_or(i32::MAX)
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
    fn cpu_thread_count_is_positive_and_bounded() {
        assert_eq!(cpu_thread_count(0), 1);
        assert_eq!(cpu_thread_count(8), 8);
        assert_eq!(cpu_thread_count(usize::MAX), i32::MAX);
    }
}
