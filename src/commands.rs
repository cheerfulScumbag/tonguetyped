//! Shared command implementations used by both the plain CLI dispatch in
//! `main.rs` and the interactive setup console in `setup`. Neither caller
//! shells out to the installed binary or re-implements these behaviors
//! itself; both call the exact same functions here, so there is exactly one
//! place that knows how to talk to the daemon, select a model, or launch the
//! daemon process.

use crate::config::Config;
use crate::doctor::{DaemonBuild, DoctorReport};
use crate::ipc::{self, Request, Response};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt};

/// How long `stop_daemon_at` waits for the daemon's reply to a `Shutdown`
/// request before giving up on the reply and falling back to polling for the
/// socket to disappear. Comfortably exceeds the daemon's own bounded reply
/// (`daemon::SHUTDOWN_WORKER_TIMEOUT` plus the instant cancel), so it only
/// fires when the daemon is truly unresponsive - the case that would otherwise
/// hang `stop`/`restart` on a socket read with no deadline of its own.
const SHUTDOWN_REPLY_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(10);

/// How long `stop_daemon_at` then polls for the control socket to disappear
/// after the `Shutdown` request, the authoritative proof the daemon exited.
const SHUTDOWN_EXIT_DEADLINE: std::time::Duration = std::time::Duration::from_secs(5);

/// Connects to the daemon's control socket, sends one request, and returns
/// its decoded response. Shared by every IPC-backed CLI command
/// (`start`/`stop`/`toggle`/`cancel`/`status`/`reload`/`last-result`) and by
/// the setup console's dictation and daemon actions, so the socket handshake
/// only exists in one place.
pub async fn send_ipc(request: Request) -> anyhow::Result<Response> {
    let sock_path = crate::daemon::socket_path()?;
    send_ipc_at(&sock_path, request).await
}

/// [`send_ipc`] against an explicit socket path. Split out so a caller can
/// bound or target a specific socket (`stop_daemon_at` below) without reaching
/// for the process-wide `daemon::socket_path()`.
pub async fn send_ipc_at(
    sock_path: &std::path::Path,
    request: Request,
) -> anyhow::Result<Response> {
    if !sock_path.exists() {
        anyhow::bail!(
            "daemon is not running (no socket at {})",
            sock_path.display()
        );
    }

    let stream = tokio::net::UnixStream::connect(sock_path).await?;
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

/// Tests the activation shortcut without ever opening a second
/// GlobalShortcuts session while a daemon owns one. On KDE a helper session's
/// teardown deactivates the shared desktop action, so a daemon-present test
/// asks the daemon's own listener instead. With no daemon there is no binding
/// to clobber, so the standalone helper path is safe.
pub async fn test_shortcut() -> Result<crate::activation::ShortcutTestOutcome, String> {
    match daemon_control_socket() {
        Some(sock_path) => test_shortcut_at(&sock_path).await,
        None => crate::activation::test_shortcut_binding().await,
    }
}

/// [`test_shortcut`] against an explicit daemon socket, so the routing can be
/// exercised headlessly with a fake listener.
pub async fn test_shortcut_at(
    sock_path: &std::path::Path,
) -> Result<crate::activation::ShortcutTestOutcome, String> {
    match send_ipc_at(sock_path, Request::ShortcutTest).await {
        Ok(Response::ShortcutTest { pressed: true }) => {
            Ok(crate::activation::ShortcutTestOutcome::Pressed)
        }
        Ok(Response::ShortcutTest { pressed: false }) => {
            Ok(crate::activation::ShortcutTestOutcome::TimedOut)
        }
        Ok(Response::Error { message }) => Err(message),
        Ok(other) => Err(format!("unexpected reply from the daemon: {other:?}")),
        Err(error) => Err(format!(
            "could not ask the running daemon to test the shortcut: {error}"
        )),
    }
}

/// Opens the desktop's shortcut editor on the daemon's own session when a
/// daemon is running, falling back to the standalone session only when none
/// is. A daemon that exists but fails the request is reported as an error -
/// silently opening a competing session is exactly the bug this avoids.
pub async fn configure_shortcut(
    cancel: tokio::sync::oneshot::Receiver<()>,
) -> Result<crate::activation::ReconfigureOutcome, String> {
    match daemon_control_socket() {
        Some(sock_path) => configure_shortcut_at(&sock_path, cancel).await,
        None => crate::activation::reconfigure_shortcut(cancel).await,
    }
}

/// [`configure_shortcut`] against an explicit daemon socket. Cancelling the
/// wait sends a separate `CancelShortcutConfigure` request (a second
/// connection), because the original connection is still waiting for the
/// configure reply; the daemon settles that request on the unchanged trigger.
/// The cancel round-trip completes before this returns, so a caller that
/// starts a replacement dialog afterwards cannot have a stale cancel end it.
pub async fn configure_shortcut_at(
    sock_path: &std::path::Path,
    mut cancel: tokio::sync::oneshot::Receiver<()>,
) -> Result<crate::activation::ReconfigureOutcome, String> {
    let response = send_ipc_at(sock_path, Request::ConfigureShortcut);
    tokio::pin!(response);
    let response = tokio::select! {
        response = &mut response => response,
        cancelled = &mut cancel => {
            if cancelled.is_ok() {
                let _ = send_ipc_at(sock_path, Request::CancelShortcutConfigure).await;
            }
            (&mut response).await
        }
    };
    match response {
        Ok(Response::ShortcutConfigured {
            changed: true,
            trigger_description,
        }) => Ok(crate::activation::ReconfigureOutcome::Changed {
            trigger_description,
        }),
        Ok(Response::ShortcutConfigured {
            changed: false,
            trigger_description,
        }) => Ok(crate::activation::ReconfigureOutcome::Unchanged {
            trigger_description,
        }),
        Ok(Response::Error { message }) => Err(message),
        Ok(other) => Err(format!("unexpected reply from the daemon: {other:?}")),
        Err(error) => Err(format!(
            "could not ask the running daemon to configure the shortcut: {error}"
        )),
    }
}

/// The live daemon socket, or `None` when nothing is listening. A stale
/// socket file is removed by the probe, matching `daemon_socket_exists`.
fn daemon_control_socket() -> Option<std::path::PathBuf> {
    let sock_path = crate::daemon::socket_path().ok()?;
    socket_is_alive(&sock_path).then_some(sock_path)
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
/// whichever CLI/TUI invocation started it (see `data/tonguetyped-daemon.desktop`'s
/// `Exec=tonguetyped daemon`, used identically for desktop autostart), so it
/// is spawned as a separate OS process via `current_exe()` rather than as an
/// in-process tokio task that would die with the setup console. This re-executes
/// the same binary's already-tested `daemon` subcommand - it does not
/// reimplement or shell out to a *different* program, and no daemon logic
/// lives here.
pub fn spawn_daemon() -> anyhow::Result<()> {
    use std::os::unix::process::CommandExt;

    let exe = std::env::current_exe()?;
    let mut command = std::process::Command::new(exe);
    command
        .arg("daemon")
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null());
    // A new session detaches the daemon from the launching terminal, so
    // closing that terminal (its SIGHUP) does not take the daemon down too.
    // SAFETY: `setsid` is async-signal-safe and touches no memory the forked
    // child shares with the parent.
    unsafe {
        command.pre_exec(|| {
            if libc::setsid() == -1 {
                return Err(std::io::Error::last_os_error());
            }
            Ok(())
        });
    }
    command.spawn()?;
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
    stop_daemon_at_with(sock_path, SHUTDOWN_REPLY_TIMEOUT, SHUTDOWN_EXIT_DEADLINE).await
}

/// [`stop_daemon_at`] with the two deadlines injected, so a test can exercise a
/// silent daemon's fall-through path in milliseconds instead of real seconds.
async fn stop_daemon_at_with(
    sock_path: &std::path::Path,
    reply_timeout: std::time::Duration,
    exit_deadline: std::time::Duration,
) -> anyhow::Result<()> {
    if !socket_is_alive(sock_path) {
        anyhow::bail!(
            "daemon is not running (no socket at {})",
            sock_path.display()
        );
    }

    // The daemon bounds its own reply (it cancels any in-flight worker, then
    // waits at most `daemon::SHUTDOWN_WORKER_TIMEOUT` for it), so this is a
    // generous safety net: without it a daemon that never answers would hang
    // `stop`/`restart` on the socket read forever. On timeout we do not fail
    // outright - the socket disappearing is the authoritative proof the daemon
    // exited - so fall through to the poll below.
    if let Ok(reply) =
        tokio::time::timeout(reply_timeout, send_ipc_at(sock_path, Request::Shutdown)).await
    {
        match reply {
            Ok(Response::Ok) => {}
            Ok(Response::Error { message }) => {
                anyhow::bail!("daemon refused shutdown request: {message}")
            }
            Ok(other) => anyhow::bail!("unexpected daemon response to shutdown: {other:?}"),
            Err(error) => anyhow::bail!("failed to reach daemon: {error}"),
        }
    }

    let deadline = tokio::time::Instant::now() + exit_deadline;
    while sock_path.exists() {
        if tokio::time::Instant::now() >= deadline {
            anyhow::bail!(
                "daemon did not exit within {:?} of the shutdown request",
                exit_deadline
            );
        }
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;
    }
    Ok(())
}

/// Stops the running daemon (if any) and waits for its socket to clear
/// before launching a fresh one via `spawn_daemon` - the same detached
/// background launch the setup console uses, so `daemon restart` returns once
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

/// One formatted output line plus whether it represents an error/failure.
/// The CLI prints every line the same way `main.rs` always has.
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
/// cancel/status/reload/last-result` has always printed it on stdout. The CLI
/// raises `Response::Error` as a hard `anyhow` failure before formatting;
/// this renders it as one error-flagged line.
pub fn format_response_lines(response: &Response) -> Vec<OutputLine> {
    match response {
        Response::Ok => vec![line("ok")],
        Response::Busy => vec![line("busy")],
        Response::RecordingStarted => vec![line("recording started")],
        Response::RecordingStopped => vec![line("recording stopped")],
        Response::Cancelled => vec![line("cancelled")],
        Response::ShortcutTest { pressed: true } => {
            vec![line("shortcut press detected - the daemon received it")]
        }
        Response::ShortcutTest { pressed: false } => vec![error_line(
            "no shortcut press detected within 15s".to_string(),
        )],
        Response::ShortcutConfigured {
            changed,
            trigger_description,
        } => {
            let trigger = crate::activation::keybind_display(trigger_description);
            if *changed {
                vec![line(format!("shortcut changed to {trigger}"))]
            } else {
                vec![line(format!("shortcut unchanged ({trigger})"))]
            }
        }
        Response::Error { message } => vec![error_line(message.clone())],
        Response::Status {
            state,
            activation_mode,
            operation_error,
            config_error,
            shortcut_status,
            activation_error,
            shortcut_trigger,
            build,
            output_configured,
            output_backend,
            output_warning,
            output_fallbacks,
            last_output_failure,
            ..
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
                    shortcut_status_label(shortcut_status)
                )),
            ];
            if let Some(trigger) = shortcut_trigger {
                lines.push(line(format!("shortcut key:     {trigger}")));
            }
            if let Some(backend) = output_backend {
                let configured = output_configured
                    .as_deref()
                    .map(|configured| format!(" (configured {configured})"))
                    .unwrap_or_default();
                lines.push(line(format!("output backend:   {backend}{configured}")));
            } else if let Some(configured) = output_configured {
                lines.push(error_line(format!(
                    "output backend:   none resolved (configured {configured})"
                )));
            }
            if *output_fallbacks > 0 {
                lines.push(error_line(format!(
                    "clipboard fallbacks: {output_fallbacks}"
                )));
            }
            if let Some(error) = last_output_failure {
                lines.push(error_line(format!("last output error: {error}")));
            }
            if let Some(warning) = output_warning {
                lines.push(error_line(format!("output warning:   {warning}")));
            }
            if let Some(error) = config_error {
                lines.push(error_line(format!("config error:     {error}")));
            }
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

/// The user-facing label for the daemon's shortcut state. Kept in one place
/// so text status and doctor cannot drift on what "available"
/// means; it never implies a key is grabbed when the action is unbound or the
/// listener failed.
pub fn shortcut_status_label(status: &ipc::ShortcutStatus) -> &'static str {
    match status {
        ipc::ShortcutStatus::Initializing => "initializing",
        ipc::ShortcutStatus::Available => "available",
        ipc::ShortcutStatus::Unbound => "unbound (no key set)",
        ipc::ShortcutStatus::Failed => "failed",
    }
}

/// Formats a `DoctorReport` exactly the way `tonguetyped doctor` has always
/// printed it.
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
        line(format!("backend pref:   {}", report.preferred_backend)),
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
        "clipboard:      {} {}",
        report.clipboard_helper,
        if report.clipboard_available {
            "found"
        } else {
            "not found"
        }
    )));
    match &report.output_method_problem {
        None => lines.push(line(format!(
            "output method:  {} (available)",
            report.output_method
        ))),
        Some(problem) => lines.push(error_line(format!(
            "output method:  {} unavailable - {problem}",
            report.output_method
        ))),
    }
    if report.output_method.inserts_text() {
        match &report.output_backend {
            Some(backend) => lines.push(line(format!("output backend: {backend}"))),
            None => lines.push(error_line("output backend: none resolved")),
        }
        if let Some(warning) = &report.output_warning {
            lines.push(error_line(format!("output warning: {warning}")));
        }
    }
    lines.push(line(format!(
        "shortcut portal: {}",
        match &report.shortcut_status {
            None => "not tested (run `tonguetyped shortcut-test`)",
            Some(ipc::ShortcutStatus::Failed) => "unavailable",
            Some(status) => shortcut_status_label(status),
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
        DaemonBuild::Reported(build) if build.ends_with("(unknown)") => line(format!(
            "daemon build:   {build} - the daemon records no git commit, so this binary \
             ({own_build}) cannot confirm whether they are the same build"
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
    fn daemon_build_with_unknown_commit_is_not_claimed_to_differ() {
        let own = "0.1.0 (b2c3d4e)";
        let daemon_unknown =
            daemon_build_line(own, &DaemonBuild::Reported("0.1.0 (unknown)".into()));
        assert!(!daemon_unknown.is_error);
        assert!(!daemon_unknown.text.contains("differs from this binary"));
        assert!(!daemon_unknown.text.contains("tonguetyped daemon restart"));
        assert!(daemon_unknown.text.contains("cannot confirm"));
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

    /// A daemon stand-in that accepts the connection and never answers must not
    /// hang `stop`: the bounded reply wait gives up, and the socket poll's own
    /// deadline is what ends the call. The deadlines are injected as
    /// milliseconds so the test proves the fall-through without real waits.
    #[tokio::test]
    async fn stop_daemon_gives_up_on_a_silent_daemon_without_hanging() {
        let root = unique_temp_dir("silent-daemon-stop");
        std::fs::create_dir_all(&root).unwrap();
        let sock_path = root.join("control.sock");
        let listener = std::os::unix::net::UnixListener::bind(&sock_path).unwrap();

        // Hold the accepted connection open without ever writing a reply.
        let held = std::thread::spawn(move || {
            if let Ok((stream, _)) = listener.accept() {
                std::thread::sleep(std::time::Duration::from_secs(30));
                drop(stream);
            }
        });

        let error = stop_daemon_at_with(
            &sock_path,
            std::time::Duration::from_millis(50),
            std::time::Duration::from_millis(100),
        )
        .await
        .unwrap_err();
        assert!(
            error.to_string().contains("did not exit within"),
            "expected the socket-poll deadline to end the wait, got: {error}"
        );

        drop(held);
        std::fs::remove_dir_all(root).unwrap();
    }

    /// A fake daemon for one shortcut request: asserts the request type and
    /// replies with `response`.
    fn serve_one_shortcut_request(
        listener: tokio::net::UnixListener,
        expected: Request,
        response: Response,
    ) -> tokio::task::JoinHandle<()> {
        tokio::spawn(async move {
            let (stream, _) = listener.accept().await.unwrap();
            let (reader, mut writer) = stream.into_split();
            let mut reader = tokio::io::BufReader::new(reader);
            let mut line = String::new();
            reader.read_line(&mut line).await.unwrap();
            let request: Request = ipc::decode_frame(&line).unwrap();
            assert_eq!(
                std::mem::discriminant(&request),
                std::mem::discriminant(&expected),
                "unexpected request: {request:?}"
            );
            let frame = ipc::encode_frame(&response).unwrap();
            writer.write_all(frame.as_bytes()).await.unwrap();
        })
    }

    #[tokio::test]
    async fn shortcut_test_routes_to_the_daemon_socket() {
        let root = unique_temp_dir("shortcut-test-ipc");
        std::fs::create_dir_all(&root).unwrap();
        let sock_path = root.join("control.sock");
        let listener = tokio::net::UnixListener::bind(&sock_path).unwrap();
        let server = serve_one_shortcut_request(
            listener,
            Request::ShortcutTest,
            Response::ShortcutTest { pressed: true },
        );

        // No portal is involved on this path; a real daemon reply is enough.
        assert_eq!(
            test_shortcut_at(&sock_path).await.unwrap(),
            crate::activation::ShortcutTestOutcome::Pressed
        );

        server.await.unwrap();
        std::fs::remove_dir_all(root).unwrap();
    }

    #[tokio::test]
    async fn cancelling_a_daemon_configure_settles_on_the_unchanged_trigger() {
        let root = unique_temp_dir("shortcut-configure-ipc");
        std::fs::create_dir_all(&root).unwrap();
        let sock_path = root.join("control.sock");
        let listener = tokio::net::UnixListener::bind(&sock_path).unwrap();

        let configure_path = sock_path.clone();
        let server = tokio::spawn(async move {
            let (stream, _) = listener.accept().await.unwrap();
            let (reader, mut writer) = stream.into_split();
            let mut reader = tokio::io::BufReader::new(reader);
            let mut line = String::new();
            reader.read_line(&mut line).await.unwrap();
            assert!(matches!(
                ipc::decode_frame::<Request>(&line).unwrap(),
                Request::ConfigureShortcut
            ));

            // The cancel arrives on its own connection while the configure
            // request is still waiting, exactly like the setup console's
            // cancel of an open shortcut dialog.
            let (cancel_stream, _) = listener.accept().await.unwrap();
            let (cancel_reader, mut cancel_writer) = cancel_stream.into_split();
            let mut cancel_reader = tokio::io::BufReader::new(cancel_reader);
            let mut cancel_line = String::new();
            cancel_reader.read_line(&mut cancel_line).await.unwrap();
            assert!(matches!(
                ipc::decode_frame::<Request>(&cancel_line).unwrap(),
                Request::CancelShortcutConfigure
            ));
            cancel_writer
                .write_all(ipc::encode_frame(&Response::Ok).unwrap().as_bytes())
                .await
                .unwrap();

            let frame = ipc::encode_frame(&Response::ShortcutConfigured {
                changed: false,
                trigger_description: "Super+O".to_string(),
            })
            .unwrap();
            writer.write_all(frame.as_bytes()).await.unwrap();
        });

        let (cancel_tx, cancel_rx) = tokio::sync::oneshot::channel();
        let call =
            tokio::spawn(async move { configure_shortcut_at(&configure_path, cancel_rx).await });
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;
        cancel_tx.send(()).unwrap();

        assert_eq!(
            call.await.unwrap().unwrap(),
            crate::activation::ReconfigureOutcome::Unchanged {
                trigger_description: "Super+O".to_string(),
            }
        );
        server.await.unwrap();
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn unbound_shortcut_status_is_not_rendered_as_available() {
        let response = Response::Status {
            state: "idle".to_string(),
            activation_mode: "toggle".to_string(),
            operation_error: None,
            config_error: None,
            shortcut_status: ipc::ShortcutStatus::Unbound,
            activation_error: None,
            shortcut_trigger: None,
            build: Some("0.2.2".to_string()),
            model: None,
            backend: None,
            device: None,
            output_configured: None,
            output_backend: None,
            output_warning: None,
            output_fallbacks: 0,
            last_output_failure: None,
        };
        let text = format_response_lines(&response)
            .into_iter()
            .map(|line| line.text)
            .collect::<Vec<_>>()
            .join("\n");
        assert!(text.contains("shortcut status:  unbound (no key set)"));
        assert!(!text.contains("shortcut status:  available"));
    }
}
