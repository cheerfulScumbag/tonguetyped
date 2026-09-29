use anyhow::Context;
use indicatif::{ProgressBar, ProgressStyle};
use std::path::PathBuf;
use tokio::io::AsyncWriteExt;

pub struct ModelCatalog;

impl ModelCatalog {
    /// The transcribe.cpp GGUF model backing the GPU inference backends
    /// (see `gpu-vulkan`/`gpu-cuda` features and `src/inference.rs`). Not
    /// listed in `model_names()`: it is an internal counterpart to the
    /// user-selected CPU model, not a user-facing choice in `setup`.
    pub const GPU_MODEL_NAME: &'static str = "whisper-small-gpu-q5_k_m";

    pub fn model_names() -> &'static [&'static str] {
        &["whisper-small-q5_1"]
    }

    pub fn model_file_name(model_name: &str) -> anyhow::Result<&'static str> {
        match model_name {
            "whisper-small-q5_1" => Ok("ggml-small-q5_1.bin"),
            Self::GPU_MODEL_NAME => Ok("whisper-small-Q5_K_M.gguf"),
            _ => anyhow::bail!("unsupported model: {model_name}"),
        }
    }

    pub fn model_url(model_name: &str) -> anyhow::Result<String> {
        if model_name == Self::GPU_MODEL_NAME {
            return Ok(format!(
                "https://huggingface.co/handy-computer/whisper-small-gguf/resolve/main/{}",
                Self::model_file_name(model_name)?
            ));
        }
        Ok(format!(
            "https://huggingface.co/ggerganov/whisper.cpp/resolve/main/{}",
            Self::model_file_name(model_name)?
        ))
    }

    pub fn model_path(model_name: &str) -> anyhow::Result<PathBuf> {
        Ok(crate::inference::InferenceEngine::models_dir()?
            .join(Self::model_file_name(model_name)?))
    }
}

pub struct DownloadManager {
    client: reqwest::Client,
}

impl DownloadManager {
    pub fn new() -> anyhow::Result<Self> {
        let client = reqwest::Client::builder()
            .connect_timeout(std::time::Duration::from_secs(30))
            .read_timeout(std::time::Duration::from_secs(60))
            .build()?;
        Ok(DownloadManager { client })
    }

    pub async fn download(&self, model_name: &str) -> anyhow::Result<PathBuf> {
        let url = ModelCatalog::model_url(model_name)?;
        let dest_path = ModelCatalog::model_path(model_name)?;
        let tmp_path = dest_path.with_extension("download");

        if dest_path.exists() {
            return Ok(dest_path);
        }
        if let Some(parent) = dest_path.parent() {
            tokio::fs::create_dir_all(parent).await?;
        }

        let mut file = tokio::fs::OpenOptions::new()
            .create(true)
            .truncate(true)
            .write(true)
            .open(&tmp_path)
            .await
            .context("failed to create download temp file")?;

        let response = self
            .client
            .get(&url)
            .send()
            .await
            .context("failed to start download")?
            .error_for_status()
            .context("model download failed")?;

        let total_size = response.content_length().unwrap_or(0);

        let pb = ProgressBar::new(total_size);
        pb.set_style(
            ProgressStyle::default_bar()
                .template("{msg} {bar:40} {bytes}/{total_bytes} ({eta})")
                .unwrap(),
        );
        pb.set_message(format!("Downloading {}", model_name));

        let mut stream = response.bytes_stream();
        let mut downloaded: u64 = 0;

        use futures_util::StreamExt;
        while let Some(chunk) = stream.next().await {
            let chunk = chunk.context("download chunk error")?;
            file.write_all(&chunk).await?;
            downloaded += chunk.len() as u64;
            pb.set_position(downloaded);
        }

        file.flush().await?;
        pb.finish_with_message(format!("Downloaded {}", model_name));

        tokio::fs::rename(&tmp_path, &dest_path)
            .await
            .context("failed to rename downloaded file")?;

        Ok(dest_path)
    }

    pub async fn ensure_vad_model(&self) -> anyhow::Result<PathBuf> {
        let path = crate::inference::InferenceEngine::models_dir()?.join("silero_vad_v4.onnx");
        if path.exists() {
            return Ok(path);
        }
        let temporary = path.with_extension("download");
        let response = self
            .client
            .get("https://raw.githubusercontent.com/cjpais/Handy/8f9cf53cd1410cda26beea39ff802ac306e39585/src-tauri/resources/models/silero_vad_v4.onnx")
            .send()
            .await?
            .error_for_status()?;
        let bytes = response.bytes().await?;
        tokio::fs::write(&temporary, bytes).await?;
        tokio::fs::rename(&temporary, &path).await?;
        Ok(path)
    }
}
