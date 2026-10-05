//! Shared command implementations used by both the plain CLI dispatch in
//! `main.rs` and the interactive dashboard in `tui`. Neither caller shells
//! out to the installed binary or re-implements these behaviors itself - both
//! call the exact same functions here, so there is exactly one place that
//! knows how to talk to the daemon, install/activate a model, or launch the
//! daemon process.

use crate::config::Config;
use crate::doctor::DoctorReport;
use crate::ipc::{self, Request, Response};
use crate::model::{DownloadManager, DownloadOutcome, ProgressCallback};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt};

/// Connects to the daemon's control socket, sends one request, and returns
/// its decoded response. Shared by every IPC-backed CLI command
/// (`start`/`stop`/`toggle`/`cancel`/`status`/`reload`/`last-result`) and by
/// the dashboard's equivalent actions, so the socket handshake only exists in
/// one place.
pub async fn send_ipc(request: Request) -> anyhow::Result<Response> {
    let sock_path = crate::daemon::socket_path()?;

    if !sock_path.exists() {
        anyhow::bail!(
            "daemon is not running (no socket at {})",
            sock_path.display()
        );
    }

    let stream = tokio::net::UnixStream::connect(&sock_path).await?;
    let (reader, mut writer) = stream.into_split();

    let frame = ipc::encode_frame(&request)?;
    writer.write_all(frame.as_bytes()).await?;

    let mut reader = tokio::io::BufReader::new(reader);
    let mut line = String::new();
    reader.read_line(&mut line).await?;

    ipc::decode_frame(&line)
}

/// Whether the daemon appears to be running, judged the same way `send_ipc`
/// does (control socket present) - cheap, synchronous, no connection made.
pub fn daemon_socket_exists() -> bool {
    crate::daemon::socket_path()
        .map(|path| path.exists())
        .unwrap_or(false)
}

/// Launches `tonguetyped daemon` as a detached child process and returns
/// immediately - the daemon is a long-running service that must outlive
/// whichever CLI/TUI invocation started it (see `data/tonguetyped.desktop`'s
/// `Exec=tonguetyped daemon`, used identically for desktop autostart), so it
/// is spawned as a separate OS process via `current_exe()` rather than as an
/// in-process tokio task that would die with the dashboard. This re-executes
/// the same binary's already-tested `daemon` subcommand - it does not
/// reimplement or shell out to a *different* program, and no daemon logic
/// lives here.
pub fn spawn_daemon() -> anyhow::Result<()> {
    let exe = std::env::current_exe()?;
    std::process::Command::new(exe)
        .arg("daemon")
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .spawn()?;
    Ok(())
}

pub struct ModelRow {
    pub id: &'static str,
    pub quant: &'static str,
    pub size_bytes: u64,
    pub license_spdx: &'static str,
    pub installed: bool,
    pub active: bool,
}

/// Catalog rows in the same order `tonguetyped model list` prints them.
pub fn model_rows(config: &Config) -> Vec<ModelRow> {
    crate::catalog::ENTRIES
        .iter()
        .map(|entry| ModelRow {
            id: entry.id,
            quant: entry.quant,
            size_bytes: entry.size_bytes,
            license_spdx: entry.license_spdx,
            installed: crate::catalog::is_installed(entry.id),
            active: entry.id == config.model.active_model,
        })
        .collect()
}

pub fn human_size(bytes: u64) -> String {
    const UNITS: &[&str] = &["B", "KiB", "MiB", "GiB"];
    let mut size = bytes as f64;
    let mut unit = 0;
    while size >= 1024.0 && unit < UNITS.len() - 1 {
        size /= 1024.0;
        unit += 1;
    }
    if unit == 0 {
        format!("{bytes} {}", UNITS[unit])
    } else {
        format!("{size:.1} {}", UNITS[unit])
    }
}

/// Saves `id` as the active model (same effect as `tonguetyped model use`).
pub fn select_model(id: &str) -> anyhow::Result<()> {
    let mut config = Config::load()?;
    config.model.active_model = id.to_string();
    config.save()
}

/// Everything that happened while activating a catalog model from the
/// dashboard: download/verify, save the selection, reload a running daemon,
/// and confirm the new model actually loads. A failure at any step after the
/// download is reported on the matching field rather than raised as an
/// `Err`, so the dashboard can show exactly which step failed instead of
/// losing partial progress - but `succeeded()` is `false` whenever any step
/// did not complete, so the caller never has to guess which fields to check
/// before claiming activation worked.
pub struct ModelActivation {
    pub download: DownloadOutcome,
    pub daemon_reload: Option<Result<(), String>>,
    pub confirmation: crate::doctor::DoctorReport,
}

impl ModelActivation {
    /// Whether the model is confirmed active on a reachable backend - the
    /// one bit the dashboard needs before it may claim success. `None` would
    /// mean a running daemon's reload failed, which must not be reported as
    /// activation succeeding even though the config file was already saved.
    pub fn succeeded(&self) -> bool {
        !matches!(self.daemon_reload, Some(Err(_))) && self.confirmation.model_ready
    }
}

/// Downloads (if needed) and verifies `id`, saves it as the active model,
/// reloads a running daemon so it picks up the change immediately, and
/// re-runs diagnostics to confirm the model actually loads and report which
/// backend/device it loaded on. Mirrors `tonguetyped model install --use`
/// plus `tonguetyped reload` plus `tonguetyped doctor`, composed into one
/// action because the dashboard presents model activation as a single step.
pub async fn activate_model(
    id: &str,
    on_progress: Option<ProgressCallback>,
) -> anyhow::Result<ModelActivation> {
    if crate::catalog::find(id).is_none() {
        anyhow::bail!("unknown catalog model: {id}");
    }
    let manager = DownloadManager::new()?;
    let (_, download) = manager.install_catalog_model(id, on_progress).await?;

    select_model(id)?;

    let daemon_reload = if daemon_socket_exists() {
        Some(match send_ipc(Request::ReloadConfig).await {
            Ok(Response::Ok) => Ok(()),
            Ok(Response::Error { message }) => Err(message),
            Ok(other) => Err(format!("unexpected daemon response: {other:?}")),
            Err(error) => Err(error.to_string()),
        })
    } else {
        None
    };

    let config = Config::load()?;
    let confirmation = crate::doctor::run_doctor(&config).await?;

    Ok(ModelActivation {
        download,
        daemon_reload,
        confirmation,
    })
}

/// One formatted output line plus whether it represents an error/failure -
/// the dashboard uses the flag to color the line; the CLI ignores it and
/// prints every line the same way `main.rs` always has.
pub struct OutputLine {
    pub text: String,
    pub is_error: bool,
}

fn line(text: impl Into<String>) -> OutputLine {
    OutputLine {
        text: text.into(),
        is_error: false,
    }
}

fn error_line(text: impl Into<String>) -> OutputLine {
    OutputLine {
        text: text.into(),
        is_error: true,
    }
}

/// Formats an IPC response exactly the way `tonguetyped start/stop/toggle/
/// cancel/status/reload/last-result` has always printed it on stdout, so the
/// dashboard's equivalent actions render the identical text. Unlike the CLI
/// (which raises `Response::Error` as a hard `anyhow` failure and exits), this
/// renders it as one error-flagged line - the dashboard must show the failure
/// inline rather than tearing down the whole interactive session over it.
pub fn format_response_lines(response: &Response) -> Vec<OutputLine> {
    match response {
        Response::Ok => vec![line("ok")],
        Response::Busy => vec![line("busy")],
        Response::RecordingStarted => vec![line("recording started")],
        Response::RecordingStopped => vec![line("recording stopped")],
        Response::Cancelled => vec![line("cancelled")],
        Response::Error { message } => vec![error_line(message.clone())],
        Response::Status {
            state,
            activation_mode,
            operation_error,
            shortcut_status,
            activation_error,
        } => {
            let mut lines = vec![
                line(format!("state:            {state}")),
                line(format!("activation mode:  {activation_mode}")),
                line(format!(
                    "shortcut status:  {}",
                    match shortcut_status {
                        ipc::ShortcutStatus::Initializing => "initializing",
                        ipc::ShortcutStatus::Available => "available",
                        ipc::ShortcutStatus::Failed => "failed",
                    }
                )),
            ];
            if let Some(error) = operation_error {
                lines.push(error_line(format!("last error:       {error}")));
            }
            if let Some(error) = activation_error {
                lines.push(error_line(format!("shortcut error:   {error}")));
            }
            lines
        }
        Response::LastResult { text, timestamp } => vec![
            line(format!("result:   {text}")),
            line(format!("time:     {timestamp}")),
        ],
    }
}

/// Formats a `DoctorReport` exactly the way `tonguetyped doctor` has always
/// printed it, shared with the dashboard's Doctor screen.
pub fn format_doctor_lines(report: &DoctorReport) -> Vec<OutputLine> {
    let mut lines = vec![
        line(format!("compositor:     {}", report.compositor)),
        line(format!(
            "overlay:        {}",
            if report.layer_shell_overlay_available {
                "layer-shell available"
            } else {
                "layer-shell unavailable (falls back to OSD/notification)"
            }
        )),
        line(format!(
            "audio:          {}",
            if report.audio_available {
                "available"
            } else {
                "unavailable"
            }
        )),
        line(format!(
            "audio devices:  {}",
            report.audio_devices.join(", ")
        )),
        line(format!(
            "model:          {}",
            if report.model_ready {
                "ready"
            } else if report.model_error.is_some() {
                "invalid"
            } else {
                "not found"
            }
        )),
        line(format!("model id:       {}", report.model_id)),
        line(format!("model path:     {}", report.model_path)),
        line(format!("backend:        {}", report.inference_backend)),
        line(format!("device:         {}", report.inference_device)),
    ];
    if let Some(error) = &report.model_error {
        lines.push(error_line(format!("model error:    {error}")));
    }
    lines.push(line(format!(
        "helpers:        {}",
        if report.helpers_found.is_empty() {
            "none".to_string()
        } else {
            report.helpers_found.join(", ")
        }
    )));
    lines.push(line(format!(
        "output method:  {}",
        if report.output_method_available {
            "available"
        } else {
            "unavailable (none mode only)"
        }
    )));
    lines.push(line(format!(
        "shortcut portal: {}",
        match report.shortcut_status {
            None => "not tested (run `tonguetyped shortcut-test`)",
            Some(ipc::ShortcutStatus::Initializing) => "initializing",
            Some(ipc::ShortcutStatus::Available) => "available",
            Some(ipc::ShortcutStatus::Failed) => "unavailable",
        }
    )));
    if let Some(error) = &report.shortcut_portal_error {
        lines.push(error_line(format!("shortcut error:  {error}")));
    }
    lines
}
