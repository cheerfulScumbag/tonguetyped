//! The interactive setup console: a four-tab Ratatui utility (Audio &
//! Capture, Model & Engine, Output & Delivery, Interface & System) that edits
//! the in-memory `Config` as each control changes and writes it to disk on
//! `Ctrl+S` or on leaving from the root (`q`, or `Esc` with no sub-menu open).
//!
//! The state machine (`App`) is pure: key handling only edits state and
//! queues `Effect`s for anything that touches the outside world (daemon IPC,
//! the desktop overlay, the shortcut portal, model downloads). `run` executes
//! those effects on background threads and feeds their `TaskResult`s back, so
//! the render loop never blocks and tests can drive every key path headlessly.

use super::{Capabilities, ProvisionHandle, ReconfigureHandle, SetupOutcome};
use crate::activation::ReconfigureOutcome;
use crate::audio;
use crate::config::{ActivationMode, Config, OutputMethod, OverlayConfig, PasteKeys};
use crate::feedback::FeedbackEvent;
use crate::ipc::{Request, Response};
use crate::overlay::{
    clamp_look_value, default_shape, has_look_choice, shows_shape_choice, style_look_options,
    OverlayHandle, POSITION_VALUES, SHAPE_VALUES, STYLE_VALUES,
};
use crate::text_input::TextField;
use crossterm::event::{
    self, DisableBracketedPaste, EnableBracketedPaste, Event, KeyCode, KeyEvent, KeyEventKind,
    KeyModifiers,
};
use crossterm::execute;
use crossterm::terminal::{
    disable_raw_mode, enable_raw_mode, EnterAlternateScreen, LeaveAlternateScreen,
};
use ratatui::backend::CrosstermBackend;
use ratatui::Terminal;
use std::cell::Cell;
use std::io;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

mod model_view;
mod render;
#[cfg(test)]
mod tests;
mod theme;

use model_view::{ModelFilter, ModelSort};
use theme::Theme;

/// How long the desktop overlay test keeps the real overlay on screen.
const OVERLAY_TEST_DURATION: Duration = Duration::from_secs(4);
/// After a dictation finishes, the daemon may still be typing the transcript
/// into this terminal. Keys keep landing in the capture box for this long
/// after the daemon reports idle, so typed text never fires setup commands.
const CAPTURE_GRACE: Duration = Duration::from_millis(1500);
/// How often the background poller asks the daemon for its status.
const STATUS_INTERVAL: Duration = Duration::from_millis(500);
/// How long an informational message stays in the message row.
const INFO_MESSAGE_TTL: Duration = Duration::from_secs(5);

/// The output methods in the order the Delivery selector lists them.
const DELIVERY_METHODS: [OutputMethod; 4] = [
    OutputMethod::Type,
    OutputMethod::Paste,
    OutputMethod::Clipboard,
    OutputMethod::None,
];

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum Tab {
    Audio,
    Model,
    Output,
    System,
}

impl Tab {
    const ALL: [Tab; 4] = [Tab::Audio, Tab::Model, Tab::Output, Tab::System];

    fn index(self) -> usize {
        Self::ALL.iter().position(|tab| *tab == self).unwrap_or(0)
    }

    fn offset(self, delta: isize) -> Tab {
        let len = Self::ALL.len() as isize;
        Self::ALL[(self.index() as isize + delta).rem_euclid(len) as usize]
    }

    fn title(self) -> &'static str {
        match self {
            Tab::Audio => "Audio & Capture",
            Tab::Model => "Model & Engine",
            Tab::Output => "Output & Delivery",
            Tab::System => "Interface & System",
        }
    }

    fn short_title(self) -> &'static str {
        match self {
            Tab::Audio => "Audio",
            Tab::Model => "Model",
            Tab::Output => "Output",
            Tab::System => "System",
        }
    }

    /// The control focus lands on the first time the tab opens.
    fn first_control(self) -> Control {
        match self {
            Tab::Audio => Control::Microphone(0),
            Tab::Model => Control::ModelTable,
            Tab::Output => Control::OutputMethod,
            Tab::System => Control::OverlayEnabled,
        }
    }
}

/// Every focusable control. Which ones a tab shows depends on the current
/// configuration (`App::layout`), so focus is stored as a `Control` rather
/// than a position and falls back to the tab's first control when its control
/// disappears.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum Control {
    Microphone(usize),
    Dictation,
    ActivationMode,
    Shortcut,
    PauseMedia,
    FeedbackSounds,
    ModelTable,
    Backend,
    Translate,
    Decoding,
    OutputMethod,
    TypingBackend,
    PasteKeys,
    AutoSubmit,
    SpokenPunctuation,
    RemoveFillers,
    HistoryEnabled,
    Field(Field),
    OverlayEnabled,
    OverlayPosition,
    OverlayStyle,
    OverlayLook,
    OverlayShape,
    OverlayTest,
    DaemonStart,
    DaemonStop,
    DaemonRestart,
    Autostart,
}

/// The editable text fields. `Decode(0)` is the vocabulary prompt and
/// `Decode(1..)` the `DECODE_KNOBS` knobs, all shown in the decoding drawer.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum Field {
    Replacements,
    CleanupCommand,
    CleanupTimeout,
    MaxEntries,
    MaxAge,
    TranscriptFolder,
    Decode(usize),
}

/// How a control responds to keys, which also decides its footer hints.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum Kind {
    /// One option of a vertical radio list: Space selects it.
    Radio,
    /// A checkbox: Space toggles it.
    Check,
    /// A horizontal selector or stepper: h/l (or arrows) step it, Space
    /// cycles to the next choice.
    Choice,
    /// A button: Enter (or Space) runs it.
    Action,
    /// A text field: Enter starts editing it.
    Text,
    /// The model catalog table: j/k move its cursor, Space selects the row.
    Table,
}

impl Control {
    pub(super) fn kind(self) -> Kind {
        match self {
            Control::Microphone(_) => Kind::Radio,
            Control::PauseMedia
            | Control::FeedbackSounds
            | Control::Translate
            | Control::AutoSubmit
            | Control::SpokenPunctuation
            | Control::RemoveFillers
            | Control::HistoryEnabled
            | Control::Autostart => Kind::Check,
            Control::ActivationMode
            | Control::Backend
            | Control::OutputMethod
            | Control::TypingBackend
            | Control::PasteKeys
            | Control::OverlayEnabled
            | Control::OverlayPosition
            | Control::OverlayStyle
            | Control::OverlayLook
            | Control::OverlayShape => Kind::Choice,
            Control::Dictation
            | Control::Shortcut
            | Control::Decoding
            | Control::OverlayTest
            | Control::DaemonStart
            | Control::DaemonStop
            | Control::DaemonRestart => Kind::Action,
            Control::Field(_) => Kind::Text,
            Control::ModelTable => Kind::Table,
        }
    }
}

/// The colour role of a piece of informational text. Every role that carries
/// meaning also carries it in words or symbols, never in colour alone.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum Tone {
    Text,
    Dim,
    Accent,
    Ok,
    Warn,
    Error,
    Recording,
}

pub(super) enum Row {
    Control(Control),
    /// One line of label-free information, cut to the pane width.
    Info(Vec<(String, Tone)>),
    /// A sentence wrapped onto as many lines as the pane width needs.
    Wrapped(String, Tone),
    /// The live microphone input-level gauge.
    Meter,
}

pub(super) struct Pane {
    pub(super) title: &'static str,
    pub(super) rows: Vec<Row>,
}

/// One tab's panes. On a wide terminal `left` and `right` sit side by side;
/// on a narrow one they stack into a single scrolling column. The Model tab
/// also shows the catalog table, which takes the left column (or the top of
/// the narrow column) by itself.
pub(super) struct TabLayout {
    pub(super) table: bool,
    pub(super) left: Vec<Pane>,
    pub(super) right: Vec<Pane>,
}

impl TabLayout {
    /// Every focusable control in focus order.
    fn controls(&self) -> Vec<Control> {
        let mut controls = Vec::new();
        if self.table {
            controls.push(Control::ModelTable);
        }
        for pane in self.left.iter().chain(&self.right) {
            controls.extend(pane.controls());
        }
        controls
    }

    /// The controls grouped by pane, in focus order, with the table (if any)
    /// as its own group, so h/l can jump between panes.
    fn groups(&self, wide: bool) -> Vec<Vec<Control>> {
        let mut groups = Vec::new();
        if self.table {
            groups.push(vec![Control::ModelTable]);
        }
        if wide && !self.table {
            // Side by side, h/l moves between the two columns.
            for column in [&self.left, &self.right] {
                let controls: Vec<Control> = column.iter().flat_map(Pane::controls).collect();
                if !controls.is_empty() {
                    groups.push(controls);
                }
            }
        } else {
            for pane in self.left.iter().chain(&self.right) {
                let controls = pane.controls();
                if !controls.is_empty() {
                    groups.push(controls);
                }
            }
        }
        groups
    }
}

impl Pane {
    fn controls(&self) -> Vec<Control> {
        self.rows
            .iter()
            .filter_map(|row| match row {
                Row::Control(control) => Some(*control),
                _ => None,
            })
            .collect()
    }
}

/// What the daemon reported on the last status poll.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum DaemonState {
    /// No poll has answered yet.
    Unknown,
    Stopped,
    Idle,
    Recording,
    Processing,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct DaemonSnapshot {
    pub(super) state: DaemonState,
    /// The backend the daemon resolved, e.g. `transcribe.cpp/vulkan`.
    pub(super) backend: Option<String>,
    pub(super) error: Option<String>,
}

impl DaemonSnapshot {
    const UNKNOWN: Self = Self {
        state: DaemonState::Unknown,
        backend: None,
        error: None,
    };

    fn from_response(response: &Response) -> Self {
        match response {
            Response::Status {
                state,
                backend,
                operation_error,
                ..
            } => Self {
                state: match state.as_str() {
                    "recording" => DaemonState::Recording,
                    "processing" => DaemonState::Processing,
                    _ => DaemonState::Idle,
                },
                backend: backend.clone(),
                error: operation_error.clone(),
            },
            _ => Self {
                state: DaemonState::Idle,
                backend: None,
                error: None,
            },
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum DaemonAction {
    Start,
    Stop,
    Restart,
}

impl DaemonAction {
    fn progress(self) -> &'static str {
        match self {
            DaemonAction::Start => "Starting the daemon...",
            DaemonAction::Stop => "Stopping the daemon...",
            DaemonAction::Restart => "Restarting the daemon...",
        }
    }
}

/// Work the run loop performs outside the state machine.
#[derive(Debug, Clone)]
pub(super) enum Effect {
    /// `true` starts a dictation, `false` stops the running one.
    Dictation(bool),
    Daemon(DaemonAction),
    FetchLastResult,
    OverlayShow(OverlayConfig),
    OverlayHide,
    OpenShortcutDialog,
    CancelShortcutDialog,
    Download(String),
}

/// A finished (or progressing) piece of background work, applied by
/// `App::apply`.
#[derive(Debug, Clone)]
pub(super) enum TaskResult {
    Status(DaemonSnapshot),
    Dictation(Result<(), String>),
    Daemon(Result<String, String>),
    /// The daemon's newest transcript and when it was delivered (Unix
    /// seconds), if it has one.
    LastResult(Option<(String, u64)>),
    /// Whether the desktop accepted the overlay test (a layer-shell
    /// compositor is available).
    OverlayShown(bool),
    Shortcut(Result<ReconfigureOutcome, String>),
    DownloadProgress(u64, u64),
    Download(Result<crate::model::DownloadOutcome, String>),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct Message {
    pub(super) text: String,
    pub(super) tone: Tone,
    expires: Option<Instant>,
}

/// Text the daemon typed into this terminal while a dictation finished.
#[derive(Debug, Clone, Default)]
pub(super) struct Capture {
    pub(super) text: String,
    /// When capturing ends; `None` while the daemon is still processing.
    until: Option<Instant>,
}

pub(super) struct OverlayTest {
    pub(super) until: Instant,
    shown: OverlayConfig,
}

pub(super) struct Download {
    pub(super) id: String,
    pub(super) progress: (u64, u64),
    pub(super) result: Option<Result<crate::model::DownloadOutcome, String>>,
}

pub(super) struct App {
    pub(super) config: Config,
    /// The configuration as last read from or written to disk. Saving applies
    /// only the fields that differ from it onto the current file.
    baseline: Config,
    pub(super) capabilities: Capabilities,
    theme: Theme,
    pub(super) tab: Tab,
    focus: [Control; 4],
    pub(super) model_cursor: usize,
    pub(super) installed: Vec<bool>,
    /// Which rows the Speech models table shows and in what order. Session
    /// state only: it never changes the catalog or the saved configuration.
    model_filter: ModelFilter,
    model_sort: ModelSort,
    pub(super) editing: Option<(Field, TextField)>,
    pub(super) drawer: Option<usize>,
    pub(super) message: Option<Message>,
    pub(super) daemon: DaemonSnapshot,
    pub(super) daemon_busy: Option<DaemonAction>,
    pub(super) dictation_pending: bool,
    pub(super) capture: Option<Capture>,
    /// When the dictation in progress (or the last one) started, in Unix
    /// seconds, so an older transcript is never shown as its result.
    dictation_since: Option<u64>,
    pub(super) last_result: Option<String>,
    pub(super) overlay_test: Option<OverlayTest>,
    pub(super) shortcut_waiting: bool,
    pub(super) shortcut_feedback: Option<Result<ReconfigureOutcome, String>>,
    pub(super) download: Option<Download>,
    pub(super) mic_monitor: audio::MicMonitor,
    effects: Vec<Effect>,
    saved: bool,
    exit: Option<SetupOutcome>,
    /// Whether the last render had room for two columns; h/l pane movement
    /// follows what is on screen.
    pub(super) wide: Cell<bool>,
    pub(super) scroll: [Cell<u16>; 2],
    /// The catalog table's first visible row; `None` until the first frame
    /// centres the active model.
    pub(super) table_scroll: Cell<Option<usize>>,
    pub(super) drawer_scroll: Cell<u16>,
}

impl App {
    /// `fresh` is true when no config file exists yet. The defaults deliver
    /// nothing (`none` with history off), so a fresh install starts on
    /// typing, the primary dictation path, as an unsaved edit like any other.
    /// An existing file's `none` is the user's choice and is left alone.
    fn new(mut config: Config, capabilities: Capabilities, theme: Theme, fresh: bool) -> Self {
        let baseline = config.clone();
        if fresh
            && !config.history.enabled
            && config.output.method == OutputMethod::None
            && capabilities
                .output_method_problem(OutputMethod::Type)
                .is_none()
        {
            config.output.method = OutputMethod::Type;
        }
        let model_cursor = crate::catalog::ENTRIES
            .iter()
            .position(|entry| entry.id == config.model.active_model)
            .unwrap_or(0);
        let installed = crate::catalog::ENTRIES
            .iter()
            .map(|entry| crate::catalog::is_installed(entry.id))
            .collect();
        let microphone = capabilities.microphone_index(&config.audio.microphone);
        Self {
            config,
            baseline,
            capabilities,
            theme,
            tab: Tab::Audio,
            focus: [
                Control::Microphone(microphone),
                Tab::Model.first_control(),
                Tab::Output.first_control(),
                Tab::System.first_control(),
            ],
            model_cursor,
            installed,
            model_filter: ModelFilter::default(),
            model_sort: ModelSort::default(),
            editing: None,
            drawer: None,
            message: None,
            daemon: DaemonSnapshot::UNKNOWN,
            daemon_busy: None,
            dictation_pending: false,
            capture: None,
            dictation_since: None,
            last_result: None,
            overlay_test: None,
            shortcut_waiting: false,
            shortcut_feedback: None,
            download: None,
            mic_monitor: audio::MicMonitor::new(),
            effects: Vec::new(),
            saved: false,
            exit: None,
            wide: Cell::new(true),
            scroll: [Cell::new(0), Cell::new(0)],
            table_scroll: Cell::new(None),
            drawer_scroll: Cell::new(0),
        }
    }

    pub(super) fn take_effects(&mut self) -> Vec<Effect> {
        std::mem::take(&mut self.effects)
    }

    /// Whether the in-memory configuration differs from what is on disk.
    pub(super) fn dirty(&self) -> bool {
        toml::Value::try_from(&self.config).ok() != toml::Value::try_from(&self.baseline).ok()
    }

    pub(super) fn focused(&self) -> Control {
        let wanted = self.focus[self.tab.index()];
        let controls = self.layout().controls();
        if controls.contains(&wanted) {
            wanted
        } else {
            controls
                .first()
                .copied()
                .unwrap_or(self.tab.first_control())
        }
    }

    fn set_focus(&mut self, control: Control) {
        let previous = self.focused();
        self.focus[self.tab.index()] = control;
        if matches!(control, Control::Microphone(_)) && control != previous {
            self.mic_monitor.defer_restart();
        }
    }

    fn info(&mut self, text: impl Into<String>) {
        self.message = Some(Message {
            text: text.into(),
            tone: Tone::Ok,
            expires: Some(Instant::now() + INFO_MESSAGE_TTL),
        });
    }

    fn warn(&mut self, text: impl Into<String>) {
        self.message = Some(Message {
            text: text.into(),
            tone: Tone::Warn,
            expires: None,
        });
    }

    /// A usage hint for the control being edited, shown until the edit ends.
    fn hint(&mut self, text: impl Into<String>) {
        self.message = Some(Message {
            text: text.into(),
            tone: Tone::Dim,
            expires: None,
        });
    }

    fn error(&mut self, text: impl Into<String>) {
        self.message = Some(Message {
            text: text.into(),
            tone: Tone::Error,
            expires: None,
        });
    }

    // ---- layout -------------------------------------------------------

    pub(super) fn layout(&self) -> TabLayout {
        match self.tab {
            Tab::Audio => self.audio_layout(),
            Tab::Model => self.model_layout(),
            Tab::Output => self.output_layout(),
            Tab::System => self.system_layout(),
        }
    }

    pub(super) fn selected_microphone(&self) -> usize {
        self.capabilities
            .microphone_index(&self.config.audio.microphone)
    }

    pub(super) fn dictation_badge(&self) -> (&'static str, Tone) {
        if self.dictation_pending {
            return ("[ WAITING... ]", Tone::Dim);
        }
        match self.daemon.state {
            DaemonState::Recording => ("[ \u{1F534} RECORDING... ]", Tone::Recording),
            DaemonState::Processing => ("[ TRANSCRIBING... ]", Tone::Accent),
            DaemonState::Stopped => ("[ DAEMON STOPPED ]", Tone::Warn),
            DaemonState::Unknown => ("[ CHECKING... ]", Tone::Dim),
            DaemonState::Idle => ("[ IDLE ]", Tone::Text),
        }
    }

    fn audio_layout(&self) -> TabLayout {
        // The meter leads, so a long device list never scrolls it away.
        let microphone: Vec<Row> = std::iter::once(Row::Meter)
            .chain(
                (0..self.capabilities.microphones.len())
                    .map(|index| Row::Control(Control::Microphone(index))),
            )
            .collect();

        let (badge, tone) = self.dictation_badge();
        let mut dictation = vec![
            Row::Info(vec![("Status  ".into(), Tone::Dim), (badge.into(), tone)]),
            Row::Control(Control::Dictation),
        ];
        if let Some(capture) = &self.capture {
            dictation.push(Row::Info(vec![
                ("Typed here: ".into(), Tone::Dim),
                (capture_preview(&capture.text), Tone::Text),
            ]));
        } else if let Some(text) = &self.last_result {
            dictation.push(Row::Info(vec![
                ("Last result: ".into(), Tone::Dim),
                (text.clone(), Tone::Text),
            ]));
        }
        if self.dirty() {
            dictation.push(Row::Wrapped(
                "Dictation uses the saved settings; Ctrl+S applies your edits.".into(),
                Tone::Dim,
            ));
        }

        let mut activation = vec![
            Row::Control(Control::ActivationMode),
            Row::Control(Control::Shortcut),
        ];
        if self.shortcut_waiting {
            activation.push(Row::Wrapped(
                "A shortcut window is open on your desktop. Set the key there and close it; \
                 Enter or Esc keeps the current one."
                    .into(),
                Tone::Warn,
            ));
        } else if let Some(feedback) = &self.shortcut_feedback {
            activation.push(match feedback {
                Ok(ReconfigureOutcome::Changed {
                    trigger_description,
                }) => Row::Info(vec![(
                    format!(
                        "Bound: {}",
                        crate::activation::keybind_label(trigger_description)
                    ),
                    Tone::Ok,
                )]),
                Ok(ReconfigureOutcome::Unchanged {
                    trigger_description,
                }) => Row::Info(vec![(
                    format!(
                        "No change: still {}",
                        crate::activation::keybind_display(trigger_description)
                    ),
                    Tone::Warn,
                )]),
                Err(error) => Row::Wrapped(format!("Error: {error}"), Tone::Error),
            });
        }

        TabLayout {
            table: false,
            left: vec![Pane {
                title: "Microphone",
                rows: microphone,
            }],
            right: vec![
                Pane {
                    title: "Live dictation",
                    rows: dictation,
                },
                Pane {
                    title: "Activation",
                    rows: activation,
                },
                Pane {
                    title: "Audio",
                    rows: vec![
                        Row::Control(Control::PauseMedia),
                        Row::Control(Control::FeedbackSounds),
                    ],
                },
            ],
        }
    }

    fn model_layout(&self) -> TabLayout {
        let entry =
            &crate::catalog::ENTRIES[self.model_cursor.min(crate::catalog::ENTRIES.len() - 1)];
        let info = entry.info();
        let status = self.model_status(entry.id);
        let detail = vec![
            Row::Info(vec![(entry.id.to_string(), Tone::Accent)]),
            Row::Info(vec![
                ("Family   ".into(), Tone::Dim),
                (info.family_label().into(), Tone::Text),
            ]),
            Row::Info(vec![
                ("License  ".into(), Tone::Dim),
                (entry.license_spdx.into(), Tone::Text),
            ]),
            Row::Info(vec![
                ("Language ".into(), Tone::Dim),
                (info.language_summary(), Tone::Text),
            ]),
            Row::Info(vec![
                ("Input    ".into(), Tone::Dim),
                (input_limit(info.long_form), Tone::Text),
            ]),
            Row::Info(vec![("Status   ".into(), Tone::Dim), status]),
        ];

        let mut backend = vec![Row::Control(Control::Backend)];
        let unavailable: Vec<&str> = self
            .capabilities
            .inference_backends
            .iter()
            .filter(|choice| choice.unavailable.is_some())
            .map(|choice| choice.name)
            .collect();
        if !unavailable.is_empty() {
            backend.push(Row::Wrapped(
                format!("\u{00d7} unavailable here: {}", unavailable.join(", ")),
                Tone::Dim,
            ));
        }

        TabLayout {
            table: true,
            left: Vec::new(),
            right: vec![
                Pane {
                    title: "Model detail",
                    rows: detail,
                },
                Pane {
                    title: "Inference backend",
                    rows: backend,
                },
                Pane {
                    title: "Translation & decoding",
                    rows: vec![
                        Row::Control(Control::Translate),
                        Row::Control(Control::Decoding),
                    ],
                },
            ],
        }
    }

    fn model_status(&self, id: &str) -> (String, Tone) {
        if let Some(download) = self.download.as_ref().filter(|download| download.id == id) {
            return match &download.result {
                None => {
                    let (done, total) = download.progress;
                    let percent = (done * 100).checked_div(total).unwrap_or(0);
                    (format!("Downloading {percent}%"), Tone::Accent)
                }
                Some(Ok(outcome)) => (
                    format!("Installed ({})", super::outcome_label(*outcome)),
                    Tone::Ok,
                ),
                Some(Err(error)) => (format!("Download failed: {error}"), Tone::Error),
            };
        }
        let installed = crate::catalog::ENTRIES
            .iter()
            .position(|entry| entry.id == id)
            .and_then(|index| self.installed.get(index).copied())
            .unwrap_or(false);
        let active = self.config.model.active_model == id;
        match (active, installed) {
            (true, true) => ("\u{25cf} Active, installed".into(), Tone::Ok),
            (true, false) => ("\u{25cf} Active, not downloaded".into(), Tone::Warn),
            (false, true) => ("Installed - Space selects".into(), Tone::Text),
            (false, false) => ("Not downloaded - Space fetches".into(), Tone::Dim),
        }
    }

    fn output_layout(&self) -> TabLayout {
        let method = self.config.output.method;
        let mut delivery = vec![Row::Control(Control::OutputMethod)];
        if method.inserts_text() && self.capabilities.has_type_backend() {
            delivery.push(Row::Control(Control::TypingBackend));
        }
        if method == OutputMethod::Paste {
            delivery.push(Row::Control(Control::PasteKeys));
        }
        if method.inserts_text() {
            delivery.push(Row::Control(Control::AutoSubmit));
        }
        if let Some(problem) = self.capabilities.output_method_problem(method) {
            delivery.push(Row::Wrapped(format!("! {problem}"), Tone::Warn));
        } else if method == OutputMethod::None && !self.config.history.enabled {
            delivery.push(Row::Wrapped(
                "! Transcripts go nowhere: nothing is delivered and history is off. Type \
                 types into the focused application."
                    .into(),
                Tone::Warn,
            ));
        } else {
            let blocked: Vec<String> = DELIVERY_METHODS
                .iter()
                .filter(|candidate| {
                    self.capabilities
                        .output_method_problem(**candidate)
                        .is_some()
                })
                .map(|candidate| method_label(*candidate).to_string())
                .collect();
            if !blocked.is_empty() {
                delivery.push(Row::Info(vec![(
                    format!(
                        "\u{00d7} {} need a missing helper (see doctor)",
                        blocked.join(", ")
                    ),
                    Tone::Dim,
                )]));
            }
        }

        let filters = vec![
            Row::Control(Control::SpokenPunctuation),
            Row::Control(Control::RemoveFillers),
            Row::Control(Control::Field(Field::Replacements)),
            Row::Control(Control::Field(Field::CleanupCommand)),
            Row::Control(Control::Field(Field::CleanupTimeout)),
        ];

        let mut history = vec![Row::Control(Control::HistoryEnabled)];
        if self.config.history.enabled {
            history.push(Row::Control(Control::Field(Field::MaxEntries)));
            history.push(Row::Control(Control::Field(Field::MaxAge)));
            history.push(Row::Control(Control::Field(Field::TranscriptFolder)));
        } else {
            history.push(Row::Wrapped(
                "Off: no history database or transcript files are written.".into(),
                Tone::Dim,
            ));
        }

        TabLayout {
            table: false,
            left: vec![
                Pane {
                    title: "Delivery",
                    rows: delivery,
                },
                Pane {
                    title: "History & logs",
                    rows: history,
                },
            ],
            right: vec![Pane {
                title: "Text filters",
                rows: filters,
            }],
        }
    }

    fn system_layout(&self) -> TabLayout {
        let mut overlay = vec![
            Row::Control(Control::OverlayEnabled),
            Row::Control(Control::OverlayPosition),
            Row::Control(Control::OverlayStyle),
        ];
        if has_look_choice(&self.config.overlay.style) {
            overlay.push(Row::Control(Control::OverlayLook));
        }
        if shows_shape_choice(&self.config.overlay.style, &self.overlay_look()) {
            overlay.push(Row::Control(Control::OverlayShape));
        }

        let mut test = vec![Row::Control(Control::OverlayTest)];
        if let Some(seconds) = self.overlay_seconds_left() {
            test.push(Row::Info(vec![(
                format!("Showing on your desktop now, {seconds}s left"),
                Tone::Accent,
            )]));
        } else {
            test.push(Row::Wrapped(
                "Shows the real overlay for 4 seconds with the style, position, look, and \
                 shape set above."
                    .into(),
                Tone::Dim,
            ));
            if !self.config.overlay.enabled {
                test.push(Row::Wrapped(
                    "The overlay is off for dictation; the test still shows it.".into(),
                    Tone::Dim,
                ));
            }
        }

        let mut daemon = vec![Row::Info(vec![
            ("State  ".into(), Tone::Dim),
            daemon_state_label(self.daemon.state),
        ])];
        if let Some(action) = self.daemon_busy {
            daemon.push(Row::Info(vec![(action.progress().into(), Tone::Accent)]));
        }
        daemon.extend([
            Row::Control(Control::DaemonStart),
            Row::Control(Control::DaemonStop),
            Row::Control(Control::DaemonRestart),
            Row::Control(Control::Autostart),
        ]);

        TabLayout {
            table: false,
            left: vec![
                Pane {
                    title: "Desktop overlay",
                    rows: overlay,
                },
                Pane {
                    title: "Live screen test",
                    rows: test,
                },
            ],
            right: vec![Pane {
                title: "Daemon",
                rows: daemon,
            }],
        }
    }

    /// Whole seconds left in the desktop overlay test, while one runs.
    pub(super) fn overlay_seconds_left(&self) -> Option<u64> {
        self.overlay_test.as_ref().map(|test| {
            test.until
                .saturating_duration_since(Instant::now())
                .as_secs_f32()
                .ceil() as u64
        })
    }

    // ---- choices --------------------------------------------------------

    /// The options of a `Kind::Choice` control: each label, whether it can be
    /// picked here, and the index of the current value.
    pub(super) fn choice(&self, control: Control) -> (Vec<(String, bool)>, usize) {
        match control {
            Control::ActivationMode => (
                vec![("Hold".into(), true), ("Toggle".into(), true)],
                usize::from(self.config.activation.mode == ActivationMode::Toggle),
            ),
            Control::Backend => {
                let options: Vec<(String, bool)> =
                    if self.capabilities.inference_backends.is_empty() {
                        vec![("auto".into(), true)]
                    } else {
                        self.capabilities
                            .inference_backends
                            .iter()
                            .map(|choice| (choice.name.to_string(), choice.unavailable.is_none()))
                            .collect()
                    };
                let current = options
                    .iter()
                    .position(|(name, _)| *name == self.config.model.preferred_backend)
                    .unwrap_or(0);
                (options, current)
            }
            Control::OutputMethod => (
                DELIVERY_METHODS
                    .iter()
                    .map(|method| {
                        (
                            method_label(*method).to_string(),
                            self.capabilities.output_method_problem(*method).is_none(),
                        )
                    })
                    .collect(),
                DELIVERY_METHODS
                    .iter()
                    .position(|method| *method == self.config.output.method)
                    .unwrap_or(0),
            ),
            Control::TypingBackend => {
                let values = self.capabilities.typing_backend_values();
                let current = values
                    .iter()
                    .position(|value| *value == self.config.output.typing_backend)
                    .unwrap_or(0);
                (
                    values.into_iter().map(|value| (value, true)).collect(),
                    current,
                )
            }
            Control::PasteKeys => (
                PasteKeys::ALL
                    .iter()
                    .map(|keys| (keys.to_string(), true))
                    .collect(),
                PasteKeys::ALL
                    .iter()
                    .position(|keys| *keys == self.config.output.paste_keys)
                    .unwrap_or(0),
            ),
            Control::OverlayEnabled => (
                vec![("Off".into(), true), ("On".into(), true)],
                usize::from(self.config.overlay.enabled),
            ),
            Control::OverlayPosition => (
                POSITION_VALUES
                    .iter()
                    .map(|value| (value.to_string(), true))
                    .collect(),
                POSITION_VALUES
                    .iter()
                    .position(|value| *value == self.config.overlay.position)
                    .unwrap_or(0),
            ),
            Control::OverlayStyle => (
                crate::overlay::STYLE_LABELS
                    .iter()
                    .map(|label| (label.to_string(), true))
                    .collect(),
                STYLE_VALUES
                    .iter()
                    .position(|value| *value == self.config.overlay.style)
                    .unwrap_or(0),
            ),
            Control::OverlayLook => {
                let options = style_look_options(&self.config.overlay.style);
                let look = self.overlay_look();
                (
                    options
                        .iter()
                        .map(|(_, label)| (label.to_string(), true))
                        .collect(),
                    options
                        .iter()
                        .position(|(value, _)| *value == look)
                        .unwrap_or(0),
                )
            }
            Control::OverlayShape => (
                crate::overlay::SHAPE_LABELS
                    .iter()
                    .map(|label| (label.to_string(), true))
                    .collect(),
                SHAPE_VALUES
                    .iter()
                    .position(|value| *value == self.config.overlay.waveform_shape)
                    .unwrap_or(0),
            ),
            _ => (Vec::new(), 0),
        }
    }

    /// The configured look, resolved against what the style offers.
    fn overlay_look(&self) -> String {
        clamp_look_value(&self.config.overlay.style, &self.config.overlay.look)
    }

    /// Moves a choice `delta` steps, skipping options that cannot work here
    /// and wrapping at the ends.
    fn step(&mut self, control: Control, delta: isize) {
        let (options, current) = self.choice(control);
        if options.is_empty() {
            return;
        }
        let len = options.len() as isize;
        let mut next = current as isize;
        for _ in 0..options.len() {
            next = (next + delta).rem_euclid(len);
            if options[next as usize].1 {
                break;
            }
        }
        let next = next as usize;
        if next == current || !options[next].1 {
            return;
        }
        self.set_choice(control, next);
    }

    fn set_choice(&mut self, control: Control, index: usize) {
        let config = &mut self.config;
        match control {
            Control::ActivationMode => {
                config.activation.mode = if index == 0 {
                    ActivationMode::Hold
                } else {
                    ActivationMode::Toggle
                };
            }
            Control::Backend => {
                if let Some(choice) = self.capabilities.inference_backends.get(index) {
                    config.model.preferred_backend = choice.name.to_string();
                }
            }
            Control::OutputMethod => {
                let method = DELIVERY_METHODS[index];
                config.output.method = method;
                if !method.inserts_text() {
                    config.output.typing_backend = "auto".to_string();
                    config.output.auto_submit = false;
                }
            }
            Control::TypingBackend => {
                if let Some(value) = self.capabilities.typing_backend_values().get(index) {
                    config.output.typing_backend = value.clone();
                }
            }
            Control::PasteKeys => config.output.paste_keys = PasteKeys::ALL[index],
            Control::OverlayEnabled => config.overlay.enabled = index == 1,
            Control::OverlayPosition => {
                config.overlay.position = POSITION_VALUES[index].to_string();
            }
            Control::OverlayStyle => {
                config.overlay.style = STYLE_VALUES[index].to_string();
                // A new style may offer a different set of looks: keep a
                // stored look it still offers, otherwise snap to its first,
                // then start its own waveform shape.
                config.overlay.look = clamp_look_value(&config.overlay.style, &config.overlay.look);
                config.overlay.waveform_shape = default_shape(&config.overlay.style).to_string();
            }
            Control::OverlayLook => {
                if let Some((value, _)) = style_look_options(&config.overlay.style).get(index) {
                    config.overlay.look = value.to_string();
                }
            }
            Control::OverlayShape => {
                config.overlay.waveform_shape = SHAPE_VALUES[index].to_string();
            }
            _ => {}
        }
    }

    pub(super) fn checked(&self, control: Control) -> bool {
        let config = &self.config;
        match control {
            Control::PauseMedia => config.audio.pause_media,
            Control::FeedbackSounds => config.audio.feedback_sounds,
            Control::Translate => config.transcription.translate,
            Control::AutoSubmit => config.output.auto_submit,
            Control::SpokenPunctuation => config.text.spoken_punctuation,
            Control::RemoveFillers => config.text.remove_fillers,
            Control::HistoryEnabled => config.history.enabled,
            Control::Autostart => config.startup.autostart,
            _ => false,
        }
    }

    fn toggle(&mut self, control: Control) {
        let config = &mut self.config;
        let flag = match control {
            Control::PauseMedia => &mut config.audio.pause_media,
            Control::FeedbackSounds => &mut config.audio.feedback_sounds,
            Control::Translate => &mut config.transcription.translate,
            Control::AutoSubmit => &mut config.output.auto_submit,
            Control::SpokenPunctuation => &mut config.text.spoken_punctuation,
            Control::RemoveFillers => &mut config.text.remove_fillers,
            Control::HistoryEnabled => &mut config.history.enabled,
            Control::Autostart => &mut config.startup.autostart,
            _ => return,
        };
        *flag = !*flag;
    }

    // ---- text fields ----------------------------------------------------

    pub(super) fn field_value(&self, field: Field) -> String {
        let config = &self.config;
        match field {
            Field::Replacements => config.text.replacements_form_value(),
            Field::CleanupCommand => config.text.post_process_command.clone(),
            Field::CleanupTimeout => config.text.post_process_timeout_seconds.to_string(),
            Field::MaxEntries => config.history.max_entries.to_string(),
            Field::MaxAge => config.history.max_age_days.to_string(),
            Field::TranscriptFolder => config.history.transcript_folder.clone(),
            Field::Decode(0) => config.transcription.initial_prompt.clone(),
            Field::Decode(index) => config
                .transcription
                .decoding_form_values()
                .get(index - 1)
                .cloned()
                .unwrap_or_default(),
        }
    }

    /// Validates and applies an edited text field, leaving the configuration
    /// untouched and returning the problem when the value is invalid.
    fn apply_field(&mut self, field: Field, raw: &str) -> Result<(), String> {
        let config = &mut self.config;
        match field {
            Field::Replacements => config.text.apply_replacements_form(raw)?,
            Field::CleanupCommand => config.text.post_process_command = raw.trim().to_string(),
            Field::CleanupTimeout => match raw.trim().parse::<u64>() {
                Ok(value) if value > 0 => config.text.post_process_timeout_seconds = value,
                _ => return Err("Timeout must be a whole number of seconds (at least 1).".into()),
            },
            Field::MaxEntries => match raw.trim().parse::<u64>() {
                Ok(value) => config.history.max_entries = value,
                Err(_) => {
                    return Err("Maximum entries must be a whole number (0 = no limit).".into())
                }
            },
            Field::MaxAge => match raw.trim().parse::<u64>() {
                Ok(value) if value <= i64::MAX as u64 / 86_400 => {
                    config.history.max_age_days = value
                }
                Ok(_) => return Err("Maximum age is too large.".into()),
                Err(_) => return Err("Maximum age must be a whole number (0 = no limit).".into()),
            },
            Field::TranscriptFolder => {
                let raw = raw.trim();
                if raw.is_empty() {
                    config.history.transcript_folder = String::new();
                } else {
                    let expanded = crate::history::expand_home(raw);
                    if !expanded.is_absolute() {
                        return Err(
                            "Enter an absolute folder path, or one starting with ~/.".into()
                        );
                    }
                    config.history.transcript_folder = expanded.to_string_lossy().into_owned();
                }
            }
            Field::Decode(index) => {
                let mut prompt = config.transcription.initial_prompt.clone();
                let mut knobs = config.transcription.decoding_form_values();
                if index == 0 {
                    prompt = raw.to_string();
                } else if let Some(knob) = knobs.get_mut(index - 1) {
                    *knob = raw.to_string();
                }
                let knobs: [&str; crate::config::DECODE_KNOBS.len()] =
                    std::array::from_fn(|knob| knobs[knob].as_str());
                config.transcription.apply_decoding_form(&prompt, &knobs)?;
            }
        }
        Ok(())
    }

    // ---- keys -----------------------------------------------------------

    pub(super) fn handle_key(&mut self, key: KeyEvent) {
        let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);
        if ctrl && key.code == KeyCode::Char('c') {
            self.quit_discarding();
            return;
        }
        if self.capture.is_some() {
            self.handle_capture_key(key);
            return;
        }
        if self.editing.is_some() {
            self.handle_edit_key(key);
            return;
        }
        if self
            .message
            .as_ref()
            .is_some_and(|message| message.tone == Tone::Error)
        {
            // An error stays until the next key so it cannot flash past.
            self.message = None;
        }
        if ctrl && key.code == KeyCode::Char('s') {
            self.save();
            return;
        }
        if ctrl && key.code == KeyCode::Char('r') {
            self.open_shortcut_dialog();
            return;
        }
        if self.shortcut_waiting && matches!(key.code, KeyCode::Esc | KeyCode::Enter) {
            self.keep_current_shortcut();
            return;
        }
        if let Some(row) = self.drawer {
            if self.handle_drawer_key(key, row) {
                return;
            }
        }
        match key.code {
            KeyCode::Char(digit @ '1'..='4') => {
                self.switch_tab(Tab::ALL[digit as usize - '1' as usize]);
            }
            KeyCode::Char(']') | KeyCode::Tab => self.switch_tab(self.tab.offset(1)),
            KeyCode::Char('[') | KeyCode::BackTab => self.switch_tab(self.tab.offset(-1)),
            KeyCode::Down | KeyCode::Char('j') => self.move_vertical(1),
            KeyCode::Up | KeyCode::Char('k') => self.move_vertical(-1),
            KeyCode::PageDown => self.move_table(10),
            KeyCode::PageUp => self.move_table(-10),
            KeyCode::Left | KeyCode::Char('h') => self.move_horizontal(-1),
            KeyCode::Right | KeyCode::Char('l') => self.move_horizontal(1),
            KeyCode::Char(' ') => self.activate(false),
            KeyCode::Enter => self.activate(true),
            KeyCode::Char('i') => self.cycle_model_install_filter(),
            KeyCode::Char('f') => self.cycle_model_family(),
            KeyCode::Char('t') => self.cycle_model_quant(),
            KeyCode::Char('s') => self.cycle_model_sort(),
            KeyCode::Char('c') => self.clear_model_filters(),
            KeyCode::Char('q') | KeyCode::Esc => self.quit_saving(),
            _ => {}
        }
    }

    pub(super) fn handle_paste(&mut self, text: &str) {
        if let Some(capture) = &mut self.capture {
            capture.text.push_str(text);
        } else if let Some((_, field)) = &mut self.editing {
            for c in text.chars().filter(|c| !c.is_control()) {
                field.insert(c);
            }
        }
    }

    fn handle_capture_key(&mut self, key: KeyEvent) {
        let Some(capture) = &mut self.capture else {
            return;
        };
        match key.code {
            KeyCode::Esc => self.end_capture(),
            KeyCode::Enter => capture.text.push('\n'),
            KeyCode::Tab => capture.text.push('\t'),
            KeyCode::Backspace => {
                capture.text.pop();
            }
            KeyCode::Char(c) if !key.modifiers.contains(KeyModifiers::CONTROL) => {
                capture.text.push(c);
            }
            _ => {}
        }
    }

    fn handle_edit_key(&mut self, key: KeyEvent) {
        let Some((field_id, field)) = &mut self.editing else {
            return;
        };
        match key.code {
            KeyCode::Esc => {
                self.editing = None;
                self.message = None;
            }
            KeyCode::Enter => {
                let field_id = *field_id;
                let raw = field.value().to_string();
                match self.apply_field(field_id, &raw) {
                    Ok(()) => {
                        self.editing = None;
                        self.message = None;
                    }
                    Err(problem) => self.error(problem),
                }
            }
            KeyCode::Left => field.move_left(),
            KeyCode::Right => field.move_right(),
            KeyCode::Home => field.home(),
            KeyCode::End => field.end(),
            KeyCode::Backspace => field.backspace(),
            KeyCode::Char(c) if !key.modifiers.contains(KeyModifiers::CONTROL) => field.insert(c),
            _ => {}
        }
    }

    /// The decoding drawer's own keys. Returns `false` for a key the drawer
    /// leaves to the tab (tab switches close it first).
    fn handle_drawer_key(&mut self, key: KeyEvent, row: usize) -> bool {
        let rows = 1 + crate::config::DECODE_KNOBS.len();
        match key.code {
            KeyCode::Esc => self.drawer = None,
            KeyCode::Down | KeyCode::Char('j') => self.drawer = Some((row + 1).min(rows - 1)),
            KeyCode::Up | KeyCode::Char('k') => self.drawer = Some(row.saturating_sub(1)),
            KeyCode::Enter | KeyCode::Char(' ') => self.start_editing(Field::Decode(row)),
            KeyCode::Char('1'..='4' | '[' | ']' | 'q') | KeyCode::Tab | KeyCode::BackTab => {
                self.drawer = None;
                return false;
            }
            _ => {}
        }
        true
    }

    fn start_editing(&mut self, field: Field) {
        self.editing = Some((field, TextField::new(self.field_value(field))));
        let hint = match field {
            Field::Replacements => Some(format!(
                "Write from {} to, separating entries with '{}'; escape '{}', '>', and '\\' \
                 with '\\'. Empty clears them.",
                crate::config::REPLACEMENT_ARROW,
                crate::config::REPLACEMENT_ENTRY_SEPARATOR,
                crate::config::REPLACEMENT_ENTRY_SEPARATOR,
            )),
            Field::CleanupCommand => {
                Some("A command that reads the transcript on stdin and prints the cleaned text. Empty turns it off.".into())
            }
            Field::TranscriptFolder => Some(
                "An absolute folder (or ~/...) that gets one file per transcript. Empty turns it off."
                    .into(),
            ),
            Field::MaxEntries | Field::MaxAge => Some("0 means no limit.".into()),
            _ => None,
        };
        match hint {
            Some(hint) => self.hint(hint),
            None => self.message = None,
        }
    }

    fn switch_tab(&mut self, tab: Tab) {
        self.tab = tab;
        self.drawer = None;
    }

    fn move_vertical(&mut self, delta: isize) {
        let focused = self.focused();
        if focused == Control::ModelTable {
            let rows = self.model_rows();
            let Some(position) = rows.iter().position(|index| *index == self.model_cursor) else {
                // The cursor's model is filtered out; land on the nearest row
                // that is on screen instead of stepping from a hidden row.
                if let Some(first) = rows.first() {
                    self.model_cursor = *first;
                }
                return;
            };
            let next = position as isize + delta;
            if (0..rows.len() as isize).contains(&next) {
                self.model_cursor = rows[next as usize];
                return;
            }
            if delta < 0 {
                return;
            }
        }
        let controls = self.layout().controls();
        let Some(index) = controls.iter().position(|control| *control == focused) else {
            return;
        };
        let next = (index as isize + delta).clamp(0, controls.len() as isize - 1) as usize;
        self.set_focus(controls[next]);
    }

    fn move_table(&mut self, delta: isize) {
        if self.focused() == Control::ModelTable {
            let rows = self.model_rows();
            let Some(position) = rows.iter().position(|index| *index == self.model_cursor) else {
                if let Some(first) = rows.first() {
                    self.model_cursor = *first;
                }
                return;
            };
            let next = (position as isize + delta).clamp(0, rows.len() as isize - 1) as usize;
            self.model_cursor = rows[next];
        }
    }

    fn move_horizontal(&mut self, delta: isize) {
        let focused = self.focused();
        if focused.kind() == Kind::Choice {
            self.step(focused, delta);
            return;
        }
        let groups = self.layout().groups(self.wide.get());
        let Some(group) = groups.iter().position(|group| group.contains(&focused)) else {
            return;
        };
        let next = (group as isize + delta).clamp(0, groups.len() as isize - 1) as usize;
        if next != group {
            self.set_focus(groups[next][0]);
        }
    }

    /// Space (`enter == false`) or Enter on the focused control.
    fn activate(&mut self, enter: bool) {
        let focused = self.focused();
        match focused.kind() {
            Kind::Radio => {
                if let Control::Microphone(index) = focused {
                    if let Some((value, _)) = self.capabilities.microphones.get(index) {
                        self.config.audio.microphone = value.clone();
                    }
                }
            }
            Kind::Check => self.toggle(focused),
            Kind::Choice => self.step(focused, 1),
            Kind::Table => self.select_model(),
            Kind::Text => {
                if let Control::Field(field) = focused {
                    self.start_editing(field);
                }
            }
            Kind::Action => self.run_action(focused, enter),
        }
    }

    fn run_action(&mut self, control: Control, _enter: bool) {
        match control {
            Control::Dictation => self.toggle_dictation(),
            Control::Shortcut => self.open_shortcut_dialog(),
            Control::Decoding => self.drawer = Some(0),
            Control::OverlayTest => self.start_overlay_test(),
            Control::DaemonStart => self.daemon_action(DaemonAction::Start),
            Control::DaemonStop => self.daemon_action(DaemonAction::Stop),
            Control::DaemonRestart => self.daemon_action(DaemonAction::Restart),
            _ => {}
        }
    }

    fn select_model(&mut self) {
        let entry = &crate::catalog::ENTRIES[self.model_cursor];
        let installed = self.installed[self.model_cursor];
        let downloading = self
            .download
            .as_ref()
            .is_some_and(|download| download.result.is_none());
        if !installed && downloading && self.download.as_ref().is_some_and(|d| d.id != entry.id) {
            self.warn("Another model is still downloading; select this one when it finishes.");
            return;
        }
        let previous = self.config.model.active_model.clone();
        self.config.model.active_model = entry.id.to_string();
        if let Err(error) = self.config.validate() {
            // A model whose input cap is below the configured recording limit
            // would make the whole file invalid; refuse it with the reason.
            self.config.model.active_model = previous;
            self.error(error.to_string());
            return;
        }
        if !installed && !downloading {
            self.download = Some(Download {
                id: entry.id.to_string(),
                progress: (0, 0),
                result: None,
            });
            self.effects.push(Effect::Download(entry.id.to_string()));
        }
    }

    // ---- model table view ----------------------------------------------

    /// Whether the table owns the keys right now.
    fn table_focused(&self) -> bool {
        self.focused() == Control::ModelTable && self.drawer.is_none()
    }

    /// The catalog indices the table shows, in order. Cursor movement,
    /// scrolling, and rendering all read the same list.
    pub(super) fn model_rows(&self) -> Vec<usize> {
        model_view::visible(self.model_filter, self.model_sort, &self.installed)
    }

    /// Whether the active model is in the catalog but hidden by a filter, so
    /// the table can say so instead of silently dropping its marker.
    pub(super) fn active_model_hidden(&self, rows: &[usize]) -> bool {
        self.model_filter.is_active()
            && crate::catalog::ENTRIES
                .iter()
                .position(|entry| entry.id == self.config.model.active_model)
                .is_some_and(|index| !rows.contains(&index))
    }

    /// After the visible rows change, keep the cursor on a row that is still
    /// on screen and re-centre the table around it. An empty result keeps the
    /// cursor where it was, so the detail pane still describes the last
    /// inspected model while the table explains why nothing matches.
    fn keep_model_cursor_visible(&mut self) {
        let rows = self.model_rows();
        if rows.is_empty() || rows.contains(&self.model_cursor) {
            return;
        }
        let cursor = self.model_cursor;
        self.model_cursor = rows
            .iter()
            .copied()
            .min_by_key(|index| index.abs_diff(cursor))
            .unwrap_or(cursor);
    }

    fn model_view_changed(&mut self) {
        self.keep_model_cursor_visible();
        self.table_scroll.set(None);
    }

    /// `i`: cycle any -> installed -> not installed -> any.
    pub(super) fn cycle_model_install_filter(&mut self) {
        if !self.table_focused() {
            return;
        }
        self.model_filter.install = self.model_filter.install.next();
        self.model_view_changed();
    }

    /// `f`: cycle every family in catalog order, then back to all.
    pub(super) fn cycle_model_family(&mut self) {
        if !self.table_focused() {
            return;
        }
        self.model_filter.family =
            model_view::cycle(&model_view::families(), self.model_filter.family);
        self.model_view_changed();
    }

    /// `t`: cycle every quantization in catalog order, then back to all.
    pub(super) fn cycle_model_quant(&mut self) {
        if !self.table_focused() {
            return;
        }
        self.model_filter.quant = model_view::cycle(&model_view::quants(), self.model_filter.quant);
        self.model_view_changed();
    }

    /// `s`: cycle the sort order, keeping the cursor on the same model.
    pub(super) fn cycle_model_sort(&mut self) {
        if !self.table_focused() {
            return;
        }
        self.model_sort = self.model_sort.next();
        self.model_view_changed();
    }

    /// `c`: clear every filter, showing the whole catalog again.
    pub(super) fn clear_model_filters(&mut self) {
        if !self.table_focused() {
            return;
        }
        self.model_filter.clear();
        self.model_view_changed();
    }

    fn toggle_dictation(&mut self) {
        if self.dictation_pending {
            return;
        }
        match self.daemon.state {
            DaemonState::Stopped => {
                self.error("The daemon is not running. Start it in [4] Interface & System.");
            }
            DaemonState::Processing => {}
            DaemonState::Recording => {
                self.dictation_pending = true;
                // The daemon types the transcript into whatever has focus,
                // usually this terminal: capture it rather than act on it.
                self.capture = Some(Capture::default());
                self.effects.push(Effect::Dictation(false));
            }
            DaemonState::Idle | DaemonState::Unknown => {
                self.dictation_pending = true;
                self.last_result = None;
                self.effects.push(Effect::Dictation(true));
            }
        }
    }

    fn end_capture(&mut self) {
        if let Some(capture) = self.capture.take() {
            if !capture.text.trim().is_empty() {
                self.last_result = Some(capture.text.trim().to_string());
            }
            self.effects.push(Effect::FetchLastResult);
        }
    }

    /// Opens the desktop dialog, or reopens it while one is already open
    /// (the executor waits for the old request to settle first).
    fn open_shortcut_dialog(&mut self) {
        self.shortcut_waiting = true;
        self.shortcut_feedback = None;
        self.effects.push(Effect::OpenShortcutDialog);
    }

    /// The user closed the desktop dialog without a change: stop waiting and
    /// settle on whatever is bound, reported as "no change", not a failure.
    fn keep_current_shortcut(&mut self) {
        self.shortcut_waiting = false;
        self.effects.push(Effect::CancelShortcutDialog);
        self.shortcut_feedback = Some(Ok(ReconfigureOutcome::Unchanged {
            trigger_description: self.config.activation.keybind.clone(),
        }));
    }

    fn start_overlay_test(&mut self) {
        let shown = self.config.overlay.clone();
        self.overlay_test = Some(OverlayTest {
            until: Instant::now() + OVERLAY_TEST_DURATION,
            shown: shown.clone(),
        });
        self.effects.push(Effect::OverlayShow(shown));
    }

    fn daemon_action(&mut self, action: DaemonAction) {
        if self.daemon_busy.is_some() {
            return;
        }
        self.daemon_busy = Some(action);
        self.effects.push(Effect::Daemon(action));
    }

    // ---- background results and timers -------------------------------

    pub(super) fn apply(&mut self, result: TaskResult) {
        match result {
            TaskResult::Status(snapshot) => self.apply_status(snapshot),
            TaskResult::Dictation(outcome) => {
                self.dictation_pending = false;
                if let Err(error) = outcome {
                    self.capture = None;
                    self.error(format!("Dictation request failed: {error}"));
                }
            }
            TaskResult::Daemon(outcome) => {
                self.daemon_busy = None;
                match outcome {
                    Ok(text) => self.info(text),
                    Err(error) => self.error(error),
                }
            }
            TaskResult::LastResult(result) => {
                let since = self.dictation_since.unwrap_or(0);
                self.last_result = Some(match result {
                    Some((text, delivered)) if delivered >= since && !text.trim().is_empty() => {
                        text
                    }
                    _ => NO_SPEECH.to_string(),
                });
            }
            TaskResult::OverlayShown(true) => {}
            TaskResult::OverlayShown(false) => {
                self.overlay_test = None;
                self.error(
                    "This desktop offers no layer-shell overlay, so dictation uses desktop \
                     notifications here instead.",
                );
            }
            TaskResult::Shortcut(outcome) => {
                self.shortcut_waiting = false;
                if let Ok(ReconfigureOutcome::Changed {
                    trigger_description,
                }) = &outcome
                {
                    self.config.activation.keybind =
                        crate::activation::keybind_label(trigger_description);
                }
                self.shortcut_feedback = Some(outcome);
            }
            TaskResult::DownloadProgress(done, total) => {
                if let Some(download) = &mut self.download {
                    download.progress = (done, total);
                }
            }
            TaskResult::Download(outcome) => {
                let installed_id = match (&self.download, outcome.is_ok()) {
                    (Some(download), true) => Some(download.id.clone()),
                    _ => None,
                };
                if let Some(index) = installed_id.as_deref().and_then(|id| {
                    crate::catalog::ENTRIES
                        .iter()
                        .position(|entry| entry.id == id)
                }) {
                    self.installed[index] = true;
                }
                if let Some(download) = &mut self.download {
                    download.result = Some(outcome);
                }
                if installed_id.is_some() {
                    // An installed/not-installed filter can make this row
                    // disappear now that the download finished.
                    self.keep_model_cursor_visible();
                }
            }
        }
    }

    fn apply_status(&mut self, snapshot: DaemonSnapshot) {
        let previous = self.daemon.state;
        let now = snapshot.state;
        if now == DaemonState::Recording && previous != DaemonState::Recording {
            // A second of slack for the clock tick between here and delivery.
            self.dictation_since = Some(unix_seconds().saturating_sub(1));
            self.last_result = None;
        }
        if now == DaemonState::Processing && self.capture.is_none() {
            // A dictation stopped from the desktop shortcut types into this
            // terminal too.
            self.capture = Some(Capture::default());
        }
        if let Some(capture) = &mut self.capture {
            if matches!(now, DaemonState::Idle | DaemonState::Stopped) && capture.until.is_none() {
                capture.until = Some(Instant::now() + CAPTURE_GRACE);
            }
        }
        if previous == DaemonState::Recording && now == DaemonState::Idle && self.capture.is_none()
        {
            self.effects.push(Effect::FetchLastResult);
        }
        self.daemon = snapshot;
    }

    /// Per-frame timers: the overlay test countdown (and its live restyle),
    /// the capture grace period, and message expiry.
    pub(super) fn tick(&mut self, now: Instant) {
        if let Some(test) = &mut self.overlay_test {
            if now >= test.until {
                self.overlay_test = None;
                self.effects.push(Effect::OverlayHide);
            } else if !overlay_matches(&test.shown, &self.config.overlay) {
                test.shown = self.config.overlay.clone();
                self.effects.push(Effect::OverlayShow(test.shown.clone()));
            }
        }
        if self
            .capture
            .as_ref()
            .and_then(|capture| capture.until)
            .is_some_and(|until| now >= until)
        {
            self.end_capture();
        }
        if self
            .message
            .as_ref()
            .and_then(|message| message.expires)
            .is_some_and(|expires| now >= expires)
        {
            self.message = None;
        }
    }

    /// Keeps the live level meter bound to the right microphone: the focused
    /// row on the Audio tab (so moving through the list is audible before
    /// selecting), the selected one otherwise, and the selected one during the
    /// overlay test so its waveform follows the voice.
    pub(super) fn refresh_mic(&mut self) {
        let wanted = if self.overlay_test.is_some() {
            Some(self.selected_microphone())
        } else if self.tab == Tab::Audio {
            match self.focused() {
                Control::Microphone(index) => Some(index),
                _ => Some(self.selected_microphone()),
            }
        } else {
            None
        };
        let settled = self.mic_monitor.poll();
        match wanted {
            None => {
                if self.mic_monitor.active_index().is_some() || self.mic_monitor.restart_pending() {
                    self.mic_monitor.stop();
                }
            }
            Some(index) => {
                let stale = self.mic_monitor.active_index() != Some(index)
                    && !self.mic_monitor.restart_pending();
                if settled || stale {
                    if let Some((device, _)) = self.capabilities.microphones.get(index) {
                        let device = device.clone();
                        self.mic_monitor.sync(index, &device);
                    }
                }
            }
        }
        if self.overlay_test.is_some() {
            audio::publish_level(self.mic_monitor.level());
        }
    }

    // ---- saving ---------------------------------------------------------

    fn save(&mut self) {
        match self.persist() {
            Ok(true) => self.info("Saved. A running daemon applies it live."),
            Ok(false) => self.info("No changes to save."),
            Err(error) => self.error(format!("Not saved: {error}")),
        }
    }

    /// Writes the fields that changed since the last load or save onto the
    /// current file, so edits made elsewhere meanwhile survive. Returns
    /// whether anything was written.
    fn persist(&mut self) -> anyhow::Result<bool> {
        let edited = toml::Value::try_from(&self.config)?;
        let baseline = toml::Value::try_from(&self.baseline)?;
        if edited == baseline {
            return Ok(false);
        }
        if self.config.output.method != self.baseline.output.method {
            if let Some(problem) = self
                .capabilities
                .output_method_problem(self.config.output.method)
            {
                anyhow::bail!("{problem}");
            }
        }
        let mut current = toml::Value::try_from(&read_config()?)?;
        merge_changes(&baseline, &edited, &mut current);
        let merged: Config = current.try_into()?;
        merged.validate()?;
        write_config(&merged)?;
        self.baseline = merged.clone();
        self.config = merged;
        self.saved = true;
        Ok(true)
    }

    fn quit_saving(&mut self) {
        match self.persist() {
            Ok(_) => self.finish(),
            Err(error) => self.error(format!(
                "Not saved: {error} Fix it, or press Ctrl+C to quit without saving."
            )),
        }
    }

    fn quit_discarding(&mut self) {
        if self.dirty() {
            self.exit = Some(SetupOutcome::Cancelled);
            self.cleanup();
        } else {
            self.finish();
        }
    }

    fn finish(&mut self) {
        self.exit = Some(if self.saved {
            SetupOutcome::Saved
        } else {
            SetupOutcome::Unchanged
        });
        self.cleanup();
    }

    /// Leaves nothing running on the desktop after setup closes.
    fn cleanup(&mut self) {
        if self.overlay_test.take().is_some() {
            self.effects.push(Effect::OverlayHide);
        }
        if self.shortcut_waiting {
            self.shortcut_waiting = false;
            self.effects.push(Effect::CancelShortcutDialog);
        }
        self.mic_monitor.stop();
    }
}

#[cfg(not(test))]
fn read_config() -> anyhow::Result<Config> {
    Config::reload()
}

#[cfg(not(test))]
fn write_config(config: &Config) -> anyhow::Result<()> {
    crate::autostart::save_configuration(config)
}

// Unit tests never touch the real config directory or autostart entry: the
// file is a per-thread value they seed and inspect.
#[cfg(test)]
thread_local! {
    static TEST_DISK: std::cell::RefCell<Config> = std::cell::RefCell::new(Config::default());
}

#[cfg(test)]
fn read_config() -> anyhow::Result<Config> {
    Ok(TEST_DISK.with(|disk| disk.borrow().clone()))
}

#[cfg(test)]
fn write_config(config: &Config) -> anyhow::Result<()> {
    config.validate()?;
    TEST_DISK.with(|disk| *disk.borrow_mut() = config.clone());
    Ok(())
}

/// Applies every leaf that differs between `base` and `edited` onto `target`,
/// recursing into tables so unrelated keys in `target` are kept. A key that
/// `edited` dropped (an unset optional, a removed replacement) is removed.
pub(super) fn merge_changes(base: &toml::Value, edited: &toml::Value, target: &mut toml::Value) {
    use toml::Value;
    match (base, edited, target) {
        (Value::Table(base), Value::Table(edited), Value::Table(target)) => {
            for (key, value) in edited {
                match (base.get(key), target.get_mut(key)) {
                    (Some(old), _) if old == value => {}
                    (Some(old), Some(slot)) if old.is_table() && value.is_table() => {
                        merge_changes(old, value, slot);
                    }
                    _ => {
                        target.insert(key.clone(), value.clone());
                    }
                }
            }
            for key in base.keys() {
                if !edited.contains_key(key) {
                    target.remove(key);
                }
            }
        }
        (_, edited, target) => *target = edited.clone(),
    }
}

/// What the Live dictation card shows when a dictation produced no text.
const NO_SPEECH: &str = "(no speech heard)";

fn unix_seconds() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|elapsed| elapsed.as_secs())
        .unwrap_or(0)
}

fn overlay_matches(a: &OverlayConfig, b: &OverlayConfig) -> bool {
    toml::Value::try_from(a).ok() == toml::Value::try_from(b).ok()
}

pub(super) fn method_label(method: OutputMethod) -> &'static str {
    match method {
        OutputMethod::Type => "Type",
        OutputMethod::Paste => "Paste",
        OutputMethod::Clipboard => "Clipboard",
        OutputMethod::None => "None",
    }
}

fn input_limit(long_form: crate::catalog::LongForm) -> String {
    match long_form {
        crate::catalog::LongForm::Unbounded => "any length".into(),
        crate::catalog::LongForm::SoftWindow { window_seconds } => {
            format!("best under {window_seconds}s")
        }
        crate::catalog::LongForm::HardCap { cap_seconds } => format!("at most {cap_seconds}s"),
    }
}

pub(super) fn daemon_state_label(state: DaemonState) -> (String, Tone) {
    match state {
        DaemonState::Unknown => ("\u{25cc} CHECKING".into(), Tone::Dim),
        DaemonState::Stopped => ("\u{25cb} STOPPED".into(), Tone::Warn),
        DaemonState::Idle => ("\u{25cf} ACTIVE".into(), Tone::Ok),
        DaemonState::Recording => ("\u{25cf} RECORDING".into(), Tone::Recording),
        DaemonState::Processing => ("\u{25cf} TRANSCRIBING".into(), Tone::Accent),
    }
}

/// The captured text on one line: newlines shown as a return symbol.
fn capture_preview(text: &str) -> String {
    if text.is_empty() {
        "(waiting for the transcript...)".into()
    } else {
        text.replace('\n', " \u{23ce} ").replace('\t', " ")
    }
}

// ---- running ---------------------------------------------------------------

pub(super) fn run(capabilities: Capabilities) -> anyhow::Result<SetupOutcome> {
    let fresh = !Config::config_path()?.exists();
    let config = Config::reload()?;
    let mut app = App::new(config, capabilities, Theme::from_env(), fresh);
    let mut executor = Executor::start();

    enable_raw_mode()?;
    let mut stdout = io::stdout();
    if let Err(error) = execute!(stdout, EnterAlternateScreen, EnableBracketedPaste) {
        let _ = disable_raw_mode();
        return Err(error.into());
    }
    let mut guard = TerminalGuard {
        terminal: Terminal::new(CrosstermBackend::new(stdout))?,
    };
    let outcome = run_loop(&mut app, &mut executor, &mut guard.terminal);
    for effect in app.take_effects() {
        executor.run(effect);
    }
    drop(guard);
    outcome
}

fn run_loop(
    app: &mut App,
    executor: &mut Executor,
    terminal: &mut Terminal<CrosstermBackend<io::Stdout>>,
) -> anyhow::Result<SetupOutcome> {
    loop {
        for result in executor.poll() {
            app.apply(result);
        }
        app.tick(Instant::now());
        app.refresh_mic();
        for effect in app.take_effects() {
            executor.run(effect);
        }
        if let Some(outcome) = app.exit.take() {
            return Ok(outcome);
        }
        terminal.draw(|frame| app.render(frame))?;
        if event::poll(Duration::from_millis(66))? {
            match event::read()? {
                Event::Key(key) if key.kind != KeyEventKind::Release => app.handle_key(key),
                Event::Paste(text) => app.handle_paste(&text),
                _ => {}
            }
        }
    }
}

/// Restores the terminal however setup ends - a normal exit, an error, or a
/// panic unwinding through `run`.
struct TerminalGuard {
    terminal: Terminal<CrosstermBackend<io::Stdout>>,
}

impl Drop for TerminalGuard {
    fn drop(&mut self) {
        let _ = disable_raw_mode();
        let _ = execute!(
            self.terminal.backend_mut(),
            DisableBracketedPaste,
            LeaveAlternateScreen
        );
        let _ = self.terminal.show_cursor();
    }
}

/// The overlay this process flashes for the desktop test - separate from the
/// daemon's own, so testing never disturbs a dictation in progress.
static TEST_OVERLAY: OverlayHandle = OverlayHandle::new();

type Slot = Arc<Mutex<Option<TaskResult>>>;

/// Runs effects on background threads (each with its own small tokio
/// runtime, since setup's render loop is synchronous) and collects their
/// results, plus a long-lived daemon status poller.
struct Executor {
    slots: Vec<Slot>,
    status: Arc<Mutex<Option<DaemonSnapshot>>>,
    stop: Arc<AtomicBool>,
    /// Overlay commands run in order on one worker, so a hide can never
    /// overtake the show it ends while the first show is still probing.
    overlay: std::sync::mpsc::Sender<(Effect, Slot)>,
    shortcut: Option<ReconfigureHandle>,
    /// A reopen requested while `shortcut` was still open: its wait was
    /// cancelled, and the replacement dialog opens only once that wait
    /// settles. Starting it immediately would race the cancel (the daemon
    /// could reject the new dialog as "already open", or the stale cancel
    /// could end the new one).
    shortcut_reopen: bool,
    download: Option<ProvisionHandle>,
    download_progress: (u64, u64),
}

impl Executor {
    fn start() -> Self {
        let status = Arc::new(Mutex::new(None));
        let stop = Arc::new(AtomicBool::new(false));
        let status_for_thread = status.clone();
        let stop_for_thread = stop.clone();
        std::thread::spawn(move || poll_status(status_for_thread, stop_for_thread));
        let (overlay, overlay_commands) = std::sync::mpsc::channel();
        std::thread::spawn(move || run_overlay_commands(overlay_commands));
        Self {
            slots: Vec::new(),
            status,
            stop,
            overlay,
            shortcut: None,
            shortcut_reopen: false,
            download: None,
            download_progress: (0, 0),
        }
    }

    /// Runs `future` on its own thread; `failed` builds the result reported
    /// when that thread cannot even start its runtime.
    fn spawn<F>(&mut self, future: F, failed: fn(String) -> TaskResult)
    where
        F: std::future::Future<Output = TaskResult> + Send + 'static,
    {
        let slot: Slot = Arc::new(Mutex::new(None));
        let slot_for_thread = slot.clone();
        std::thread::spawn(move || {
            let result = match tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
            {
                Ok(runtime) => runtime.block_on(future),
                Err(error) => failed(error.to_string()),
            };
            if let Ok(mut guard) = slot_for_thread.lock() {
                *guard = Some(result);
            }
        });
        self.slots.push(slot);
    }

    fn run(&mut self, effect: Effect) {
        match effect {
            Effect::Dictation(start) => self.spawn(
                async move {
                    let request = if start { Request::Start } else { Request::Stop };
                    TaskResult::Dictation(match crate::commands::send_ipc(request).await {
                        Ok(Response::Error { message }) => Err(message),
                        Ok(_) => Ok(()),
                        Err(error) => Err(error.to_string()),
                    })
                },
                |error| TaskResult::Dictation(Err(error)),
            ),
            Effect::Daemon(action) => self.spawn(
                async move { TaskResult::Daemon(run_daemon_action(action).await) },
                |error| TaskResult::Daemon(Err(error)),
            ),
            Effect::FetchLastResult => self.spawn(
                async {
                    TaskResult::LastResult(
                        match crate::commands::send_ipc(Request::GetLastResult).await {
                            Ok(Response::LastResult { text, timestamp }) => Some((text, timestamp)),
                            _ => None,
                        },
                    )
                },
                |_| TaskResult::LastResult(None),
            ),
            effect @ (Effect::OverlayShow(_) | Effect::OverlayHide) => {
                let reports = matches!(effect, Effect::OverlayShow(_));
                let slot: Slot = Arc::new(Mutex::new(None));
                if self.overlay.send((effect, slot.clone())).is_ok() && reports {
                    self.slots.push(slot);
                }
            }
            Effect::OpenShortcutDialog => match &self.shortcut {
                Some(handle) => {
                    handle.cancel();
                    self.shortcut_reopen = true;
                }
                None => self.shortcut = Some(super::reconfigure_shortcut_async()),
            },
            Effect::CancelShortcutDialog => {
                self.shortcut_reopen = false;
                if let Some(handle) = self.shortcut.take() {
                    handle.cancel();
                }
            }
            Effect::Download(id) => {
                self.download_progress = (0, 0);
                self.download = Some(super::provision_model_async(id));
            }
        }
    }

    fn poll(&mut self) -> Vec<TaskResult> {
        let mut results = Vec::new();
        if let Some(snapshot) = self.status.lock().ok().and_then(|mut guard| guard.take()) {
            results.push(TaskResult::Status(snapshot));
        }
        self.slots.retain(
            |slot| match slot.lock().ok().and_then(|mut guard| guard.take()) {
                Some(result) => {
                    results.push(result);
                    false
                }
                None => true,
            },
        );
        if let Some(handle) = &self.shortcut {
            if let Some(outcome) = handle.result.lock().ok().and_then(|mut guard| guard.take()) {
                self.shortcut = None;
                if std::mem::take(&mut self.shortcut_reopen) {
                    self.shortcut = Some(super::reconfigure_shortcut_async());
                } else {
                    results.push(TaskResult::Shortcut(outcome));
                }
            }
        }
        if let Some(handle) = &self.download {
            let progress = handle.progress.lock().map(|guard| *guard).unwrap_or((0, 0));
            if progress != self.download_progress {
                self.download_progress = progress;
                results.push(TaskResult::DownloadProgress(progress.0, progress.1));
            }
            if let Some(outcome) = handle.result.lock().ok().and_then(|mut guard| guard.take()) {
                self.download = None;
                results.push(TaskResult::Download(outcome));
            }
        }
        results
    }
}

impl Drop for Executor {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
    }
}

/// The overlay worker. The first show probes the compositor (up to two
/// seconds), so none of this runs on the render thread. A hide before any
/// show has nothing to clear and never starts the overlay.
fn run_overlay_commands(commands: std::sync::mpsc::Receiver<(Effect, Slot)>) {
    for (effect, slot) in commands {
        let result = match effect {
            Effect::OverlayShow(config) => Some(TaskResult::OverlayShown(
                TEST_OVERLAY.try_send(FeedbackEvent::Recording, &config),
            )),
            _ => {
                if TEST_OVERLAY.is_started() {
                    TEST_OVERLAY.try_hide();
                }
                None
            }
        };
        if let Ok(mut guard) = slot.lock() {
            *guard = result;
        }
    }
}

fn poll_status(latest: Arc<Mutex<Option<DaemonSnapshot>>>, stop: Arc<AtomicBool>) {
    let Ok(runtime) = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
    else {
        return;
    };
    while !stop.load(Ordering::Relaxed) {
        let snapshot = runtime.block_on(daemon_snapshot());
        if let Ok(mut guard) = latest.lock() {
            *guard = Some(snapshot);
        }
        std::thread::sleep(STATUS_INTERVAL);
    }
}

async fn daemon_snapshot() -> DaemonSnapshot {
    let stopped = DaemonSnapshot {
        state: DaemonState::Stopped,
        backend: None,
        error: None,
    };
    if !crate::commands::daemon_socket_exists() {
        return stopped;
    }
    match tokio::time::timeout(
        Duration::from_secs(2),
        crate::commands::send_ipc(Request::Status),
    )
    .await
    {
        Ok(Ok(response)) => DaemonSnapshot::from_response(&response),
        _ => stopped,
    }
}

/// Starts, stops, or restarts the daemon and waits for it to answer. A
/// restart whose relaunch fails tries once more, so a transient failure does
/// not leave dictation without a daemon.
async fn run_daemon_action(action: DaemonAction) -> Result<String, String> {
    use crate::commands;
    match action {
        DaemonAction::Start => {
            if commands::daemon_socket_exists() {
                return Ok("The daemon is already running.".into());
            }
            commands::spawn_daemon()
                .map_err(|error| format!("Could not start the daemon: {error}"))?;
            wait_for_daemon("started").await
        }
        DaemonAction::Stop => commands::stop_daemon()
            .await
            .map(|()| "Daemon stopped.".into())
            .map_err(|error| format!("Could not stop the daemon: {error}")),
        DaemonAction::Restart => {
            if commands::daemon_socket_exists() {
                commands::stop_daemon().await.map_err(|error| {
                    format!("Could not stop the daemon, so it keeps running: {error}")
                })?;
            }
            if let Err(first) = commands::spawn_daemon() {
                tokio::time::sleep(Duration::from_millis(500)).await;
                commands::spawn_daemon().map_err(|error| {
                    format!("The daemon stopped but could not start again: {first}; {error}")
                })?;
            }
            wait_for_daemon("restarted").await
        }
    }
}

async fn wait_for_daemon(verb: &str) -> Result<String, String> {
    for _ in 0..100 {
        tokio::time::sleep(Duration::from_millis(300)).await;
        if crate::commands::daemon_socket_exists()
            && crate::commands::send_ipc(Request::Status).await.is_ok()
        {
            return Ok(format!("Daemon {verb}."));
        }
    }
    Ok(format!(
        "Daemon {verb}, but it has not answered yet (it may be loading its model)."
    ))
}
