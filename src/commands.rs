//! Shared command implementations used by both the plain CLI dispatch in
//! `main.rs` and the interactive dashboard in `tui`. Neither caller shells
//! out to the installed binary or re-implements these behaviors itself - both
//! call the exact same functions here, so there is exactly one place that
//! knows how to talk to the daemon, install/activate a model, or launch the
//! daemon process.

use crate::config::Config;
use crate::doctor::{DaemonBuild, DoctorReport};
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

/// Whether a daemon is actually listening on the control socket - not just
/// whether the socket *file* is present. A daemon that dies without reaching
/// its own graceful shutdown path (the one that removes the socket as its
/// last step, see `daemon::run_daemon`) leaves that file behind, so a bare
/// `Path::exists()` would mistake a dead daemon's leftovers for a live one.
/// Probes the same way `daemon::acquire_instance_lock_at` already does
/// before a fresh daemon binds the socket: a failed connect means nothing is
/// listening, so the stale file is removed here too, rather than left for
/// every other caller (`stop_daemon`/`restart_daemon` below included) to
/// trip over again.
pub fn daemon_socket_exists() -> bool {
    crate::daemon::socket_path()
        .map(|path| socket_is_alive(&path))
        .unwrap_or(false)
}

fn socket_is_alive(sock_path: &std::path::Path) -> bool {
    if !sock_path.exists() {
        return false;
    }
    if std::os::unix::net::UnixStream::connect(sock_path).is_ok() {
        return true;
    }
    let _ = std::fs::remove_file(sock_path);
    false
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

/// Sends a graceful shutdown request to the running daemon and waits for its
/// control socket to disappear, so callers (`tonguetyped daemon stop`,
/// `restart_daemon` below) only return once the old process has actually
/// exited rather than racing a `restart`'s subsequent `spawn_daemon` against
/// it. Fails immediately, without hanging, when no daemon is running -
/// including when a stale socket file is left over from one that already
/// died, rather than attempting (and failing) a connection to it.
pub async fn stop_daemon() -> anyhow::Result<()> {
    let sock_path = crate::daemon::socket_path()?;
    stop_daemon_at(&sock_path).await
}

async fn stop_daemon_at(sock_path: &std::path::Path) -> anyhow::Result<()> {
    if !socket_is_alive(sock_path) {
        anyhow::bail!(
            "daemon is not running (no socket at {})",
            sock_path.display()
        );
    }

    match send_ipc(Request::Shutdown).await {
        Ok(Response::Ok) => {}
        Ok(Response::Error { message }) => {
            anyhow::bail!("daemon refused shutdown request: {message}")
        }
        Ok(other) => anyhow::bail!("unexpected daemon response to shutdown: {other:?}"),
        Err(error) => anyhow::bail!("failed to reach daemon: {error}"),
    }

    let deadline = tokio::time::Instant::now() + std::time::Duration::from_secs(5);
    while sock_path.exists() {
        if tokio::time::Instant::now() >= deadline {
            anyhow::bail!("daemon did not exit within 5s of the shutdown request");
        }
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;
    }
    Ok(())
}

/// Stops the running daemon (if any) and waits for its socket to clear
/// before launching a fresh one via `spawn_daemon` - the same detached
/// background launch the dashboard uses, so `daemon restart` returns once
/// the new daemon is underway instead of blocking in the foreground. A
/// stale socket file with nothing listening behind it (`daemon_socket_exists`
/// returns `false` and removes it) is treated as no daemon running, so this
/// proceeds straight to `spawn_daemon` instead of calling `stop_daemon`
/// against a connection that was always going to refuse.
pub async fn restart_daemon() -> anyhow::Result<()> {
    if daemon_socket_exists() {
        stop_daemon().await?;
    }
    spawn_daemon()
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
            build,
        } => {
            let mut lines = vec![
                line(format!("state:            {state}")),
                line(format!(
                    "daemon build:     {}",
                    build
                        .as_deref()
                        .unwrap_or("unknown (predates build reporting)")
                )),
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
        line(format!("build:          {}", report.build)),
        daemon_build_line(&report.build, &report.daemon_build),
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
        line(format!(
            "backends:       {}",
            format_backend_availability(&report.backend_availability)
        )),
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

/// Compares the running daemon's build with this binary's, so a daemon left
/// running from an older build is called out instead of silently trusted.
fn daemon_build_line(own_build: &str, daemon_build: &DaemonBuild) -> OutputLine {
    const RESTART: &str = "run `tonguetyped daemon restart` to pick up this build";
    match daemon_build {
        DaemonBuild::NotRunning => line("daemon build:   not running"),
        DaemonBuild::Unreported => error_line(format!(
            "daemon build:   unknown - the daemon predates build reporting, so it is an \
             older build; {RESTART}"
        )),
        DaemonBuild::Reported(build) if own_build.ends_with("(unknown)") => line(format!(
            "daemon build:   {build} - this binary records no git commit ({own_build}), so it \
             cannot confirm whether the daemon is the same build"
        )),
        DaemonBuild::Reported(build) if build != own_build => error_line(format!(
            "daemon build:   {build} - differs from this binary ({own_build}); {RESTART}"
        )),
        DaemonBuild::Reported(build) => {
            line(format!("daemon build:   {build} (matches this binary)"))
        }
    }
}

/// Renders every backend kind's availability in load-priority order, e.g.
/// `available: vulkan, cpu; unavailable: cuda, rocm, metal`.
fn format_backend_availability(backends: &[crate::inference::BackendAvailability]) -> String {
    let kinds = |available: bool| {
        let kinds: Vec<&str> = backends
            .iter()
            .filter(|backend| backend.available == available)
            .map(|backend| backend.kind.as_str())
            .collect();
        if kinds.is_empty() {
            "none".to_string()
        } else {
            kinds.join(", ")
        }
    };
    format!("available: {}; unavailable: {}", kinds(true), kinds(false))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn unique_temp_dir(label: &str) -> std::path::PathBuf {
        std::env::temp_dir().join(format!(
            "tonguetyped-{label}-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ))
    }

    /// Leaves a control socket *file* on disk with nothing listening behind
    /// it - exactly what remains when a daemon process dies without going
    /// through its own graceful shutdown (which normally unlinks the socket
    /// as its last step). Binding a `UnixListener` and dropping it does not
    /// unlink the file, which is what makes this scenario reproducible
    /// without spawning a real daemon process.
    fn leave_stale_socket(path: &std::path::Path) {
        drop(std::os::unix::net::UnixListener::bind(path).unwrap());
    }

    #[test]
    fn stale_socket_is_not_treated_as_alive_and_is_cleaned_up() {
        let root = unique_temp_dir("stale-socket");
        std::fs::create_dir_all(&root).unwrap();
        let sock_path = root.join("control.sock");
        leave_stale_socket(&sock_path);
        assert!(sock_path.exists());

        assert!(!socket_is_alive(&sock_path));
        assert!(
            !sock_path.exists(),
            "stale socket file should have been removed"
        );

        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn daemon_build_mismatch_is_flagged_with_a_restart_hint() {
        let own = "0.1.0 (b2c3d4e)";
        let older = daemon_build_line(own, &DaemonBuild::Reported("0.1.0 (a1b2c3d)".into()));
        assert!(older.is_error);
        assert!(older.text.contains("differs from this binary"));
        assert!(older.text.contains("tonguetyped daemon restart"));

        let unreported = daemon_build_line(own, &DaemonBuild::Unreported);
        assert!(unreported.is_error);
        assert!(unreported.text.contains("older build"));

        let same = daemon_build_line(own, &DaemonBuild::Reported(own.into()));
        assert!(!same.is_error);
        assert!(same.text.contains("matches this binary"));

        let not_running = daemon_build_line(own, &DaemonBuild::NotRunning);
        assert!(!not_running.is_error);
        assert!(not_running.text.contains("not running"));
    }

    #[test]
    fn daemon_build_without_commit_is_not_claimed_to_match() {
        let own = "0.1.0 (unknown)";
        let same = daemon_build_line(own, &DaemonBuild::Reported(own.into()));
        assert!(!same.is_error);
        assert!(!same.text.contains("matches this binary"));
        assert!(same.text.contains("cannot confirm"));

        let differing = daemon_build_line(own, &DaemonBuild::Reported("0.1.0 (a1b2c3d)".into()));
        assert!(!differing.is_error);
        assert!(!differing.text.contains("differs from this binary"));
        assert!(differing.text.contains("cannot confirm"));
    }

    #[test]
    fn backend_availability_lists_both_available_and_unavailable_kinds() {
        let backend = |kind: &str, available| crate::inference::BackendAvailability {
            kind: kind.to_string(),
            available,
        };
        assert_eq!(
            format_backend_availability(&[
                backend("cuda", false),
                backend("vulkan", true),
                backend("cpu", true),
            ]),
            "available: vulkan, cpu; unavailable: cuda"
        );
        assert_eq!(
            format_backend_availability(&[backend("cpu", true)]),
            "available: cpu; unavailable: none"
        );
    }

    #[test]
    fn status_without_build_from_an_older_daemon_still_decodes() {
        let frame = r#"{"type":"status","state":"idle","activation_mode":"hold","operation_error":null,"shortcut_status":"available","activation_error":null}"#;
        let response: Response = ipc::decode_frame(frame).unwrap();
        assert!(matches!(response, Response::Status { build: None, .. }));
    }

    #[test]
    fn live_socket_is_treated_as_alive() {
        let root = unique_temp_dir("live-socket");
        std::fs::create_dir_all(&root).unwrap();
        let sock_path = root.join("control.sock");
        let listener = std::os::unix::net::UnixListener::bind(&sock_path).unwrap();

        assert!(socket_is_alive(&sock_path));
        assert!(sock_path.exists());

        drop(listener);
        std::fs::remove_dir_all(root).unwrap();
    }

    /// Regression test for the bug this module's `daemon_socket_exists`/
    /// `stop_daemon` fix: a stale socket left behind by a dead daemon used
    /// to be indistinguishable from a live one (`Path::exists()` alone),
    /// so `restart_daemon`'s `if daemon_socket_exists() { stop_daemon()... }`
    /// guard would call `stop_daemon`, which tried to send `Shutdown` over
    /// the dead socket and failed with a raw connection-refused error -
    /// surfacing as "failed to restart daemon: failed to reach daemon:
    /// Connection refused" instead of proceeding to `spawn_daemon`.
    /// `stop_daemon_at` is tested directly (rather than through
    /// `restart_daemon`/`spawn_daemon`, which re-execs the current binary -
    /// the test binary itself here, not `tonguetyped`) to construct the
    /// scenario directly, the same way `daemon::acquire_instance_lock_at`'s
    /// tests do.
    #[tokio::test]
    async fn stop_daemon_reports_not_running_for_a_stale_socket() {
        let root = unique_temp_dir("stale-socket-stop");
        std::fs::create_dir_all(&root).unwrap();
        let sock_path = root.join("control.sock");
        leave_stale_socket(&sock_path);

        let error = stop_daemon_at(&sock_path).await.unwrap_err();
        let message = error.to_string();
        assert!(
            message.contains("daemon is not running"),
            "expected a \"daemon is not running\" message, got: {message}"
        );
        assert!(
            !message.contains("Connection refused"),
            "stale socket must not surface a raw connection-refused error, got: {message}"
        );

        // `socket_is_alive` already removed the stale file as a side effect
        // of the liveness probe above, which is what lets a subsequent
        // `restart_daemon` skip `stop_daemon` entirely and proceed straight
        // to `spawn_daemon` instead of bailing out on it.
        assert!(!sock_path.exists());

        std::fs::remove_dir_all(root).unwrap();
    }
}
