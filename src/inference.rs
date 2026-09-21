use anyhow::Context;
use std::path::PathBuf;
use transcribe_rs::whisper_cpp::WhisperEngine;
use transcribe_rs::SpeechModel;

pub struct InferenceEngine {
    engine: Option<WhisperEngine>,
    model_path: PathBuf,
    loaded: bool,
}

impl InferenceEngine {
    pub fn new(model_path: PathBuf) -> Self {
        InferenceEngine {
            engine: None,
            model_path,
            loaded: false,
        }
    }

    pub fn load(&mut self) -> anyhow::Result<()> {
        if self.loaded {
            return Ok(());
        }

        if !self.model_path.exists() {
            anyhow::bail!("model file not found: {}", self.model_path.display());
        }

        let engine =
            WhisperEngine::load(&self.model_path).context("failed to create WhisperEngine")?;

        self.engine = Some(engine);
        self.loaded = true;
        Ok(())
    }

    pub fn unload(&mut self) {
        self.engine = None;
        self.loaded = false;
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

    pub fn models_dir() -> PathBuf {
        let dir = directories::BaseDirs::new()
            .map(|b| b.data_dir().join("tonguetyped").join("models"))
            .unwrap_or_else(|| PathBuf::from(".local/share/tonguetyped/models"));
        std::fs::create_dir_all(&dir).ok();
        dir
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_models_dir_returns_path() {
        let dir = InferenceEngine::models_dir();
        assert!(dir.to_str().is_some());
    }
}
