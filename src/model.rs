use anyhow::Context;
use indicatif::{ProgressBar, ProgressStyle};
use sha2::{Digest, Sha256};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use tokio::io::{AsyncReadExt, AsyncSeekExt, AsyncWriteExt};

/// Reports `(downloaded_bytes, expected_total_bytes)` after each chunk of a
/// model download. Passing `None` to a download method keeps the default
/// indicatif terminal bar (`tonguetyped model install`, scripted `setup`);
/// a caller that cannot share a terminal with indicatif - the Ratatui setup
/// console, which owns the alternate screen - passes `Some` instead and
/// draws its own progress from the callback.
pub type ProgressCallback = Arc<dyn Fn(u64, u64) + Send + Sync>;

pub struct DownloadManager {
    client: reqwest::Client,
}

/// Outcome of a resumable, verified download.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DownloadOutcome {
    /// The destination file already existed; nothing was downloaded.
    AlreadyInstalled,
    /// A previously interrupted download was resumed to completion.
    Resumed,
    /// The file was downloaded from scratch.
    Fresh,
}

impl DownloadManager {
    pub fn new() -> anyhow::Result<Self> {
        let client = reqwest::Client::builder()
            .connect_timeout(std::time::Duration::from_secs(30))
            .read_timeout(std::time::Duration::from_secs(60))
            .build()?;
        Ok(DownloadManager { client })
    }

    /// Downloads a catalog model (see `crate::catalog`), verifying it against
    /// its pinned size and SHA-256 once complete. Resumes an interrupted
    /// download when a partial `.download` temp file is already present.
    pub async fn install_catalog_model(
        &self,
        id: &str,
        on_progress: Option<ProgressCallback>,
    ) -> anyhow::Result<(PathBuf, DownloadOutcome)> {
        let entry = crate::catalog::find(id)
            .ok_or_else(|| anyhow::anyhow!("unknown catalog model: {id}"))?;
        let dest_path = crate::catalog::model_path(id)?;
        self.fetch_verified(
            &entry.download_url(),
            &dest_path,
            entry.size_bytes,
            entry.sha256,
            entry.id,
            on_progress,
        )
        .await
    }

    async fn fetch_verified(
        &self,
        url: &str,
        dest_path: &Path,
        expected_size: u64,
        expected_sha256: &str,
        label: &str,
        on_progress: Option<ProgressCallback>,
    ) -> anyhow::Result<(PathBuf, DownloadOutcome)> {
        if dest_path.exists() {
            return Ok((dest_path.to_path_buf(), DownloadOutcome::AlreadyInstalled));
        }
        if let Some(parent) = dest_path.parent() {
            tokio::fs::create_dir_all(parent).await?;
        }
        let tmp_path = dest_path.with_extension("download");

        let mut resume_from = match tokio::fs::metadata(&tmp_path).await {
            Ok(metadata) => metadata.len(),
            Err(_) => 0,
        };
        // A stale partial download can never be larger than the pinned size;
        // treat that as corruption rather than trusting a Range request built
        // from it.
        if resume_from >= expected_size {
            resume_from = 0;
        }

        let mut request = self.client.get(url);
        if resume_from > 0 {
            request = request.header(reqwest::header::RANGE, format!("bytes={resume_from}-"));
        }
        let response = request.send().await.context("failed to start download")?;
        let resuming = resume_from > 0 && response.status() == reqwest::StatusCode::PARTIAL_CONTENT;
        // The server can legally ignore a Range request and send the whole
        // file back with 200 OK; when that happens, restart clean rather than
        // appending a full response onto existing bytes.
        if resume_from > 0 && !resuming {
            resume_from = 0;
        }
        let response = response
            .error_for_status()
            .context("model download failed")?;

        let mut file = tokio::fs::OpenOptions::new()
            .create(true)
            .write(true)
            .truncate(!resuming)
            .open(&tmp_path)
            .await
            .context("failed to open download temp file")?;
        if resuming {
            file.seek(std::io::SeekFrom::Start(resume_from)).await?;
        }

        // A caller with its own terminal UI (the setup console's alternate
        // screen) passes `on_progress` and draws its own indicator; indicatif's
        // bar would otherwise write over that screen since both share the same
        // terminal regardless of which fd they target.
        let pb = on_progress.is_none().then(|| {
            let pb = ProgressBar::new(expected_size);
            pb.set_style(
                ProgressStyle::default_bar()
                    .template("{msg} {bar:40} {bytes}/{total_bytes} ({eta})")
                    .unwrap(),
            );
            pb.set_message(format!(
                "{} {label}",
                if resuming { "Resuming" } else { "Downloading" }
            ));
            pb.set_position(resume_from);
            pb
        });

        let mut stream = response.bytes_stream();
        let mut downloaded = resume_from;

        use futures_util::StreamExt;
        while let Some(chunk) = stream.next().await {
            let chunk = chunk.context("download chunk error")?;
            file.write_all(&chunk).await?;
            downloaded += chunk.len() as u64;
            if let Some(pb) = &pb {
                pb.set_position(downloaded);
            }
            if let Some(callback) = &on_progress {
                callback(downloaded, expected_size);
            }
        }
        file.flush().await?;
        drop(file);
        if let Some(pb) = &pb {
            pb.finish_with_message(format!("Downloaded {label}, verifying checksum"));
        }

        if let Err(error) = verify_sha256(&tmp_path, expected_sha256).await {
            let _ = tokio::fs::remove_file(&tmp_path).await;
            return Err(error);
        }

        tokio::fs::rename(&tmp_path, dest_path)
            .await
            .context("failed to rename downloaded file")?;

        Ok((
            dest_path.to_path_buf(),
            if resuming {
                DownloadOutcome::Resumed
            } else {
                DownloadOutcome::Fresh
            },
        ))
    }

    /// Removes a locally installed catalog model. Returns `false` if it was
    /// not installed.
    pub fn remove_catalog_model(id: &str) -> anyhow::Result<bool> {
        let path = crate::catalog::model_path(id)?;
        if !path.exists() {
            return Ok(false);
        }
        std::fs::remove_file(&path)
            .with_context(|| format!("failed to remove {}", path.display()))?;
        Ok(true)
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

async fn verify_sha256(path: &Path, expected: &str) -> anyhow::Result<()> {
    let mut file = tokio::fs::File::open(path)
        .await
        .context("failed to open downloaded file for verification")?;
    let mut hasher = Sha256::new();
    // A large fixed-size buffer inside an `async fn` bloats the generated
    // state machine (it's stored inline across the `.await` points below,
    // not on the heap) - 64 KiB matches the size already used for the same
    // job in `examples/transcribe_benchmark.rs`'s synchronous hasher.
    let mut buffer = [0_u8; 64 * 1024];
    loop {
        let read = file.read(&mut buffer).await?;
        if read == 0 {
            break;
        }
        hasher.update(&buffer[..read]);
    }
    let actual = format!("{:x}", hasher.finalize());
    if actual != expected {
        anyhow::bail!(
            "downloaded file failed SHA-256 verification: expected {expected}, got {actual}"
        );
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;
    use tokio::io::{AsyncBufReadExt, BufReader};
    use tokio::net::TcpListener;

    /// A minimal single-connection HTTP/1.1 server for exercising the
    /// resumable-download path: understands only `GET` and an optional
    /// `Range: bytes=N-` request header, ignores everything else.
    async fn serve_one(listener: TcpListener, body: Arc<Vec<u8>>, support_range: bool) {
        let (socket, _) = listener.accept().await.unwrap();
        let (read_half, mut write_half) = socket.into_split();
        let mut reader = BufReader::new(read_half);

        let mut range_start: Option<u64> = None;
        loop {
            let mut line = String::new();
            let n = reader.read_line(&mut line).await.unwrap();
            if n == 0 || line == "\r\n" {
                break;
            }
            if let Some(rest) = line
                .trim_end()
                .strip_prefix("Range: bytes=")
                .or_else(|| line.trim_end().strip_prefix("range: bytes="))
            {
                let start = rest.trim_end_matches('-').parse::<u64>().unwrap();
                range_start = Some(start);
            }
        }

        let (status, payload, extra_headers) = match (support_range, range_start) {
            (true, Some(start)) if (start as usize) < body.len() => {
                let slice = &body[start as usize..];
                (
                    "206 Partial Content",
                    slice.to_vec(),
                    format!(
                        "Content-Range: bytes {}-{}/{}\r\n",
                        start,
                        body.len() - 1,
                        body.len()
                    ),
                )
            }
            _ => ("200 OK", body.to_vec(), String::new()),
        };

        let header = format!(
            "HTTP/1.1 {status}\r\nContent-Length: {}\r\n{extra_headers}Connection: close\r\n\r\n",
            payload.len()
        );
        write_half.write_all(header.as_bytes()).await.unwrap();
        write_half.write_all(&payload).await.unwrap();
        write_half.flush().await.unwrap();
    }

    async fn spawn_server(body: Vec<u8>, support_range: bool) -> String {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let body = Arc::new(body);
        tokio::spawn(serve_one(listener, body, support_range));
        format!("http://{addr}")
    }

    fn sha256_hex(data: &[u8]) -> String {
        let mut hasher = Sha256::new();
        hasher.update(data);
        format!("{:x}", hasher.finalize())
    }

    fn models_root(tag: &str) -> PathBuf {
        std::env::temp_dir().join(format!(
            "tonguetyped-model-test-{tag}-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ))
    }

    #[tokio::test]
    async fn fresh_download_is_verified_and_renamed_into_place() {
        let body = b"hello tonguetyped model bytes".to_vec();
        let expected_sha256 = sha256_hex(&body);
        let url = spawn_server(body.clone(), true).await;

        let root = models_root("fresh");
        std::fs::create_dir_all(&root).unwrap();
        let dest = root.join("model.bin");

        let manager = DownloadManager::new().unwrap();
        let (path, outcome) = manager
            .fetch_verified(
                &format!("{url}/file"),
                &dest,
                body.len() as u64,
                &expected_sha256,
                "test-model",
                None,
            )
            .await
            .unwrap();

        assert_eq!(path, dest);
        assert_eq!(outcome, DownloadOutcome::Fresh);
        assert_eq!(std::fs::read(&dest).unwrap(), body);
        assert!(!dest.with_extension("download").exists());
        std::fs::remove_dir_all(root).unwrap();
    }

    #[tokio::test]
    async fn interrupted_download_resumes_from_partial_file() {
        let body = b"0123456789abcdefghijklmnopqrstuvwxyz".to_vec();
        let expected_sha256 = sha256_hex(&body);
        let url = spawn_server(body.clone(), true).await;

        let root = models_root("resume");
        std::fs::create_dir_all(&root).unwrap();
        let dest = root.join("model.bin");
        let tmp = dest.with_extension("download");
        std::fs::write(&tmp, &body[..10]).unwrap();

        let manager = DownloadManager::new().unwrap();
        let (path, outcome) = manager
            .fetch_verified(
                &format!("{url}/file"),
                &dest,
                body.len() as u64,
                &expected_sha256,
                "test-model",
                None,
            )
            .await
            .unwrap();

        assert_eq!(path, dest);
        assert_eq!(outcome, DownloadOutcome::Resumed);
        assert_eq!(std::fs::read(&dest).unwrap(), body);
        std::fs::remove_dir_all(root).unwrap();
    }

    #[tokio::test]
    async fn server_ignoring_range_restarts_cleanly() {
        let body = b"full response every time, no partial content".to_vec();
        let expected_sha256 = sha256_hex(&body);
        // support_range = false: server always answers 200 with the full body.
        let url = spawn_server(body.clone(), false).await;

        let root = models_root("no-range");
        std::fs::create_dir_all(&root).unwrap();
        let dest = root.join("model.bin");
        let tmp = dest.with_extension("download");
        std::fs::write(&tmp, b"stale partial junk").unwrap();

        let manager = DownloadManager::new().unwrap();
        let (path, outcome) = manager
            .fetch_verified(
                &format!("{url}/file"),
                &dest,
                body.len() as u64,
                &expected_sha256,
                "test-model",
                None,
            )
            .await
            .unwrap();

        assert_eq!(path, dest);
        assert_eq!(outcome, DownloadOutcome::Fresh);
        assert_eq!(std::fs::read(&dest).unwrap(), body);
        std::fs::remove_dir_all(root).unwrap();
    }

    #[tokio::test]
    async fn checksum_mismatch_is_rejected_and_cleans_up_the_temp_file() {
        let body = b"this is not what we expected".to_vec();
        let url = spawn_server(body.clone(), true).await;

        let root = models_root("bad-hash");
        std::fs::create_dir_all(&root).unwrap();
        let dest = root.join("model.bin");

        let manager = DownloadManager::new().unwrap();
        let result = manager
            .fetch_verified(
                &format!("{url}/file"),
                &dest,
                body.len() as u64,
                "0000000000000000000000000000000000000000000000000000000000000000",
                "test-model",
                None,
            )
            .await;

        assert!(result.is_err());
        assert!(!dest.exists());
        assert!(!dest.with_extension("download").exists());
        std::fs::remove_dir_all(root).unwrap();
    }

    #[tokio::test]
    async fn already_installed_model_is_not_redownloaded() {
        let root = models_root("already-installed");
        std::fs::create_dir_all(&root).unwrap();
        let dest = root.join("model.bin");
        std::fs::write(&dest, b"already here").unwrap();

        let manager = DownloadManager::new().unwrap();
        // No server is listening on this address; a redownload attempt would
        // fail to connect, proving this path never sends a request.
        let (path, outcome) = manager
            .fetch_verified(
                "http://127.0.0.1:1",
                &dest,
                12,
                "irrelevant",
                "test-model",
                None,
            )
            .await
            .unwrap();

        assert_eq!(path, dest);
        assert_eq!(outcome, DownloadOutcome::AlreadyInstalled);
        std::fs::remove_dir_all(root).unwrap();
    }

    #[tokio::test]
    async fn oversized_partial_file_is_treated_as_corrupt_and_restarted() {
        // Guards the `resume_from >= expected_size` clamp: a partial file
        // larger than the pinned size can only be corruption (e.g. the
        // catalog was repinned to a smaller file), never a legitimate resume.
        let body = b"short".to_vec();
        let expected_sha256 = sha256_hex(&body);
        let url = spawn_server(body.clone(), true).await;

        let root = models_root("oversized-partial");
        std::fs::create_dir_all(&root).unwrap();
        let dest = root.join("model.bin");
        let tmp = dest.with_extension("download");
        std::fs::write(&tmp, vec![0_u8; 1000]).unwrap();

        let manager = DownloadManager::new().unwrap();
        let (path, outcome) = manager
            .fetch_verified(
                &format!("{url}/file"),
                &dest,
                body.len() as u64,
                &expected_sha256,
                "test-model",
                None,
            )
            .await
            .unwrap();

        assert_eq!(path, dest);
        assert_eq!(outcome, DownloadOutcome::Fresh);
        assert_eq!(std::fs::read(&dest).unwrap(), body);
        std::fs::remove_dir_all(root).unwrap();
    }
}
