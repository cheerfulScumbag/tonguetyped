use crate::config::Config;
use crate::ipc::{encode_frame, decode_frame, Request, Response};
use crate::coordinator::{Coordinator, CoordinatorCommand, CoordinatorResponse};
use std::path::PathBuf;
use std::sync::Arc;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::net::{UnixListener, UnixStream};

pub fn runtime_dir() -> PathBuf {
    let dir = if let Ok(runtime) = std::env::var("XDG_RUNTIME_DIR") {
        PathBuf::from(runtime).join("tonguetyped")
    } else {
        PathBuf::from("/tmp/tonguetyped-runtime")
    };
    std::fs::create_dir_all(&dir).ok();
    dir
}

pub fn socket_path() -> PathBuf {
    runtime_dir().join("control.sock")
}

pub fn lock_path() -> PathBuf {
    runtime_dir().join("daemon.lock")
}

pub async fn run_daemon(config: Config) -> anyhow::Result<()> {
    let sock_path = socket_path();

    if sock_path.exists() {
        let _ = std::fs::remove_file(&sock_path);
    }

    let listener = UnixListener::bind(&sock_path)?;

    let coordinator = Arc::new(Coordinator::new(config));

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

async fn dispatch(
    coordinator: &Arc<Coordinator>,
    request: Request,
) -> Response {
    let cmd = match request {
        Request::Start => CoordinatorCommand::Start,
        Request::Stop => CoordinatorCommand::Stop,
        Request::Toggle => CoordinatorCommand::Toggle,
        Request::Cancel => CoordinatorCommand::Cancel,
        Request::Status => CoordinatorCommand::GetStatus,
        Request::Doctor => {
            let config = Config::load().unwrap_or_default();
            let report = crate::doctor::run_doctor(&config.model.selected);
            return Response::DoctorResult {
                compositor: report.compositor,
                desktop: report.desktop,
                audio_available: report.audio_available,
                model_ready: report.model_ready,
                socket_health: report.socket_health,
                helpers_found: report.helpers_found,
            };
        }
        Request::ReloadConfig => {
            match Config::reload() {
                Ok(_config) => {
                    return Response::Ok;
                }
                Err(e) => {
                    return Response::Error {
                        message: format!("config reload failed: {}", e),
                    };
                }
            }
        }
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
        Request::HoldPress => CoordinatorCommand::HoldPress,
        Request::HoldRelease => CoordinatorCommand::HoldRelease,
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
            recording,
            processing,
            activation_mode,
        } => Response::Status {
            state,
            recording,
            processing,
            activation_mode,
        },
    }
}