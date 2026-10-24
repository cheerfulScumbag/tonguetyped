use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
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
    pub text: TextConfig,
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

/// Where a finished transcript goes. The transcript is also saved to the
/// history database and transcript folder only when
/// [`HistoryConfig::enabled`] is on; the in-memory `last-result` always tracks
/// the current daemon session. `type` and `paste` fall back to leaving the
/// transcript on the clipboard when the typing helper fails, so a transcript
/// is never lost.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, Default)]
#[serde(rename_all = "kebab-case")]
pub enum OutputMethod {
    /// Do not deliver the transcript anywhere.
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

    /// Whether this method needs the session clipboard helper: `clipboard`
    /// copies directly, and `paste` copies before pressing the paste keys.
    /// `type` only copies as a failure fallback, so it is not a startup
    /// requirement.
    pub fn needs_clipboard(self) -> bool {
        matches!(self, OutputMethod::Clipboard | OutputMethod::Paste)
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
    /// chunking, so a model with no input ceiling decodes the whole buffer in
    /// one call; a model that advertises an audio limit is still bounded
    /// regardless - a hard input cap (canary, cohere, qwen3_asr) to its cap, or
    /// a model whose cap is on its output (moonshine) to half the
    /// `max_audio_ms` it advertises - so this setting only ever narrows the
    /// model's own bound.
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
}

/// Text post-processing applied to every finished transcript before it is
/// output or recorded: a replacement table for recurring mistranscriptions,
/// opt-in spoken punctuation and filler-word removal, and an optional external
/// command that cleans the text up (see `crate::text`). All of it is pure Rust
/// and local - the external command is the user's own, run with the transcript
/// on stdin and the cleaned text expected on stdout.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TextConfig {
    /// A map of literal mistranscriptions to their corrections, applied to
    /// the whole transcript, case-insensitively and only on word boundaries.
    /// Written in TOML as a `[text.replacements]` table, e.g.
    /// `"vox type" = "Voxtype"`.
    #[serde(default)]
    pub replacements: BTreeMap<String, String>,
    /// Convert spoken punctuation to symbols (`"comma"` to `","`, `"new line"`
    /// to a newline, brackets, braces, and so on). Off by default; useful when
    /// dictating code.
    #[serde(default = "default_false")]
    pub spoken_punctuation: bool,
    /// Drop filler words (`"um"`, `"uh"`, `"hmm"`, ...) from the transcript.
    /// Off by default, and only ever applied when `transcription.language` is
    /// English, so it can never delete real words in another language.
    #[serde(default = "default_false")]
    pub remove_fillers: bool,
    /// An optional external command run after the pure-Rust passes: the
    /// transcript is written to its stdin and the cleaned text is read back
    /// from its stdout (a local LLM cleanup pipe, a `sed` script, ...). Empty
    /// (the default) disables it. Any failure or `post_process_timeout_seconds`
    /// timeout falls back to the transcript without the command.
    #[serde(default)]
    pub post_process_command: String,
    /// How many seconds `post_process_command` may run before it is killed and
    /// its output discarded.
    #[serde(default = "default_post_process_timeout_seconds")]
    pub post_process_timeout_seconds: u64,
}

impl Default for TextConfig {
    fn default() -> Self {
        Self {
            replacements: BTreeMap::new(),
            spoken_punctuation: false,
            remove_fillers: false,
            post_process_command: String::new(),
            post_process_timeout_seconds: default_post_process_timeout_seconds(),
        }
    }
}

/// The separator both configuration UIs use between replacement entries in
/// their single-line "Replacements" field, and between an entry's two sides.
pub(crate) const REPLACEMENT_ENTRY_SEPARATOR: &str = ";";
pub(crate) const REPLACEMENT_ARROW: &str = "=>";

/// Escapes the escape character, the entry separator, and the arrow's `>` in a
/// replacement key or value so that either can hold them without corrupting the
/// single-line form the configuration UIs round-trip. Escaping the `>` breaks
/// any `=>` the field holds, so it can never be read back as the arrow.
fn escape_replacement_field(text: &str) -> String {
    let mut escaped = String::with_capacity(text.len());
    for character in text.chars() {
        match character {
            '\\' => escaped.push_str("\\\\"),
            ';' => escaped.push_str("\\;"),
            '>' => escaped.push_str("\\>"),
            other => escaped.push(other),
        }
    }
    escaped
}

/// Reverses [`escape_replacement_field`]: `\\` becomes `\`, `\;` becomes `;`,
/// and `\>` becomes `>`. A backslash before any other character is left
/// untouched.
fn unescape_replacement_field(text: &str) -> String {
    let mut unescaped = String::with_capacity(text.len());
    let mut characters = text.chars();
    while let Some(character) = characters.next() {
        if character != '\\' {
            unescaped.push(character);
            continue;
        }
        match characters.next() {
            Some('\\') => unescaped.push('\\'),
            Some(';') => unescaped.push(';'),
            Some('>') => unescaped.push('>'),
            Some(other) => {
                unescaped.push('\\');
                unescaped.push(other);
            }
            None => unescaped.push('\\'),
        }
    }
    unescaped
}

/// Splits the single-line replacement form on unescaped `;` separators,
/// keeping each entry's escape sequences intact for
/// [`unescape_replacement_field`]. The serializer writes one space after each
/// separator, so that space is consumed here; that keeps a separator's spacing
/// out of the next entry's leading whitespace, which is significant.
fn split_replacement_entries(raw: &str) -> Vec<String> {
    let mut entries = Vec::new();
    let mut current = String::new();
    let mut characters = raw.chars();
    while let Some(character) = characters.next() {
        match character {
            '\\' => {
                current.push('\\');
                if let Some(next) = characters.next() {
                    current.push(next);
                }
            }
            ';' => {
                entries.push(std::mem::take(&mut current));
                if matches!(characters.clone().next(), Some(' ')) {
                    characters.next();
                }
            }
            other => current.push(other),
        }
    }
    entries.push(current);
    entries
}

impl TextConfig {
    /// The one-line summary of the text-processing settings for the
    /// configuration UIs, e.g. `2 replacements, spoken punctuation`, or `off`.
    pub(crate) fn processing_summary(&self) -> String {
        let mut parts = Vec::new();
        let replacements = self.replacements.len();
        if replacements > 0 {
            parts.push(match replacements {
                1 => "1 replacement".to_string(),
                n => format!("{n} replacements"),
            });
        }
        if self.spoken_punctuation {
            parts.push("spoken punctuation".to_string());
        }
        if self.remove_fillers {
            parts.push("filler removal".to_string());
        }
        if !self.post_process_command.trim().is_empty() {
            parts.push("cleanup command".to_string());
        }
        if parts.is_empty() {
            "off".to_string()
        } else {
            parts.join(", ")
        }
    }

    /// The replacement table as the configuration UIs' single-line field:
    /// each entry `from => to`, separated by `;`. A `\`, `;`, or `>` inside a
    /// key or value is escaped with a backslash. A key that is empty or only
    /// whitespace is ignored at match time, so it is omitted here too: the
    /// form's parser rejects an empty left side, and the form must never emit
    /// an entry it would then refuse.
    pub(crate) fn replacements_form_value(&self) -> String {
        self.replacements
            .iter()
            .filter(|(from, _)| !from.trim().is_empty())
            .map(|(from, to)| {
                format!(
                    "{} {REPLACEMENT_ARROW} {}",
                    escape_replacement_field(from),
                    escape_replacement_field(to)
                )
            })
            .collect::<Vec<_>>()
            .join(&format!("{REPLACEMENT_ENTRY_SEPARATOR} "))
    }

    /// Parses the configuration UIs' single-line replacement field. Each entry
    /// is `from => to`, separated by `;`, with `\`, the separator, and the arrow's
    /// `>` escaped by a backslash. The one space the serializer writes on each
    /// side of the arrow is the delimiter and is dropped; any other leading or
    /// trailing whitespace in a key or value is significant and preserved, so
    /// a value of `. `, ` `, or `\n` survives a save. An empty field clears the
    /// table. Returns a message naming the bad entry without changing anything.
    pub(crate) fn apply_replacements_form(&mut self, raw: &str) -> Result<(), String> {
        let mut replacements = BTreeMap::new();
        for entry in split_replacement_entries(raw) {
            if entry.trim().is_empty() {
                continue;
            }
            let Some((from, to)) = entry.split_once(REPLACEMENT_ARROW) else {
                return Err(format!(
                    "Each replacement must be 'from {REPLACEMENT_ARROW} to', separated by \
                     '{REPLACEMENT_ENTRY_SEPARATOR}' (got {entry:?})."
                ));
            };
            let from = unescape_replacement_field(from.strip_suffix(' ').unwrap_or(from));
            if from.trim().is_empty() {
                return Err(format!(
                    "A replacement's left side is empty (in {entry:?})."
                ));
            }
            let to = unescape_replacement_field(to.strip_prefix(' ').unwrap_or(to));
            replacements.insert(from, to);
        }
        self.replacements = replacements;
        Ok(())
    }

    /// Rejects a timeout that could never be waited on. The replacement table
    /// is left alone: a hand-written empty key is simply ignored at match time.
    pub(crate) fn validate(&self) -> anyhow::Result<()> {
        if self.post_process_timeout_seconds == 0 {
            anyhow::bail!("text.post_process_timeout_seconds must be a positive integer");
        }
        if std::time::Instant::now()
            .checked_add(std::time::Duration::from_secs(
                self.post_process_timeout_seconds,
            ))
            .is_none()
        {
            anyhow::bail!("text.post_process_timeout_seconds is too large");
        }
        Ok(())
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct HistoryConfig {
    /// Whether finished transcripts are kept at all: the history database and
    /// the transcript-folder export are both gated on this. Off by default, so
    /// a fresh install only types the text and writes nothing to disk. An
    /// existing configuration that says `enabled = true` keeps saving until
    /// the user turns it off.
    #[serde(default = "default_false")]
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
    /// A folder to write each finished transcript into as a plain-text file
    /// stamped with the date and time (one file per dictation). Empty (the
    /// default) disables the export, and so does `enabled = false`: a folder
    /// set on its own writes nothing. The files are write-once: TongueTyped
    /// never reads, monitors, or prunes them again, so they persist
    /// regardless of the database retention above. No audio is ever written.
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
            enabled: false,
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

    /// How the transcript-folder export reads in a configuration summary: the
    /// folder path when the export is active, or a gated label when no file
    /// would be written - either because the folder is empty or because
    /// keeping transcripts is off.
    pub fn folder_summary(&self) -> String {
        let folder = self.transcript_folder.trim();
        if !self.enabled {
            "disabled (history off)".to_string()
        } else if folder.is_empty() {
            "disabled".to_string()
        } else {
            folder.to_string()
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
    // Which overlay shape to draw - the reviewed list lives in
    // `overlay::STYLE_VALUES`. The style is authoritative for placement and for
    // which looks and waveform shapes it offers (see `overlay::look_options` /
    // `overlay::offers_shape`). `src/overlay.rs::style_for` parses this, falling
    // back to `badge` for an unrecognized value exactly like `anchor_for` falls
    // back on `position`.
    #[serde(default = "default_overlay_style")]
    pub style: String,
    // Which look the chosen style shows: its resting shape (`"plain"`), its
    // voice-reactive waveform (`"waveform"`, the default), or live streaming
    // text (`"live-text"`). Which values a style offers depends on the style -
    // the round styles take plain or waveform, the wide styles take waveform or
    // live-text, the box takes live-text only, and the blob has no options.
    // Both the overlay (`overlay::resolved_look`) and `Config`'s load/reload
    // migration clamp an unsupported value to the style's first offered look,
    // so a stored value never conflicts with the style. This replaces the old
    // `live_waveform` boolean, which `migrate_legacy_overlay_config` folds in
    // (true -> `waveform`, false -> `plain`, but a caption style keeps
    // `live-text`).
    #[serde(default = "default_overlay_look")]
    pub look: String,
    // Which waveform glyph a wide style draws for the `"waveform"` look:
    // uniform bars (`"bars"`, the default) or the long pill's continuous
    // squiggly line (`"squiggle"`). Only the wide styles offer the choice (see
    // `overlay::offers_shape`); the round styles and the live-text look ignore it.
    #[serde(default = "default_overlay_waveform_shape")]
    pub waveform_shape: String,
}

impl Default for OverlayConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            position: default_overlay_position(),
            monitor: default_overlay_monitor(),
            style: default_overlay_style(),
            look: default_overlay_look(),
            waveform_shape: default_overlay_waveform_shape(),
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

fn default_post_process_timeout_seconds() -> u64 {
    10
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

fn default_overlay_look() -> String {
    "waveform".to_string()
}

fn default_overlay_waveform_shape() -> String {
    "bars".to_string()
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

/// Rewrites a config file's legacy `[overlay]` table. Folds the retired
/// `[overlay] live_waveform` boolean and its older `streaming_indicator` name
/// into the current `look` string, keeping the caption styles on live text so
/// they do not turn into a waveform; folds the retired `live-text-minimal`
/// style onto the surviving glass `live-text` caption with the live-text look;
/// clamps a stored look to one the configured style offers, so the settings
/// UIs and the preview never show a look the style cannot paint; and supplies
/// the style's own waveform shape when the config names none, so the long pill
/// keeps the squiggle it always drew. An explicit `look` that the style offers
/// and an explicit shape are never overwritten. Mutates `raw` in place and
/// returns whether a migration happened.
fn migrate_legacy_overlay_config(raw: &mut toml::Value) -> bool {
    let Some(overlay) = raw
        .as_table_mut()
        .and_then(|table| table.get_mut("overlay"))
        .and_then(|overlay| overlay.as_table_mut())
    else {
        return false;
    };
    let mut migrated = false;

    // The `live-text-minimal` caption was retired; move it onto the surviving
    // glass caption so the config keeps a caption instead of falling through to
    // the badge, and keep it showing live text.
    if overlay.get("style").and_then(toml::Value::as_str) == Some("live-text-minimal") {
        overlay.insert(
            "style".to_string(),
            toml::Value::String("live-text".to_string()),
        );
        overlay.insert(
            "look".to_string(),
            toml::Value::String("live-text".to_string()),
        );
        migrated = true;
    }

    let live_waveform = overlay.remove("live_waveform");
    let streaming_indicator = overlay.remove("streaming_indicator");
    if let Some(value) = live_waveform.or(streaming_indicator) {
        migrated = true;
        let style = overlay
            .get("style")
            .and_then(toml::Value::as_str)
            .unwrap_or("badge");
        // The legacy boolean only distinguished waveform from plain; a caption
        // style keeps live text (see `overlay::legacy_waveform_look`).
        let look = crate::overlay::legacy_waveform_look(style, value.as_bool() != Some(false));
        overlay
            .entry("look".to_string())
            .or_insert_with(|| toml::Value::String(look.to_string()));
    }

    let style = overlay
        .get("style")
        .and_then(toml::Value::as_str)
        .unwrap_or("badge")
        .to_string();
    // A caption style with no explicit look keeps showing live text rather
    // than falling through to the generic waveform default (the pre-rework
    // README's documented `style = "live-text"` setting carried no toggle).
    if overlay.get("look").is_none()
        && crate::overlay::is_live_text_style(crate::overlay::style_for(&style))
    {
        overlay.insert(
            "look".to_string(),
            toml::Value::String("live-text".to_string()),
        );
        migrated = true;
    }

    let look = overlay
        .get("look")
        .and_then(toml::Value::as_str)
        .unwrap_or("waveform");
    let clamped = crate::overlay::clamp_look_value(&style, look);
    if clamped != look {
        overlay.insert("look".to_string(), toml::Value::String(clamped));
        migrated = true;
    }

    // A style that never named a shape keeps the one it always drew: the long
    // pill's squiggle, rather than falling through to the generic bars default.
    if overlay.get("waveform_shape").is_none() {
        let default = crate::overlay::default_shape(&style);
        if default != "bars" {
            overlay.insert(
                "waveform_shape".to_string(),
                toml::Value::String(default.to_string()),
            );
            migrated = true;
        }
    }

    migrated
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
        // A family that rejects over-length input (canary, cohere, qwen3_asr)
        // is bounded to its cap by the engine on every call, including the file
        // path with
        // chunking off; this check additionally keeps the configured live
        // recording limit within the cap. With chunking on that limit is
        // `chunk_seconds`, otherwise `max_recording_seconds`. Every catalog
        // entry has `VariantInfo` (see `catalog::VARIANTS`), so this reads the
        // model's own limit rather than assuming whisper.
        let entry = crate::catalog::find(&self.model.active_model)
            .expect("the catalog check above already accepted this id");
        if let crate::catalog::LongForm::HardCap { cap_seconds } = entry.info().long_form {
            let per_call_seconds = if self.transcription.chunk_seconds > 0 {
                self.transcription
                    .max_recording_seconds
                    .min(self.transcription.chunk_seconds)
            } else {
                self.transcription.max_recording_seconds
            };
            if per_call_seconds > u64::from(cap_seconds) {
                anyhow::bail!(
                    "the active model {} accepts at most {cap_seconds}s of audio per call, but the \
                     per-call buffer is {per_call_seconds}s; select a model that accepts longer \
                     recordings, lower transcription.max_recording_seconds, or lower \
                     transcription.chunk_seconds",
                    self.model.active_model,
                );
            }
        }
        self.transcription.validate_decoding()?;
        self.text.validate()?;
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
            "auto" | "wtype" | "enigo" | "dotool" | "libei"
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
        assert!(
            !config.history.enabled,
            "saving transcripts must be opt-in, and a fresh install is off"
        );
        assert!(config.history.transcript_folder.is_empty());
    }

    #[test]
    fn history_defaults_off_but_an_existing_enabled_config_stays_on() {
        let parsed: Config = toml::from_str("[history]\nenabled = true\n").unwrap();
        assert!(
            parsed.history.enabled,
            "an explicit enabled = true must never be silently turned off"
        );

        let missing: Config = toml::from_str("[history]\nmax_entries = 7\n").unwrap();
        assert!(
            !missing.history.enabled,
            "a config without the flag gets the new off default"
        );
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
    fn history_folder_summary_gates_on_keeping_transcripts() {
        let mut config = Config::default();
        config.history.enabled = true;
        config.history.transcript_folder = "/tmp/tt-out".to_string();
        assert_eq!(config.history.folder_summary(), "/tmp/tt-out");

        config.history.enabled = false;
        assert_eq!(
            config.history.folder_summary(),
            "disabled (history off)",
            "a folder set while keeping transcripts is off must not read as active"
        );

        config.history.enabled = true;
        config.history.transcript_folder = String::new();
        assert_eq!(config.history.folder_summary(), "disabled");
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
    fn text_processing_defaults_are_off_and_valid_and_round_trip() {
        let config = Config::default();
        config.validate().unwrap();
        assert!(config.text.replacements.is_empty());
        assert!(!config.text.spoken_punctuation);
        assert!(!config.text.remove_fillers);
        assert!(config.text.post_process_command.is_empty());
        assert_eq!(config.text.post_process_timeout_seconds, 10);
        assert_eq!(config.text.processing_summary(), "off");
        // A default config must survive its own save: an empty replacement
        // table must serialize back to something that loads unchanged.
        let saved = toml::to_string_pretty(&config).unwrap();
        let reloaded: Config = toml::from_str(&saved).unwrap();
        assert!(reloaded.text.replacements.is_empty());
        assert_eq!(reloaded.text.post_process_timeout_seconds, 10);

        let loaded: Config = toml::from_str(
            "[text]\nspoken_punctuation = true\nremove_fillers = true\n\
             post_process_command = \"llm-clean\"\npost_process_timeout_seconds = 3\n\
             [text.replacements]\n\"vox type\" = \"Voxtype\"\n",
        )
        .unwrap();
        loaded.validate().unwrap();
        assert!(loaded.text.spoken_punctuation);
        assert!(loaded.text.remove_fillers);
        assert_eq!(loaded.text.post_process_command, "llm-clean");
        assert_eq!(loaded.text.post_process_timeout_seconds, 3);
        assert_eq!(loaded.text.replacements.get("vox type").unwrap(), "Voxtype");
        assert_eq!(
            loaded.text.processing_summary(),
            "1 replacement, spoken punctuation, filler removal, cleanup command"
        );
    }

    #[test]
    fn text_replacements_form_round_trips_and_rejects_bad_entries() {
        let mut text = TextConfig::default();
        text.apply_replacements_form("vox type => Voxtype; tee vee => TV")
            .unwrap();
        assert_eq!(text.replacements.get("vox type").unwrap(), "Voxtype");
        assert_eq!(text.replacements.get("tee vee").unwrap(), "TV");
        assert_eq!(
            text.replacements_form_value(),
            "tee vee => TV; vox type => Voxtype"
        );

        let error = text.apply_replacements_form("no arrow here").unwrap_err();
        assert!(error.contains("from => to"), "{error}");
        assert_eq!(
            text.replacements.len(),
            2,
            "a bad form must not change the table"
        );
        text.apply_replacements_form("").unwrap();
        assert!(text.replacements.is_empty());
    }

    #[test]
    fn text_replacements_form_round_trips_separators_and_backslashes() {
        let mut text = TextConfig::default();
        text.replacements
            .insert("a".to_string(), "x; b => y".to_string());
        text.replacements
            .insert("c\\d".to_string(), "\\;".to_string());

        let form = text.replacements_form_value();
        let mut reparsed = TextConfig::default();
        reparsed.apply_replacements_form(&form).unwrap();
        assert_eq!(reparsed.replacements.get("a").unwrap(), "x; b => y");
        assert_eq!(reparsed.replacements.get("c\\d").unwrap(), "\\;");
        assert_eq!(reparsed.replacements, text.replacements);
    }

    #[test]
    fn text_replacements_form_round_trips_arrows_and_whitespace_values() {
        let mut text = TextConfig::default();
        text.replacements
            .insert("a => b".to_string(), "c".to_string());
        text.replacements
            .insert("scratch that".to_string(), "\n".to_string());
        text.replacements
            .insert("pause".to_string(), " ".to_string());

        let form = text.replacements_form_value();
        let mut reparsed = TextConfig::default();
        reparsed.apply_replacements_form(&form).unwrap();
        assert_eq!(reparsed.replacements.get("a => b").unwrap(), "c");
        assert_eq!(reparsed.replacements.get("scratch that").unwrap(), "\n");
        assert_eq!(reparsed.replacements.get("pause").unwrap(), " ");
        assert_eq!(reparsed.replacements, text.replacements);
    }

    #[test]
    fn text_replacements_form_preserves_edge_whitespace_and_skips_blank_keys() {
        let mut text = TextConfig::default();
        text.replacements
            .insert("period".to_string(), ". ".to_string());
        text.replacements
            .insert(" lead".to_string(), " x ".to_string());
        text.replacements
            .insert("trail ".to_string(), "y\t".to_string());
        text.replacements
            .insert(" ".to_string(), "blank-key".to_string());
        text.replacements
            .insert(String::new(), "empty-key".to_string());

        let form = text.replacements_form_value();
        let mut reparsed = TextConfig::default();
        reparsed
            .apply_replacements_form(&form)
            .expect("the form must not emit an entry it then rejects");
        assert_eq!(reparsed.replacements.get("period").unwrap(), ". ");
        assert_eq!(reparsed.replacements.get(" lead").unwrap(), " x ");
        assert_eq!(reparsed.replacements.get("trail ").unwrap(), "y\t");
        assert!(!reparsed.replacements.contains_key(" "));
        assert!(!reparsed.replacements.contains_key(""));
    }

    #[test]
    fn rejects_a_zero_post_process_timeout() {
        let mut config = Config::default();
        config.text.post_process_timeout_seconds = 0;
        assert!(config.validate().is_err());
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
    fn accepts_the_native_libei_typing_backend() {
        let mut config = Config::default();
        config.output.typing_backend = "libei".to_string();
        config.validate().unwrap();
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
    fn hard_capped_models_activate_under_the_default_config() {
        // `select_model` saves the config, which validates it, so a user with
        // the default config (600 s recording limit, 30 s chunk window) must be
        // able to activate a hard-capped model: chunking keeps every call at
        // 30 s, well within the family cap.
        for id in [
            "canary-1b-v2-f16",
            "cohere-transcribe-03-2026-f16",
            "qwen3-asr-1.7b-q4_k_m",
        ] {
            let mut config = Config::default();
            config.model.active_model = id.to_string();
            assert_eq!(config.transcription.max_recording_seconds, 600);
            assert_eq!(config.transcription.chunk_seconds, 30);
            config.validate().unwrap();
        }
    }

    #[test]
    fn rejects_a_recording_limit_above_a_hard_cap_models_input_window() {
        let mut config = Config::default();
        config.model.active_model = "canary-1b-v2-f16".to_string();

        // Chunking is on by default, so every call is bounded by chunk_seconds
        // and a recording limit above the cap is still fine.
        config.transcription.max_recording_seconds = 500;
        config.validate().unwrap();

        // With chunking off the whole recording reaches the model, so the same
        // limit is rejected.
        config.transcription.chunk_seconds = 0;
        let error = config.validate().unwrap_err().to_string();
        assert!(error.contains("accepts at most 400s"), "{error}");
        assert!(error.contains("canary-1b-v2-f16"), "{error}");

        // A chunk window larger than the cap is rejected even with chunking on.
        config.transcription.chunk_seconds = 600;
        assert!(config.validate().is_err());

        // At the cap it validates, and a no-practical-limit family is unaffected.
        config.transcription.chunk_seconds = 400;
        config.validate().unwrap();

        config.model.active_model = "parakeet-tdt-0.6b-v2-f16".to_string();
        config.transcription.max_recording_seconds = 500;
        config.transcription.chunk_seconds = 0;
        config.validate().unwrap();
    }

    #[test]
    fn qwen3_asr_uses_its_derived_decoder_context_cap() {
        // 5,218 s is the engine's max_audio_ms from the 65,536-token decoder
        // context (minus the prompt overhead and generation reserve) at 80 ms
        // per audio token (10 ms per mel frame), not a rounded guess.
        let mut config = Config::default();
        config.model.active_model = "qwen3-asr-1.7b-q4_k_m".to_string();
        config.transcription.chunk_seconds = 0;
        config.transcription.max_recording_seconds = 5218;
        config.validate().unwrap();

        config.transcription.max_recording_seconds = 5219;
        let error = config.validate().unwrap_err().to_string();
        assert!(error.contains("accepts at most 5218s"), "{error}");
        assert!(error.contains("qwen3-asr-1.7b-q4_k_m"), "{error}");

        // Chunking bounds each call, so the recording limit may exceed the cap.
        config.transcription.chunk_seconds = 30;
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
        assert_eq!(config.overlay.look, "waveform");
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
    fn overlay_look_and_waveform_shape_default_and_round_trip_through_toml() {
        let config: Config = toml::from_str("[overlay]\nenabled = true\n").unwrap();
        assert_eq!(config.overlay.look, "waveform");
        assert_eq!(config.overlay.waveform_shape, "bars");

        let config: Config = toml::from_str(
            "[overlay]\nenabled = true\nlook = \"plain\"\nwaveform_shape = \"squiggle\"\n",
        )
        .unwrap();
        assert_eq!(config.overlay.look, "plain");
        assert_eq!(config.overlay.waveform_shape, "squiggle");
    }

    #[test]
    fn legacy_waveform_toggles_migrate_to_look() {
        // Both legacy booleans fold into `look`: false named the resting shape
        // and true the waveform.
        let mut raw: toml::Value =
            toml::from_str("[overlay]\nstreaming_indicator = false\n").unwrap();
        assert!(migrate_legacy_overlay_config(&mut raw));
        assert!(raw["overlay"].get("streaming_indicator").is_none());
        assert_eq!(raw["overlay"]["look"].as_str(), Some("plain"));

        let mut raw: toml::Value = toml::from_str("[overlay]\nlive_waveform = true\n").unwrap();
        assert!(migrate_legacy_overlay_config(&mut raw));
        assert!(raw["overlay"].get("live_waveform").is_none());
        assert_eq!(raw["overlay"]["look"].as_str(), Some("waveform"));

        // No legacy key means no migration.
        let mut untouched: toml::Value = toml::from_str("[overlay]\nstyle = \"pill\"\n").unwrap();
        assert!(!migrate_legacy_overlay_config(&mut untouched));

        // `live_waveform` wins over the older `streaming_indicator` name, and
        // an explicit `look` the style offers is never overwritten.
        let mut both: toml::Value =
            toml::from_str("[overlay]\nstreaming_indicator = false\nlive_waveform = true\n")
                .unwrap();
        assert!(migrate_legacy_overlay_config(&mut both));
        assert_eq!(both["overlay"]["look"].as_str(), Some("waveform"));

        let mut explicit: toml::Value =
            toml::from_str("[overlay]\nstyle = \"pill\"\nlive_waveform = true\nlook = \"plain\"\n")
                .unwrap();
        assert!(migrate_legacy_overlay_config(&mut explicit));
        assert_eq!(explicit["overlay"]["look"].as_str(), Some("plain"));
    }

    #[test]
    fn legacy_caption_configs_migrate_to_the_live_text_look() {
        // A live-text caption keeps showing live text, whether its old waveform
        // toggle is folded in or the config only named the style (the pre-rework
        // README's documented setting) and so had no look key.
        for content in [
            "[overlay]\nstyle = \"live-text\"\nlive_waveform = true\n",
            "[overlay]\nstyle = \"live-text-box\"\nlive_waveform = true\n",
            "[overlay]\nstyle = \"live-text\"\n",
            "[overlay]\nstyle = \"live-text-box\"\n",
        ] {
            let mut raw: toml::Value = toml::from_str(content).unwrap();
            assert!(migrate_legacy_overlay_config(&mut raw), "{content}");
            let config: Config = raw.try_into().unwrap();
            assert!(config.overlay.style.starts_with("live-text"), "{content}");
            assert_eq!(config.overlay.look, "live-text", "{content}");
        }
    }

    #[test]
    fn retired_live_text_minimal_migrates_to_the_live_text_caption() {
        // The dropped minimal caption folds onto the surviving glass caption,
        // staying on live text rather than falling through to the badge, with
        // or without the old waveform boolean still in the file, and even if a
        // look was stored beside the retired style.
        for content in [
            "[overlay]\nstyle = \"live-text-minimal\"\nlive_waveform = true\n",
            "[overlay]\nstyle = \"live-text-minimal\"\n",
            "[overlay]\nstyle = \"live-text-minimal\"\nlook = \"waveform\"\n",
        ] {
            let mut raw: toml::Value = toml::from_str(content).unwrap();
            assert!(migrate_legacy_overlay_config(&mut raw), "{content}");
            let config: Config = raw.try_into().unwrap();
            assert_eq!(config.overlay.style, "live-text");
            assert_eq!(config.overlay.look, "live-text", "{content}");
        }
    }

    #[test]
    fn stored_look_is_normalized_to_the_configured_style() {
        // Any pair the style does not offer is clamped to one it does, so the
        // settings UIs never report a look the style cannot paint.
        let mut raw: toml::Value =
            toml::from_str("[overlay]\nstyle = \"live-text-box\"\nlook = \"waveform\"\n").unwrap();
        assert!(migrate_legacy_overlay_config(&mut raw));
        let config: Config = raw.try_into().unwrap();
        assert_eq!(config.overlay.look, "live-text");

        let mut raw: toml::Value =
            toml::from_str("[overlay]\nstyle = \"pill\"\nlook = \"live-text\"\n").unwrap();
        assert!(migrate_legacy_overlay_config(&mut raw));
        let config: Config = raw.try_into().unwrap();
        assert_eq!(config.overlay.look, "plain");
    }

    #[test]
    fn long_pill_keeps_its_squiggle_when_the_config_named_no_shape() {
        // A long-pill config that never named a shape keeps the continuous
        // squiggle it always drew instead of falling through to `bars`.
        let mut raw: toml::Value = toml::from_str("[overlay]\nstyle = \"long-pill\"\n").unwrap();
        assert!(migrate_legacy_overlay_config(&mut raw));
        let config: Config = raw.try_into().unwrap();
        assert_eq!(config.overlay.waveform_shape, "squiggle");

        // An explicit shape is never overwritten, and the other wide styles
        // keep the uniform bars default.
        let mut explicit: toml::Value =
            toml::from_str("[overlay]\nstyle = \"long-pill\"\nwaveform_shape = \"bars\"\n")
                .unwrap();
        assert!(!migrate_legacy_overlay_config(&mut explicit));
        let config: Config = explicit.try_into().unwrap();
        assert_eq!(config.overlay.waveform_shape, "bars");

        let mut pill: toml::Value = toml::from_str("[overlay]\nstyle = \"pill\"\n").unwrap();
        assert!(!migrate_legacy_overlay_config(&mut pill));
        let config: Config = pill.try_into().unwrap();
        assert_eq!(config.overlay.waveform_shape, "bars");
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
    fn only_copying_methods_need_the_clipboard_helper() {
        // `type` copies only as a failure fallback, so a missing clipboard
        // helper is not a startup warning there; `clipboard` and `paste`
        // cannot work without it.
        for (method, inserts, needs_clipboard) in [
            (OutputMethod::None, false, false),
            (OutputMethod::Type, true, false),
            (OutputMethod::Clipboard, false, true),
            (OutputMethod::Paste, true, true),
        ] {
            assert_eq!(method.inserts_text(), inserts, "{method}");
            assert_eq!(method.needs_clipboard(), needs_clipboard, "{method}");
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
