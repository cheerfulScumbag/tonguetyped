use crate::audio;

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct DoctorReport {
    pub compositor: String,
    pub desktop: String,
    pub audio_available: bool,
    pub audio_devices: Vec<String>,
    pub model_ready: bool,
    pub model_path: String,
    pub socket_health: String,
    pub helpers_found: Vec<String>,
    pub output_method_available: bool,
}

pub fn run_doctor(config: &crate::config::Config) -> DoctorReport {
    let compositor = detect_compositor();
    let desktop = detect_desktop();
    let audio_available =
        audio::AudioRecorder::new(&config.audio.microphone, 16_000, None, None).is_ok();
    let audio_devices = audio::list_devices()
        .unwrap_or_default()
        .into_iter()
        .map(|d| d.name)
        .collect();
    let model_path = crate::model::ModelCatalog::model_path(&config.model.selected)
        .unwrap_or_else(|_| crate::inference::InferenceEngine::models_dir().join("invalid"));
    let mut engine = crate::inference::InferenceEngine::new(model_path.clone());
    let model_ready = engine.load().is_ok();
    engine.unload();
    let socket_health = check_socket_health();
    let helpers_found = crate::output::list_available_backends();
    let output_method_available = crate::output::has_any_type_backend();

    DoctorReport {
        compositor,
        desktop,
        audio_available,
        audio_devices,
        model_ready,
        model_path: model_path.to_string_lossy().to_string(),
        socket_health,
        helpers_found,
        output_method_available,
    }
}

fn detect_compositor() -> String {
    std::env::var("XDG_SESSION_TYPE").unwrap_or_else(|_| "unknown".to_string())
}

fn detect_desktop() -> String {
    std::env::var("XDG_CURRENT_DESKTOP").unwrap_or_else(|_| "unknown".to_string())
}

fn check_socket_health() -> String {
    let Ok(runtime_dir) = crate::daemon::runtime_dir() else {
        return "runtime_unavailable".to_string();
    };
    let sock = runtime_dir.join("control.sock");
    if sock.exists() {
        "existing".to_string()
    } else {
        "not_running".to_string()
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
    )
}
