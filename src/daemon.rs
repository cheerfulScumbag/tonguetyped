use crate::config::Config;
use crate::coordinator::{Coordinator, CoordinatorCommand, CoordinatorResponse};
use crate::ipc::{decode_frame, encode_frame, Request, Response};
use std::fs::{File, OpenOptions};
use std::io::Write;
use std::path::PathBuf;
use std::sync::Arc;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::net::{UnixListener, UnixStream};

pub fn runtime_dir() -> anyhow::Result<PathBuf> {
    let runtime = std::env::var_os("XDG_RUNTIME_DIR")
        .filter(|value| !value.is_empty())
        .ok_or_else(|| anyhow::anyhow!("XDG_RUNTIME_DIR is not set"))?;
    let dir = PathBuf::from(runtime).join("tonguetyped");
    std::fs::create_dir_all(&dir)?;
    Ok(dir)
}

pub fn socket_path() -> anyhow::Result<PathBuf> {
    Ok(runtime_dir()?.join("control.sock"))
}

pub fn lock_path() -> anyhow::Result<PathBuf> {
    Ok(runtime_dir()?.join("daemon.lock"))
}

pub async fn run_daemon(config: Config) -> anyhow::Result<()> {
    let sock_path = socket_path()?;
    let _lock = acquire_instance_lock(&sock_path)?;
    prepare_dependencies(&config).await?;

    let coordinator = Arc::new(Coordinator::new(config)?);
    let listener = UnixListener::bind(&sock_path)?;
    let (ready_tx, ready_rx) = tokio::sync::oneshot::channel();
    let activation = coordinator.clone();
    let activation_status = coordinator.clone();
    let keybind = activation.activation_keybind();
    tokio::spawn(async move {
        let listener = tokio::spawn(crate::activation::listen(activation, keybind, ready_tx));
        match ready_rx.await {
            Ok(Ok(())) => activation_status.set_runtime_ready(),
            Ok(Err(error)) => activation_status.set_runtime_error(error),
            Err(_) => {}
        }
        match listener.await {
            Ok(Err(error)) => {
                tracing::error!("activation listener failed: {error}");
                activation_status.set_runtime_error(format!("activation listener failed: {error}"));
            }
            Ok(Ok(())) => activation_status
                .set_runtime_error("activation listener stopped unexpectedly".to_string()),
            Err(error) => activation_status
                .set_runtime_error(format!("activation listener task failed: {error}")),
        }
    });

    tracing::info!("daemon listening on {}", sock_path.display());

    loop {
        let (stream, _addr) = listener.accept().await?;
        let coord = coordinator.clone();
        tokio::spawn(async move {
            if let Err(e) = handle_connection(stream, coord).await {
                tracing::error!("connection error: {}", e);
            }
        });
    }
}

async fn prepare_dependencies(config: &Config) -> anyhow::Result<()> {
    let download_manager = crate::model::DownloadManager::new()?;
    if !crate::model::ModelCatalog::model_path(&config.model.selected)?.exists() {
        download_manager.download(&config.model.selected).await?;
    }
    if config.transcription.vad_enabled {
        download_manager.ensure_vad_model().await?;
    }
    Ok(())
}

struct InstanceLock {
    _file: File,
}

fn acquire_instance_lock(sock_path: &std::path::Path) -> anyhow::Result<InstanceLock> {
    let path = lock_path()?;
    acquire_instance_lock_at(&path, sock_path)
}

fn acquire_instance_lock_at(
    path: &std::path::Path,
    sock_path: &std::path::Path,
) -> anyhow::Result<InstanceLock> {
    let mut file = OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .open(path)?;
    file.try_lock()
        .map_err(|_| anyhow::anyhow!("daemon is already running"))?;
    if sock_path.exists() {
        if std::os::unix::net::UnixStream::connect(sock_path).is_ok() {
            anyhow::bail!("daemon is already running");
        }
        std::fs::remove_file(sock_path)?;
    }
    file.set_len(0)?;
    writeln!(file, "{}", std::process::id())?;
    file.sync_all()?;
    Ok(InstanceLock { _file: file })
}

async fn handle_connection(
    stream: UnixStream,
    coordinator: Arc<Coordinator>,
) -> anyhow::Result<()> {
    let (reader, mut writer) = stream.into_split();
    let mut reader = BufReader::new(reader);
    let mut line = String::new();

    loop {
        line.clear();
        let n = reader.read_line(&mut line).await?;
        if n == 0 {
            break;
        }

        let request: Request = match decode_frame(&line) {
            Ok(req) => req,
            Err(e) => {
                let resp = Response::Error {
                    message: format!("invalid request: {}", e),
                };
                let frame = encode_frame(&resp)?;
                writer.write_all(frame.as_bytes()).await?;
                continue;
            }
        };

        let response = dispatch(&coordinator, request).await;
        let frame = encode_frame(&response)?;
        writer.write_all(frame.as_bytes()).await?;
    }

    Ok(())
}

pub async fn dispatch(coordinator: &Arc<Coordinator>, request: Request) -> Response {
    let cmd = match request {
        Request::Start => CoordinatorCommand::Start,
        Request::Stop => CoordinatorCommand::Stop,
        Request::Toggle => CoordinatorCommand::Toggle,
        Request::Cancel => CoordinatorCommand::Cancel,
        Request::Status => CoordinatorCommand::GetStatus,
        Request::ReloadConfig => match Config::reload() {
            Ok(config) => match coordinator.validate_reload(&config).map(|()| config) {
                Ok(config) => match prepare_dependencies(&config)
                    .await
                    .and_then(|()| coordinator.reload_config(config))
                {
                    Ok(()) => return Response::Ok,
                    Err(e) => {
                        return Response::Error {
                            message: format!("config reload failed: {}", e),
                        }
                    }
                },
                Err(e) => {
                    return Response::Error {
                        message: format!("config reload failed: {}", e),
                    }
                }
            },
            Err(e) => {
                return Response::Error {
                    message: format!("config reload failed: {}", e),
                };
            }
        },
        Request::GetLastResult => {
            let result = coordinator.get_last_result();
            match result {
                Some((text, ts)) => {
                    return Response::LastResult {
                        text,
                        timestamp: ts,
                    };
                }
                None => {
                    return Response::Error {
                        message: "no last result available".to_string(),
                    };
                }
            }
        }
    };

    match coordinator.handle_command(cmd).await {
        Ok(resp) => coordinator_response_to_ipc(resp),
        Err(e) => Response::Error {
            message: format!("command failed: {}", e),
        },
    }
}

fn coordinator_response_to_ipc(resp: CoordinatorResponse) -> Response {
    match resp {
        CoordinatorResponse::Ok => Response::Ok,
        CoordinatorResponse::Busy => Response::Busy,
        CoordinatorResponse::RecordingStarted => Response::RecordingStarted,
        CoordinatorResponse::RecordingStopped => Response::RecordingStopped,
        CoordinatorResponse::Cancelled => Response::Cancelled,
        CoordinatorResponse::Ignored(reason) => Response::Error {
            message: format!("command ignored: {}", reason),
        },
        CoordinatorResponse::Status {
            state,
            activation_mode,
            operation_error,
            shortcut_status,
            activation_error,
        } => Response::Status {
            state,
            activation_mode,
            operation_error,
            shortcut_status,
            activation_error,
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn instance_lock_cannot_be_stolen_during_startup() {
        let root = std::env::temp_dir().join(format!(
            "tonguetyped-lock-test-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir_all(&root).unwrap();
        let lock_path = root.join("daemon.lock");
        let socket_path = root.join("control.sock");

        let first = acquire_instance_lock_at(&lock_path, &socket_path).unwrap();
        assert!(acquire_instance_lock_at(&lock_path, &socket_path).is_err());
        drop(first);
        assert!(acquire_instance_lock_at(&lock_path, &socket_path).is_ok());

        std::fs::remove_dir_all(root).unwrap();
    }
}
