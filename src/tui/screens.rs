//! State and rendering for the dashboard's interactive sub-screens that
//! aren't just "run one action and show the result": the model screen
//! (catalog activation + inference backend), the daemon actions, and the
//! settings screens the home screen's Settings panel opens (inference
//! backend, translation, decoding, microphone, activation, shortcut,
//! transcript output, typing backend, startup, overlay).
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

    pub(super) fn backend_widget(&self) -> Paragraph<'static> {
        let focused = self.focus == ModelFocus::Backend;
        let lines = backend_panel_lines(
            &self.backends,
            &self.configured_backend,
            focused.then_some(self.backend_selected),
        );
        Paragraph::new(lines).block(
            Block::default()
                .borders(Borders::ALL)
                .border_style(focus_border(focused))
                .title("Inference backend"),
        )
    }

    pub(super) fn list_widget(&self, height: u16) -> Paragraph<'static> {
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
                let text = format!(
                    "{marker}{:<32} {:<8} {:>10}  {:<9}",
                    row.id,
                    row.quant,
                    commands::human_size(row.size_bytes),
                    status
                );
                let style = selection_style(highlighted);
                Line::from(Span::styled(text, style))
            })
            .collect();
        let visible_rows = usize::from(height.saturating_sub(2)).max(1);
        let scroll = self.selected.saturating_sub(visible_rows.saturating_sub(1)) as u16;
        Paragraph::new(lines)
            .block(
                Block::default()
                    .borders(Borders::ALL)
                    .border_style(focus_border(self.focus == ModelFocus::Catalog))
                    .title("Model catalog (MODEL ID / QUANT / SIZE / STATUS)"),
            )
            .scroll((scroll, 0))
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
/// highlight, or `None` when the panel is not focused.
fn backend_panel_lines(
    backends: &[BackendChoice],
    configured: &str,
    cursor: Option<usize>,
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
            let style = if highlighted {
                Style::default()
                    .fg(Color::Cyan)
                    .add_modifier(Modifier::BOLD)
            } else {
                Style::default()
            };
            Line::from(Span::styled(text, style))
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

    pub(super) fn list_widget(&self, config: &Config) -> Paragraph<'static> {
        let lines = backend_panel_lines(
            &self.backends,
            &config.model.preferred_backend,
            Some(self.selected),
        );
        Paragraph::new(lines).block(
            Block::default()
                .borders(Borders::ALL)
                .title("Inference backend"),
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

    pub(super) fn list_widget(&self) -> Paragraph<'static> {
        let rows = DAEMON_ACTIONS.map(String::from);
        selection_list("Daemon", &rows, self.selected)
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

    pub(super) fn list_widget(&self) -> Paragraph<'static> {
        let rows = AUTOSTART_LABELS.map(String::from);
        selection_list("Startup", &rows, self.selected)
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

    pub(super) fn list_widget(&self) -> Paragraph<'static> {
        let rows: Vec<String> = self
            .options
            .iter()
            .map(|(_, label)| label.clone())
            .collect();
        selection_list("Microphone", &rows, self.selected)
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

    pub(super) fn list_widget(&self) -> Paragraph<'static> {
        let rows = crate::config::ACTIVATION_MODE_LABELS.map(String::from);
        selection_list("Activation", &rows, self.selected)
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

    pub(super) fn list_widget(&self) -> Paragraph<'static> {
        let rows = crate::config::TRANSLATE_LABELS.map(String::from);
        selection_list("Translation", &rows, self.selected)
    }

    pub(super) fn result_line(&self) -> Line<'static> {
        result_line(&self.result, "Translation saved.")
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

    pub(super) fn body(&self, bound: &str) -> Paragraph<'static> {
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
            lines.push(Line::from(Span::styled(
                format!("{marker}{action}"),
                selection_style(selected),
            )));
        }
        lines.push(Line::from(""));
        lines.push(self.status_line());
        Paragraph::new(lines).block(Block::default().borders(Borders::ALL).title("Shortcut"))
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
    /// end.
    pub(super) fn footer(&self) -> &'static str {
        match self.status {
            ShortcutStatus::DialogOpen => "Enter keep current  Ctrl+R reopen  Esc back",
            _ => "↑/↓ choose action  Enter run  Ctrl+R system dialog  Esc back",
        }
    }
}

/// Transcript output: the four methods, then - for the methods that insert
/// text - Enter-after and, for `paste`, the paste keys. Like the Overlay
/// screen, the option rows follow the saved method, and Enter on one toggles
/// or cycles its value.
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

    /// Applies the highlighted row. A method can only be applied while it
    /// works here; choosing one that cannot leaves the configuration
    /// untouched and returns the refusal for the result line, so the screen
    /// explains what to install instead of silently saving a method that can
    /// never deliver a transcript. Leaving `type`/`paste` also clears
    /// Enter-after, which only those methods accept.
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
            return Ok(());
        }
        match self.selected {
            Self::SUBMIT_ROW => config.output.auto_submit = !config.output.auto_submit,
            Self::PASTE_KEYS_ROW => {
                let index = PasteKeys::ALL
                    .iter()
                    .position(|keys| *keys == config.output.paste_keys)
                    .unwrap_or(0);
                config.output.paste_keys = PasteKeys::ALL[(index + 1) % PasteKeys::ALL.len()];
            }
            _ => {}
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
                    "press Enter"
                } else {
                    "nothing"
                }
            ));
        }
        if config.output.method == OutputMethod::Paste {
            rows.push(format!(
                "  {:<14}{}",
                "Paste keys",
                paste_keys_labels()[PasteKeys::ALL
                    .iter()
                    .position(|keys| *keys == config.output.paste_keys)
                    .unwrap_or(0)]
            ));
        }
        rows
    }

    pub(super) fn list_widget(&self, config: &Config) -> Paragraph<'static> {
        selection_list("Transcript output", &self.rows(config), self.selected)
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

    pub(super) fn list_widget(&self) -> Paragraph<'static> {
        selection_list("Typing backend", &self.values, self.selected)
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

/// The Overlay screen keeps the wizard's enabled/position/style/waveform
/// choices on one screen: Disabled/Enabled always shown, the three
/// configuration rows shown only while the overlay is enabled. Enter toggles
/// an enable row and cycles position/style/waveform through their real
/// config values.
pub(super) struct OverlayScreen {
    pub selected: usize,
    pub result: Option<Result<(), String>>,
}

impl OverlayScreen {
    const ENABLED_ROWS: usize = 5;

    pub(super) fn new(config: &Config) -> Self {
        Self {
            selected: usize::from(config.overlay.enabled),
            result: None,
        }
    }

    fn row_count(config: &Config) -> usize {
        if config.overlay.enabled {
            Self::ENABLED_ROWS
        } else {
            2
        }
    }

    pub(super) fn move_selection(&mut self, delta: i32, config: &Config) {
        let len = Self::row_count(config) as i32;
        let next = (self.selected as i32 + delta).clamp(0, len - 1) as usize;
        self.selected = next;
    }

    pub(super) fn apply(&mut self, config: &mut Config) {
        match self.selected {
            0 => config.overlay.enabled = false,
            1 => config.overlay.enabled = true,
            2 => {
                config.overlay.position =
                    next_value(&overlay::POSITION_VALUES, &config.overlay.position)
            }
            3 => config.overlay.style = next_value(&overlay::STYLE_VALUES, &config.overlay.style),
            4 => config.overlay.live_waveform = !config.overlay.live_waveform,
            _ => {}
        }
    }

    pub(super) fn rows(&self, config: &Config) -> Vec<String> {
        let mut rows = vec!["Disabled".to_string(), "Enabled".to_string()];
        if config.overlay.enabled {
            rows.push(format!("{:<12}{}", "Position", config.overlay.position));
            rows.push(format!(
                "{:<12}{}",
                "Style",
                overlay_style_label(&config.overlay.style)
            ));
            rows.push(format!(
                "{:<12}{}",
                "Waveform",
                if config.overlay.live_waveform {
                    overlay::WAVEFORM_LABELS[1]
                } else {
                    overlay::WAVEFORM_LABELS[0]
                }
            ));
        }
        rows
    }

    pub(super) fn list_widget(&self, config: &Config) -> Paragraph<'static> {
        let rows = self.rows(config);
        selection_list("Overlay", &rows, self.selected)
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

    pub(super) fn list_widget(&self) -> Paragraph<'static> {
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
                Line::from(spans)
            })
            .collect();
        Paragraph::new(lines)
            .wrap(Wrap { trim: false })
            .block(Block::default().borders(Borders::ALL).title(self.title))
    }

    pub(super) fn result_line(&self, success: &str) -> Line<'static> {
        result_line(&self.result, success)
    }
}

/// The value half of one `TextFieldsScreen` row. The selected field draws a
/// reversed cell at the cursor so the editing position is visible; an
/// unselected field is plain text.
fn field_value_spans(field: &TextField, selected: bool) -> Vec<Span<'static>> {
    if !selected {
        return vec![Span::raw(field.value().to_string())];
    }
    let (before, cursor_char, after) = field.split_at_cursor();
    let reversed = Style::default().add_modifier(Modifier::REVERSED);
    vec![
        Span::raw(before),
        Span::styled(
            cursor_char.map(String::from).unwrap_or_else(|| " ".into()),
            reversed,
        ),
        Span::raw(after),
    ]
}

/// The dim hint drawn in place of an empty text field. When the field is
/// selected its first character carries the cursor, so the hint starts in
/// the same column on every row.
fn placeholder_spans(text: &str, selected: bool) -> Vec<Span<'static>> {
    let dim = Style::default().fg(Color::DarkGray);
    let mut chars = text.chars();
    match chars.next().filter(|_| selected) {
        Some(first) => vec![
            Span::styled(first.to_string(), dim.add_modifier(Modifier::REVERSED)),
            Span::styled(chars.as_str().to_string(), dim),
        ],
        None => vec![Span::styled(text.to_string(), dim)],
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

fn overlay_style_label(style: &str) -> &'static str {
    overlay::STYLE_VALUES
        .iter()
        .position(|value| *value == style)
        .map(|index| overlay::STYLE_LABELS[index])
        .unwrap_or(overlay::STYLE_LABELS[0])
}

fn next_value(values: &[&str], current: &str) -> String {
    let index = values
        .iter()
        .position(|value| *value == current)
        .unwrap_or(0);
    values[(index + 1) % values.len()].to_string()
}

pub(super) fn selection_style(selected: bool) -> Style {
    if selected {
        Style::default()
            .fg(Color::Cyan)
            .add_modifier(Modifier::BOLD)
    } else {
        Style::default()
    }
}

/// The dashboard's standard selectable list panel: every row is
/// `"> "`/`"  "`-prefixed, the selected row cyan+bold.
fn selection_list(title: &str, rows: &[String], selected: usize) -> Paragraph<'static> {
    let lines: Vec<Line> = rows
        .iter()
        .enumerate()
        .map(|(index, row)| {
            let marker = if index == selected { "> " } else { "  " };
            Line::from(Span::styled(
                format!("{marker}{row}"),
                selection_style(index == selected),
            ))
        })
        .collect();
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
    fn overlay_cycles_every_position_value_then_wraps() {
        let mut config = Config::default();
        let mut screen = OverlayScreen::new(&config);
        screen.selected = 2;
        let start = config.overlay.position.clone();
        let mut seen = vec![start.clone()];
        for _ in 1..overlay::POSITION_VALUES.len() {
            screen.apply(&mut config);
            seen.push(config.overlay.position.clone());
        }
        assert_eq!(
            seen.len(),
            overlay::POSITION_VALUES.len(),
            "cycling once must visit every position value"
        );
        screen.apply(&mut config);
        assert_eq!(
            config.overlay.position, start,
            "one more cycle wraps back to the starting position"
        );
        let mut seen_sorted = seen;
        let mut expected: Vec<String> = overlay::POSITION_VALUES
            .iter()
            .map(|value| value.to_string())
            .collect();
        expected.sort();
        seen_sorted.sort();
        assert_eq!(seen_sorted, expected);
    }

    #[test]
    fn overlay_cycles_every_style_value_then_wraps() {
        let mut config = Config::default();
        let mut screen = OverlayScreen::new(&config);
        screen.selected = 3;
        let start = config.overlay.style.clone();
        let mut seen = vec![start.clone()];
        for _ in 1..overlay::STYLE_VALUES.len() {
            screen.apply(&mut config);
            seen.push(config.overlay.style.clone());
        }
        screen.apply(&mut config);
        assert_eq!(config.overlay.style, start);
        let mut seen_sorted = seen;
        let mut expected: Vec<String> = overlay::STYLE_VALUES
            .iter()
            .map(|value| value.to_string())
            .collect();
        expected.sort();
        seen_sorted.sort();
        assert_eq!(seen_sorted, expected);
    }

    #[test]
    fn overlay_disabling_hides_the_three_configuration_rows() {
        let mut config = Config::default();
        let screen = OverlayScreen::new(&config);
        assert_eq!(screen.rows(&config).len(), OverlayScreen::ENABLED_ROWS);

        config.overlay.enabled = false;
        let rows = screen.rows(&config);
        assert_eq!(rows, vec!["Disabled".to_string(), "Enabled".to_string()]);
    }

    #[test]
    fn overlay_waveform_row_uses_the_shared_labels() {
        let mut config = Config::default();
        let mut screen = OverlayScreen::new(&config);
        screen.selected = 4;
        let labels = [overlay::WAVEFORM_LABELS[0], overlay::WAVEFORM_LABELS[1]];
        let before = screen.rows(&config)[4].clone();
        assert!(labels.iter().any(|label| before.contains(label)));
        screen.apply(&mut config);
        let after = screen.rows(&config)[4].clone();
        assert!(labels.iter().any(|label| after.contains(label)));
        assert_ne!(
            before, after,
            "applying the row should toggle the waveform choice"
        );
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

        // Enter on the option rows toggles paste-and-enter and cycles the
        // paste keys.
        screen.selected = 4;
        screen.apply(&mut config).unwrap();
        assert!(config.output.auto_submit);
        screen.selected = 5;
        screen.apply(&mut config).unwrap();
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
    fn typing_backend_screen_warns_when_the_helper_list_is_empty() {
        let capabilities = capabilities_without_backends();
        let screen = TypingBackendScreen::new(&capabilities, &Config::default());
        assert_eq!(screen.values, vec!["auto".to_string()]);
        let warning = screen.warning.expect("no helpers should warn");
        assert!(warning.contains("install"));
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
            .draw(|frame| frame.render_widget(screen.backend_widget(), frame.area()))
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
            .draw(|frame| frame.render_widget(screen.list_widget(config), frame.area()))
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
}
