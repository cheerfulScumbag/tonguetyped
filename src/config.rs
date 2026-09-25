use serde::{Deserialize, Serialize};
use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

static TEMPORARY_FILE_SEQUENCE: AtomicU64 = AtomicU64::new(0);

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
#[serde(deny_unknown_fields)]
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

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
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
#[serde(deny_unknown_fields)]
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
#[serde(deny_unknown_fields)]
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
#[serde(deny_unknown_fields)]
pub struct AudioConfig {
    #[serde(default = "default_microphone")]
    pub microphone: String,
    #[serde(default = "default_false")]
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
            feedback_sounds: false,
            feedback_volume: default_feedback_volume(),
            feedback_device: default_feedback_device(),
            mute_playback: MutePlaybackConfig::default(),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
#[serde(deny_unknown_fields)]
pub struct MutePlaybackConfig {
    #[serde(default = "default_false")]
    pub enabled: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
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
#[serde(deny_unknown_fields)]
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
#[serde(deny_unknown_fields)]
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
#[serde(deny_unknown_fields)]
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
#[serde(deny_unknown_fields)]
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
#[serde(deny_unknown_fields)]
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
#[serde(deny_unknown_fields)]
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
    pub fn config_path() -> anyhow::Result<PathBuf> {
        let dir = directories::BaseDirs::new()
            .map(|b| b.config_dir().join("tonguetyped"))
            .ok_or_else(|| anyhow::anyhow!("cannot determine the user configuration directory"))?;
        fs::create_dir_all(&dir)?;
        Ok(dir.join("config.toml"))
    }

    pub fn load() -> anyhow::Result<Self> {
        let path = Self::config_path()?;
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
        let path = Self::config_path()?;
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
        let path = Self::config_path()?;
        let content = toml::to_string_pretty(self)?;
        atomic_write(&path, content.as_bytes())?;
        Ok(())
    }

    pub fn validate(&self) -> anyhow::Result<()> {
        crate::activation::portal_trigger(&self.activation.keybind)?;
        crate::model::ModelCatalog::model_file_name(&self.model.selected)?;
        if self.transcription.max_recording_seconds == 0 {
            anyhow::bail!("max_recording_seconds must be a positive integer");
        }
        if std::time::Instant::now()
            .checked_add(std::time::Duration::from_secs(
                self.transcription.max_recording_seconds,
            ))
            .is_none()
        {
            anyhow::bail!("max_recording_seconds is too large");
        }
        if self.model.idle_unload.policy == IdleUnloadPolicy::AfterIdle {
            let seconds = self
                .model
                .idle_unload
                .timeout_minutes
                .checked_mul(60)
                .ok_or_else(|| anyhow::anyhow!("idle unload timeout is too large"))?;
            if std::time::Instant::now()
                .checked_add(std::time::Duration::from_secs(seconds))
                .is_none()
            {
                anyhow::bail!("idle unload timeout is too large");
            }
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
        if self.audio.feedback_sounds {
            anyhow::bail!("audio.feedback_sounds is not supported in Stage 1");
        }
        if self.audio.mute_playback.enabled {
            anyhow::bail!("audio.mute_playback.enabled is not supported in Stage 1");
        }
        if self.history.save_recordings {
            anyhow::bail!("history.save_recordings is not supported in Stage 1");
        }
        if self.overlay.enabled {
            anyhow::bail!("overlay.enabled is not supported in Stage 1");
        }
        Ok(())
    }
}

pub(crate) fn atomic_write(path: &Path, content: &[u8]) -> anyhow::Result<()> {
    let file_name = path
        .file_name()
        .and_then(|name| name.to_str())
        .ok_or_else(|| anyhow::anyhow!("configuration path has no file name"))?;
    let sequence = TEMPORARY_FILE_SEQUENCE.fetch_add(1, Ordering::Relaxed);
    let temporary = path.with_file_name(format!(
        ".{file_name}.{}.{sequence}.tmp",
        std::process::id()
    ));
    atomic_write_at(path, &temporary, content)
}

fn atomic_write_at(path: &Path, temporary: &Path, content: &[u8]) -> anyhow::Result<()> {
    let mut created = false;
    let result = (|| {
        let mut options = fs::OpenOptions::new();
        options.write(true).create_new(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            options.mode(0o600);
        }
        let mut file = options.open(temporary)?;
        created = true;
        file.write_all(content)?;
        file.sync_all()?;
        fs::rename(temporary, path)?;
        created = false;
        Ok::<_, anyhow::Error>(())
    })();
    if created {
        let _ = fs::remove_file(temporary);
    }
    result
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn defaults_are_valid_and_match_stage_one_contract() {
        let config = Config::default();
        config.validate().unwrap();
        assert!(!config.audio.feedback_sounds);
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

    #[test]
    fn rejects_enabled_later_stage_features() {
        let mut config = Config::default();
        config.audio.feedback_sounds = true;
        assert!(config.validate().is_err());

        let mut config = Config::default();
        config.audio.mute_playback.enabled = true;
        assert!(config.validate().is_err());

        let mut config = Config::default();
        config.history.save_recordings = true;
        assert!(config.validate().is_err());

        let mut config = Config::default();
        config.overlay.enabled = true;
        assert!(config.validate().is_err());

        let mut config = Config::default();
        config.startup.autostart = true;
        assert!(config.validate().is_ok());
    }

    #[test]
    fn rejects_unknown_fields_and_unrepresentable_recording_duration() {
        assert!(toml::from_str::<Config>("[transcription]\nmax_recording_second = 10\n").is_err());

        let mut config = Config::default();
        config.transcription.max_recording_seconds = u64::MAX;
        assert!(config.validate().is_err());

        let mut config = Config::default();
        config.model.idle_unload.timeout_minutes = u64::MAX;
        assert!(config.validate().is_err());
    }

    #[test]
    fn atomic_write_failure_preserves_destination_and_foreign_temporary_file() {
        let root = std::env::temp_dir().join(format!(
            "tonguetyped-config-atomic-{}-{}",
            std::process::id(),
            TEMPORARY_FILE_SEQUENCE.fetch_add(1, Ordering::Relaxed)
        ));
        fs::create_dir_all(&root).unwrap();
        let destination = root.join("config.toml");
        let temporary = root.join("occupied.tmp");
        fs::write(&destination, "original").unwrap();
        fs::write(&temporary, "another writer").unwrap();

        assert!(atomic_write_at(&destination, &temporary, b"replacement").is_err());
        assert_eq!(fs::read_to_string(&destination).unwrap(), "original");
        assert_eq!(fs::read_to_string(&temporary).unwrap(), "another writer");
        fs::remove_dir_all(root).unwrap();
    }
}
