use crate::audio;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt};

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct DoctorReport {
    pub compositor: String,
    pub layer_shell_overlay_available: bool,
    pub audio_available: bool,
    pub audio_devices: Vec<String>,
    pub model_ready: bool,
    pub model_error: Option<String>,
    pub model_path: String,
    pub model_id: String,
    #[serde(default)]
    pub preferred_backend: String,
    pub inference_backend: String,
    pub inference_device: String,
    /// Every backend kind and whether this build and host can use it, next to
    /// the single active one above.
    pub backend_availability: Vec<crate::inference::BackendAvailability>,
    /// This (the `doctor` process's own) binary's `build_info::VERSION`.
    pub build: String,
    pub daemon_build: DaemonBuild,
    pub helpers_found: Vec<String>,
    /// The clipboard helper this session copies with (`wl-copy` or `xclip`)
    /// and whether it is installed.
    pub clipboard_helper: String,
    pub clipboard_available: bool,
    pub output_method: crate::config::OutputMethod,
    /// Why the configured output method cannot work here, naming the remedy,
    /// or `None` when it can.
    pub output_method_problem: Option<String>,
    /// The typing backend the hidden probes selected, `None` when nothing
    /// works. Absent from reports written by an older build.
    #[serde(default)]
    pub output_backend: Option<String>,
    /// A pinned-backend substitution or dead end, in one sentence.
    #[serde(default)]
    pub output_warning: Option<String>,
    pub shortcut_status: Option<crate::ipc::ShortcutStatus>,
    pub shortcut_portal_error: Option<String>,
}

/// What the running daemon reported about its own build, if anything.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub enum DaemonBuild {
    /// No daemon answered a status request.
    NotRunning,
    /// A daemon answered but reported no build: it predates build reporting.
    Unreported,
    Reported(String),
}

/// The parts of the daemon's `Response::Status` that `doctor` reports on.
#[derive(Debug, Clone, PartialEq, Eq)]
struct DaemonHealth {
    shortcut_status: crate::ipc::ShortcutStatus,
    activation_error: Option<String>,
    build: Option<String>,
}

pub async fn run_doctor(config: &crate::config::Config) -> anyhow::Result<DoctorReport> {
    let compositor = detect_compositor();
    let layer_shell_overlay_available = crate::overlay::probe_available();
    let audio_available = audio::AudioRecorder::new(&config.audio.microphone, 16_000, None, None)
        .and_then(|mut recorder| {
            recorder.start()?;
            recorder.stop();
            Ok(())
        })
        .is_ok();
    let audio_devices = audio::list_devices()
        .unwrap_or_default()
        .into_iter()
        .map(|d| d.name)
        .collect();
    let model_path = crate::catalog::model_path(&config.model.active_model)?;
    let preference = crate::inference::BackendPreference::parse(&config.model.preferred_backend)?;
    let (model_ready, model_error, active_backend) = if model_path.exists() {
        let mut engine = crate::inference::InferenceEngine::new(model_path.clone(), preference);
        match engine.load() {
            Ok(()) => {
                let backend = engine.active_backend_info();
                engine.unload();
                (true, None, backend)
            }
            Err(error) => (false, Some(format!("{error:#}")), None),
        }
    } else {
        (false, None, None)
    };
    let helpers_found = crate::output::list_available_backends();
    let clipboard_available = crate::output::clipboard_available();
    let typing = crate::output::resolve_typing(&config.output);
    let output_method_problem = if config.output.method.inserts_text() && typing.resolved.is_none()
    {
        Some(typing.failure_message())
    } else {
        crate::output::method_unavailable_reason(
            config.output.method,
            typing.resolved.is_some(),
            clipboard_available,
        )
        .map(str::to_string)
    };
    let (shortcut_status, shortcut_portal_error, daemon_build) = match daemon_health().await {
        Some(health) => (
            Some(health.shortcut_status),
            health.activation_error,
            health
                .build
                .map_or(DaemonBuild::Unreported, DaemonBuild::Reported),
        ),
        None => (None, None, DaemonBuild::NotRunning),
    };

    // Without a loaded model, report what would be used: the pinned backend
    // (never substituted by another), or the auto chain's capability probe.
    let backend =
        active_backend.unwrap_or_else(|| crate::inference::preferred_backend_info(preference));

    Ok(DoctorReport {
        compositor,
        layer_shell_overlay_available,
        audio_available,
        audio_devices,
        model_ready,
        model_error,
        model_path: model_path.to_string_lossy().to_string(),
        model_id: config.model.active_model.clone(),
        preferred_backend: config.model.preferred_backend.clone(),
        inference_backend: backend.backend,
        inference_device: backend.device,
        backend_availability: crate::inference::backend_availability(),
        build: crate::build_info::VERSION.to_string(),
        daemon_build,
        helpers_found,
        clipboard_helper: crate::output::clipboard_helper().to_string(),
        clipboard_available,
        output_method: config.output.method,
        output_method_problem,
        output_backend: typing.resolved.clone(),
        output_warning: typing.warning(),
        shortcut_status,
        shortcut_portal_error,
    })
}

async fn daemon_health() -> Option<DaemonHealth> {
    let socket_path = crate::daemon::socket_path().ok()?;
    if !socket_path.exists() {
        return None;
    }
    tokio::time::timeout(
        std::time::Duration::from_millis(500),
        daemon_health_at(&socket_path),
    )
    .await
    .ok()
    .flatten()
}

async fn daemon_health_at(socket_path: &std::path::Path) -> Option<DaemonHealth> {
    let stream = tokio::net::UnixStream::connect(socket_path).await.ok()?;
    let (reader, mut writer) = stream.into_split();
    let frame = crate::ipc::encode_frame(&crate::ipc::Request::Status).ok()?;
    writer.write_all(frame.as_bytes()).await.ok()?;
    let mut reader = tokio::io::BufReader::new(reader);
    let mut line = String::new();
    reader.read_line(&mut line).await.ok()?;
    match crate::ipc::decode_frame::<crate::ipc::Response>(&line).ok()? {
        crate::ipc::Response::Status {
            shortcut_status,
            activation_error,
            build,
            ..
        } => Some(DaemonHealth {
            shortcut_status,
            activation_error,
            build,
        }),
        _ => None,
    }
}

fn detect_compositor() -> String {
    let session_type = std::env::var("XDG_SESSION_TYPE")
        .ok()
        .filter(|value| !value.is_empty());
    let desktop = [
        "XDG_CURRENT_DESKTOP",
        "XDG_SESSION_DESKTOP",
        "DESKTOP_SESSION",
    ]
    .into_iter()
    .find_map(|name| std::env::var(name).ok().filter(|value| !value.is_empty()));

    match (desktop, session_type) {
        (Some(desktop), Some(session_type)) if !desktop.eq_ignore_ascii_case(&session_type) => {
            format!("{desktop} ({session_type})")
        }
        (Some(desktop), _) => desktop,
        (None, Some(session_type)) => session_type,
        (None, None) => "unknown".to_string(),
    }
}

/// Sends a test transcript through the configured output method and
/// returns a sentence saying where it went.
pub fn typing_test(config: &crate::config::Config) -> anyhow::Result<String> {
    use crate::config::OutputMethod;
    use crate::output::Delivery;
    if config.output.method == OutputMethod::None {
        anyhow::bail!("the output test needs output.method 'type', 'clipboard', or 'paste'");
    }
    const EXPECTED: &str = "TongueTyped typing test";

    // The only place a real "did the key arrive" check belongs. For typing,
    // give the round trip a surface TongueTyped owns, so it cannot type into
    // an unrelated application. Without a layer-shell Wayland session, fall
    // through to the focused-application test for clipboard/paste and older
    // sessions.
    if config.output.method == OutputMethod::Type {
        let output_config = config.output.clone();
        let outcome = crate::typing_test_surface::run(
            EXPECTED,
            std::time::Duration::from_secs(20),
            move || crate::output::output_text(EXPECTED, &output_config),
        );
        match outcome {
            crate::typing_test_surface::SurfaceTest::Unavailable => {}
            crate::typing_test_surface::SurfaceTest::NoFocus => {
                return Ok(
                    "Opened the TongueTyped test window, but it never received keyboard focus, \
                     so the round-trip test was not run."
                        .to_string(),
                )
            }
            crate::typing_test_surface::SurfaceTest::Completed { delivery, captured } => {
                let delivery = delivery?;
                return Ok(match delivery {
                    Delivery::Inserted if captured == EXPECTED => {
                        "Typed the test text into the TongueTyped test window.".to_string()
                    }
                    Delivery::Inserted => format!(
                        "Typing reported success, but the test window received \"{captured}\" \
                         instead of the test text."
                    ),
                    Delivery::Kept => "Kept the test text in TongueTyped.".to_string(),
                    Delivery::Copied => "Copied the test text to the clipboard.".to_string(),
                    Delivery::CopiedAfterFailure(error) => format!(
                        "Typing failed ({error}); copied the test text to the clipboard instead."
                    ),
                    Delivery::SubmitFailed(error) => {
                        format!("Typed the test text, but pressing Enter failed ({error}).")
                    }
                });
            }
        }
    }

    let delivery = crate::output::output_text(EXPECTED, &config.output)?;
    Ok(match delivery {
        Delivery::Kept => "Kept the test text in TongueTyped.".to_string(),
        Delivery::Inserted if config.output.method == OutputMethod::Paste => {
            "Pasted the test text into the focused application (delivery not verified by \
             TongueTyped)."
                .to_string()
        }
        Delivery::Inserted => {
            "Typed the test text into the focused application (delivery not verified by \
             TongueTyped)."
                .to_string()
        }
        Delivery::Copied => "Copied the test text to the clipboard.".to_string(),
        Delivery::CopiedAfterFailure(error) => {
            format!("Typing failed ({error}); copied the test text to the clipboard instead.")
        }
        Delivery::SubmitFailed(error) if config.output.method == OutputMethod::Paste => {
            format!("Pasted the test text, but pressing Enter failed ({error}).")
        }
        Delivery::SubmitFailed(error) => {
            format!("Typed the test text, but pressing Enter failed ({error}).")
        }
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn reads_activation_health_and_build_from_running_daemon() {
        let root = std::env::temp_dir().join(format!(
            "tonguetyped-doctor-test-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir_all(&root).unwrap();
        let socket_path = root.join("control.sock");
        let listener = tokio::net::UnixListener::bind(&socket_path).unwrap();
        let server = tokio::spawn(async move {
            let (stream, _) = listener.accept().await.unwrap();
            let (reader, mut writer) = stream.into_split();
            let mut reader = tokio::io::BufReader::new(reader);
            let mut line = String::new();
            reader.read_line(&mut line).await.unwrap();
            let request: crate::ipc::Request = crate::ipc::decode_frame(&line).unwrap();
            assert!(matches!(request, crate::ipc::Request::Status));
            let response = crate::ipc::Response::Status {
                state: "idle".to_string(),
                activation_mode: "hold".to_string(),
                operation_error: Some("output failed".to_string()),
                config_error: None,
                shortcut_status: crate::ipc::ShortcutStatus::Failed,
                activation_error: Some("activation listener failed".to_string()),
                shortcut_trigger: None,
                build: Some("0.1.0 (a1b2c3d)".to_string()),
                model: None,
                backend: None,
                device: None,
                output_configured: None,
                output_backend: None,
                output_warning: None,
                output_fallbacks: 0,
                last_output_failure: None,
            };
            writer
                .write_all(crate::ipc::encode_frame(&response).unwrap().as_bytes())
                .await
                .unwrap();
        });

        assert_eq!(
            daemon_health_at(&socket_path).await,
            Some(DaemonHealth {
                shortcut_status: crate::ipc::ShortcutStatus::Failed,
                activation_error: Some("activation listener failed".to_string()),
                build: Some("0.1.0 (a1b2c3d)".to_string()),
            })
        );
        server.await.unwrap();
        std::fs::remove_dir_all(root).unwrap();
    }
}
