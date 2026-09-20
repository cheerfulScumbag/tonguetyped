use serde::{Deserialize, Serialize};
use std::fs;
use std::path::PathBuf;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Config {
    #[serde(default)]
    pub activation: ActivationConfig,
    #[serde(default)]
    pub model: ModelConfig,
    #[serde(default)]
    pub audio: AudioConfig,
    #[serde(default)]
    pub output: OutputConfig,
    #[serde(default)]
    pub transcription: TranscriptionConfig,
    #[serde(default)]
    pub history: HistoryConfig,
    #[serde(default)]
    pub overlay: OverlayConfig,
    #[serde(default)]
    pub startup: StartupConfig,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            activation: ActivationConfig::default(),
            model: ModelConfig::default(),
            audio: AudioConfig::default(),
            output: OutputConfig::default(),
            transcription: TranscriptionConfig::default(),
            history: HistoryConfig::default(),
            overlay: OverlayConfig::default(),
            startup: StartupConfig::default(),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ActivationConfig {
    #[serde(default = "default_activation_mode")]
    pub mode: ActivationMode,
    #[serde(default = "default_keybind")]
    pub keybind: String,
    #[serde(default = "default_keybind_status")]
    pub keybind_status: String,
}

impl Default for ActivationConfig {
    fn default() -> Self {
        ActivationConfig {
            mode: ActivationMode::Hold,
            keybind: default_keybind(),
            keybind_status: default_keybind_status(),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "lowercase")]
pub enum ActivationMode {
    Hold,
    Toggle,
}

impl std::fmt::Display for ActivationMode {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ActivationMode::Hold => write!(f, "hold"),
            ActivationMode::Toggle => write!(f, "toggle"),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ModelConfig {
    #[serde(default = "default_model_selected")]
    pub selected: String,
    #[serde(default)]
    pub idle_unload: IdleUnloadConfig,
}

impl Default for ModelConfig {
    fn default() -> Self {
        Self {
            selected: default_model_selected(),
            idle_unload: IdleUnloadConfig::default(),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct IdleUnloadConfig {
    #[serde(default = "default_idle_unload_policy")]
    pub policy: IdleUnloadPolicy,
    #[serde(default = "default_idle_timeout_minutes")]
    pub timeout_minutes: u64,
}

impl Default for IdleUnloadConfig {
    fn default() -> Self {
        Self {
            policy: IdleUnloadPolicy::AfterIdle,
            timeout_minutes: default_idle_timeout_minutes(),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Default)]
#[serde(rename_all = "snake_case")]
pub enum IdleUnloadPolicy {
    #[default]
    Never,
    AfterTranscription,
    AfterIdle,
}

impl std::fmt::Display for IdleUnloadPolicy {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            IdleUnloadPolicy::Never => write!(f, "never"),
            IdleUnloadPolicy::AfterTranscription => write!(f, "after_transcription"),
            IdleUnloadPolicy::AfterIdle => write!(f, "after_idle"),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AudioConfig {
    #[serde(default = "default_microphone")]
    pub microphone: String,
    #[serde(default = "default_true")]
    pub feedback_sounds: bool,
    #[serde(default = "default_feedback_volume")]
    pub feedback_volume: f64,
    #[serde(default = "default_feedback_device")]
    pub feedback_device: String,
    #[serde(default)]
    pub mute_playback: MutePlaybackConfig,
}

impl Default for AudioConfig {
    fn default() -> Self {
        Self {
            microphone: default_microphone(),
            feedback_sounds: true,
            feedback_volume: default_feedback_volume(),
            feedback_device: default_feedback_device(),
            mute_playback: MutePlaybackConfig::default(),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct MutePlaybackConfig {
    #[serde(default = "default_false")]
    pub enabled: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct OutputConfig {
    #[serde(default = "default_output_method")]
    pub method: OutputMethod,
    #[serde(default = "default_typing_backend")]
    pub typing_backend: String,
    #[serde(default = "default_false")]
    pub auto_submit: bool,
    #[serde(default)]
    pub paste: PasteConfig,
}

impl Default for OutputConfig {
    fn default() -> Self {
        Self {
            method: OutputMethod::None,
            typing_backend: default_typing_backend(),
            auto_submit: false,
            paste: PasteConfig::default(),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Default)]
#[serde(rename_all = "kebab-case")]
pub enum OutputMethod {
    #[default]
    None,
    Type,
}

impl std::fmt::Display for OutputMethod {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            OutputMethod::None => write!(f, "none"),
            OutputMethod::Type => write!(f, "type"),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PasteConfig {
    #[serde(default = "default_paste_shortcut")]
    pub shortcut: String,
    #[serde(default = "default_true")]
    pub restore_clipboard: bool,
    #[serde(default = "default_50")]
    pub delay_before_ms: u64,
    #[serde(default = "default_300")]
    pub delay_after_ms: u64,
}

impl Default for PasteConfig {
    fn default() -> Self {
        Self {
            shortcut: default_paste_shortcut(),
            restore_clipboard: true,
            delay_before_ms: default_50(),
            delay_after_ms: default_300(),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TranscriptionConfig {
    #[serde(default = "default_true")]
    pub vad_enabled: bool,
    #[serde(default = "default_language")]
    pub language: String,
    #[serde(default = "default_true")]
    pub trailing_space: bool,
    #[serde(default = "default_max_recording_seconds")]
    pub max_recording_seconds: u64,
}

impl Default for TranscriptionConfig {
    fn default() -> Self {
        Self {
            vad_enabled: true,
            language: default_language(),
            trailing_space: true,
            max_recording_seconds: default_max_recording_seconds(),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct HistoryConfig {
    #[serde(default = "default_true")]
    pub enabled: bool,
    #[serde(default = "default_max_entries")]
    pub max_entries: u64,
    #[serde(default = "default_false")]
    pub save_recordings: bool,
    #[serde(default)]
    pub recording_expiry: RecordingExpiryConfig,
}

impl Default for HistoryConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            max_entries: default_max_entries(),
            save_recordings: false,
            recording_expiry: RecordingExpiryConfig::default(),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RecordingExpiryConfig {
    #[serde(default = "default_recording_expiry_policy")]
    pub policy: RecordingExpiryPolicy,
    #[serde(default = "default_1440")]
    pub after_minutes: u64,
}

impl Default for RecordingExpiryConfig {
    fn default() -> Self {
        Self {
            policy: RecordingExpiryPolicy::Immediately,
            after_minutes: default_1440(),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Default)]
#[serde(rename_all = "snake_case")]
pub enum RecordingExpiryPolicy {
    #[default]
    Immediately,
    AfterMinutes,
    Never,
}

impl std::fmt::Display for RecordingExpiryPolicy {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            RecordingExpiryPolicy::Immediately => write!(f, "immediately"),
            RecordingExpiryPolicy::AfterMinutes => write!(f, "after_minutes"),
            RecordingExpiryPolicy::Never => write!(f, "never"),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct OverlayConfig {
    #[serde(default = "default_false")]
    pub enabled: bool,
    #[serde(default = "default_overlay_position")]
    pub position: String,
    #[serde(default = "default_overlay_monitor")]
    pub monitor: String,
}

impl Default for OverlayConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            position: default_overlay_position(),
            monitor: default_overlay_monitor(),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct StartupConfig {
    #[serde(default = "default_false")]
    pub autostart: bool,
}

fn default_activation_mode() -> ActivationMode {
    ActivationMode::Hold
}

fn default_keybind() -> String {
    "Super+O".to_string()
}

fn default_keybind_status() -> String {
    "untested".to_string()
}

fn default_model_selected() -> String {
    "whisper-small-q5_1".to_string()
}

fn default_idle_unload_policy() -> IdleUnloadPolicy {
    IdleUnloadPolicy::AfterIdle
}

fn default_idle_timeout_minutes() -> u64 {
    15
}

fn default_microphone() -> String {
    "default".to_string()
}

fn default_true() -> bool {
    true
}

fn default_false() -> bool {
    false
}

fn default_feedback_volume() -> f64 {
    0.7
}

fn default_feedback_device() -> String {
    "default".to_string()
}

fn default_output_method() -> OutputMethod {
    OutputMethod::None
}

fn default_typing_backend() -> String {
    "auto".to_string()
}

fn default_paste_shortcut() -> String {
    "ctrl+shift+v".to_string()
}

fn default_50() -> u64 {
    50
}

fn default_300() -> u64 {
    300
}

fn default_language() -> String {
    "auto".to_string()
}

fn default_max_recording_seconds() -> u64 {
    120
}

fn default_max_entries() -> u64 {
    500
}

fn default_recording_expiry_policy() -> RecordingExpiryPolicy {
    RecordingExpiryPolicy::Immediately
}

fn default_1440() -> u64 {
    1440
}

fn default_overlay_position() -> String {
    "top-right".to_string()
}

fn default_overlay_monitor() -> String {
    "active".to_string()
}

impl Config {
    pub fn config_path() -> PathBuf {
        let dir = directories::BaseDirs::new()
            .map(|b| b.config_dir().join("tonguetyped"))
            .unwrap_or_else(|| PathBuf::from(".config/tonguetyped"));
        fs::create_dir_all(&dir).ok();
        dir.join("config.toml")
    }

    pub fn load() -> anyhow::Result<Self> {
        let path = Self::config_path();
        if !path.exists() {
            let config = Config::default();
            config.save()?;
            return Ok(config);
        }
        let content = fs::read_to_string(&path)?;
        let config: Config = toml::from_str(&content)?;
        config.validate()?;
        Ok(config)
    }

    pub fn reload() -> anyhow::Result<Self> {
        let path = Self::config_path();
        if !path.exists() {
            return Ok(Config::default());
        }
        let content = fs::read_to_string(&path)?;
        let config: Config = toml::from_str(&content)?;
        config.validate()?;
        Ok(config)
    }

    pub fn save(&self) -> anyhow::Result<()> {
        self.validate()?;
        let path = Self::config_path();
        let content = toml::to_string_pretty(self)?;
        let tmp_path = path.with_extension("tmp");
        fs::write(&tmp_path, &content)?;
        fs::rename(&tmp_path, &path)?;
        Ok(())
    }

    pub fn validate(&self) -> anyhow::Result<()> {
        crate::activation::portal_trigger(&self.activation.keybind)?;
        crate::model::ModelCatalog::model_file_name(&self.model.selected)?;
        if self.transcription.max_recording_seconds == 0 {
            anyhow::bail!("max_recording_seconds must be a positive integer");
        }

        if self.output.method == OutputMethod::None && self.output.auto_submit {
            anyhow::bail!("auto_submit cannot be true when output.method is 'none'");
        }
        if !matches!(
            self.output.typing_backend.as_str(),
            "auto" | "wtype" | "enigo" | "dotool"
        ) {
            anyhow::bail!("unsupported typing backend: {}", self.output.typing_backend);
        }

        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn defaults_are_valid_and_match_stage_one_contract() {
        let config = Config::default();
        config.validate().unwrap();
        assert_eq!(config.transcription.max_recording_seconds, 120);
        assert_eq!(config.model.selected, "whisper-small-q5_1");
        assert_eq!(config.model.idle_unload.policy, IdleUnloadPolicy::AfterIdle);
        assert_eq!(config.history.max_entries, 500);
    }

    #[test]
    fn rejects_unsupported_runtime_selections() {
        let mut config = Config::default();
        config.output.typing_backend = "typo".to_string();
        assert!(config.validate().is_err());

        config.output.typing_backend = "auto".to_string();
        config.model.selected = "../custom".to_string();
        assert!(config.validate().is_err());
    }
}
