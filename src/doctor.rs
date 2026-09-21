use crate::audio;

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct DoctorReport {
    pub compositor: String,
    pub audio_available: bool,
    pub audio_devices: Vec<String>,
    pub model_ready: bool,
    pub model_path: String,
    pub helpers_found: Vec<String>,
    pub output_method_available: bool,
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
    let mut engine = crate::inference::InferenceEngine::new(model_path.clone());
    let model_ready = engine.load().is_ok();
    engine.unload();
    let helpers_found = crate::output::list_available_backends();
    let output_method_available = config.output.method == crate::config::OutputMethod::None
        || crate::output::type_backend_available(&config.output.typing_backend);
    let shortcut_portal_error = crate::activation::portal_error(&config.activation.keybind).await;

    Ok(DoctorReport {
        compositor,
        audio_available,
        audio_devices,
        model_ready,
        model_path: model_path.to_string_lossy().to_string(),
        helpers_found,
        output_method_available,
        shortcut_portal_error,
    })
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
