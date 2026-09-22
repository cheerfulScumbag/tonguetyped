use crate::audio;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt};

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct DoctorReport {
    pub compositor: String,
    pub audio_available: bool,
    pub audio_devices: Vec<String>,
    pub model_ready: bool,
    pub model_error: Option<String>,
    pub model_path: String,
    pub helpers_found: Vec<String>,
    pub output_method_available: bool,
    pub shortcut_portal_checked: bool,
    pub shortcut_portal_error: Option<String>,
}

pub async fn run_doctor(config: &crate::config::Config) -> anyhow::Result<DoctorReport> {
    let compositor = detect_compositor();
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
    let model_path = crate::model::ModelCatalog::model_path(&config.model.selected)?;
    let (model_ready, model_error) = if model_path.exists() {
        let mut engine = crate::inference::InferenceEngine::new(model_path.clone());
        match engine.load() {
            Ok(()) => {
                engine.unload();
                (true, None)
            }
            Err(error) => (false, Some(error.to_string())),
        }
    } else {
        (false, None)
    };
    let helpers_found = crate::output::list_available_backends();
    let output_method_available = config.output.method == crate::config::OutputMethod::None
        || crate::output::type_backend_available(&config.output.typing_backend);
    let daemon_activation_error = daemon_activation_error().await;
    let shortcut_portal_checked = daemon_activation_error.is_some();
    let shortcut_portal_error = daemon_activation_error.flatten();

    Ok(DoctorReport {
        compositor,
        audio_available,
        audio_devices,
        model_ready,
        model_error,
        model_path: model_path.to_string_lossy().to_string(),
        helpers_found,
        output_method_available,
        shortcut_portal_checked,
        shortcut_portal_error,
    })
}

async fn daemon_activation_error() -> Option<Option<String>> {
    let socket_path = crate::daemon::socket_path().ok()?;
    if !socket_path.exists() {
        return None;
    }
    tokio::time::timeout(
        std::time::Duration::from_millis(500),
        daemon_activation_error_at(&socket_path),
    )
    .await
    .ok()
    .flatten()
}

async fn daemon_activation_error_at(socket_path: &std::path::Path) -> Option<Option<String>> {
    let stream = tokio::net::UnixStream::connect(socket_path).await.ok()?;
    let (reader, mut writer) = stream.into_split();
    let frame = crate::ipc::encode_frame(&crate::ipc::Request::Status).ok()?;
    writer.write_all(frame.as_bytes()).await.ok()?;
    let mut reader = tokio::io::BufReader::new(reader);
    let mut line = String::new();
    reader.read_line(&mut line).await.ok()?;
    match crate::ipc::decode_frame::<crate::ipc::Response>(&line).ok()? {
        crate::ipc::Response::Status {
            activation_error, ..
        } => Some(activation_error),
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

pub fn typing_test(config: &crate::config::Config) -> anyhow::Result<()> {
    if config.output.method == crate::config::OutputMethod::None {
        anyhow::bail!("typing test requires output.method = 'type'");
    }
    crate::output::output_text(
        "TongueTyped typing test",
        &config.output.method,
        &config.output.typing_backend,
        config.output.auto_submit,
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn reads_activation_health_from_running_daemon() {
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
                activation_error: Some("activation listener failed".to_string()),
            };
            writer
                .write_all(crate::ipc::encode_frame(&response).unwrap().as_bytes())
                .await
                .unwrap();
        });

        assert_eq!(
            daemon_activation_error_at(&socket_path).await,
            Some(Some("activation listener failed".to_string()))
        );
        server.await.unwrap();
        std::fs::remove_dir_all(root).unwrap();
    }
}
