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
    /// The dictation shortcut the desktop currently has bound for this app,
    /// as reported by the global-shortcuts portal and normalized into the
    /// app's own wording (KDE's `Meta+O` is stored as `Super+O`), or empty
    /// when none is known yet. TongueTyped never chooses or registers a key:
    /// the binding belongs to the desktop, so this is only the last value the
    /// portal told us is bound. Render it with
    /// `crate::activation::keybind_display`, never parse or re-register it.
    #[serde(default)]
    pub keybind: String,
}

impl Default for ActivationConfig {
    fn default() -> Self {
        ActivationConfig {
            mode: ActivationMode::Hold,
            keybind: String::new(),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "lowercase")]
pub enum ActivationMode {
    Hold,
    Toggle,
}

/// The two `ActivationMode` choices as both configuration UIs label them, in
/// order (index 0 = `Hold`, index 1 = `Toggle`).
pub(crate) const ACTIVATION_MODE_LABELS: [&str; 2] = [
    "Hold the shortcut while speaking",
    "Press once to start and again to stop",
];

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
    /// Which `crate::catalog` GGUF entry `InferenceEngine` loads, on every
    /// backend alike (CPU, Vulkan, CUDA, ROCm, Metal) - managed with
    /// `tonguetyped model`. Backend-neutral: the same file is requested on
    /// whichever accelerator is available and on the CPU fallback, so
    /// switching backends (or losing accelerator hardware) never changes
    /// which model is in use. A config written before this field existed is
    /// migrated automatically - see `migrate_legacy_model_config`.
    #[serde(default = "default_active_model")]
    pub active_model: String,
    /// Which inference backend `InferenceEngine::load` uses: `"auto"` tries
    /// every compiled-in accelerator in priority order and falls back to CPU;
    /// any other `crate::inference::BACKEND_PREFERENCES` value pins that one
    /// backend and fails loudly rather than falling back.
    #[serde(default = "default_preferred_backend")]
    pub preferred_backend: String,
    #[serde(default)]
    pub idle_unload: IdleUnloadConfig,
}

impl Default for ModelConfig {
    fn default() -> Self {
        Self {
            active_model: default_active_model(),
            preferred_backend: default_preferred_backend(),
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
    /// Pause MPRIS media players that are playing when a recording starts and
    /// resume them when it ends (see `crate::media`). Off by default.
    #[serde(default = "default_false")]
    pub pause_media: bool,
}

impl Default for AudioConfig {
    fn default() -> Self {
        Self {
            microphone: default_microphone(),
            feedback_sounds: false,
            feedback_volume: default_feedback_volume(),
            feedback_device: default_feedback_device(),
            mute_playback: MutePlaybackConfig::default(),
            pause_media: false,
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
    /// Press Enter after the transcript is typed or pasted, submitting it in
    /// chat boxes and terminals. Only `type` and `paste` insert text, so
    /// `validate` rejects it for the other methods.
    #[serde(default = "default_false")]
    pub auto_submit: bool,
    #[serde(default)]
    pub paste_keys: PasteKeys,
}

impl Default for OutputConfig {
    fn default() -> Self {
        Self {
            method: OutputMethod::None,
            typing_backend: default_typing_backend(),
            auto_submit: false,
            paste_keys: PasteKeys::default(),
        }
    }
}

/// Where a finished transcript goes. Every method also records it in history
/// and `last-result`; `type` and `paste` fall back to leaving it on the
/// clipboard when the typing helper fails, so a transcript is never lost.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, Default)]
#[serde(rename_all = "kebab-case")]
pub enum OutputMethod {
    /// Keep the transcript in TongueTyped only.
    #[default]
    None,
    /// Type the transcript into the focused application.
    Type,
    /// Copy the transcript to the clipboard without typing anything.
    Clipboard,
    /// Copy the transcript to the clipboard, then press the paste keys.
    Paste,
}

impl OutputMethod {
    /// Whether this method inserts text into the focused application, the
    /// only case where pressing Enter afterwards (`auto_submit`) means
    /// anything.
    pub fn inserts_text(self) -> bool {
        matches!(self, OutputMethod::Type | OutputMethod::Paste)
    }
}

impl std::fmt::Display for OutputMethod {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            OutputMethod::None => write!(f, "none"),
            OutputMethod::Type => write!(f, "type"),
            OutputMethod::Clipboard => write!(f, "clipboard"),
            OutputMethod::Paste => write!(f, "paste"),
        }
    }
}

/// The key combination `paste` mode presses after copying. Ctrl+V pastes in
/// most applications; terminals bind it to something else and paste the
/// clipboard on Shift+Insert instead.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, Default)]
#[serde(rename_all = "kebab-case")]
pub enum PasteKeys {
    #[default]
    CtrlV,
    ShiftInsert,
}

impl PasteKeys {
    pub const ALL: [PasteKeys; 2] = [PasteKeys::CtrlV, PasteKeys::ShiftInsert];
}

impl std::fmt::Display for PasteKeys {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            PasteKeys::CtrlV => write!(f, "Ctrl+V"),
            PasteKeys::ShiftInsert => write!(f, "Shift+Insert"),
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
    /// Longest audio window handed to the decoder in one `run` call, in
    /// seconds. A recording (or file) longer than this is decoded in
    /// back-to-back chunks that each fit the window, so one decode never
    /// holds an unbounded buffer in memory or blocks for minutes on end.
    /// Chunk boundaries fall at the quietest point near each limit, so a
    /// split lands in a pause rather than through a word. `0` disables
    /// chunking and always sends the whole buffer in one call.
    #[serde(default = "default_chunk_seconds")]
    pub chunk_seconds: u64,
    /// Text the Whisper decoder reads as if it came just before the
    /// recording, which biases it toward the names, jargon and product terms
    /// it contains. Whisper also copies the prompt's style, so a punctuated
    /// sentence keeps punctuation in the transcript where a bare word list
    /// drops it. Empty (the default) sends no prompt at all.
    #[serde(default)]
    pub initial_prompt: String,
    /// Translate the speech into English instead of transcribing it in the
    /// spoken language. Ignored by models that cannot translate (the `.en`
    /// models).
    #[serde(default = "default_false")]
    pub translate: bool,
    /// The Whisper decode knobs in `DECODE_KNOBS`. Each is absent by default,
    /// which keeps the model's own value; see `DECODE_KNOBS` for the ranges.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub temperature: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub temperature_increment: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub compression_ratio_threshold: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub logprob_threshold: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub no_speech_threshold: Option<f64>,
}

impl Default for TranscriptionConfig {
    fn default() -> Self {
        Self {
            vad_enabled: true,
            language: default_language(),
            trailing_space: true,
            max_recording_seconds: default_max_recording_seconds(),
            chunk_seconds: default_chunk_seconds(),
            initial_prompt: String::new(),
            translate: false,
            temperature: None,
            temperature_increment: None,
            compression_ratio_threshold: None,
            logprob_threshold: None,
            no_speech_threshold: None,
        }
    }
}

/// One optional numeric Whisper decode knob in `TranscriptionConfig`: its
/// TOML key, the label both configuration UIs show, and the values it
/// accepts. A knob that is unset keeps the model's own default.
pub(crate) struct DecodeKnob {
    pub key: &'static str,
    pub label: &'static str,
    /// The value transcribe.cpp uses when the knob is unset.
    pub model_default: &'static str,
    /// The accepted values, worded to follow "must be".
    pub range: &'static str,
    accepts: fn(f64) -> bool,
    pub get: fn(&TranscriptionConfig) -> Option<f64>,
    pub set: fn(&mut TranscriptionConfig, Option<f64>),
}

impl DecodeKnob {
    pub fn accepts(&self, value: f64) -> bool {
        (self.accepts)(value)
    }
}

/// The numeric Whisper decode knobs, in the order both configuration UIs
/// list them. The thresholds take `inf` (`-inf` for the log-probability
/// one) to turn that check off, as transcribe.cpp's own sentinels do.
pub(crate) const DECODE_KNOBS: [DecodeKnob; 5] = [
    DecodeKnob {
        key: "temperature",
        label: "Temperature",
        model_default: "0",
        range: "between 0 and 1",
        accepts: |value| (0.0..=1.0).contains(&value),
        get: |config| config.temperature,
        set: |config, value| config.temperature = value,
    },
    DecodeKnob {
        key: "temperature_increment",
        label: "Temperature step",
        model_default: "0.2",
        range: "between 0 and 1 (0 turns the retry off)",
        accepts: |value| (0.0..=1.0).contains(&value),
        get: |config| config.temperature_increment,
        set: |config, value| config.temperature_increment = value,
    },
    DecodeKnob {
        key: "compression_ratio_threshold",
        label: "Compression limit",
        model_default: "2.4",
        range: "above 0, or inf to turn it off",
        accepts: |value| value > 0.0,
        get: |config| config.compression_ratio_threshold,
        set: |config, value| config.compression_ratio_threshold = value,
    },
    DecodeKnob {
        key: "logprob_threshold",
        label: "Log-prob limit",
        model_default: "-1",
        range: "0 or below, or -inf to turn it off",
        accepts: |value| value <= 0.0,
        get: |config| config.logprob_threshold,
        set: |config, value| config.logprob_threshold = value,
    },
    DecodeKnob {
        key: "no_speech_threshold",
        label: "No-speech limit",
        model_default: "0.6",
        range: "between 0 and 1, or inf to turn it off",
        accepts: |value| (0.0..=1.0).contains(&value) || value == f64::INFINITY,
        get: |config| config.no_speech_threshold,
        set: |config, value| config.no_speech_threshold = value,
    },
];

/// The label both configuration UIs give `TranscriptionConfig::initial_prompt`.
pub(crate) const INITIAL_PROMPT_LABEL: &str = "Vocabulary prompt";

/// The dim text both configuration UIs show in an empty vocabulary prompt.
pub(crate) const INITIAL_PROMPT_PLACEHOLDER: &str =
    "a sentence with your terms, e.g. I dictate in TongueTyped on KWin.";

/// The one-line note both configuration UIs show under the decoding fields.
pub(crate) const DECODING_HINT: &str =
    "Whisper models only. Thresholds take inf (log-prob: -inf) to turn off.";

/// The `TranscriptionConfig::translate` choices as both configuration UIs
/// label them, in order (index 0 = transcribe, index 1 = translate).
pub(crate) const TRANSLATE_LABELS: [&str; 2] = [
    "Transcribe in the spoken language",
    "Translate speech into English",
];

impl TranscriptionConfig {
    /// Why `initial_prompt` cannot be sent to the decoder, if it cannot: an
    /// interior NUL cannot cross the C boundary, and transcribe.cpp rejects a
    /// `<|...|>` special-token tag at run time, which would fail every
    /// dictation instead of failing here.
    fn initial_prompt_error(&self) -> Option<&'static str> {
        let prompt = &self.initial_prompt;
        if prompt.contains('\0') {
            return Some("must not contain a NUL character");
        }
        let has_tag = prompt
            .match_indices("<|")
            .any(|(start, _)| prompt[start + 2..].contains("|>"));
        has_tag.then_some("must not contain a <|...|> tag")
    }

    fn validate_decoding(&self) -> anyhow::Result<()> {
        if let Some(problem) = self.initial_prompt_error() {
            anyhow::bail!("transcription.initial_prompt {problem}");
        }
        for knob in &DECODE_KNOBS {
            if let Some(value) = (knob.get)(self) {
                if !knob.accepts(value) {
                    anyhow::bail!("transcription.{} must be {}", knob.key, knob.range);
                }
            }
        }
        Ok(())
    }

    /// Reads the configuration UIs' decoding form: the vocabulary prompt
    /// followed by one entry per `DECODE_KNOBS` knob, where an empty entry
    /// keeps the model default. Changes nothing and returns a message naming
    /// the field when any entry is invalid.
    pub(crate) fn apply_decoding_form(
        &mut self,
        prompt: &str,
        knobs: &[&str; DECODE_KNOBS.len()],
    ) -> Result<(), String> {
        let mut candidate = self.clone();
        candidate.initial_prompt = prompt.trim().to_string();
        if let Some(problem) = candidate.initial_prompt_error() {
            return Err(format!("{INITIAL_PROMPT_LABEL} {problem}."));
        }
        for (knob, raw) in DECODE_KNOBS.iter().zip(knobs) {
            let raw = raw.trim();
            let value = if raw.is_empty() {
                None
            } else {
                match raw.parse::<f64>() {
                    Ok(value) if knob.accepts(value) => Some(value),
                    _ => {
                        return Err(format!(
                            "{} must be {}, or empty for the model default.",
                            knob.label, knob.range
                        ))
                    }
                }
            };
            (knob.set)(&mut candidate, value);
        }
        *self = candidate;
        Ok(())
    }

    /// The decoding form's starting text for each `DECODE_KNOBS` knob: the
    /// configured value, or empty for the model default.
    pub(crate) fn decoding_form_values(&self) -> [String; DECODE_KNOBS.len()] {
        DECODE_KNOBS.each_ref().map(|knob| {
            (knob.get)(self)
                .map(|value| value.to_string())
                .unwrap_or_default()
        })
    }

    /// A one-line summary of the decoding settings for the configuration
    /// UIs, e.g. `vocabulary prompt, 2 overrides`.
    pub(crate) fn decoding_summary(&self) -> String {
        let overrides = DECODE_KNOBS
            .iter()
            .filter(|knob| (knob.get)(self).is_some())
            .count();
        let mut parts = Vec::new();
        if !self.initial_prompt.trim().is_empty() {
            parts.push("vocabulary prompt".to_string());
        }
        match overrides {
            0 => {}
            1 => parts.push("1 override".to_string()),
            n => parts.push(format!("{n} overrides")),
        }
        if parts.is_empty() {
            "model defaults".to_string()
        } else {
            parts.join(", ")
        }
    }

    /// The dim text for each empty decoding-form field, in form order: the
    /// vocabulary prompt, then each `DECODE_KNOBS` knob's model default.
    pub(crate) fn decoding_form_placeholders() -> Vec<String> {
        std::iter::once(INITIAL_PROMPT_PLACEHOLDER.to_string())
            .chain(
                DECODE_KNOBS
                    .iter()
                    .map(|knob| format!("model default ({})", knob.model_default)),
            )
            .collect()
    }

    /// The `TRANSLATE_LABELS` entry for the current `translate` choice.
    pub(crate) fn translate_label(&self) -> &'static str {
        TRANSLATE_LABELS[usize::from(self.translate)]
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct HistoryConfig {
    #[serde(default = "default_true")]
    pub enabled: bool,
    /// How many transcript rows the history database keeps. `0` means no
    /// entry-count limit. Paired with `max_age_days`: pruning on write drops
    /// rows that either limit cuts, whichever comes first.
    #[serde(default = "default_max_entries")]
    pub max_entries: u64,
    /// How many days a transcript row may stay in the history database. `0`
    /// means no age limit. Both this and `max_entries` are always in effect;
    /// neither depends on the output method.
    #[serde(default = "default_max_age_days")]
    pub max_age_days: u64,
    /// A folder to also write each finished transcript into as a plain-text
    /// file stamped with the date and time (one file per dictation). Empty
    /// (the default) disables the export. The files are write-once:
    /// TongueTyped never reads, monitors, or prunes them again, so they
    /// persist regardless of the database retention above. No audio is ever
    /// written.
    #[serde(default = "default_transcript_folder")]
    pub transcript_folder: String,
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
            max_age_days: default_max_age_days(),
            transcript_folder: default_transcript_folder(),
            save_recordings: false,
            recording_expiry: RecordingExpiryConfig::default(),
        }
    }
}

impl HistoryConfig {
    /// A one-line summary of the retention limits for the configuration UIs,
    /// e.g. `100 entries, 30 days`. A zero limit reads as `unlimited`.
    pub fn retention_summary(&self) -> String {
        let entries = match self.max_entries {
            0 => "unlimited entries".to_string(),
            n => format!("{n} entries"),
        };
        let age = match self.max_age_days {
            0 => "unlimited age".to_string(),
            n => format!("{n} days"),
        };
        format!("{entries}, {age}")
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
    // Visual dictation feedback defaults on: `OverlayHandle::try_send` probes
    // for wlr-layer-shell once and `feedback.rs` falls back to the existing
    // Plasma OSD / notification chain whenever it (or any desktop
    // notification service) is unavailable, so this never blocks recording -
    // see src/overlay.rs's module docs.
    #[serde(default = "default_true")]
    pub enabled: bool,
    #[serde(default = "default_overlay_position")]
    pub position: String,
    // Which output(s) the overlay renders on: `"all"` (the default) shows it
    // on every connected output at once, the legacy `"active"` is folded into
    // the same all-screens behavior, and any other value names a single output
    // to pin the overlay to. See `overlay::target_for`.
    #[serde(default = "default_overlay_monitor")]
    pub monitor: String,
    // Which recording glyph the overlay shows: the plain pulsing dot
    // (`false`) or the live multi-bar microphone waveform (`true`, the
    // default). Every style's recording animation is voice-reactive either
    // way - this only picks the busier glyph, feeding the real microphone
    // level through `audio::publish_level` to the overlay actor. Renamed from
    // `streaming_indicator`, which described the old synthetic stand-in for
    // Handy's streaming panel; the serde alias and `migrate_legacy_overlay_config`
    // keep existing config files loading and rewrite the key on save.
    #[serde(default = "default_true", alias = "streaming_indicator")]
    pub live_waveform: bool,
    // Which overlay look to show - the reviewed list lives in
    // `overlay::STYLE_VALUES`. `src/overlay.rs::style_for` parses this,
    // falling back to `badge` for an unrecognized value exactly like
    // `anchor_for` falls back on `position`.
    #[serde(default = "default_overlay_style")]
    pub style: String,
}

impl Default for OverlayConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            position: default_overlay_position(),
            monitor: default_overlay_monitor(),
            live_waveform: true,
            style: default_overlay_style(),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
#[serde(deny_unknown_fields)]
pub struct StartupConfig {
    #[serde(default = "default_false")]
    pub autostart: bool,
}

/// The `StartupConfig::autostart` choices as both configuration UIs label
/// them, in order (index 0 = start manually, index 1 = autostart).
pub(crate) const STARTUP_LABELS: [&str; 2] =
    ["Start manually", "Start TongueTyped when you sign in"];

fn default_activation_mode() -> ActivationMode {
    ActivationMode::Hold
}

fn default_active_model() -> String {
    crate::catalog::DEFAULT_MODEL_ID.to_string()
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

fn default_preferred_backend() -> String {
    "auto".to_string()
}

fn default_typing_backend() -> String {
    "auto".to_string()
}

fn default_language() -> String {
    "auto".to_string()
}

fn default_max_recording_seconds() -> u64 {
    600
}

fn default_chunk_seconds() -> u64 {
    30
}

fn default_max_entries() -> u64 {
    100
}

fn default_max_age_days() -> u64 {
    30
}

fn default_transcript_folder() -> String {
    String::new()
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
    "all".to_string()
}

fn default_overlay_style() -> String {
    "badge".to_string()
}

/// Rewrites a config file's pre-consolidation `[model]` table - the GPU-only
/// `gpu_model` plus the separate, CPU-only legacy GGML `selected` field - into
/// the single backend-neutral `active_model` this version reads. Mutates
/// `raw` in place and returns whether a migration actually happened, so
/// `Config::load` knows whether to persist the rewritten file. A no-op (and
/// returns `false`) on a config that already has `active_model`, or has
/// neither legacy field (nothing to migrate - `#[serde(default)]` fills it).
///
/// Picks `gpu_model` when present (it already names a real catalog GGUF
/// entry, and preserves exactly what a GPU build was actually running);
/// otherwise falls back to `catalog::DEFAULT_MODEL_ID`, the GGUF equivalent of
/// the legacy `selected` field's one possible value (`whisper-small-q5_1`).
fn migrate_legacy_model_config(raw: &mut toml::Value) -> bool {
    let Some(model) = raw
        .as_table_mut()
        .and_then(|table| table.get_mut("model"))
        .and_then(|model| model.as_table_mut())
    else {
        return false;
    };
    if model.contains_key("active_model") {
        return false;
    }
    let legacy_gpu_model = model.remove("gpu_model");
    let legacy_selected = model.remove("selected");
    if legacy_gpu_model.is_none() && legacy_selected.is_none() {
        return false;
    }
    let active_model = legacy_gpu_model
        .as_ref()
        .and_then(|value| value.as_str())
        .filter(|id| crate::catalog::find(id).is_some())
        .unwrap_or(crate::catalog::DEFAULT_MODEL_ID)
        .to_string();
    model.insert(
        "active_model".to_string(),
        toml::Value::String(active_model),
    );
    true
}

/// Rewrites a config file's legacy `[activation]` table, which used to carry
/// a second `keybind_status` field alongside `keybind`. TongueTyped no longer
/// chooses a default key and no longer stores a separate status: `keybind`
/// now holds only what the desktop reports is bound. Removes `keybind_status`
/// (so `#[serde(deny_unknown_fields)]` keeps loading old files) and drops a
/// `keybind` that was never confirmed by the desktop (the old status
/// `"untested"` meant it was a typed-but-unverified value, never a portal
/// report). A real reported status is the desktop's own descriptor of what is
/// bound, so it replaces whatever typed `keybind` sat beside it. Mutates
/// `raw` in place and returns whether a migration happened.
fn migrate_legacy_activation_config(raw: &mut toml::Value) -> bool {
    let Some(activation) = raw
        .as_table_mut()
        .and_then(|table| table.get_mut("activation"))
        .and_then(|activation| activation.as_table_mut())
    else {
        return false;
    };
    let Some(status) = activation.remove("keybind_status") else {
        return false;
    };
    let reported = status
        .as_str()
        .map(str::trim)
        .filter(|status| !status.is_empty());
    match reported {
        Some("untested") | None => {
            // Never confirmed by the desktop: the value was the old
            // app-chosen/typed guess, not something bound, so forget it.
            activation.remove("keybind");
        }
        Some(reported) => {
            // A real descriptor was recorded: it is what the desktop actually
            // has bound, so it is the source of truth and replaces any old
            // app-chosen/typed `keybind`.
            activation.insert(
                "keybind".to_string(),
                toml::Value::String(crate::activation::keybind_label(reported)),
            );
        }
    }
    true
}

/// Renames the legacy `[overlay] streaming_indicator` key to its current
/// `live_waveform` name. The field kept its meaning when the waveform became
/// truly microphone-reactive, so only the key changes; an explicit value is
/// carried over untouched (a config that never had the key keeps the new
/// default on the next save). Mutates `raw` in place and returns whether a
/// migration happened.
fn migrate_legacy_overlay_config(raw: &mut toml::Value) -> bool {
    let Some(overlay) = raw
        .as_table_mut()
        .and_then(|table| table.get_mut("overlay"))
        .and_then(|overlay| overlay.as_table_mut())
    else {
        return false;
    };
    let Some(value) = overlay.remove("streaming_indicator") else {
        return false;
    };
    overlay.entry("live_waveform".to_string()).or_insert(value);
    true
}

/// Drops the legacy `[output.paste]` table. It held a paste shortcut, a
/// restore-clipboard switch, and two delays that no code ever read; the
/// `paste` output method that replaced it needs only `output.paste_keys`, so
/// none of the old values carry over. Mutates `raw` in place and returns
/// whether a migration happened.
fn migrate_legacy_output_config(raw: &mut toml::Value) -> bool {
    let Some(output) = raw
        .as_table_mut()
        .and_then(|table| table.get_mut("output"))
        .and_then(|output| output.as_table_mut())
    else {
        return false;
    };
    if !output.get("paste").is_some_and(toml::Value::is_table) {
        return false;
    }
    output.remove("paste");
    true
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
        let mut raw: toml::Value = toml::from_str(&content)?;
        let migrated = migrate_legacy_model_config(&mut raw)
            | migrate_legacy_activation_config(&mut raw)
            | migrate_legacy_overlay_config(&mut raw)
            | migrate_legacy_output_config(&mut raw);
        let config: Config = raw.try_into()?;
        config.validate()?;
        if migrated {
            config.save()?;
        }
        Ok(config)
    }

    pub fn reload() -> anyhow::Result<Self> {
        let path = Self::config_path()?;
        if !path.exists() {
            return Ok(Config::default());
        }
        let content = fs::read_to_string(&path)?;
        let mut raw: toml::Value = toml::from_str(&content)?;
        migrate_legacy_model_config(&mut raw);
        migrate_legacy_activation_config(&mut raw);
        migrate_legacy_overlay_config(&mut raw);
        migrate_legacy_output_config(&mut raw);
        let config: Config = raw.try_into()?;
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
        if crate::catalog::find(&self.model.active_model).is_none() {
            anyhow::bail!("unsupported model: {}", self.model.active_model);
        }
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
        self.transcription.validate_decoding()?;
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

        crate::inference::BackendPreference::parse(&self.model.preferred_backend)?;
        if self.output.auto_submit && !self.output.method.inserts_text() {
            anyhow::bail!(
                "auto_submit cannot be true when output.method is '{}'",
                self.output.method
            );
        }
        if !matches!(
            self.output.typing_backend.as_str(),
            "auto" | "wtype" | "enigo" | "dotool"
        ) {
            anyhow::bail!("unsupported typing backend: {}", self.output.typing_backend);
        }
        if !self.audio.feedback_volume.is_finite()
            || !(0.0..=1.0).contains(&self.audio.feedback_volume)
        {
            anyhow::bail!("audio.feedback_volume must be between 0 and 1");
        }
        if self.audio.mute_playback.enabled {
            anyhow::bail!("audio.mute_playback.enabled is not supported in Stage 1");
        }
        if self.history.save_recordings {
            anyhow::bail!("history.save_recordings is not supported in Stage 1");
        }
        if self.history.max_age_days > i64::MAX as u64 / 86_400 {
            anyhow::bail!("history.max_age_days is too large");
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

/// Serializes tests that redirect config resolution by setting the
/// process-wide `XDG_CONFIG_HOME` env var (`Config::load`/`config_path`
/// resolve it via `directories::BaseDirs`), so tests in other modules that
/// need a temp configuration directory can share the same lock instead of
/// racing this one.
#[cfg(test)]
pub(crate) static XDG_CONFIG_HOME_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn defaults_are_valid_and_match_stage_one_contract() {
        let config = Config::default();
        config.validate().unwrap();
        assert!(!config.audio.feedback_sounds);
        assert_eq!(config.transcription.max_recording_seconds, 600);
        assert_eq!(config.transcription.chunk_seconds, 30);
        assert_eq!(config.model.active_model, crate::catalog::DEFAULT_MODEL_ID);
        assert_eq!(config.model.preferred_backend, "auto");
        assert_eq!(config.model.idle_unload.policy, IdleUnloadPolicy::AfterIdle);
        assert_eq!(config.history.max_entries, 100);
        assert_eq!(config.history.max_age_days, 30);
        assert!(config.history.transcript_folder.is_empty());
    }

    #[test]
    fn history_retention_round_trips_and_summarizes() {
        let config: Config = toml::from_str(
            "[history]\nmax_entries = 7\nmax_age_days = 3\ntranscript_folder = \"/tmp/tt\"\n",
        )
        .unwrap();
        config.validate().unwrap();
        assert_eq!(config.history.max_entries, 7);
        assert_eq!(config.history.max_age_days, 3);
        assert_eq!(config.history.transcript_folder, "/tmp/tt");
        assert_eq!(config.history.retention_summary(), "7 entries, 3 days");

        let unlimited: Config =
            toml::from_str("[history]\nmax_entries = 0\nmax_age_days = 0\n").unwrap();
        assert_eq!(
            unlimited.history.retention_summary(),
            "unlimited entries, unlimited age"
        );
    }

    #[test]
    fn chunk_seconds_round_trips_and_can_be_disabled() {
        let config: Config = toml::from_str("[transcription]\nchunk_seconds = 45\n").unwrap();
        config.validate().unwrap();
        assert_eq!(config.transcription.chunk_seconds, 45);
        let saved: Config = toml::from_str(&toml::to_string_pretty(&config).unwrap()).unwrap();
        assert_eq!(saved.transcription.chunk_seconds, 45);

        // 0 is the documented "no chunking" value and stays valid.
        let disabled: Config = toml::from_str("[transcription]\nchunk_seconds = 0\n").unwrap();
        disabled.validate().unwrap();
        assert_eq!(disabled.transcription.chunk_seconds, 0);
    }

    #[test]
    fn decode_options_default_to_off_and_stay_out_of_a_saved_file() {
        let config = Config::default();
        assert!(config.transcription.initial_prompt.is_empty());
        assert!(!config.transcription.translate);
        for knob in &DECODE_KNOBS {
            assert_eq!((knob.get)(&config.transcription), None, "{}", knob.key);
        }
        assert_eq!(config.transcription.decoding_summary(), "model defaults");

        // An unset knob is omitted rather than written as a value, so the
        // model default stays in charge after a save.
        let saved = toml::to_string_pretty(&config).unwrap();
        for knob in &DECODE_KNOBS {
            assert!(!saved.contains(knob.key), "{saved}");
        }
        let reloaded: Config = toml::from_str(&saved).unwrap();
        assert_eq!(reloaded.transcription.temperature, None);
    }

    #[test]
    fn decode_options_round_trip_through_toml() {
        let config: Config = toml::from_str(
            "[transcription]\ninitial_prompt = \"TongueTyped, KWin\"\ntranslate = true\n\
             temperature = 0.2\ntemperature_increment = 0.0\n\
             compression_ratio_threshold = inf\nlogprob_threshold = -inf\n\
             no_speech_threshold = 0.5\n",
        )
        .unwrap();
        config.validate().unwrap();
        let transcription = &config.transcription;
        assert_eq!(transcription.initial_prompt, "TongueTyped, KWin");
        assert!(transcription.translate);
        assert_eq!(transcription.temperature, Some(0.2));
        assert_eq!(transcription.temperature_increment, Some(0.0));
        assert_eq!(
            transcription.compression_ratio_threshold,
            Some(f64::INFINITY)
        );
        assert_eq!(transcription.logprob_threshold, Some(f64::NEG_INFINITY));
        assert_eq!(transcription.no_speech_threshold, Some(0.5));
        assert_eq!(
            transcription.decoding_summary(),
            "vocabulary prompt, 5 overrides"
        );

        let saved: Config = toml::from_str(&toml::to_string_pretty(&config).unwrap()).unwrap();
        assert_eq!(saved.transcription.temperature, Some(0.2));
        assert_eq!(
            saved.transcription.logprob_threshold,
            Some(f64::NEG_INFINITY)
        );
    }

    #[test]
    fn rejects_out_of_range_decode_options() {
        let rejected: [(&str, f64); 9] = [
            ("temperature", -0.1),
            ("temperature", 1.5),
            ("temperature", f64::NAN),
            ("temperature_increment", f64::INFINITY),
            ("compression_ratio_threshold", 0.0),
            ("logprob_threshold", 0.5),
            ("no_speech_threshold", 1.2),
            ("no_speech_threshold", f64::NEG_INFINITY),
            ("compression_ratio_threshold", f64::NAN),
        ];
        for (key, value) in rejected {
            let knob = DECODE_KNOBS.iter().find(|knob| knob.key == key).unwrap();
            let mut config = Config::default();
            (knob.set)(&mut config.transcription, Some(value));
            let error = config.validate().unwrap_err().to_string();
            assert!(error.contains(key), "{key} = {value}: {error}");
        }

        let mut config = Config::default();
        config.transcription.initial_prompt = "say <|en|> please".to_string();
        assert!(config.validate().is_err());
        config.transcription.initial_prompt = "nul\0byte".to_string();
        assert!(config.validate().is_err());
        config.transcription.initial_prompt = "a <| b, c |> d".to_string();
        assert!(config.validate().is_err());
        config.transcription.initial_prompt = "x <| y and a|b > c".to_string();
        config.validate().unwrap();
    }

    #[test]
    fn decoding_form_parses_blank_numbers_as_model_defaults() {
        let mut transcription = TranscriptionConfig {
            temperature: Some(0.3),
            ..Default::default()
        };
        assert_eq!(
            transcription.decoding_form_values(),
            ["0.3", "", "", "", ""].map(String::from)
        );
        transcription
            .apply_decoding_form(" TongueTyped ", &["", "0", "inf", "-2", " 0.4 "])
            .unwrap();
        assert_eq!(transcription.initial_prompt, "TongueTyped");
        assert_eq!(transcription.temperature, None);
        assert_eq!(transcription.temperature_increment, Some(0.0));
        assert_eq!(
            transcription.compression_ratio_threshold,
            Some(f64::INFINITY)
        );
        assert_eq!(transcription.logprob_threshold, Some(-2.0));
        assert_eq!(transcription.no_speech_threshold, Some(0.4));
        assert_eq!(
            transcription.decoding_form_values(),
            ["", "0", "inf", "-2", "0.4"].map(String::from)
        );
    }

    #[test]
    fn decoding_form_rejects_bad_input_without_changing_anything() {
        let mut transcription = TranscriptionConfig::default();
        let error = transcription
            .apply_decoding_form("TongueTyped", &["warm", "", "", "", ""])
            .unwrap_err();
        assert_eq!(
            error,
            "Temperature must be between 0 and 1, or empty for the model default."
        );
        let error = transcription
            .apply_decoding_form("", &["", "", "", "1", ""])
            .unwrap_err();
        assert!(
            error.starts_with("Log-prob limit must be 0 or below"),
            "{error}"
        );
        let error = transcription
            .apply_decoding_form("<|en|>", &["", "", "", "", ""])
            .unwrap_err();
        assert!(error.starts_with(INITIAL_PROMPT_LABEL), "{error}");
        assert!(transcription.initial_prompt.is_empty());
        assert_eq!(transcription.temperature, None);
    }

    #[test]
    fn rejects_an_unrepresentable_history_age() {
        let mut config = Config::default();
        config.history.max_age_days = u64::MAX;
        assert!(config.validate().is_err());
    }

    #[test]
    fn rejects_unsupported_runtime_selections() {
        let mut config = Config::default();
        config.output.typing_backend = "typo".to_string();
        assert!(config.validate().is_err());

        config.output.typing_backend = "auto".to_string();
        config.model.preferred_backend = "cdua".to_string();
        assert!(config.validate().is_err());

        config.model.preferred_backend = "cpu".to_string();
        config.validate().unwrap();
        config.model.active_model = "../custom".to_string();
        assert!(config.validate().is_err());
    }

    #[test]
    fn rejects_unknown_model_selection() {
        let mut config = Config::default();
        config.model.active_model = "not-a-catalog-entry".to_string();
        assert!(config.validate().is_err());

        config.model.active_model = "whisper-tiny-q5_k_m".to_string();
        config.validate().unwrap();
    }

    #[test]
    fn rejects_enabled_later_stage_features() {
        let mut config = Config::default();
        config.audio.mute_playback.enabled = true;
        assert!(config.validate().is_err());

        let mut config = Config::default();
        config.history.save_recordings = true;
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

    #[test]
    fn migrates_legacy_gpu_model_field_into_active_model() {
        let mut raw: toml::Value = toml::from_str(
            "[model]\nselected = \"whisper-small-q5_1\"\ngpu_model = \"whisper-tiny-q5_k_m\"\n",
        )
        .unwrap();
        assert!(migrate_legacy_model_config(&mut raw));
        let config: Config = raw.try_into().unwrap();
        config.validate().unwrap();
        assert_eq!(config.model.active_model, "whisper-tiny-q5_k_m");
    }

    #[test]
    fn migrates_legacy_selected_only_config_to_the_default_catalog_model() {
        // Predates `gpu_model` entirely (a config written before slice 2's
        // catalog existed) - only the legacy GGML `selected` field is present.
        let mut raw: toml::Value =
            toml::from_str("[model]\nselected = \"whisper-small-q5_1\"\n").unwrap();
        assert!(migrate_legacy_model_config(&mut raw));
        let config: Config = raw.try_into().unwrap();
        config.validate().unwrap();
        assert_eq!(config.model.active_model, crate::catalog::DEFAULT_MODEL_ID);
    }

    #[test]
    fn migration_is_a_no_op_once_active_model_is_present() {
        let mut raw: toml::Value =
            toml::from_str("[model]\nactive_model = \"whisper-tiny-q5_k_m\"\n").unwrap();
        assert!(!migrate_legacy_model_config(&mut raw));
        let config: Config = raw.try_into().unwrap();
        assert_eq!(config.model.active_model, "whisper-tiny-q5_k_m");
    }

    #[test]
    fn migration_is_a_no_op_for_a_config_with_no_model_table_at_all() {
        let mut raw: toml::Value = toml::from_str("[activation]\nmode = \"toggle\"\n").unwrap();
        assert!(!migrate_legacy_model_config(&mut raw));
        let config: Config = raw.try_into().unwrap();
        assert_eq!(config.model.active_model, crate::catalog::DEFAULT_MODEL_ID);
    }

    #[test]
    fn activation_migration_drops_a_never_confirmed_keybind() {
        // The old `"untested"` status meant the value was a typed guess the
        // desktop never confirmed, so it must not survive as a binding.
        let mut raw: toml::Value = toml::from_str(
            "[activation]\nmode = \"hold\"\nkeybind = \"Super+O\"\nkeybind_status = \"untested\"\n",
        )
        .unwrap();
        assert!(migrate_legacy_activation_config(&mut raw));
        let config: Config = raw.try_into().unwrap();
        assert_eq!(config.activation.keybind, "");
    }

    #[test]
    fn activation_migration_keeps_a_desktop_reported_binding() {
        let mut raw: toml::Value =
            toml::from_str("[activation]\nkeybind = \"Alt+R\"\nkeybind_status = \"Alt+R\"\n")
                .unwrap();
        assert!(migrate_legacy_activation_config(&mut raw));
        let config: Config = raw.try_into().unwrap();
        assert_eq!(config.activation.keybind, "Alt+R");
    }

    #[test]
    fn activation_migration_prefers_the_desktop_reported_status() {
        // Old configs kept the typed/never-confirmed guess in `keybind` and
        // the desktop's real report in `keybind_status`; the report is what
        // was actually bound, so it must win.
        let mut raw: toml::Value = toml::from_str(
            "[activation]\nkeybind = \"Super+O\"\nkeybind_status = \"Ctrl+Shift+Space\"\n",
        )
        .unwrap();
        assert!(migrate_legacy_activation_config(&mut raw));
        let config: Config = raw.try_into().unwrap();
        assert_eq!(config.activation.keybind, "Ctrl+Shift+Space");
    }

    #[test]
    fn activation_migration_normalizes_the_reported_status() {
        let mut raw: toml::Value =
            toml::from_str("[activation]\nkeybind_status = \"Meta+O\"\n").unwrap();
        assert!(migrate_legacy_activation_config(&mut raw));
        let config: Config = raw.try_into().unwrap();
        assert_eq!(config.activation.keybind, "Super+O");
    }

    #[test]
    fn activation_migration_falls_back_to_the_reported_status() {
        let mut raw: toml::Value =
            toml::from_str("[activation]\nkeybind_status = \"Ctrl+Shift+Space\"\n").unwrap();
        assert!(migrate_legacy_activation_config(&mut raw));
        let config: Config = raw.try_into().unwrap();
        assert_eq!(config.activation.keybind, "Ctrl+Shift+Space");
    }

    #[test]
    fn activation_migration_is_a_no_op_without_keybind_status() {
        let mut raw: toml::Value = toml::from_str("[activation]\nkeybind = \"Super+O\"\n").unwrap();
        assert!(!migrate_legacy_activation_config(&mut raw));
        let config: Config = raw.try_into().unwrap();
        assert_eq!(config.activation.keybind, "Super+O");
    }

    #[test]
    fn load_migrates_and_persists_a_pre_consolidation_config_file_on_disk() {
        let _guard = XDG_CONFIG_HOME_LOCK
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let root = std::env::temp_dir().join(format!(
            "tonguetyped-config-migrate-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        let config_dir = root.join("tonguetyped");
        fs::create_dir_all(&config_dir).unwrap();
        fs::write(
            config_dir.join("config.toml"),
            "[model]\nselected = \"whisper-small-q5_1\"\ngpu_model = \"whisper-tiny-q5_k_m\"\n",
        )
        .unwrap();

        let previous = std::env::var_os("XDG_CONFIG_HOME");
        std::env::set_var("XDG_CONFIG_HOME", &root);

        let config = Config::load().unwrap();
        assert_eq!(config.model.active_model, "whisper-tiny-q5_k_m");

        // The migration is persisted: a second load sees the new format
        // directly, with no legacy fields left to migrate.
        let on_disk = fs::read_to_string(config_dir.join("config.toml")).unwrap();
        assert!(on_disk.contains("active_model = \"whisper-tiny-q5_k_m\""));
        assert!(!on_disk.contains("gpu_model"));
        assert!(!on_disk.contains("selected"));
        let reloaded = Config::load().unwrap();
        assert_eq!(reloaded.model.active_model, "whisper-tiny-q5_k_m");

        match previous {
            Some(value) => std::env::set_var("XDG_CONFIG_HOME", value),
            None => std::env::remove_var("XDG_CONFIG_HOME"),
        }
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn feedback_settings_default_the_overlay_on_and_sounds_off() {
        let mut config = Config::default();
        assert!(config.overlay.enabled);
        assert!(config.overlay.live_waveform);
        assert!(!config.audio.feedback_sounds);

        config.audio.feedback_sounds = true;
        config.audio.feedback_volume = 1.0;
        config.validate().unwrap();

        assert!(toml::from_str::<Config>(
            "[overlay]\nenabled = true\nbackend = \"notification\"\n"
        )
        .is_err());

        config.audio.feedback_volume = f64::NAN;
        assert!(config.validate().is_err());
        config.audio.feedback_volume = 1.01;
        assert!(config.validate().is_err());
    }

    #[test]
    fn media_pause_defaults_off_and_round_trips_through_toml() {
        assert!(!Config::default().audio.pause_media);
        let config: Config = toml::from_str("[audio]\nmicrophone = \"default\"\n").unwrap();
        assert!(!config.audio.pause_media);

        let config: Config = toml::from_str("[audio]\npause_media = true\n").unwrap();
        assert!(config.audio.pause_media);
        config.validate().unwrap();
        let reloaded: Config = toml::from_str(&toml::to_string(&config).unwrap()).unwrap();
        assert!(reloaded.audio.pause_media);
    }

    #[test]
    fn live_waveform_defaults_on_and_round_trips_through_toml() {
        let config: Config = toml::from_str("[overlay]\nenabled = true\n").unwrap();
        assert!(config.overlay.live_waveform);

        let config: Config =
            toml::from_str("[overlay]\nenabled = true\nlive_waveform = false\n").unwrap();
        assert!(!config.overlay.live_waveform);
    }

    #[test]
    fn legacy_streaming_indicator_still_loads_and_migrates_to_live_waveform() {
        // The serde alias keeps an un-migrated file readable even on paths
        // that parse TOML directly.
        let config: Config =
            toml::from_str("[overlay]\nenabled = true\nstreaming_indicator = false\n").unwrap();
        assert!(!config.overlay.live_waveform);

        // The on-load migration renames the key and preserves its value.
        let mut raw: toml::Value =
            toml::from_str("[overlay]\nstreaming_indicator = false\n").unwrap();
        assert!(migrate_legacy_overlay_config(&mut raw));
        assert!(raw["overlay"].get("streaming_indicator").is_none());
        assert_eq!(raw["overlay"]["live_waveform"].as_bool(), Some(false));

        // No legacy key means no migration, and an explicit new key wins over a
        // stale legacy one.
        let mut untouched: toml::Value = toml::from_str("[overlay]\nstyle = \"pill\"\n").unwrap();
        assert!(!migrate_legacy_overlay_config(&mut untouched));
        let mut both: toml::Value =
            toml::from_str("[overlay]\nstreaming_indicator = false\nlive_waveform = true\n")
                .unwrap();
        assert!(migrate_legacy_overlay_config(&mut both));
        assert_eq!(both["overlay"]["live_waveform"].as_bool(), Some(true));
    }

    #[test]
    fn legacy_paste_table_is_dropped_so_old_configs_still_load() {
        // Every config saved before the paste output method carried a dead
        // `[output.paste]` table; with `deny_unknown_fields` it must be
        // migrated away or those configs would stop loading.
        let mut raw: toml::Value = toml::from_str(
            "[output]\nmethod = \"type\"\n\n[output.paste]\nshortcut = \"ctrl+shift+v\"\n\
             restore_clipboard = true\ndelay_before_ms = 50\ndelay_after_ms = 300\n",
        )
        .unwrap();
        assert!(migrate_legacy_output_config(&mut raw));
        let config: Config = raw.try_into().unwrap();
        assert_eq!(config.output.method, OutputMethod::Type);
        assert_eq!(config.output.paste_keys, PasteKeys::CtrlV);

        let mut untouched: toml::Value = toml::from_str("[output]\nmethod = \"paste\"\n").unwrap();
        assert!(!migrate_legacy_output_config(&mut untouched));
    }

    #[test]
    fn output_methods_and_paste_keys_round_trip_through_toml() {
        for (name, method) in [
            ("none", OutputMethod::None),
            ("type", OutputMethod::Type),
            ("clipboard", OutputMethod::Clipboard),
            ("paste", OutputMethod::Paste),
        ] {
            let config: Config =
                toml::from_str(&format!("[output]\nmethod = \"{name}\"\n")).unwrap();
            assert_eq!(config.output.method, method);
            assert_eq!(method.to_string(), name);
        }
        let config: Config =
            toml::from_str("[output]\nmethod = \"paste\"\npaste_keys = \"shift-insert\"\n")
                .unwrap();
        assert_eq!(config.output.paste_keys, PasteKeys::ShiftInsert);
        let saved = toml::to_string_pretty(&config).unwrap();
        assert!(saved.contains("paste_keys = \"shift-insert\""), "{saved}");
    }

    #[test]
    fn auto_submit_is_only_valid_for_methods_that_insert_text() {
        let mut config = Config::default();
        config.output.auto_submit = true;
        for (method, valid) in [
            (OutputMethod::None, false),
            (OutputMethod::Clipboard, false),
            (OutputMethod::Type, true),
            (OutputMethod::Paste, true),
        ] {
            config.output.method = method;
            assert_eq!(config.validate().is_ok(), valid, "{method}");
        }
    }

    #[test]
    fn overlay_monitor_defaults_to_all_and_round_trips_through_toml() {
        let config = Config::default();
        assert_eq!(config.overlay.monitor, "all");

        let config: Config =
            toml::from_str("[overlay]\nenabled = true\nmonitor = \"HDMI-A-1\"\n").unwrap();
        assert_eq!(config.overlay.monitor, "HDMI-A-1");
    }

    #[test]
    fn overlay_style_defaults_to_badge_and_round_trips_through_toml() {
        let config = Config::default();
        assert_eq!(config.overlay.style, "badge");

        let config: Config =
            toml::from_str("[overlay]\nenabled = true\nstyle = \"pill\"\n").unwrap();
        assert_eq!(config.overlay.style, "pill");
    }
}
