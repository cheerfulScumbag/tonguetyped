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
    let (ready_tx, ready_rx) = tokio::sync::oneshot::channel();
    let activation = coordinator.clone();
    let keybind = activation.activation_keybind();
    let mut activation_task = tokio::spawn(async move {
        if let Err(error) = crate::activation::listen(activation, keybind, ready_tx).await {
            tracing::error!("activation listener failed: {error}");
        }
    });
    ready_rx
        .await
        .map_err(|_| anyhow::anyhow!("activation listener exited during startup"))?
        .map_err(anyhow::Error::msg)?;
    let listener = UnixListener::bind(&sock_path)?;

    tracing::info!("daemon listening on {}", sock_path.display());

    loop {
        let (stream, _addr) = tokio::select! {
            result = listener.accept() => result?,
            result = &mut activation_task => {
                match result {
                    Ok(()) => anyhow::bail!("activation listener exited"),
                    Err(error) => return Err(error.into()),
                }
            }
        };
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
    path: PathBuf,
    _file: File,
}

impl Drop for InstanceLock {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.path);
    }
}

fn acquire_instance_lock(sock_path: &std::path::Path) -> anyhow::Result<InstanceLock> {
    let path = lock_path()?;
    for _ in 0..2 {
        match OpenOptions::new().write(true).create_new(true).open(&path) {
            Ok(mut file) => {
                let pid = std::process::id();
                let started = process_start_time(pid)
                    .ok_or_else(|| anyhow::anyhow!("failed to read daemon process identity"))?;
                writeln!(file, "{pid} {started}")?;
                file.sync_all()?;
                if sock_path.exists() {
                    if std::os::unix::net::UnixStream::connect(sock_path).is_ok() {
                        drop(file);
                        let _ = std::fs::remove_file(&path);
                        anyhow::bail!("daemon is already running");
                    }
                    std::fs::remove_file(sock_path)?;
                }
                return Ok(InstanceLock { path, _file: file });
            }
            Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {
                let owner_alive = std::fs::read_to_string(&path).ok().is_some_and(|owner| {
                    let mut fields = owner.split_whitespace();
                    let pid = fields.next().and_then(|value| value.parse::<u32>().ok());
                    let started = fields.next().and_then(|value| value.parse::<u64>().ok());
                    pid.zip(started)
                        .is_some_and(|(pid, started)| process_start_time(pid) == Some(started))
                });
                if owner_alive {
                    anyhow::bail!("daemon is already running");
                }
                std::fs::remove_file(&path)?;
            }
            Err(error) => return Err(error.into()),
        }
    }
    anyhow::bail!("failed to acquire daemon lock")
}

fn process_start_time(pid: u32) -> Option<u64> {
    let stat = std::fs::read_to_string(format!("/proc/{pid}/stat")).ok()?;
    let mut fields = stat.rsplit_once(") ")?.1.split_whitespace();
    fields.nth(19)?.parse().ok()
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
            error,
        } => Response::Status {
            state,
            activation_mode,
            error,
        },
    }
}
