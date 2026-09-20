use anyhow::Context;
use indicatif::{ProgressBar, ProgressStyle};
use sha2::{Digest, Sha256};
use std::path::PathBuf;
use tokio::io::AsyncWriteExt;

pub struct ModelCatalog;

impl ModelCatalog {
    pub fn model_url(model_name: &str) -> String {
        format!(
            "https://huggingface.co/ggerganov/whisper.cpp/resolve/main/{}.bin",
            model_name
        )
    }

    pub fn model_path(model_name: &str) -> PathBuf {
        crate::inference::InferenceEngine::models_dir().join(format!("{}.bin", model_name))
    }
}

pub struct DownloadManager {
    client: reqwest::Client,
}

impl DownloadManager {
    pub fn new() -> anyhow::Result<Self> {
        let client = reqwest::Client::builder()
            .timeout(std::time::Duration::from_secs(60))
            .build()?;
        Ok(DownloadManager { client })
    }

    pub async fn download(
        &self,
        model_name: &str,
        cancel_token: tokio_util::sync::CancellationToken,
    ) -> anyhow::Result<PathBuf> {
        let url = ModelCatalog::model_url(model_name);
        let dest_path = ModelCatalog::model_path(model_name);
        let tmp_path = dest_path.with_extension("download");

        let mut file = tokio::fs::OpenOptions::new()
            .create(true)
            .write(true)
            .open(&tmp_path)
            .await
            .context("failed to create download temp file")?;

        let response = self
            .client
            .get(&url)
            .send()
            .await
            .context("failed to start download")?;

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
            if cancel_token.is_cancelled() {
                pb.finish_and_clear();
                anyhow::bail!("download cancelled");
            }
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

    pub async fn verify_sha256(
        &self,
        path: &PathBuf,
        _expected_hash: &str,
    ) -> anyhow::Result<bool> {
        let data = tokio::fs::read(path)
            .await
            .context("failed to read file for verification")?;
        let mut hasher = Sha256::new();
        hasher.update(&data);
        let hash = format!("{:x}", hasher.finalize());
        Ok(hash == _expected_hash)
    }
}