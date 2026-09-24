use anyhow::Context;
use std::path::PathBuf;
use transcribe_rs::whisper_cpp::WhisperEngine;
use transcribe_rs::SpeechModel;

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

        let options = transcribe_rs::TranscribeOptions {
            language: (language != "auto").then(|| language.to_string()),
            ..Default::default()
        };
        let result = engine
            .transcribe(audio, &options)
            .context("transcription failed")?;

        Ok(result.text)
    }

    pub fn models_dir() -> anyhow::Result<PathBuf> {
        directories::BaseDirs::new()
            .map(|b| b.data_dir().join("tonguetyped").join("models"))
            .ok_or_else(|| anyhow::anyhow!("cannot determine the user data directory"))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_models_dir_returns_path() {
        let dir = InferenceEngine::models_dir().unwrap();
        assert!(dir.to_str().is_some());
    }
}
