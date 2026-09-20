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

pub fn run_doctor(model_name: &str) -> DoctorReport {
    let compositor = detect_compositor();
    let desktop = detect_desktop();
    let audio_available = audio::check_audio_available();
    let audio_devices = audio::list_devices()
        .unwrap_or_default()
        .into_iter()
        .map(|d| d.name)
        .collect();
    let model_path = crate::inference::InferenceEngine::models_dir()
        .join(format!("{}.bin", model_name));
    let model_ready = model_path.exists();
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

pub fn run_typing_test() -> anyhow::Result<()> {
    let backend = crate::output::probe_type_backend();
    if backend == "none" {
        println!("WARNING: No typing backend found! Text output will only work in 'none' mode.");
    } else {
        println!("Typing test skipped (explicit user trigger). Backend available: {}", backend);
    }
    Ok(())
}

fn detect_compositor() -> String {
    std::env::var("XDG_SESSION_TYPE")
        .unwrap_or_else(|_| "unknown".to_string())
}

fn detect_desktop() -> String {
    std::env::var("XDG_CURRENT_DESKTOP")
        .unwrap_or_else(|_| "unknown".to_string())
}

fn check_socket_health() -> String {
    let runtime_dir = crate::daemon::runtime_dir();
    let sock = runtime_dir.join("control.sock");
    if sock.exists() {
        "existing".to_string()
    } else {
        "not_running".to_string()
    }
}

pub fn typing_test() {
    println!("Typing test: this is an explicit user-triggered test.");
    println!("No text will be injected into random windows.");
}