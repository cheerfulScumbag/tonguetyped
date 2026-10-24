use crate::config::Config;
use crate::coordinator::{Coordinator, CoordinatorCommand, CoordinatorResponse};
use crate::ipc::{decode_frame, encode_frame, Request, Response};
use std::fs::{File, OpenOptions};
use std::io::Write;
use std::path::PathBuf;
use std::sync::Arc;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::net::{UnixListener, UnixStream};

/// How long a `Shutdown` request waits for an in-flight recording/processing
/// worker to finish before replying. Cancellation makes the normal case finish
/// long before this; the bound keeps a worker that ignores cancellation from
/// hanging `stop`/`restart`.
const SHUTDOWN_WORKER_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(3);

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

    let preference = crate::inference::BackendPreference::parse(&config.model.preferred_backend)?;
    let backend = crate::inference::preferred_backend_info(preference);
    tracing::info!(
        model_id = %config.model.active_model,
        inference_backend = %backend.backend,
        inference_device = %backend.device,
        "inference configuration"
    );
    // Resolve the typing backend once, with hidden probes only: no window, no
    // focus, no key event, no portal dialog. A pinned backend that cannot work
    // is replaced by a working helper here rather than failing on every
    // dictation, and the warning says what was substituted and why. Methods
    // that never type (`none`, `clipboard`) skip the probe entirely, so they
    // cannot warn about a missing typing helper they do not need.
    let typing = if config.output.method.inserts_text() {
        let typing = crate::output::resolve_typing(&config.output);
        if let Some(warning) = typing.warning() {
            tracing::warn!("{warning}");
        }
        tracing::info!(
            configured = %typing.configured,
            resolved = typing.resolved.as_deref().unwrap_or("none"),
            "typing backend resolved"
        );
        typing
    } else {
        crate::output::TypingPlan::unprobed(&config.output.typing_backend)
    };
    warn_missing_clipboard_helper(config.output.method);
    if config.history.enabled {
        tracing::warn!(
            "saving transcripts is on; turn it off in Transcript output > Save transcript history"
        );
    }

    let coordinator = Arc::new(Coordinator::new(config, typing)?);
    spawn_config_watcher(coordinator.clone());
    let listener = UnixListener::bind(&sock_path)?;
    let shutdown = Arc::new(tokio::sync::Notify::new());
    // The daemon owns the one GlobalShortcuts session. Its supervisor reports
    // its own health directly and rebinds after session loss; IPC shortcut
    // test/configure requests route through this handle instead of opening a
    // competing helper session.
    coordinator.set_activation_handle(crate::activation::spawn(coordinator.clone()));

    tracing::info!("daemon listening on {}", sock_path.display());

    loop {
        tokio::select! {
            accepted = listener.accept() => {
                let (stream, _addr) = accepted?;
                let coord = coordinator.clone();
                let shutdown = shutdown.clone();
                tokio::spawn(async move {
                    if let Err(e) = handle_connection(stream, coord, shutdown).await {
                        tracing::error!("connection error: {}", e);
                    }
                });
            }
            _ = shutdown.notified() => break,
        }
    }

    tracing::info!("daemon shutting down");
    // Release the loaded inference engine (and stop its idle-unload timer)
    // before removing the socket or exiting. The engine free touches the GPU
    // driver, so it must happen on this thread while the process is alive;
    // leaving it to the detached timer thread races exit-time NVIDIA teardown
    // and segfaults. Doing it before the socket is removed also keeps a
    // concurrent `restart` from loading a second engine while this one is still
    // being freed.
    let release_error = coordinator.shutdown().err();
    // Release the instance lock before removing the socket: `stop_daemon`
    // treats socket-absence as proof the process (and its lock) is gone, so
    // a `restart` racing a `spawn_daemon` against a still-held lock is only
    // ruled out if the lock is actually free by the time the socket is.
    drop(_lock);
    std::fs::remove_file(&sock_path).ok();
    if let Some(error) = release_error {
        // The engine is still loaded because a worker is still active after the
        // bounded wait, or an in-flight transcription held the inference lock
        // past the bounded release. Freeing it now would race that live GPU work
        // and exit-time driver teardown, so exit without running destructors: the
        // socket is already gone, so `stop`/`restart` proceeds.
        tracing::error!(
            "failed to release the inference engine on shutdown ({error}); \
             exiting without freeing it"
        );
        std::process::exit(0);
    }
    Ok(())
}

async fn prepare_dependencies(config: &Config) -> anyhow::Result<()> {
    let download_manager = crate::model::DownloadManager::new()?;
    if !crate::catalog::is_installed(&config.model.active_model) {
        download_manager
            .install_catalog_model(&config.model.active_model, None)
            .await?;
    }
    if config.transcription.vad_enabled {
        download_manager.ensure_vad_model().await?;
    }
    Ok(())
}

/// Start watching the resolved config file and reload the running daemon after
/// each settled change, so a config edit takes effect live without a manual
/// `tonguetyped reload` or a daemon restart. A rejected change is logged,
/// reported in `status`, and shown through the overlay, while the daemon keeps
/// running on its last good config.
fn spawn_config_watcher(coordinator: Arc<Coordinator>) {
    let path = match Config::config_path() {
        Ok(path) => path,
        Err(error) => {
            tracing::warn!("cannot watch the config file for changes: {error}");
            return;
        }
    };
    tokio::spawn(crate::config_watch::watch(
        path,
        crate::config_watch::POLL_INTERVAL,
        crate::config_watch::DEBOUNCE,
        move || {
            let coordinator = coordinator.clone();
            async move {
                if let Err(error) = reload_config_from_disk(&coordinator).await {
                    tracing::warn!("config reload failed after a config file change: {error}");
                    coordinator.show_config_error();
                }
            }
        },
    ));
}

/// Exit status for "another daemon already holds the instance lock". The
/// systemd units list it in `RestartPreventExitStatus`, so a second unit start
/// fails honestly once instead of restarting forever while a manually started
/// daemon owns the lock.
pub const EXIT_DAEMON_ALREADY_RUNNING: i32 = 3;

/// Typed "another daemon is already running" outcome, so `main` can map it to
/// its own exit status instead of anyhow's generic 1.
#[derive(Debug)]
pub struct DaemonAlreadyRunning;

impl std::fmt::Display for DaemonAlreadyRunning {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str("daemon is already running")
    }
}

impl std::error::Error for DaemonAlreadyRunning {}

/// The process exit status for a failed daemon start. Only "another daemon is
/// already running" gets its own status; every other failure stays 1 and keeps
/// systemd's `Restart=on-failure` behavior.
pub fn exit_code_for_error(error: &anyhow::Error) -> i32 {
    if error.is::<DaemonAlreadyRunning>() {
        EXIT_DAEMON_ALREADY_RUNNING
    } else {
        1
    }
}

#[derive(Debug)]
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
    match file.try_lock() {
        Ok(()) => {}
        Err(std::fs::TryLockError::WouldBlock) => {
            return Err(anyhow::Error::new(DaemonAlreadyRunning));
        }
        // A real locking failure (ENOLCK/EIO on the runtime directory) is not
        // another daemon: reporting it as one would exit 3 and stop systemd
        // from retrying a genuinely broken start.
        Err(std::fs::TryLockError::Error(error)) => {
            return Err(
                anyhow::Error::new(error).context(format!("could not lock {}", path.display()))
            );
        }
    }
    // Holding the lock means no other daemon owns this socket: a daemon that
    // released the lock but has not removed the socket yet is already shutting
    // down. Treat the file as stale rather than probing it and mistaking that
    // handover for a live duplicate.
    if sock_path.exists() {
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
    shutdown: Arc<tokio::sync::Notify>,
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

        let is_shutdown = matches!(request, Request::Shutdown);
        let response = dispatch(&coordinator, request).await;
        let frame = encode_frame(&response)?;
        writer.write_all(frame.as_bytes()).await?;
        writer.flush().await?;
        if is_shutdown {
            shutdown.notify_one();
            break;
        }
    }

    Ok(())
}

/// Load the configuration from disk and apply it through the coordinator:
/// `Config::reload` -> `Coordinator::validate_reload` -> `prepare_dependencies`
/// -> `Coordinator::reload_config`. This is the single reload implementation,
/// shared by the `ReloadConfig` request (`tonguetyped reload`) and the
/// config-file watcher, so a hot reload and a manual one behave identically.
///
/// The outcome is also recorded as the coordinator's `config_error`, so a
/// rejected file shows up in `status` regardless of which path tried it, and a
/// later success clears it. `prepare_dependencies` is boxed because it embeds
/// the HTTP download client's state machine (tens of KiB); boxing keeps this
/// function's future - and so every caller's, including `dispatch`'s - small
/// on a branch most requests never take.
pub async fn reload_config_from_disk(coordinator: &Arc<Coordinator>) -> anyhow::Result<()> {
    let result = async {
        let config = Config::reload()?;
        coordinator.validate_reload(&config)?;
        Box::pin(prepare_dependencies(&config)).await?;
        // Re-probe on every reload: the pinned backend may have changed, and
        // a helper installed or a compositor restarted since load must not be
        // hidden by a process-lifetime answer. Methods that never type skip
        // the probe, exactly as at startup.
        let typing = if config.output.method.inserts_text() {
            let typing = crate::output::resolve_typing(&config.output);
            if let Some(warning) = typing.warning() {
                tracing::warn!("{warning}");
            }
            typing
        } else {
            crate::output::TypingPlan::unprobed(&config.output.typing_backend)
        };
        warn_missing_clipboard_helper(config.output.method);
        coordinator.reload_config_with_typing(config, typing)
    }
    .await;
    match &result {
        Ok(()) => coordinator.clear_config_error(),
        Err(error) => coordinator.set_config_error(error.to_string()),
    }
    result
}

/// Warns once when the configured method needs the clipboard helper and this
/// session has none. `type` is excluded: it copies only as a failure fallback,
/// so a missing clipboard helper is not a startup problem there.
fn warn_missing_clipboard_helper(method: crate::config::OutputMethod) {
    if method.needs_clipboard() && !crate::output::clipboard_available() {
        tracing::warn!("{}", crate::output::clipboard_helper_warning());
    }
}

/// Routes every shortcut request through the daemon's activation handle, the
/// only owner of the GlobalShortcuts session. Returns `None` for requests
/// that are not shortcut-related, so `dispatch` can keep one match over the
/// rest. Split out so the routing (including "no listener is running") is
/// testable without a full `Coordinator`.
async fn dispatch_shortcut(
    handle: Option<&crate::activation::ActivationHandle>,
    request: &Request,
) -> Option<Response> {
    match request {
        Request::ShortcutTest => Some(match handle {
            None => Response::Error {
                message: "the daemon's shortcut listener is not running".to_string(),
            },
            Some(handle) => match handle.test_shortcut().await {
                Ok(crate::activation::ShortcutTestOutcome::Pressed) => {
                    Response::ShortcutTest { pressed: true }
                }
                Ok(crate::activation::ShortcutTestOutcome::TimedOut) => {
                    Response::ShortcutTest { pressed: false }
                }
                Err(error) => Response::Error {
                    message: format!("shortcut test failed: {error}"),
                },
            },
        }),
        Request::ConfigureShortcut => Some(match handle {
            None => Response::Error {
                message: "the daemon's shortcut listener is not running".to_string(),
            },
            Some(handle) => match handle.configure_shortcut().await {
                Ok(crate::activation::ReconfigureOutcome::Changed {
                    trigger_description,
                }) => Response::ShortcutConfigured {
                    changed: true,
                    trigger_description,
                },
                Ok(crate::activation::ReconfigureOutcome::Unchanged {
                    trigger_description,
                }) => Response::ShortcutConfigured {
                    changed: false,
                    trigger_description,
                },
                Err(error) => Response::Error {
                    message: format!("shortcut configuration failed: {error}"),
                },
            },
        }),
        Request::CancelShortcutConfigure => {
            if let Some(handle) = handle {
                handle.cancel_configure().await;
            }
            Some(Response::Ok)
        }
        _ => None,
    }
}

pub async fn dispatch(coordinator: &Arc<Coordinator>, request: Request) -> Response {
    if let Some(response) = dispatch_shortcut(coordinator.activation_handle(), &request).await {
        return response;
    }
    let cmd = match request {
        Request::Start => CoordinatorCommand::Start,
        Request::Stop => CoordinatorCommand::Stop,
        Request::Toggle => CoordinatorCommand::Toggle,
        Request::Cancel => CoordinatorCommand::Cancel,
        Request::Status => CoordinatorCommand::GetStatus,
        Request::ReloadConfig => {
            return match reload_config_from_disk(coordinator).await {
                Ok(()) => Response::Ok,
                Err(e) => Response::Error {
                    message: format!("config reload failed: {}", e),
                },
            }
        }
        Request::Shutdown => {
            // Cancel rather than kill: any in-flight recording/processing is
            // torn down the same way a client-issued `cancel` would, instead
            // of leaving it to die mid-dictation when the process exits.
            // `cancel()` itself already handles the idle case as a no-op, and
            // reaches an in-flight transcription through its cancel token.
            let _ = coordinator.handle_command(CoordinatorCommand::Cancel).await;
            // Wait for the worker to actually finish before replying, but bound
            // it: the daemon releases the loaded engine after the accept loop
            // ends, and a worker that ignores cancellation must not hang
            // `stop`/`restart`. On expiry the daemon exits without freeing the
            // engine (see `run_daemon`), so the delay never exceeds this bound
            // plus the bounded engine release, both well under `stop_daemon`'s
            // socket poll deadline.
            let _ =
                tokio::time::timeout(SHUTDOWN_WORKER_TIMEOUT, coordinator.wait_for_worker()).await;
            return Response::Ok;
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
        Request::ShortcutTest | Request::ConfigureShortcut | Request::CancelShortcutConfigure => {
            unreachable!("shortcut requests are handled by dispatch_shortcut")
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
            config_error,
            shortcut_status,
            activation_error,
            shortcut_trigger,
            model,
            backend,
            output_configured,
            output_backend,
            output_warning,
            output_fallbacks,
            last_output_failure,
        } => {
            let (backend, device) = match backend {
                Some(info) => (Some(info.backend), Some(info.device)),
                None => (None, None),
            };
            Response::Status {
                state,
                activation_mode,
                operation_error,
                config_error,
                shortcut_status,
                activation_error,
                shortcut_trigger,
                build: Some(crate::build_info::VERSION.to_string()),
                model: Some(model),
                backend,
                device,
                output_configured,
                output_backend,
                output_warning,
                output_fallbacks,
                last_output_failure,
            }
        }
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
        let duplicate = acquire_instance_lock_at(&lock_path, &socket_path).unwrap_err();
        assert!(
            duplicate.is::<DaemonAlreadyRunning>(),
            "a second daemon must return the typed already-running error"
        );
        assert_eq!(duplicate.to_string(), "daemon is already running");
        assert_eq!(exit_code_for_error(&duplicate), EXIT_DAEMON_ALREADY_RUNNING);
        assert_eq!(EXIT_DAEMON_ALREADY_RUNNING, 3);
        // Every other failure keeps the ordinary exit status, so systemd still
        // restarts a genuinely broken start.
        assert_eq!(
            exit_code_for_error(&anyhow::anyhow!("model download failed")),
            1
        );
        drop(first);

        // A concurrently forked test process can retain the descriptor until exec
        // applies CLOEXEC, so assert eventual rather than instantaneous release.
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(1);
        let second = loop {
            match acquire_instance_lock_at(&lock_path, &socket_path) {
                Ok(lock) => break lock,
                Err(_) if std::time::Instant::now() < deadline => {
                    std::thread::sleep(std::time::Duration::from_millis(10));
                }
                Err(error) => panic!("instance lock was not released: {error}"),
            }
        };
        drop(second);

        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn a_socket_left_by_a_previous_daemon_is_reclaimed() {
        let root = std::env::temp_dir().join(format!(
            "tonguetyped-stale-socket-test-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir_all(&root).unwrap();
        let lock_path = root.join("daemon.lock");
        let socket_path = root.join("control.sock");
        let listener = std::os::unix::net::UnixListener::bind(&socket_path).unwrap();

        // Holding the instance lock proves no live daemon owns the socket, so
        // the leftover file is reclaimed instead of reported as a duplicate.
        let lock = acquire_instance_lock_at(&lock_path, &socket_path).unwrap();
        assert!(!socket_path.exists(), "the stale socket must be removed");
        drop(lock);
        drop(listener);

        std::fs::remove_dir_all(root).unwrap();
    }

    #[tokio::test]
    async fn shortcut_requests_route_through_the_activation_handle() {
        let (commands, mut receiver) = tokio::sync::mpsc::channel(8);
        let handle = crate::activation::ActivationHandle::for_test(commands);
        let responder = tokio::spawn(async move {
            match receiver.recv().await {
                Some(crate::activation::ActivationCommand::Test { reply }) => {
                    reply
                        .send(Ok(crate::activation::ShortcutTestOutcome::Pressed))
                        .unwrap();
                }
                _ => panic!("expected a shortcut test command"),
            }
            match receiver.recv().await {
                Some(crate::activation::ActivationCommand::Configure { reply }) => {
                    reply
                        .send(Ok(crate::activation::ReconfigureOutcome::Changed {
                            trigger_description: "Super+O".to_string(),
                        }))
                        .unwrap();
                }
                _ => panic!("expected a configure command"),
            }
            match receiver.recv().await {
                Some(crate::activation::ActivationCommand::CancelConfigure) => {}
                _ => panic!("expected a cancel command"),
            }
        });

        let test = dispatch_shortcut(Some(&handle), &Request::ShortcutTest)
            .await
            .unwrap();
        assert!(matches!(test, Response::ShortcutTest { pressed: true }));

        let configure = dispatch_shortcut(Some(&handle), &Request::ConfigureShortcut)
            .await
            .unwrap();
        assert!(matches!(
            configure,
            Response::ShortcutConfigured { changed: true, .. }
        ));

        let cancel = dispatch_shortcut(Some(&handle), &Request::CancelShortcutConfigure)
            .await
            .unwrap();
        assert!(matches!(cancel, Response::Ok));
        responder.await.unwrap();

        let missing = dispatch_shortcut(None, &Request::ShortcutTest)
            .await
            .unwrap();
        assert!(
            matches!(missing, Response::Error { message } if message.contains("not running")),
            "no listener must be reported, not a silent fallback"
        );
        assert!(
            dispatch_shortcut(None, &Request::Start).await.is_none(),
            "non-shortcut requests stay with dispatch"
        );
    }
}
