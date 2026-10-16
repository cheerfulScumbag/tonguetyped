//! State and rendering for the dashboard's interactive sub-screens that
//! aren't just "run one action and show the result": the model screen
//! (catalog activation + inference backend), the daemon actions, and the
//! settings screens the home screen's Settings panel opens (inference
//! backend, translation, decoding, text processing, microphone, activation,
//! shortcut, transcript output, typing backend, startup, overlay).
//!
//! Most settings screens here only mutate the in-memory `Config` they are
//! handed (`apply`), and the caller (`tui::App`) owns `Config::save` so a
//! failed write surfaces in the same result line a successful one would; the
//! inference backend screen instead runs the Model screen's activate-and-
//! confirm flow.

use crate::audio::{level_to_ratio, MicMonitor};
use crate::commands;
use crate::config::{ActivationMode, Config, OutputMethod, PasteKeys};
use crate::inference::BackendChoice;
use crate::overlay;
use crate::setup::{paste_keys_labels, Capabilities, OUTPUT_METHODS};
use crate::text_input::TextField;
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Gauge, Paragraph, Wrap};

/// Which of the model screen's two lists the arrow keys and Enter act on.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum ModelFocus {
    Catalog,
    Backend,
}

pub(super) struct ModelScreen {
    pub rows: Vec<commands::ModelRow>,
    pub selected: usize,
    pub focus: ModelFocus,
    /// Every backend preference with whether it can work here; only the
    /// usable ones are selectable (`usable_backends`).
    pub backends: Vec<BackendChoice>,
    pub backend_selected: usize,
    pub configured_backend: String,
}

impl ModelScreen {
    pub(super) fn new(config: &Config, backends: Vec<BackendChoice>) -> Self {
        let rows = commands::model_rows(config);
        let selected = rows.iter().position(|row| row.active).unwrap_or(0);
        let mut screen = Self {
            rows,
            selected,
            focus: ModelFocus::Catalog,
            backends,
            backend_selected: 0,
            configured_backend: config.model.preferred_backend.clone(),
        };
        let configured = screen
            .usable_backends()
            .position(|name| name == screen.configured_backend);
        screen.backend_selected = configured.unwrap_or(0);
        screen
    }

    fn usable_backends(&self) -> impl Iterator<Item = &'static str> + '_ {
        self.backends
            .iter()
            .filter(|choice| choice.unavailable.is_none())
            .map(|choice| choice.name)
    }

    pub(super) fn toggle_focus(&mut self) {
        self.focus = match self.focus {
            ModelFocus::Catalog => ModelFocus::Backend,
            ModelFocus::Backend => ModelFocus::Catalog,
        };
    }

    pub(super) fn move_selection(&mut self, delta: i32) {
        let (len, current) = match self.focus {
            ModelFocus::Catalog => (self.rows.len(), &mut self.selected),
            ModelFocus::Backend => (self.usable_backends().count(), &mut self.backend_selected),
        };
        if len == 0 {
            return;
        }
        *current = (*current as i32 + delta).clamp(0, len as i32 - 1) as usize;
    }

    pub(super) fn selected_id(&self) -> &'static str {
        self.rows[self.selected].id
    }

    pub(super) fn selected_backend(&self) -> Option<&'static str> {
        self.usable_backends().nth(self.backend_selected)
    }

    /// Rows the backend panel needs: one per usable backend, one per
    /// unavailable backend's reason, plus borders.
    pub(super) fn backend_widget_height(&self) -> u16 {
        self.backends.len() as u16 + 2
    }

    pub(super) fn backend_widget(&self, width: usize) -> Paragraph<'static> {
        let focused = self.focus == ModelFocus::Backend;
        let lines = backend_panel_lines(
            &self.backends,
            &self.configured_backend,
            focused.then_some(self.backend_selected),
            width,
        );
        Paragraph::new(lines).block(
            Block::default()
                .borders(Borders::ALL)
                .border_style(focus_border(focused))
                .title("Inference backend"),
        )
    }

    pub(super) fn list_widget(&self, width: usize, height: u16) -> Paragraph<'static> {
        let lines: Vec<Line> = self
            .rows
            .iter()
            .enumerate()
            .map(|(index, row)| {
                let highlighted = self.focus == ModelFocus::Catalog && index == self.selected;
                let marker = if highlighted { "> " } else { "  " };
                let status = if row.active {
                    "active"
                } else if row.installed {
                    "installed"
                } else {
                    "-"
                };
                // Family and license live in the detail panel below; the list
                // stays within an 80-column terminal and marks the language
                // coverage and whether the model can stream live partial text
                // (the "LIVE" tag), which a user needs to choose between
                // families.
                let live = if row.info.supports_streaming() {
                    "live"
                } else {
                    "-"
                };
                let text = format!(
                    "{marker}{live:<5}{:<31} {:<8} {:>8} {:<9} {:<13}",
                    row.id,
                    row.quant,
                    commands::human_size(row.size_bytes),
                    status,
                    row.info.language_summary(),
                );
                let mut spans = vec![Span::styled(text, selection_style(highlighted))];
                pad_selected_row(&mut spans, highlighted, width);
                Line::from(spans)
            })
            .collect();
        let visible_rows = usize::from(height.saturating_sub(2)).max(1);
        let scroll = self.selected.saturating_sub(visible_rows.saturating_sub(1)) as u16;
        Paragraph::new(lines)
            .block(
                Block::default()
                    .borders(Borders::ALL)
                    .border_style(focus_border(self.focus == ModelFocus::Catalog))
                    .title(
                        "Home \u{203a} Model (MODEL ID / QUANT / SIZE / STATUS / LIVE / LANGUAGES)",
                    ),
            )
            .scroll((scroll, 0))
    }

    /// The selected model's capabilities, so a choice can be made on more than
    /// the id: its family, language coverage, license, and how much audio it
    /// accepts in the pipeline's single buffer.
    pub(super) fn selected_detail_widget(&self) -> Paragraph<'static> {
        let row = &self.rows[self.selected];
        let label = Style::default().fg(Color::DarkGray);
        let heading = Line::from(vec![
            Span::styled("Family ", label),
            Span::raw(row.info.family_label()),
            Span::raw("   "),
            Span::styled("Languages ", label),
            Span::raw(row.info.language_summary()),
            Span::raw("   "),
            Span::styled("License ", label),
            Span::raw(row.license_spdx),
        ]);
        let note = Line::from(vec![
            Span::styled("Input limit ", label),
            Span::raw(long_form_note(row.info.long_form)),
        ]);
        let streaming = if row.info.supports_streaming() {
            "live text with a caption style while you speak; English only"
        } else {
            "no live text - the transcript appears after transcription"
        };
        let streaming_note = Line::from(vec![
            Span::styled("Streaming ", label),
            Span::raw(streaming),
        ]);
        Paragraph::new(vec![heading, note, streaming_note]).block(
            Block::default()
                .borders(Borders::ALL)
                .title("Selected model"),
        )
    }
}

/// One line describing a family's input-length behavior for the model screen's
/// detail panel, so a hard-cap or soft-window model does not surprise a user.
fn long_form_note(long_form: crate::catalog::LongForm) -> String {
    match long_form {
        crate::catalog::LongForm::Unbounded => {
            "none; the model windows or encodes internally".to_string()
        }
        crate::catalog::LongForm::SoftWindow { window_seconds } => {
            format!(
                "any length, but output can truncate past about {window_seconds}s (partial kept)"
            )
        }
        crate::catalog::LongForm::HardCap { cap_seconds } => {
            format!("{cap_seconds}s per call; longer audio is always chunked to stay within it")
        }
    }
}

fn focus_border(focused: bool) -> Style {
    if focused {
        Style::default().fg(Color::Cyan)
    } else {
        Style::default().fg(Color::DarkGray)
    }
}

/// The backend picker's rows, shared by the Model screen's backend panel and
/// the dedicated "Inference backend" settings screen so the two can never
/// disagree about which backends exist, which are usable, or which is active.
/// Only usable backends are selectable; every unavailable one stays listed
/// with its reason. `cursor` is the index *among usable backends* to
/// highlight, or `None` when the panel is not focused; the selected row is a
/// full-width highlight bar within `width` (the panel's inner width).
fn backend_panel_lines(
    backends: &[BackendChoice],
    configured: &str,
    cursor: Option<usize>,
    width: usize,
) -> Vec<Line<'static>> {
    let mut lines: Vec<Line> = backends
        .iter()
        .filter(|choice| choice.unavailable.is_none())
        .enumerate()
        .map(|(index, choice)| {
            let highlighted = cursor == Some(index);
            let marker = if highlighted { "> " } else { "  " };
            let status = if choice.name == configured {
                "active"
            } else {
                ""
            };
            let text = format!(
                "{marker}{:<53} {status}",
                crate::inference::backend_preference_label(choice.name)
            );
            let mut spans = vec![Span::styled(text, selection_style(highlighted))];
            pad_selected_row(&mut spans, highlighted, width);
            Line::from(spans)
        })
        .collect();
    lines.extend(backends.iter().filter_map(|choice| {
        choice.unavailable.as_ref().map(|reason| {
            let text = if choice.name == configured {
                format!("  {} (active) unavailable: {reason}", choice.name)
            } else {
                format!("  {} unavailable: {reason}", choice.name)
            };
            Line::from(Span::styled(text, Style::default().fg(Color::DarkGray)))
        })
    }));
    lines
}

/// The dedicated "Inference backend" settings screen, reached from its own
/// home Settings-panel row. It presents the same backend list as the Model
/// screen's backend panel (`inference::backend_choices`, shared verbatim
/// through `backend_panel_lines`) and runs the same activate-and-confirm flow
/// when a backend is chosen (`App::handle_inference_backend_key`).
pub(super) struct BackendScreen {
    pub backends: Vec<BackendChoice>,
    pub selected: usize,
}

impl BackendScreen {
    pub(super) fn new(config: &Config, backends: Vec<BackendChoice>) -> Self {
        let configured = &config.model.preferred_backend;
        let selected = backends
            .iter()
            .filter(|choice| choice.unavailable.is_none())
            .position(|choice| choice.name == configured)
            .unwrap_or(0);
        Self { backends, selected }
    }

    fn usable_backends(&self) -> impl Iterator<Item = &'static str> + '_ {
        self.backends
            .iter()
            .filter(|choice| choice.unavailable.is_none())
            .map(|choice| choice.name)
    }

    pub(super) fn move_selection(&mut self, delta: i32) {
        let len = self.usable_backends().count();
        if len == 0 {
            return;
        }
        self.selected = (self.selected as i32 + delta).clamp(0, len as i32 - 1) as usize;
    }

    pub(super) fn selected_backend(&self) -> Option<&'static str> {
        self.usable_backends().nth(self.selected)
    }

    pub(super) fn list_widget(&self, config: &Config, width: usize) -> Paragraph<'static> {
        let lines = backend_panel_lines(
            &self.backends,
            &config.model.preferred_backend,
            Some(self.selected),
            width,
        );
        Paragraph::new(lines).block(
            Block::default()
                .borders(Borders::ALL)
                .title("Home \u{203a} Inference backend"),
        )
    }
}

pub(super) const DAEMON_ACTIONS: [&str; 3] = ["Start", "Stop", "Restart"];

pub(super) struct DaemonScreen {
    pub selected: usize,
}

impl DaemonScreen {
    pub(super) fn new() -> Self {
        Self { selected: 0 }
    }

    pub(super) fn move_selection(&mut self, delta: i32) {
        let len = DAEMON_ACTIONS.len() as i32;
        let next = (self.selected as i32 + delta).clamp(0, len - 1) as usize;
        self.selected = next;
    }

    pub(super) fn selected_action(&self) -> &'static str {
        DAEMON_ACTIONS[self.selected]
    }

    pub(super) fn list_widget(&self, width: usize) -> Paragraph<'static> {
        let rows = DAEMON_ACTIONS.map(String::from);
        selection_list("Home \u{203a} Daemon", &rows, self.selected, width)
    }
}

pub(super) struct AutostartScreen {
    pub selected: usize,
    pub result: Option<Result<(), String>>,
}

pub(super) const AUTOSTART_LABELS: [&str; 2] = crate::config::STARTUP_LABELS;

impl AutostartScreen {
    pub(super) fn new(config: &Config) -> Self {
        Self {
            selected: usize::from(config.startup.autostart),
            result: None,
        }
    }

    pub(super) fn move_selection(&mut self, delta: i32) {
        let next = (self.selected as i32 + delta).clamp(0, 1) as usize;
        self.selected = next;
    }

    pub(super) fn list_widget(&self, width: usize) -> Paragraph<'static> {
        let rows = AUTOSTART_LABELS.map(String::from);
        selection_list("Home \u{203a} Startup", &rows, self.selected, width)
    }

    pub(super) fn result_line(&self) -> Line<'static> {
        result_line(&self.result, "Startup setting saved.")
    }
}

/// The Microphone settings screen: the detected input devices plus a live
/// input-level preview for whichever one is highlighted, so switching the
/// selection is audible feedback before it is applied. The preview stream and
/// its settle/error behavior live in `audio::MicMonitor`, shared verbatim
/// with the setup console's microphone step.
pub(super) struct MicrophoneScreen {
    pub options: Vec<(String, String)>,
    pub selected: usize,
    pub monitor: MicMonitor,
    pub result: Option<Result<(), String>>,
}

impl MicrophoneScreen {
    pub(super) fn new(capabilities: &Capabilities, config: &Config) -> Self {
        Self {
            options: capabilities.microphones.clone(),
            selected: capabilities.microphone_index(&config.audio.microphone),
            monitor: MicMonitor::new(),
            result: None,
        }
    }

    /// Opens the preview for the highlighted device; the dashboard calls
    /// this once when the screen opens and again after a deferred restart.
    pub(super) fn start_preview(&mut self) {
        if let Some((id, _)) = self.options.get(self.selected) {
            self.monitor.sync(self.selected, id);
        }
    }

    /// Per-frame: routes async stream errors and restarts a preview deferred
    /// by recent navigation once its settle delay has passed.
    pub(super) fn poll_preview(&mut self) {
        if self.monitor.poll() {
            self.start_preview();
        }
    }

    pub(super) fn move_selection(&mut self, delta: i32) {
        let len = self.options.len() as i32;
        if len == 0 {
            return;
        }
        let next = (self.selected as i32 + delta).clamp(0, len - 1) as usize;
        if next != self.selected {
            self.selected = next;
            self.monitor.defer_restart();
        }
    }

    pub(super) fn apply(&mut self, config: &mut Config) {
        if let Some((id, _)) = self.options.get(self.selected) {
            config.audio.microphone = id.clone();
        }
    }

    pub(super) fn list_widget(&self, width: usize) -> Paragraph<'static> {
        let rows: Vec<String> = self
            .options
            .iter()
            .map(|(_, label)| label.clone())
            .collect();
        selection_list("Home \u{203a} Microphone", &rows, self.selected, width)
    }

    pub(super) fn error(&self) -> Option<&str> {
        self.monitor.error()
    }

    pub(super) fn gauge(&self) -> Gauge<'static> {
        let ratio = level_to_ratio(self.monitor.level());
        let color = if ratio > 0.85 {
            Color::Red
        } else if ratio > 0.5 {
            Color::Yellow
        } else {
            Color::Green
        };
        Gauge::default()
            .block(Block::default().borders(Borders::ALL).title("Input level"))
            .gauge_style(Style::default().fg(color))
            .ratio(ratio)
    }

    pub(super) fn result_line(&self) -> Line<'static> {
        result_line(&self.result, "Saved - used for the next recording.")
    }
}

/// Activation mode: hold-to-talk vs press-once toggle.
pub(super) struct ActivationScreen {
    pub selected: usize,
    pub result: Option<Result<(), String>>,
}

impl ActivationScreen {
    pub(super) fn new(config: &Config) -> Self {
        Self {
            selected: usize::from(config.activation.mode == ActivationMode::Toggle),
            result: None,
        }
    }

    pub(super) fn move_selection(&mut self, delta: i32) {
        let next = (self.selected as i32 + delta).clamp(0, 1) as usize;
        self.selected = next;
    }

    pub(super) fn apply(&mut self, config: &mut Config) {
        config.activation.mode = if self.selected == 0 {
            ActivationMode::Hold
        } else {
            ActivationMode::Toggle
        };
    }

    pub(super) fn list_widget(&self, width: usize) -> Paragraph<'static> {
        let rows = crate::config::ACTIVATION_MODE_LABELS.map(String::from);
        selection_list("Home \u{203a} Activation", &rows, self.selected, width)
    }

    pub(super) fn result_line(&self) -> Line<'static> {
        result_line(&self.result, "Activation mode saved.")
    }
}

/// Translation: transcribe in the spoken language vs translate into English.
pub(super) struct TranslationScreen {
    pub selected: usize,
    pub result: Option<Result<(), String>>,
}

impl TranslationScreen {
    pub(super) fn new(config: &Config) -> Self {
        Self {
            selected: usize::from(config.transcription.translate),
            result: None,
        }
    }

    pub(super) fn move_selection(&mut self, delta: i32) {
        let next = (self.selected as i32 + delta).clamp(0, 1) as usize;
        self.selected = next;
    }

    pub(super) fn apply(&mut self, config: &mut Config) {
        config.transcription.translate = self.selected == 1;
    }

    pub(super) fn list_widget(&self, width: usize) -> Paragraph<'static> {
        let rows = crate::config::TRANSLATE_LABELS.map(String::from);
        selection_list("Home \u{203a} Translation", &rows, self.selected, width)
    }

    pub(super) fn result_line(&self) -> Line<'static> {
        result_line(&self.result, "Translation saved.")
    }
}

/// Media pause: leave playing media alone, or pause it while recording.
pub(super) struct MediaPauseScreen {
    pub selected: usize,
    pub result: Option<Result<(), String>>,
}

pub(super) const MEDIA_PAUSE_LABELS: [&str; 2] = [
    "Keep media playing while dictating",
    "Pause playing media while dictating, then resume it",
];

impl MediaPauseScreen {
    pub(super) fn new(config: &Config) -> Self {
        Self {
            selected: usize::from(config.audio.pause_media),
            result: None,
        }
    }

    pub(super) fn move_selection(&mut self, delta: i32) {
        let next = (self.selected as i32 + delta).clamp(0, 1) as usize;
        self.selected = next;
    }

    pub(super) fn apply(&mut self, config: &mut Config) {
        config.audio.pause_media = self.selected == 1;
    }

    pub(super) fn list_widget(&self, width: usize) -> Paragraph<'static> {
        let rows = MEDIA_PAUSE_LABELS.map(String::from);
        selection_list("Home \u{203a} Media pause", &rows, self.selected, width)
    }

    pub(super) fn result_line(&self) -> Line<'static> {
        result_line(&self.result, "Media pause setting saved.")
    }
}

/// The Shortcut screen: the binding the desktop reports and the two actions
/// that talk to the desktop's global-shortcuts portal. TongueTyped never
/// picks a key, so setting one always goes through the portal's native
/// "press your new shortcut" dialog; the test action only confirms that the
/// already-bound trigger reaches the app.
pub(super) struct ShortcutScreen {
    pub selected_action: usize,
    pub status: ShortcutStatus,
}

pub(super) enum ShortcutStatus {
    Idle,
    Testing,
    DialogOpen,
    Message { text: String, is_error: bool },
}

pub(super) const SHORTCUT_ACTIONS: [&str; 2] = [
    "Set shortcut via system dialog",
    "Test current shortcut - press it now",
];

impl ShortcutScreen {
    pub(super) fn new(_config: &Config) -> Self {
        Self {
            selected_action: 0,
            status: ShortcutStatus::Idle,
        }
    }

    pub(super) fn move_selection(&mut self, delta: i32) {
        let len = SHORTCUT_ACTIONS.len() as i32;
        let next = (self.selected_action as i32 + delta).clamp(0, len - 1) as usize;
        self.selected_action = next;
    }

    pub(super) fn body(&self, bound: &str, width: usize) -> Paragraph<'static> {
        let mut lines = vec![
            Line::from(format!("Currently bound: {bound}")),
            Line::from(
                "TongueTyped registers the action with your desktop and never picks a \
                        key - choose one in the system dialog.",
            ),
            Line::from(""),
        ];
        for (index, action) in SHORTCUT_ACTIONS.iter().enumerate() {
            let selected = index == self.selected_action;
            let marker = if selected { "> " } else { "  " };
            let mut spans = vec![Span::styled(
                format!("{marker}{action}"),
                selection_style(selected),
            )];
            pad_selected_row(&mut spans, selected, width);
            lines.push(Line::from(spans));
        }
        lines.push(Line::from(""));
        lines.push(self.status_line());
        Paragraph::new(lines).block(
            Block::default()
                .borders(Borders::ALL)
                .title("Home \u{203a} Shortcut"),
        )
    }

    pub(super) fn status_line(&self) -> Line<'static> {
        match &self.status {
            ShortcutStatus::Idle => Line::from("Enter runs the selected action."),
            ShortcutStatus::Testing => Line::from(Span::styled(
                "Waiting for you to press the shortcut...",
                Style::default().fg(Color::Yellow),
            )),
            ShortcutStatus::DialogOpen => Line::from(Span::styled(
                "A shortcut settings window is open - set or change your shortcut there, \
                 then close it.",
                Style::default().fg(Color::Yellow),
            )),
            ShortcutStatus::Message { text, is_error } => Line::from(Span::styled(
                text.clone(),
                Style::default().fg(if *is_error { Color::Red } else { Color::Green }),
            )),
        }
    }

    /// The key legend, aware of the in-flight state: while the desktop's
    /// settings window is open, Enter keeps the current binding and Ctrl+R
    /// reopens the dialog, so a window closed with no change is never a dead
    /// end. While the portal test runs only back is honored, so the legend
    /// names only that.
    pub(super) fn footer(&self) -> &'static str {
        match self.status {
            ShortcutStatus::DialogOpen => "Enter keep current   Ctrl+R reopen   Esc/\u{232b} back",
            ShortcutStatus::Testing => "Esc/\u{232b} back",
            _ => "↑/↓ row   Enter run   Ctrl+R dialog   Esc/\u{232b} back",
        }
    }
}

/// Transcript output: the four methods, then - for the methods that insert
/// text - Enter-after and, for `paste`, the paste keys. Like the Overlay
/// screen, the option rows change only on `←`/`→`; `Enter` only saves.
pub(super) struct OutputScreen {
    pub labels: Vec<String>,
    pub selected: usize,
    pub result: Option<Result<(), String>>,
    /// `Capabilities::output_method_problem` for each of `OUTPUT_METHODS`.
    problems: [Option<&'static str>; OUTPUT_METHODS.len()],
}

impl OutputScreen {
    const SUBMIT_ROW: usize = OUTPUT_METHODS.len();
    const PASTE_KEYS_ROW: usize = OUTPUT_METHODS.len() + 1;

    pub(super) fn new(capabilities: &Capabilities, config: &Config) -> Self {
        Self {
            labels: capabilities.output_labels(),
            selected: capabilities.output_index(config.output.method),
            result: None,
            problems: OUTPUT_METHODS.map(|method| capabilities.output_method_problem(method)),
        }
    }

    fn row_count(config: &Config) -> usize {
        match config.output.method {
            OutputMethod::Paste => Self::PASTE_KEYS_ROW + 1,
            OutputMethod::Type => Self::SUBMIT_ROW + 1,
            OutputMethod::None | OutputMethod::Clipboard => OUTPUT_METHODS.len(),
        }
    }

    pub(super) fn move_selection(&mut self, delta: i32, config: &Config) {
        let len = Self::row_count(config) as i32;
        self.selected = (self.selected as i32 + delta).clamp(0, len - 1) as usize;
    }

    /// Steps the highlighted option row: "after text" flips and the paste keys
    /// step both ways, clamped at the ends (the `←`/`→` step the captain asked
    /// for, replacing `Enter`'s forward-only cycle). Method rows have no inline
    /// value, so this returns `false` and the caller moves the highlight
    /// instead.
    pub(super) fn step(&mut self, delta: i32, config: &mut Config) -> bool {
        match self.selected {
            Self::SUBMIT_ROW => {
                config.output.auto_submit = !config.output.auto_submit;
                true
            }
            Self::PASTE_KEYS_ROW => {
                let index = PasteKeys::ALL
                    .iter()
                    .position(|keys| *keys == config.output.paste_keys)
                    .unwrap_or(0);
                let len = PasteKeys::ALL.len() as i32;
                let next = (index as i32 + delta).clamp(0, len - 1) as usize;
                config.output.paste_keys = PasteKeys::ALL[next];
                true
            }
            _ => false,
        }
    }

    /// Applies the highlighted row. A method can only be applied while it
    /// works here; choosing one that cannot leaves the configuration
    /// untouched and returns the refusal for the result line, so the screen
    /// explains what to install instead of silently saving a method that can
    /// never deliver a transcript. Leaving `type`/`paste` also clears
    /// Enter-after, which only those methods accept. The option rows have no
    /// `Enter` action: their value is stepped with `←`/`→`, so `Enter` only
    /// saves, matching the Overlay screen where `Enter` only commits.
    pub(super) fn apply(&mut self, config: &mut Config) -> Result<(), String> {
        if let Some(method) = OUTPUT_METHODS.get(self.selected).copied() {
            if self.problems[self.selected].is_some() {
                // The warning line just above names what to install; this
                // line only has to say the choice was refused.
                return Err("Cannot apply: a required helper is not installed.".to_string());
            }
            config.output.method = method;
            if !method.inserts_text() {
                config.output.typing_backend = "auto".to_string();
                config.output.auto_submit = false;
            }
        }
        Ok(())
    }

    /// The method rows, the saved one marked `(current)` since the cursor
    /// can move away from it, then that method's options indented beneath.
    pub(super) fn rows(&self, config: &Config) -> Vec<String> {
        let mut rows: Vec<String> = self
            .labels
            .iter()
            .zip(OUTPUT_METHODS)
            .map(|(label, method)| {
                if method == config.output.method {
                    format!("{label} (current)")
                } else {
                    label.clone()
                }
            })
            .collect();
        if config.output.method.inserts_text() {
            rows.push(format!(
                "  {:<14}{}",
                "After text",
                if config.output.auto_submit {
                    "[x] press Enter"
                } else {
                    "[ ] nothing"
                }
            ));
        }
        if config.output.method == OutputMethod::Paste {
            let keys = &paste_keys_labels()[PasteKeys::ALL
                .iter()
                .position(|keys| *keys == config.output.paste_keys)
                .unwrap_or(0)];
            rows.push(format!("  {:<14}{keys}", "Paste keys"));
        }
        rows
    }

    pub(super) fn list_widget(&self, config: &Config, width: usize) -> Paragraph<'static> {
        let rows = self.rows(config);
        let keys = paste_keys_labels();
        let key_options: Vec<&str> = keys.iter().map(String::as_str).collect();
        let key_index = PasteKeys::ALL
            .iter()
            .position(|keys| *keys == config.output.paste_keys)
            .unwrap_or(0);
        let key_value = key_options.get(key_index).copied().unwrap_or("");
        let lines: Vec<Line> = rows
            .iter()
            .enumerate()
            .map(|(index, row)| {
                let is_selected = index == self.selected;
                let marker = if is_selected { "> " } else { "  " };
                let mut spans = Vec::new();
                if index == Self::PASTE_KEYS_ROW {
                    spans.push(Span::styled(
                        format!("{marker}  {:<14}", "Paste keys"),
                        selection_style(is_selected),
                    ));
                    spans.extend(stepper_spans(&key_options, key_value, is_selected));
                } else {
                    spans.push(Span::styled(
                        format!("{marker}{row}"),
                        selection_style(is_selected),
                    ));
                }
                pad_selected_row(&mut spans, is_selected, width);
                Line::from(spans)
            })
            .collect();
        list_panel("Home \u{203a} Transcript output", lines)
    }

    /// The install warning for the highlighted method when it cannot work
    /// here, or an empty line - the same slot renders either way, so the
    /// layout does not shift.
    pub(super) fn warning_line(&self) -> Line<'static> {
        typing_warning_line(self.problems.get(self.selected).copied().flatten())
    }

    pub(super) fn result_line(&self) -> Line<'static> {
        result_line(&self.result, "Transcript output saved.")
    }
}

/// Which typing helper types transcripts into the focused application.
pub(super) struct TypingBackendScreen {
    pub values: Vec<String>,
    pub selected: usize,
    pub result: Option<Result<(), String>>,
    warning: Option<&'static str>,
}

impl TypingBackendScreen {
    pub(super) fn new(capabilities: &Capabilities, config: &Config) -> Self {
        let values = capabilities.typing_backend_values();
        let selected = values
            .iter()
            .position(|value| value == &config.output.typing_backend)
            .unwrap_or(0);
        Self {
            values,
            selected,
            result: None,
            warning: capabilities.typing_helper_warning(),
        }
    }

    pub(super) fn move_selection(&mut self, delta: i32) {
        let len = self.values.len() as i32;
        if len == 0 {
            return;
        }
        let next = (self.selected as i32 + delta).clamp(0, len - 1) as usize;
        self.selected = next;
    }

    pub(super) fn apply(&mut self, config: &mut Config) {
        if let Some(value) = self.values.get(self.selected) {
            config.output.typing_backend = value.clone();
        }
    }

    pub(super) fn list_widget(&self, width: usize) -> Paragraph<'static> {
        selection_list(
            "Home \u{203a} Typing backend",
            &self.values,
            self.selected,
            width,
        )
    }

    /// The install warning shown under the list when the helper list is
    /// empty, or an empty line.
    pub(super) fn warning_line(&self) -> Line<'static> {
        typing_warning_line(self.warning)
    }

    pub(super) fn result_line(&self) -> Line<'static> {
        result_line(&self.result, "Typing backend saved.")
    }
}

/// One selectable row of the Overlay screen. The row list is derived from the
/// pending values, so the look and shape rows appear only where they apply.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum OverlayRow {
    Enabled,
    Position,
    Style,
    Look,
    Shape,
}

/// The Overlay screen keeps the enabled/position/style/look choices on one
/// screen. Every row is a stepper: `←`/`→` moves the highlighted value in
/// either direction (clamped at the ends), the preview follows the pending
/// values before anything is saved, and `Enter` - or backing out - commits
/// them. The configuration rows appear only while the pending enable value is
/// on: the look row follows the chosen style (round styles: plain or waveform;
/// wide styles: waveform or live text; the box: live text only; the blob:
/// none), and the waveform-shape row appears only for a wide style whose look
/// is the waveform, so the screen never offers a nonsensical combination.
pub(super) struct OverlayScreen {
    /// Row cursor into the current row list (see `row_kinds`).
    pub selected: usize,
    /// The pending values the cursor edits, committed into `config` by
    /// `commit`. The preview reads these, so it updates on every step.
    pub enabled: bool,
    pub position: String,
    pub style: String,
    pub look: String,
    pub waveform_shape: String,
    pub result: Option<Result<(), String>>,
}

impl OverlayScreen {
    pub(super) fn new(config: &Config) -> Self {
        Self {
            selected: 0,
            enabled: config.overlay.enabled,
            position: config.overlay.position.clone(),
            style: config.overlay.style.clone(),
            look: config.overlay.look.clone(),
            waveform_shape: config.overlay.waveform_shape.clone(),
            result: None,
        }
    }

    /// The rows this screen shows, in order, given the pending values. The
    /// look row follows the pending style (skipped for a style with no options,
    /// the blob) and the shape row follows the pending style and look.
    fn row_kinds(&self) -> Vec<OverlayRow> {
        let mut rows = vec![OverlayRow::Enabled];
        if !self.enabled {
            return rows;
        }
        rows.push(OverlayRow::Position);
        rows.push(OverlayRow::Style);
        if !overlay::style_look_options(&self.style).is_empty() {
            rows.push(OverlayRow::Look);
        }
        if overlay::shows_shape_choice(&self.style, &self.look) {
            rows.push(OverlayRow::Shape);
        }
        rows
    }

    pub(super) fn move_selection(&mut self, delta: i32) {
        let len = self.row_kinds().len() as i32;
        self.selected = (self.selected as i32 + delta).clamp(0, len - 1) as usize;
    }

    /// Keeps the pending look valid for the pending style: a value the style
    /// does not offer snaps to the style's first option. A style with no
    /// options (the blob) is left alone.
    fn clamp_pending_look(&mut self) {
        let options = overlay::style_look_options(&self.style);
        let Some((first, _)) = options.first() else {
            return;
        };
        if !options.iter().any(|(value, _)| *value == self.look) {
            self.look = (*first).to_string();
        }
    }

    /// Steps the highlighted row's pending value. The enable row clamps between
    /// Disabled and Enabled; the position, look and shape rows clamp at the ends
    /// of their lists; the style row also keeps the look valid for the new
    /// style.
    pub(super) fn step(&mut self, delta: i32) {
        let row = self
            .row_kinds()
            .get(self.selected)
            .copied()
            .unwrap_or(OverlayRow::Enabled);
        match row {
            OverlayRow::Enabled => {
                self.enabled = (usize::from(self.enabled) as i32 + delta).clamp(0, 1) == 1;
            }
            OverlayRow::Position => {
                self.position = step_value(&overlay::POSITION_VALUES, &self.position, delta)
            }
            OverlayRow::Style => {
                self.style = step_value(&overlay::STYLE_VALUES, &self.style, delta);
                self.clamp_pending_look();
            }
            OverlayRow::Look => {
                let options: Vec<&str> = overlay::style_look_options(&self.style)
                    .iter()
                    .map(|(value, _)| *value)
                    .collect();
                self.look = step_value(&options, &self.look, delta);
                self.clamp_pending_look();
            }
            OverlayRow::Shape => {
                self.waveform_shape = step_value(&overlay::SHAPE_VALUES, &self.waveform_shape, delta);
            }
        }
    }

    /// Writes the pending values into the config, clamping the look to the
    /// style so the stored value stays sane.
    pub(super) fn commit(&mut self, config: &mut Config) {
        config.overlay.enabled = self.enabled;
        config.overlay.position = self.position.clone();
        config.overlay.style = self.style.clone();
        self.clamp_pending_look();
        config.overlay.look = self.look.clone();
        config.overlay.waveform_shape = self.waveform_shape.clone();
    }

    /// The row labels in display order: the enable stepper, then the
    /// configuration steppers that apply to the pending values.
    fn row_labels(&self) -> Vec<&'static str> {
        self.row_kinds()
            .iter()
            .map(|row| match row {
                OverlayRow::Enabled => "Overlay",
                OverlayRow::Position => "Position",
                OverlayRow::Style => "Style",
                OverlayRow::Look => "Look",
                OverlayRow::Shape => "Waveform shape",
            })
            .collect()
    }

    pub(super) fn list_widget(&self, width: usize) -> Paragraph<'static> {
        let rows = self.row_kinds();
        // The look labels for the pending style, so the stepper's value and its
        // option count line up with what the overlay actually offers.
        let look_labels: Vec<&'static str> = overlay::style_look_options(&self.style)
            .iter()
            .map(|(_, label)| *label)
            .collect();
        let lines: Vec<Line> = self
            .row_labels()
            .into_iter()
            .enumerate()
            .map(|(index, label)| {
                let selected = index == self.selected;
                let marker = if selected { "> " } else { "  " };
                let mut spans = vec![Span::styled(
                    format!("{marker}{label:<16}"),
                    selection_style(selected),
                )];
                let (options, value): (&[&str], &str) = match rows[index] {
                    OverlayRow::Enabled => (
                        &["Disabled", "Enabled"],
                        if self.enabled { "Enabled" } else { "Disabled" },
                    ),
                    OverlayRow::Position => (&overlay::POSITION_VALUES, self.position.as_str()),
                    OverlayRow::Style => (&overlay::STYLE_LABELS, overlay_style_label(&self.style)),
                    OverlayRow::Look => (&look_labels, overlay::look_label(&self.look)),
                    OverlayRow::Shape => (
                        &overlay::SHAPE_LABELS,
                        overlay::shape_label(&self.waveform_shape),
                    ),
                };
                spans.extend(stepper_spans(options, value, selected));
                pad_selected_row(&mut spans, selected, width);
                Line::from(spans)
            })
            .collect();
        Paragraph::new(lines).block(
            Block::default()
                .borders(Borders::ALL)
                .title("Home \u{203a} Overlay"),
        )
    }

    pub(super) fn result_line(&self) -> Line<'static> {
        result_line(&self.result, "Overlay saved.")
    }
}

/// A screen of labeled single-line text fields with one selection cursor,
/// shared by the "History retention" (two numeric fields), "Transcript
/// folder" (one path field) and "Decoding" (a prompt and five numbers)
/// screens. Editing keys act on the selected field; Escape returns home;
/// Enter runs the screen's own `apply`.
pub(super) struct TextFieldsScreen {
    pub title: &'static str,
    pub hint: &'static str,
    pub fields: Vec<(&'static str, TextField)>,
    /// Dim text shown in place of an empty field, indexed like `fields`;
    /// missing entries show nothing.
    pub placeholders: Vec<String>,
    pub selected: usize,
    pub result: Option<Result<(), String>>,
    apply: fn(&TextFieldsScreen, &mut Config) -> Result<(), String>,
}

impl TextFieldsScreen {
    /// The "History retention" screen: a maximum entry count and a maximum
    /// age in days, either of which may be `0` for no limit.
    pub(super) fn history_retention(config: &Config) -> Self {
        Self {
            title: "History retention",
            hint: "0 means no limit for that field.",
            fields: vec![
                (
                    "Maximum entries",
                    TextField::new(config.history.max_entries.to_string()),
                ),
                (
                    "Maximum age (days)",
                    TextField::new(config.history.max_age_days.to_string()),
                ),
            ],
            placeholders: Vec::new(),
            selected: 0,
            result: None,
            apply: apply_history_retention,
        }
    }

    /// The "Transcript folder" screen: the folder each finished transcript is
    /// also written into as a plain-text file. Empty disables the export.
    pub(super) fn transcript_folder(config: &Config) -> Self {
        Self {
            title: "Transcript folder",
            hint: "Leave empty to disable. A leading ~ means your home directory.",
            fields: vec![(
                "Folder",
                TextField::new(config.history.transcript_folder.clone()),
            )],
            placeholders: Vec::new(),
            selected: 0,
            result: None,
            apply: apply_transcript_folder,
        }
    }

    /// The "Decoding" screen: the vocabulary prompt and the Whisper decode
    /// knobs in `config::DECODE_KNOBS`, where an empty number keeps the
    /// model default shown beside it.
    pub(super) fn decoding(config: &Config) -> Self {
        let transcription = &config.transcription;
        let mut fields = vec![(
            crate::config::INITIAL_PROMPT_LABEL,
            TextField::new(transcription.initial_prompt.clone()),
        )];
        fields.extend(
            crate::config::DECODE_KNOBS
                .iter()
                .zip(transcription.decoding_form_values())
                .map(|(knob, value)| (knob.label, TextField::new(value))),
        );
        Self {
            title: "Decoding",
            hint: crate::config::DECODING_HINT,
            fields,
            placeholders: crate::config::TranscriptionConfig::decoding_form_placeholders(),
            selected: 0,
            result: None,
            apply: apply_decoding,
        }
    }

    pub(super) fn move_selection(&mut self, delta: i32) {
        let len = self.fields.len() as i32;
        if len == 0 {
            return;
        }
        self.selected = (self.selected as i32 + delta).clamp(0, len - 1) as usize;
    }

    pub(super) fn selected_field_mut(&mut self) -> &mut TextField {
        &mut self.fields[self.selected].1
    }

    pub(super) fn apply(&self, config: &mut Config) -> Result<(), String> {
        (self.apply)(self, config)
    }

    pub(super) fn list_widget(&self, width: usize) -> Paragraph<'static> {
        let label_width = self
            .fields
            .iter()
            .map(|(label, _)| label.chars().count())
            .max()
            .unwrap_or(0);
        let lines: Vec<Line> = self
            .fields
            .iter()
            .enumerate()
            .map(|(index, (label, field))| {
                let selected = index == self.selected;
                let marker = if selected { "> " } else { "  " };
                let mut spans = vec![Span::styled(
                    format!("{marker}{label:<label_width$}  "),
                    selection_style(selected),
                )];
                match self
                    .placeholders
                    .get(index)
                    .filter(|_| field.value().is_empty())
                {
                    Some(placeholder) => spans.extend(placeholder_spans(placeholder, selected)),
                    None => spans.extend(field_value_spans(field, selected)),
                }
                pad_selected_row(&mut spans, selected, width);
                Line::from(spans).style(selection_style(selected))
            })
            .collect();
        Paragraph::new(lines).wrap(Wrap { trim: false }).block(
            Block::default()
                .borders(Borders::ALL)
                .title(format!("Home \u{203a} {}", self.title)),
        )
    }

    pub(super) fn result_line(&self, success: &str) -> Line<'static> {
        result_line(&self.result, success)
    }
}

/// The "Text processing" screen: the replacement table, the two opt-in passes
/// (spoken punctuation and English-only filler removal), and the external
/// cleanup command with its timeout. The three text rows are edited in place;
/// Enter on a toggle row flips it. Every row is applied together, so a bad
/// replacement entry or timeout saves nothing.
pub(super) struct TextProcessingScreen {
    pub replacements: TextField,
    pub spoken_punctuation: bool,
    pub remove_fillers: bool,
    pub command: TextField,
    pub timeout: TextField,
    pub selected: usize,
    pub result: Option<Result<(), String>>,
}

impl TextProcessingScreen {
    pub(super) const ROW_REPLACEMENTS: usize = 0;
    pub(super) const ROW_SPOKEN: usize = 1;
    pub(super) const ROW_FILLERS: usize = 2;
    pub(super) const ROW_COMMAND: usize = 3;
    pub(super) const ROW_TIMEOUT: usize = 4;
    pub(super) const ROWS: usize = 5;
    const REPLACEMENTS_PLACEHOLDER: &'static str =
        "vox type => Voxtype; tee vee => TV (';', '>', and '\\' escaped with '\\')";

    pub(super) fn new(config: &Config) -> Self {
        Self {
            replacements: TextField::new(config.text.replacements_form_value()),
            spoken_punctuation: config.text.spoken_punctuation,
            remove_fillers: config.text.remove_fillers,
            command: TextField::new(config.text.post_process_command.clone()),
            timeout: TextField::new(config.text.post_process_timeout_seconds.to_string()),
            selected: 0,
            result: None,
        }
    }

    pub(super) fn move_selection(&mut self, delta: i32) {
        self.selected = (self.selected as i32 + delta).clamp(0, (Self::ROWS - 1) as i32) as usize;
    }

    pub(super) fn selected_text_field_mut(&mut self) -> Option<&mut TextField> {
        match self.selected {
            Self::ROW_REPLACEMENTS => Some(&mut self.replacements),
            Self::ROW_COMMAND => Some(&mut self.command),
            Self::ROW_TIMEOUT => Some(&mut self.timeout),
            _ => None,
        }
    }

    /// Flips the selected toggle row; a text row is left alone.
    pub(super) fn toggle_selected(&mut self) {
        match self.selected {
            Self::ROW_SPOKEN => self.spoken_punctuation = !self.spoken_punctuation,
            Self::ROW_FILLERS => self.remove_fillers = !self.remove_fillers,
            _ => {}
        }
    }

    pub(super) fn apply(&self, config: &mut Config) -> Result<(), String> {
        let mut text = config.text.clone();
        text.apply_replacements_form(self.replacements.value())?;
        text.spoken_punctuation = self.spoken_punctuation;
        text.remove_fillers = self.remove_fillers;
        text.post_process_command = self.command.value().trim().to_string();
        let timeout = self.timeout.value().trim();
        let timeout: u64 = timeout
            .parse()
            .map_err(|_| "Timeout must be a whole number of seconds.".to_string())?;
        if timeout == 0 {
            return Err("Timeout must be at least 1 second.".to_string());
        }
        text.post_process_timeout_seconds = timeout;
        text.validate().map_err(|error| error.to_string())?;
        config.text = text;
        Ok(())
    }

    pub(super) fn list_widget(&self, width: usize) -> Paragraph<'static> {
        let rows: [(&str, bool, Vec<Span<'static>>); Self::ROWS] = [
            (
                "Replacements",
                self.selected == Self::ROW_REPLACEMENTS,
                text_row_value(
                    &self.replacements,
                    self.selected == Self::ROW_REPLACEMENTS,
                    Self::REPLACEMENTS_PLACEHOLDER,
                ),
            ),
            (
                "Spoken punctuation",
                self.selected == Self::ROW_SPOKEN,
                toggle_row_value(self.spoken_punctuation, self.selected == Self::ROW_SPOKEN),
            ),
            (
                "Remove filler words",
                self.selected == Self::ROW_FILLERS,
                toggle_row_value(self.remove_fillers, self.selected == Self::ROW_FILLERS),
            ),
            (
                "Cleanup command",
                self.selected == Self::ROW_COMMAND,
                text_row_value(
                    &self.command,
                    self.selected == Self::ROW_COMMAND,
                    "empty disables it",
                ),
            ),
            (
                "Timeout (seconds)",
                self.selected == Self::ROW_TIMEOUT,
                text_row_value(&self.timeout, self.selected == Self::ROW_TIMEOUT, ""),
            ),
        ];
        let lines: Vec<Line> = rows
            .iter()
            .map(|(label, selected, value)| {
                let marker = if *selected { "> " } else { "  " };
                let mut spans = vec![Span::styled(
                    format!("{marker}{label:<20}"),
                    selection_style(*selected),
                )];
                spans.extend(value.iter().cloned());
                pad_selected_row(&mut spans, *selected, width);
                Line::from(spans).style(selection_style(*selected))
            })
            .collect();
        Paragraph::new(lines).wrap(Wrap { trim: false }).block(
            Block::default()
                .borders(Borders::ALL)
                .title("Home \u{203a} Text processing"),
        )
    }

    pub(super) fn result_line(&self) -> Line<'static> {
        result_line(&self.result, "Text processing saved.")
    }
}

/// The value half of a `TextProcessingScreen` text row: the field with its
/// cursor when selected, or a dim placeholder when empty and unselected.
fn text_row_value(field: &TextField, selected: bool, placeholder: &str) -> Vec<Span<'static>> {
    if field.value().is_empty() && !placeholder.is_empty() && !selected {
        vec![Span::styled(
            placeholder.to_string(),
            Style::default().fg(Color::DarkGray),
        )]
    } else {
        field_value_spans(field, selected)
    }
}

/// The value half of a `TextProcessingScreen` toggle row. On the selected row
/// the value uses the highlight-bar style so the bar stays uniform.
fn toggle_row_value(enabled: bool, selected: bool) -> Vec<Span<'static>> {
    let text = if enabled { "[x] on" } else { "[ ] off" };
    let style = if selected {
        selection_style(true)
    } else {
        Style::default().fg(if enabled {
            Color::Green
        } else {
            Color::DarkGray
        })
    };
    vec![Span::styled(text.to_string(), style)]
}

/// The value half of one `TextFieldsScreen` row. The selected field draws a
/// reversed cell at the cursor so the editing position is visible, and the
/// value rides on the highlight bar so the bar is not punched through; an
/// unselected field is plain text.
fn field_value_spans(field: &TextField, selected: bool) -> Vec<Span<'static>> {
    if !selected {
        return vec![Span::raw(field.value().to_string())];
    }
    let (before, cursor_char, after) = field.split_at_cursor();
    let bar = selection_style(true);
    vec![
        Span::styled(before, bar),
        Span::styled(
            cursor_char.map(String::from).unwrap_or_else(|| " ".into()),
            bar.add_modifier(Modifier::REVERSED),
        ),
        Span::styled(after, bar),
    ]
}

/// The dim hint drawn in place of an empty text field. When the field is
/// selected its first character carries the cursor, so the hint starts in
/// the same column on every row, and the hint rides on the highlight bar.
fn placeholder_spans(text: &str, selected: bool) -> Vec<Span<'static>> {
    let mut chars = text.chars();
    match chars.next().filter(|_| selected) {
        Some(first) => vec![
            Span::styled(
                first.to_string(),
                selection_style(true).add_modifier(Modifier::REVERSED),
            ),
            Span::styled(chars.as_str().to_string(), selection_style(true)),
        ],
        None => vec![Span::styled(
            text.to_string(),
            Style::default().fg(Color::DarkGray),
        )],
    }
}

fn parse_retention_number(raw: &str) -> Result<u64, String> {
    let trimmed = raw.trim();
    if trimmed.is_empty() {
        return Err("Enter a whole number (0 for no limit).".to_string());
    }
    trimmed
        .parse::<u64>()
        .map_err(|_| "Enter a whole number (0 for no limit).".to_string())
}

fn apply_history_retention(screen: &TextFieldsScreen, config: &mut Config) -> Result<(), String> {
    let entries = parse_retention_number(screen.fields[0].1.value())?;
    let age = parse_retention_number(screen.fields[1].1.value())?;
    if age > i64::MAX as u64 / 86_400 {
        return Err("Maximum age is too large.".to_string());
    }
    config.history.max_entries = entries;
    config.history.max_age_days = age;
    Ok(())
}

fn apply_transcript_folder(screen: &TextFieldsScreen, config: &mut Config) -> Result<(), String> {
    let raw = screen.fields[0].1.value().trim();
    if raw.is_empty() {
        config.history.transcript_folder = String::new();
        return Ok(());
    }
    let expanded = crate::history::expand_home(raw);
    if !expanded.is_absolute() {
        return Err("Enter an absolute folder path, or one starting with ~/.".to_string());
    }
    config.history.transcript_folder = expanded.to_string_lossy().into_owned();
    Ok(())
}

fn apply_decoding(screen: &TextFieldsScreen, config: &mut Config) -> Result<(), String> {
    let (prompt, knobs) = screen
        .fields
        .split_first()
        .expect("the decoding screen has a prompt field");
    let knobs: [&str; crate::config::DECODE_KNOBS.len()] =
        std::array::from_fn(|index| knobs[index].1.value());
    config
        .transcription
        .apply_decoding_form(prompt.1.value(), &knobs)
}

/// A one-line summary of the overlay's look for the home-screen Settings row
/// (for example "waveform, bars" or "live text"), derived from the same
/// per-style options the Overlay screen shows.
pub(super) fn overlay_look_summary(config: &Config) -> String {
    let style = &config.overlay.style;
    if overlay::style_look_options(style).is_empty() {
        return "no options".to_string();
    }
    let look = overlay::look_label(&config.overlay.look).to_lowercase();
    if overlay::shows_shape_choice(style, &config.overlay.look) {
        format!(
            "{look}, {}",
            overlay::shape_label(&config.overlay.waveform_shape).to_lowercase()
        )
    } else {
        look
    }
}

pub(super) fn overlay_style_label(style: &str) -> &'static str {
    overlay::STYLE_VALUES
        .iter()
        .position(|value| *value == style)
        .map(|index| overlay::STYLE_LABELS[index])
        .unwrap_or(overlay::STYLE_LABELS[0])
}

/// Steps `current` within `values` by `delta`, clamping at the ends so a stray
/// key can never jump across the list. Returns `current` unchanged when it is
/// not in `values`.
fn step_value(values: &[&str], current: &str, delta: i32) -> String {
    let Some(index) = values.iter().position(|value| *value == current) else {
        return current.to_string();
    };
    let next = (index as i32 + delta).clamp(0, values.len() as i32 - 1) as usize;
    values[next].to_string()
}

pub(super) fn selection_style(selected: bool) -> Style {
    if selected {
        Style::default()
            .fg(Color::Black)
            .bg(Color::Cyan)
            .add_modifier(Modifier::BOLD)
    } else {
        Style::default()
    }
}

/// Pads a selected row so its highlight bar fills the panel's inner width; a
/// no-op on an unselected row.
pub(super) fn pad_selected_row(spans: &mut Vec<Span<'static>>, selected: bool, width: usize) {
    if !selected {
        return;
    }
    let used: usize = spans.iter().map(|span| span.content.chars().count()).sum();
    if width > used {
        spans.push(Span::styled(
            " ".repeat(width - used),
            selection_style(true),
        ));
    }
}

/// The value half of a stepper row: `‹ value ›` with the arrow at each end
/// dimmed when there is nothing further that way, plus an `n / N` counter so
/// both directions are discoverable. On the selected row the pieces use the
/// highlight-bar style so the bar stays uniform.
fn stepper_spans(options: &[&str], value: &str, selected: bool) -> Vec<Span<'static>> {
    let index = options
        .iter()
        .position(|option| *option == value)
        .unwrap_or(0);
    let last = options.len().saturating_sub(1);
    let (left, right) = if index == 0 {
        ("   ", "\u{203a} ")
    } else if index == last {
        ("\u{2039} ", "   ")
    } else {
        ("\u{2039} ", "\u{203a} ")
    };
    let (arrow, value_style) = if selected {
        (
            selection_style(true),
            selection_style(true).add_modifier(Modifier::BOLD),
        )
    } else {
        (
            Style::default().fg(Color::DarkGray),
            Style::default().fg(Color::Cyan),
        )
    };
    vec![
        Span::styled(left.to_string(), arrow),
        Span::styled(value.to_string(), value_style),
        Span::styled(format!(" {right} {}/{}", index + 1, options.len()), arrow),
    ]
}

/// The dashboard's standard selectable list panel: every row is
/// `"> "`/`"  "`-prefixed and the selected row is a full-width highlight bar.
/// `width` is the panel's inner width (its area minus the two border columns).
pub(super) fn selection_list(
    title: &str,
    rows: &[String],
    selected: usize,
    width: usize,
) -> Paragraph<'static> {
    let lines: Vec<Line> = rows
        .iter()
        .enumerate()
        .map(|(index, row)| {
            let is_selected = index == selected;
            let marker = if is_selected { "> " } else { "  " };
            let mut spans = vec![Span::styled(
                format!("{marker}{row}"),
                selection_style(is_selected),
            )];
            pad_selected_row(&mut spans, is_selected, width);
            Line::from(spans)
        })
        .collect();
    list_panel(title, lines)
}

/// Wraps ready-made rows in the dashboard's standard bordered list panel.
fn list_panel(title: &str, lines: Vec<Line<'static>>) -> Paragraph<'static> {
    Paragraph::new(lines).block(
        Block::default()
            .borders(Borders::ALL)
            .title(title.to_string()),
    )
}

/// One-line feedback for a settings screen that saved, or failed to save.
fn result_line(result: &Option<Result<(), String>>, success: &str) -> Line<'static> {
    match result {
        None => Line::from(""),
        Some(Ok(())) => Line::from(Span::styled(
            success.to_string(),
            Style::default().fg(Color::Green),
        )),
        Some(Err(message)) => Line::from(Span::styled(
            message.clone(),
            Style::default().fg(Color::Red),
        )),
    }
}

/// The yellow install warning the two typing-related settings screens show
/// when no helper works - or an empty line, so the screen's layout slot
/// never shifts.
fn typing_warning_line(warning: Option<&'static str>) -> Line<'static> {
    match warning {
        Some(warning) => Line::from(Span::styled(warning, Style::default().fg(Color::Yellow))),
        None => Line::from(""),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn overlay_position_steps_both_ways_and_clamps_at_the_ends() {
        let config = Config::default();
        let mut screen = OverlayScreen::new(&config);
        screen.selected = 1; // Position row
        let start = config.overlay.position.clone();

        // Step right one value, then step back left to the start: the reverse
        // step `Enter` never offered.
        screen.step(1);
        let forward = screen.position.clone();
        assert_ne!(forward, start, "stepping right must change the value");
        screen.step(-1);
        assert_eq!(screen.position, start, "stepping left must walk back");

        // Clamp at the last value: a further right step stays put.
        screen.position = overlay::POSITION_VALUES.last().unwrap().to_string();
        screen.step(1);
        assert_eq!(
            screen.position.as_str(),
            *overlay::POSITION_VALUES.last().unwrap(),
            "the last value clamps instead of wrapping"
        );

        // Clamp at the first value: a further left step stays put.
        screen.position = overlay::POSITION_VALUES[0].to_string();
        screen.step(-1);
        assert_eq!(screen.position, overlay::POSITION_VALUES[0]);
    }

    #[test]
    fn overlay_step_visits_every_position_value_in_both_directions() {
        let config = Config::default();
        let mut screen = OverlayScreen::new(&config);
        screen.selected = 1;
        screen.position = overlay::POSITION_VALUES[0].to_string();
        let mut seen = vec![screen.position.clone()];
        for _ in 1..overlay::POSITION_VALUES.len() {
            screen.step(1);
            seen.push(screen.position.clone());
        }
        let expected: Vec<String> = overlay::POSITION_VALUES
            .iter()
            .map(|value| value.to_string())
            .collect();
        assert_eq!(seen, expected, "stepping right visits every value in order");
        for _ in 1..overlay::POSITION_VALUES.len() {
            screen.step(-1);
        }
        assert_eq!(
            screen.position,
            overlay::POSITION_VALUES[0],
            "stepping left walks all the way back to the first value"
        );
    }

    #[test]
    fn overlay_commit_writes_the_pending_value_only_on_commit() {
        let mut config = Config::default();
        let mut screen = OverlayScreen::new(&config);
        screen.selected = 2; // Style row
        screen.step(1);
        let stepped = screen.style.clone();
        assert_ne!(
            stepped, config.overlay.style,
            "stepping changes the pending value, not the config"
        );
        assert_eq!(
            config.overlay.style, "badge",
            "the config is untouched until commit, so the preview and the save stay honest"
        );
        screen.commit(&mut config);
        assert_eq!(
            config.overlay.style, stepped,
            "commit writes the pending value"
        );
    }

    #[test]
    fn overlay_disabling_hides_the_configuration_rows() {
        let config = Config::default();
        let mut screen = OverlayScreen::new(&config);
        // Default is an enabled badge: Overlay, Position, Style, Look.
        assert!(screen.enabled);
        assert_eq!(
            screen.row_labels(),
            vec!["Overlay", "Position", "Style", "Look"]
        );

        screen.selected = 0;
        screen.step(-1); // step the enable row to Disabled
        assert!(!screen.enabled);
        assert_eq!(screen.row_labels(), vec!["Overlay"]);
        // The row cursor clamps to the single remaining row.
        screen.move_selection(1);
        assert_eq!(screen.selected, 0);
    }

    #[test]
    fn overlay_look_row_cycles_the_two_badge_looks() {
        let config = Config::default();
        let mut screen = OverlayScreen::new(&config);
        // The default badge style offers waveform or plain at row index 3.
        screen.selected = 3;
        assert_eq!(screen.look, "waveform");
        screen.step(-1);
        assert_eq!(screen.look, "plain", "stepping left selects the plain look");
        screen.step(1);
        assert_eq!(
            screen.look, "waveform",
            "stepping right selects the waveform"
        );
        assert_eq!(
            screen.row_labels(),
            vec!["Overlay", "Position", "Style", "Look"]
        );
    }

    #[test]
    fn overlay_shape_row_appears_only_for_a_wide_waveform_look() {
        let mut config = Config::default();
        config.overlay.style = "long-pill".to_string();
        config.overlay.look = "waveform".to_string();
        let screen = OverlayScreen::new(&config);
        assert!(screen
            .row_labels()
            .iter()
            .any(|row| *row == "Waveform shape"));

        // Switching the pending look to live text hides the shape row.
        let mut screen = OverlayScreen::new(&config);
        screen.selected = 3; // Look row for a long pill
        screen.step(1); // waveform -> live text
        assert_eq!(screen.look, "live-text");
        assert!(!screen
            .row_labels()
            .iter()
            .any(|row| *row == "Waveform shape"));

        // A round style never shows it.
        config.overlay.style = "badge".to_string();
        config.overlay.look = "waveform".to_string();
        let screen = OverlayScreen::new(&config);
        assert!(!screen
            .row_labels()
            .iter()
            .any(|row| *row == "Waveform shape"));
    }

    #[test]
    fn output_screen_resets_the_backend_when_output_returns_to_keep() {
        let capabilities = test_capabilities();
        let mut config = Config::default();
        config.output.method = OutputMethod::Type;
        config.output.typing_backend = "wtype".to_string();
        let mut screen = OutputScreen::new(&capabilities, &config);
        screen.selected = 0;
        screen.apply(&mut config).unwrap();
        assert_eq!(config.output.method, OutputMethod::None);
        assert_eq!(config.output.typing_backend, "auto");
    }

    #[test]
    fn output_screen_lists_the_type_choice_even_without_a_helper() {
        let capabilities = capabilities_without_backends();
        let screen = OutputScreen::new(&capabilities, &Config::default());
        assert_eq!(
            screen.labels,
            vec![
                "Keep transcripts in TongueTyped".to_string(),
                "Type into the focused application".to_string(),
                "Copy to the clipboard".to_string(),
                "Paste into the focused application".to_string(),
            ],
            "the type choice must stay listed so the warning can explain it"
        );
        assert!(screen.problems[1].is_some());
    }

    #[test]
    fn output_screen_refuses_the_type_choice_without_a_working_helper() {
        let capabilities = capabilities_without_backends();
        let mut config = Config::default();
        config.output.method = OutputMethod::Type;
        config.output.typing_backend = "auto".to_string();
        let mut screen = OutputScreen::new(&capabilities, &config);
        assert_eq!(
            screen.selected, 0,
            "a configured-but-unavailable type choice must not be preselected"
        );

        screen.selected = 1;
        let refusal = screen.apply(&mut config).unwrap_err();
        assert!(
            refusal.contains("Cannot apply"),
            "the refusal must explain itself: {refusal}"
        );
        assert_eq!(
            config.output.method,
            OutputMethod::Type,
            "a refused choice must leave the configuration untouched"
        );
    }

    #[test]
    fn output_screen_applies_the_type_choice_when_a_helper_works() {
        let capabilities = test_capabilities();
        let mut config = Config::default();
        let mut screen = OutputScreen::new(&capabilities, &config);
        assert!(screen.problems.iter().all(Option::is_none));

        screen.selected = 1;
        screen.apply(&mut config).unwrap();
        assert_eq!(config.output.method, OutputMethod::Type);
    }

    #[test]
    fn output_screen_warns_for_the_highlighted_method_only() {
        let capabilities = Capabilities {
            clipboard: false,
            ..test_capabilities()
        };
        let mut screen = OutputScreen::new(&capabilities, &Config::default());
        let config = Config::default();
        screen.move_selection(1, &config);
        assert_eq!(screen.warning_line().to_string(), "");
        screen.move_selection(1, &config);
        assert!(
            screen.warning_line().to_string().contains("wl-clipboard")
                || screen.warning_line().to_string().contains("xclip"),
            "the clipboard choice must name the clipboard helper to install"
        );
    }

    #[test]
    fn output_screen_refuses_clipboard_and_paste_without_a_clipboard_helper() {
        let capabilities = Capabilities {
            clipboard: false,
            ..test_capabilities()
        };
        let mut config = Config::default();
        let mut screen = OutputScreen::new(&capabilities, &config);
        for row in [2, 3] {
            screen.selected = row;
            assert!(screen.apply(&mut config).is_err());
            assert_eq!(config.output.method, OutputMethod::None);
        }
    }

    #[test]
    fn output_screen_shows_paste_options_only_for_methods_that_use_them() {
        let capabilities = test_capabilities();
        let mut config = Config::default();
        let mut screen = OutputScreen::new(&capabilities, &config);
        assert_eq!(screen.rows(&config).len(), 4);

        screen.selected = 2;
        screen.apply(&mut config).unwrap();
        assert_eq!(config.output.method, OutputMethod::Clipboard);
        assert_eq!(
            screen.rows(&config).len(),
            4,
            "copying alone has no Enter or paste-key options"
        );

        screen.selected = 3;
        screen.apply(&mut config).unwrap();
        assert_eq!(config.output.method, OutputMethod::Paste);
        let rows = screen.rows(&config);
        assert_eq!(rows.len(), 6);
        assert!(rows[4].contains("nothing"), "{rows:?}");
        assert!(rows[5].contains("Ctrl+V"), "{rows:?}");

        // Enter on the option rows only saves; `←`/`→` (the stepper) is the
        // only thing that changes their value, matching the Overlay screen.
        screen.selected = 4;
        screen.apply(&mut config).unwrap();
        assert!(
            !config.output.auto_submit,
            "Enter must not toggle the After-text row"
        );
        screen.step(1, &mut config);
        assert!(config.output.auto_submit);
        screen.selected = 5;
        screen.apply(&mut config).unwrap();
        assert_eq!(
            config.output.paste_keys,
            PasteKeys::CtrlV,
            "Enter must not cycle the paste keys"
        );
        screen.step(1, &mut config);
        assert_eq!(config.output.paste_keys, PasteKeys::ShiftInsert);
        let rows = screen.rows(&config);
        assert!(rows[4].contains("press Enter"), "{rows:?}");
        assert!(rows[5].contains("Shift+Insert"), "{rows:?}");
        config.validate().unwrap();

        // `type` keeps Enter-after but has no paste keys row.
        screen.selected = 1;
        screen.apply(&mut config).unwrap();
        assert_eq!(screen.rows(&config).len(), 5);
        assert!(config.output.auto_submit);

        // Leaving for a method that inserts nothing clears Enter-after, which
        // would otherwise fail validation on save.
        screen.selected = 2;
        screen.apply(&mut config).unwrap();
        assert!(!config.output.auto_submit);
        config.validate().unwrap();
    }

    #[test]
    fn output_screen_paste_keys_stepper_clamps_at_the_ends() {
        let capabilities = test_capabilities();
        let mut config = Config::default();
        config.output.method = OutputMethod::Paste;
        let mut screen = OutputScreen::new(&capabilities, &config);
        screen.selected = OutputScreen::PASTE_KEYS_ROW;

        assert_eq!(config.output.paste_keys, PasteKeys::CtrlV);
        screen.step(-1, &mut config);
        assert_eq!(
            config.output.paste_keys,
            PasteKeys::CtrlV,
            "stepping left at the first value must clamp, not wrap"
        );
        screen.step(1, &mut config);
        assert_eq!(config.output.paste_keys, PasteKeys::ShiftInsert);
        screen.step(1, &mut config);
        assert_eq!(
            config.output.paste_keys,
            PasteKeys::ShiftInsert,
            "stepping right at the last value must clamp, not wrap"
        );
        screen.step(-1, &mut config);
        assert_eq!(config.output.paste_keys, PasteKeys::CtrlV);
    }

    #[test]
    fn output_screen_paste_keys_stepper_dims_the_end_arrows_it_cannot_use() {
        let capabilities = test_capabilities();
        let mut config = Config::default();
        config.output.method = OutputMethod::Paste;
        let mut screen = OutputScreen::new(&capabilities, &config);
        screen.selected = OutputScreen::PASTE_KEYS_ROW;

        let first = rendered_widget(screen.list_widget(&config, 68), 70, 12);
        let first_keys = first
            .lines()
            .find(|line| line.contains("Paste keys"))
            .expect("the paste-keys row must render");
        assert!(
            !first_keys.contains('\u{2039}')
                && first_keys.contains("Ctrl+V")
                && first_keys.contains('\u{203a}')
                && first_keys.contains("1/2"),
            "the first value must hide its left arrow:\n{first}"
        );

        screen.step(1, &mut config);
        let last = rendered_widget(screen.list_widget(&config, 68), 70, 12);
        let last_keys = last
            .lines()
            .find(|line| line.contains("Paste keys"))
            .expect("the paste-keys row must render");
        assert!(
            last_keys.contains('\u{2039}')
                && last_keys.contains("Shift+Insert")
                && !last_keys.contains('\u{203a}')
                && last_keys.contains("2/2"),
            "the last value must hide its right arrow:\n{last}"
        );
    }

    #[test]
    fn typing_backend_screen_warns_when_the_helper_list_is_empty() {
        let capabilities = capabilities_without_backends();
        let screen = TypingBackendScreen::new(&capabilities, &Config::default());
        assert_eq!(screen.values, vec!["auto".to_string()]);
        let warning = screen.warning.expect("no helpers should warn");
        // The remedy the warning names depends on the host session: install a
        // helper where one is needed, or switch to the native libei backend
        // where it works (KDE/GNOME Wayland). Either way the warning must name
        // a concrete remedy, so this stays independent of the test machine.
        assert!(
            warning.contains("install") || warning.contains("typing_backend"),
            "the warning must name a concrete remedy: {warning}"
        );
    }

    #[test]
    fn typing_backend_screen_starts_on_the_configured_backend() {
        let capabilities = test_capabilities();
        let mut config = Config::default();
        config.output.typing_backend = "enigo".to_string();
        let screen = TypingBackendScreen::new(&capabilities, &config);
        assert_eq!(screen.values[screen.selected], "enigo");
        assert!(screen.warning.is_none());
    }

    #[test]
    fn history_retention_screen_applies_valid_numbers_and_rejects_junk() {
        let mut config = Config::default();
        let screen = TextFieldsScreen::history_retention(&config);
        assert_eq!(screen.fields[0].1.value(), "100");
        assert_eq!(screen.fields[1].1.value(), "30");
        screen.apply(&mut config).unwrap();
        assert_eq!(config.history.max_entries, 100);
        assert_eq!(config.history.max_age_days, 30);

        let mut bad = TextFieldsScreen::history_retention(&config);
        bad.fields[0].1 = TextField::new("abc");
        assert!(bad.apply(&mut config).is_err());
        assert_eq!(
            config.history.max_entries, 100,
            "a rejected edit must not stick"
        );
    }

    #[test]
    fn transcript_folder_screen_requires_an_absolute_path_and_allows_clearing() {
        let mut config = Config::default();
        let mut screen = TextFieldsScreen::transcript_folder(&config);
        screen.fields[0].1 = TextField::new("relative/dir");
        assert!(screen.apply(&mut config).is_err());
        assert!(config.history.transcript_folder.is_empty());

        screen.fields[0].1 = TextField::new("/tmp/tt-out");
        screen.apply(&mut config).unwrap();
        assert_eq!(config.history.transcript_folder, "/tmp/tt-out");

        screen.fields[0].1 = TextField::new("   ");
        screen.apply(&mut config).unwrap();
        assert!(config.history.transcript_folder.is_empty());
    }

    #[test]
    fn activation_screen_reflects_and_updates_the_configured_mode() {
        let mut config = Config::default();
        let mut screen = ActivationScreen::new(&config);
        assert_eq!(screen.selected, 0);
        screen.selected = 1;
        screen.apply(&mut config);
        assert!(config.activation.mode == ActivationMode::Toggle);
        assert_eq!(ActivationScreen::new(&config).selected, 1);
    }

    #[test]
    fn media_pause_screen_starts_off_and_saves_the_choice() {
        let mut config = Config::default();
        let mut screen = MediaPauseScreen::new(&config);
        assert_eq!(screen.selected, 0);
        screen.move_selection(1);
        screen.apply(&mut config);
        assert!(config.audio.pause_media);
        assert_eq!(MediaPauseScreen::new(&config).selected, 1);
        screen.move_selection(-1);
        screen.apply(&mut config);
        assert!(!config.audio.pause_media);
    }

    fn test_capabilities() -> Capabilities {
        Capabilities {
            microphones: vec![
                (
                    "default".to_string(),
                    "System default microphone".to_string(),
                ),
                (
                    "pipewire:usb-source".to_string(),
                    "USB Microphone".to_string(),
                ),
            ],
            typing_backends: vec!["wtype".to_string(), "enigo".to_string()],
            clipboard: true,
            inference_backends: Vec::new(),
        }
    }

    fn capabilities_without_backends() -> Capabilities {
        Capabilities {
            typing_backends: Vec::new(),
            ..test_capabilities()
        }
    }

    fn choices() -> Vec<BackendChoice> {
        vec![
            BackendChoice {
                name: "auto",
                unavailable: None,
            },
            BackendChoice {
                name: "cpu",
                unavailable: None,
            },
            BackendChoice {
                name: "cuda",
                unavailable: Some("compiled without".to_string()),
            },
            BackendChoice {
                name: "vulkan",
                unavailable: None,
            },
        ]
    }

    #[test]
    fn backend_list_starts_on_the_configured_backend_and_skips_unavailable_ones() {
        let mut config = Config::default();
        config.model.preferred_backend = "vulkan".to_string();
        let mut screen = ModelScreen::new(&config, choices());
        assert_eq!(screen.focus, ModelFocus::Catalog);
        assert_eq!(screen.selected_backend(), Some("vulkan"));

        screen.toggle_focus();
        screen.move_selection(-1);
        assert_eq!(screen.selected_backend(), Some("cpu"));
        screen.move_selection(-5);
        assert_eq!(screen.selected_backend(), Some("auto"));
        screen.move_selection(10);
        assert_eq!(screen.selected_backend(), Some("vulkan"));
    }

    #[test]
    fn arrow_keys_only_move_the_focused_list() {
        let mut screen = ModelScreen::new(&Config::default(), choices());
        let catalog_start = screen.selected;
        screen.toggle_focus();
        screen.move_selection(1);
        assert_eq!(screen.selected, catalog_start);
        assert_eq!(screen.selected_backend(), Some("cpu"));

        screen.toggle_focus();
        screen.move_selection(1);
        assert_eq!(screen.selected_backend(), Some("cpu"));
    }

    fn rendered_backend_widget(config: &Config, backends: Vec<BackendChoice>) -> String {
        let screen = ModelScreen::new(config, backends);
        let backend = ratatui::backend::TestBackend::new(80, 12);
        let mut terminal = ratatui::Terminal::new(backend).unwrap();
        terminal
            .draw(|frame| frame.render_widget(screen.backend_widget(78), frame.area()))
            .unwrap();
        let buffer = terminal.backend().buffer();
        let area = *buffer.area();
        let mut text = String::new();
        for y in area.y..area.y + area.height {
            for x in area.x..area.x + area.width {
                text.push_str(buffer[(x, y)].symbol());
            }
            text.push('\n');
        }
        text
    }

    #[test]
    fn model_screen_marks_an_unavailable_configured_backend_active() {
        let mut config = Config::default();
        config.model.preferred_backend = "cuda".to_string();
        let text = rendered_backend_widget(&config, choices());
        assert!(
            text.contains("cuda (active) unavailable"),
            "an unavailable backend that is still the saved pin must be marked active:\n{text}"
        );
    }

    #[test]
    fn model_screen_does_not_mark_an_unrelated_unavailable_backend_active() {
        let text = rendered_backend_widget(&Config::default(), choices());
        assert!(
            text.contains("cuda unavailable"),
            "the unavailable backend should still be listed:\n{text}"
        );
        assert!(
            !text.contains("cuda (active)"),
            "only the saved pin may be marked active:\n{text}"
        );
    }

    fn rendered_dedicated_backend_widget(config: &Config, backends: Vec<BackendChoice>) -> String {
        let screen = BackendScreen::new(config, backends);
        let backend = ratatui::backend::TestBackend::new(80, 12);
        let mut terminal = ratatui::Terminal::new(backend).unwrap();
        terminal
            .draw(|frame| frame.render_widget(screen.list_widget(config, 78), frame.area()))
            .unwrap();
        let buffer = terminal.backend().buffer();
        let area = *buffer.area();
        let mut text = String::new();
        for y in area.y..area.y + area.height {
            for x in area.x..area.x + area.width {
                text.push_str(buffer[(x, y)].symbol());
            }
            text.push('\n');
        }
        text
    }

    #[test]
    fn backend_screen_starts_on_the_configured_backend_and_skips_unavailable_ones() {
        let mut config = Config::default();
        config.model.preferred_backend = "vulkan".to_string();
        let mut screen = BackendScreen::new(&config, choices());
        assert_eq!(screen.selected_backend(), Some("vulkan"));

        screen.move_selection(-1);
        assert_eq!(screen.selected_backend(), Some("cpu"));
        screen.move_selection(-5);
        assert_eq!(screen.selected_backend(), Some("auto"));
        screen.move_selection(10);
        assert_eq!(screen.selected_backend(), Some("vulkan"));
    }

    #[test]
    fn backend_screen_lists_unavailable_backends_verbatim() {
        let config = Config::default();

        // The dedicated screen lists unavailable backends verbatim, never
        // dropping one silently - same rule as the Model screen's panel.
        let text = rendered_dedicated_backend_widget(&config, choices());
        assert!(
            text.contains("cuda unavailable"),
            "the unavailable backend should still be listed:\n{text}"
        );
    }

    fn rendered_buffer(
        widget: impl ratatui::widgets::Widget,
        width: u16,
        height: u16,
    ) -> ratatui::buffer::Buffer {
        let backend = ratatui::backend::TestBackend::new(width, height);
        let mut terminal = ratatui::Terminal::new(backend).unwrap();
        terminal
            .draw(|frame| frame.render_widget(widget, frame.area()))
            .unwrap();
        terminal.backend().buffer().clone()
    }

    /// Renders a widget and asserts the row it draws as a highlight bar fills
    /// the whole inner width, not just the width of its text.
    fn assert_full_width_highlight_bar(
        buffer: &ratatui::buffer::Buffer,
        inner_left: u16,
        inner_width: u16,
    ) {
        let area = *buffer.area();
        let row = (area.y..area.y + area.height)
            .find(|&y| (area.x..area.x + area.width).any(|x| buffer[(x, y)].bg == Color::Cyan))
            .expect("expected exactly one row drawn as a highlight bar");
        for x in inner_left..inner_left + inner_width {
            assert_eq!(
                buffer[(x, row)].bg,
                Color::Cyan,
                "cell ({x},{row}) should be part of the full-width highlight bar"
            );
        }
    }

    #[test]
    fn shortcut_selected_action_row_is_a_full_width_highlight_bar() {
        let screen = ShortcutScreen::new(&Config::default());
        let buffer = rendered_buffer(screen.body("Super+T", 40), 42, 10);
        assert_full_width_highlight_bar(&buffer, 1, 40);
    }

    #[test]
    fn model_catalog_selected_row_is_a_full_width_highlight_bar() {
        let screen = ModelScreen::new(&Config::default(), choices());
        let buffer = rendered_buffer(screen.list_widget(100, 12), 102, 12);
        assert_full_width_highlight_bar(&buffer, 1, 100);
    }

    #[test]
    fn text_fields_selected_value_row_is_a_full_width_highlight_bar() {
        let screen = TextFieldsScreen::history_retention(&Config::default());
        let buffer = rendered_buffer(screen.list_widget(40), 42, 12);
        assert_full_width_highlight_bar(&buffer, 1, 40);
    }

    #[test]
    fn text_fields_selected_empty_row_is_a_full_width_highlight_bar() {
        let screen = TextFieldsScreen::transcript_folder(&Config::default());
        let buffer = rendered_buffer(screen.list_widget(40), 42, 12);
        assert_full_width_highlight_bar(&buffer, 1, 40);
    }

    #[test]
    fn text_fields_selected_placeholder_row_is_a_full_width_highlight_bar() {
        let screen = TextFieldsScreen::decoding(&Config::default());
        let buffer = rendered_buffer(screen.list_widget(100), 102, 12);
        assert_full_width_highlight_bar(&buffer, 1, 100);
    }

    #[test]
    fn text_processing_selected_text_row_is_a_full_width_highlight_bar() {
        let screen = TextProcessingScreen::new(&Config::default());
        let buffer = rendered_buffer(screen.list_widget(60), 62, 12);
        assert_full_width_highlight_bar(&buffer, 1, 60);
    }

    #[test]
    fn text_processing_selected_toggle_row_is_a_full_width_highlight_bar() {
        let mut screen = TextProcessingScreen::new(&Config::default());
        screen.selected = TextProcessingScreen::ROW_SPOKEN;
        let buffer = rendered_buffer(screen.list_widget(60), 62, 12);
        assert_full_width_highlight_bar(&buffer, 1, 60);
    }

    #[test]
    fn model_backend_panel_selected_row_is_a_full_width_highlight_bar() {
        let mut screen = ModelScreen::new(&Config::default(), choices());
        screen.toggle_focus();
        let buffer = rendered_buffer(screen.backend_widget(78), 80, 12);
        assert_full_width_highlight_bar(&buffer, 1, 78);
    }

    #[test]
    fn dedicated_backend_selected_row_is_a_full_width_highlight_bar() {
        let config = Config::default();
        let screen = BackendScreen::new(&config, choices());
        let buffer = rendered_buffer(screen.list_widget(&config, 78), 80, 12);
        assert_full_width_highlight_bar(&buffer, 1, 78);
    }

    fn rendered_widget(widget: impl ratatui::widgets::Widget, width: u16, height: u16) -> String {
        let backend = ratatui::backend::TestBackend::new(width, height);
        let mut terminal = ratatui::Terminal::new(backend).unwrap();
        terminal
            .draw(|frame| frame.render_widget(widget, frame.area()))
            .unwrap();
        let buffer = terminal.backend().buffer();
        let area = *buffer.area();
        let mut text = String::new();
        for y in area.y..area.y + area.height {
            for x in area.x..area.x + area.width {
                text.push_str(buffer[(x, y)].symbol());
            }
            text.push('\n');
        }
        text
    }

    #[test]
    fn model_detail_labels_language_coverage_and_a_hard_input_cap() {
        let mut config = Config::default();
        config.model.active_model = "canary-1b-v2-f16".to_string();
        let screen = ModelScreen::new(&config, choices());
        let text = rendered_widget(screen.selected_detail_widget(), 100, 4);
        assert!(text.contains("Canary"), "{text}");
        assert!(text.contains("25 languages"), "{text}");
        assert!(text.contains("400s per call"), "{text}");
    }

    #[test]
    fn model_detail_marks_a_soft_window_model() {
        let mut config = Config::default();
        config.model.active_model = "moonshine-base-q8_0".to_string();
        let screen = ModelScreen::new(&config, choices());
        let text = rendered_widget(screen.selected_detail_widget(), 100, 4);
        assert!(text.contains("Moonshine"), "{text}");
        assert!(text.contains("English"), "{text}");
        assert!(text.contains("about 48s"), "{text}");
    }
}
