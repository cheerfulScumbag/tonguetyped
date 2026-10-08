//! State and rendering for the dashboard's interactive sub-screens that
//! aren't just "run one action and show the result": the model catalog
//! (navigable list + activation), the daemon actions, and the settings
//! screens the home screen's Settings panel opens (microphone, activation,
//! shortcut, transcript output, typing backend, startup, overlay).
//!
//! Every settings screen here only mutates the in-memory `Config` it is
//! handed (`apply`); the caller (`tui::App`) owns `Config::save` so a failed
//! write surfaces in the same result line a successful one would.

use crate::audio::{level_to_ratio, MicMonitor};
use crate::commands;
use crate::config::{ActivationMode, Config, OutputMethod};
use crate::overlay;
use crate::setup::Capabilities;
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Gauge, Paragraph};

pub(super) struct ModelScreen {
    pub rows: Vec<commands::ModelRow>,
    pub selected: usize,
}

impl ModelScreen {
    pub(super) fn new(config: &Config) -> Self {
        let rows = commands::model_rows(config);
        let selected = rows.iter().position(|row| row.active).unwrap_or(0);
        Self { rows, selected }
    }

    pub(super) fn move_selection(&mut self, delta: i32) {
        let len = self.rows.len() as i32;
        if len == 0 {
            return;
        }
        let next = (self.selected as i32 + delta).clamp(0, len - 1) as usize;
        self.selected = next;
    }

    pub(super) fn selected_id(&self) -> &'static str {
        self.rows[self.selected].id
    }

    pub(super) fn list_widget(&self, height: u16) -> Paragraph<'static> {
        let lines: Vec<Line> = self
            .rows
            .iter()
            .enumerate()
            .map(|(index, row)| {
                let marker = if index == self.selected { "> " } else { "  " };
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
                let style = selection_style(index == self.selected);
                Line::from(Span::styled(text, style))
            })
            .collect();
        let visible_rows = usize::from(height.saturating_sub(2)).max(1);
        let scroll = self.selected.saturating_sub(visible_rows.saturating_sub(1)) as u16;
        Paragraph::new(lines)
            .block(
                Block::default()
                    .borders(Borders::ALL)
                    .title("Model catalog (MODEL ID / QUANT / SIZE / STATUS)"),
            )
            .scroll((scroll, 0))
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

/// The Shortcut screen: an editable keybinding string plus the two actions
/// that actually talk to the desktop's global-shortcuts portal (test-press
/// and the native reconfigure dialog). A successful test or dialog saves the
/// typed binding; merely typing and leaving does not.
pub(super) struct ShortcutScreen {
    pub input: String,
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
    "Test shortcut - press it now",
    "Set via system dialog (Ctrl+R)",
];

impl ShortcutScreen {
    pub(super) fn new(config: &Config) -> Self {
        Self {
            input: config.activation.keybind.clone(),
            selected_action: 0,
            status: ShortcutStatus::Idle,
        }
    }

    pub(super) fn move_selection(&mut self, delta: i32) {
        let len = SHORTCUT_ACTIONS.len() as i32;
        let next = (self.selected_action as i32 + delta).clamp(0, len - 1) as usize;
        self.selected_action = next;
    }

    /// While the portal test or the native dialog is in flight, every key
    /// except Escape is ignored - same permissiveness the setup console gives
    /// its in-flight reconfigure dialog.
    pub(super) fn is_busy(&self) -> bool {
        matches!(
            self.status,
            ShortcutStatus::Testing | ShortcutStatus::DialogOpen
        )
    }

    pub(super) fn body(&self, keybind_status: &str) -> Paragraph<'static> {
        let mut lines = vec![
            Line::from(format!("Shortcut: {}_", self.input)),
            Line::from(if keybind_status == "untested" {
                "Currently bound: (not confirmed by the desktop yet)".to_string()
            } else {
                format!("Currently bound: {keybind_status}")
            }),
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
            ShortcutStatus::Idle => Line::from("A successful test saves the typed shortcut."),
            ShortcutStatus::Testing => Line::from(Span::styled(
                "Waiting for you to press the shortcut...",
                Style::default().fg(Color::Yellow),
            )),
            ShortcutStatus::DialogOpen => Line::from(Span::styled(
                "A system dialog is open - press your new shortcut there now.",
                Style::default().fg(Color::Yellow),
            )),
            ShortcutStatus::Message { text, is_error } => Line::from(Span::styled(
                text.clone(),
                Style::default().fg(if *is_error { Color::Red } else { Color::Green }),
            )),
        }
    }
}

/// Transcript output method: keep transcripts in-app, or type them into the
/// focused application.
pub(super) struct OutputScreen {
    pub labels: Vec<String>,
    pub selected: usize,
    pub result: Option<Result<(), String>>,
}

impl OutputScreen {
    pub(super) fn new(capabilities: &Capabilities, config: &Config) -> Self {
        let labels = capabilities.output_labels();
        let selected = usize::from(config.output.method == OutputMethod::Type && labels.len() > 1);
        Self {
            labels,
            selected,
            result: None,
        }
    }

    pub(super) fn move_selection(&mut self, delta: i32) {
        let len = self.labels.len() as i32;
        if len == 0 {
            return;
        }
        let next = (self.selected as i32 + delta).clamp(0, len - 1) as usize;
        self.selected = next;
    }

    pub(super) fn apply(&mut self, config: &mut Config) {
        if self.selected == 0 {
            config.output.method = OutputMethod::None;
            config.output.typing_backend = "auto".to_string();
        } else {
            config.output.method = OutputMethod::Type;
        }
    }

    pub(super) fn list_widget(&self) -> Paragraph<'static> {
        selection_list("Transcript output", &self.labels, self.selected)
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

    pub(super) fn result_line(&self) -> Line<'static> {
        result_line(&self.result, "Typing backend saved.")
    }
}

/// The Overlay screen keeps the wizard's enabled/position/style/streaming
/// choices on one screen: Disabled/Enabled always shown, the three
/// configuration rows shown only while the overlay is enabled. Enter toggles
/// an enable row and cycles position/style/streaming through their real
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
            4 => config.overlay.streaming_indicator = !config.overlay.streaming_indicator,
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
                "Streaming",
                if config.overlay.streaming_indicator {
                    overlay::STREAMING_LABELS[1]
                } else {
                    overlay::STREAMING_LABELS[0]
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
    fn overlay_streaming_row_uses_the_shared_labels() {
        let mut config = Config::default();
        let mut screen = OverlayScreen::new(&config);
        screen.selected = 4;
        screen.apply(&mut config);
        let rows = screen.rows(&config);
        assert!(rows[4].contains(overlay::STREAMING_LABELS[1]));
        screen.apply(&mut config);
        let rows = screen.rows(&config);
        assert!(rows[4].contains(overlay::STREAMING_LABELS[0]));
    }

    #[test]
    fn output_screen_resets_the_backend_when_output_returns_to_keep() {
        let capabilities = test_capabilities();
        let mut config = Config::default();
        config.output.method = OutputMethod::Type;
        config.output.typing_backend = "wtype".to_string();
        let mut screen = OutputScreen::new(&capabilities, &config);
        screen.selected = 0;
        screen.apply(&mut config);
        assert_eq!(config.output.method, OutputMethod::None);
        assert_eq!(config.output.typing_backend, "auto");
    }

    #[test]
    fn typing_backend_screen_starts_on_the_configured_backend() {
        let capabilities = test_capabilities();
        let mut config = Config::default();
        config.output.typing_backend = "enigo".to_string();
        let screen = TypingBackendScreen::new(&capabilities, &config);
        assert_eq!(screen.values[screen.selected], "enigo");
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
        }
    }
}
