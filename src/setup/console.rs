use super::{Capabilities, ModelRequirement, ProvisionHandle, ReconfigureHandle, SetupOutcome};
use crate::audio::{self, level_to_ratio};
use crate::config::{ActivationMode, Config, OutputMethod};
use crate::overlay::{
    POSITION_VALUES as OVERLAY_POSITION_VALUES, STREAMING_LABELS as OVERLAY_STREAMING_LABELS,
    STYLE_LABELS as OVERLAY_STYLE_LABELS, STYLE_VALUES as OVERLAY_STYLE_VALUES,
};
use crossterm::event::{self, Event, KeyCode, KeyEvent, KeyEventKind, KeyModifiers};
use crossterm::execute;
use crossterm::terminal::{
    disable_raw_mode, enable_raw_mode, EnterAlternateScreen, LeaveAlternateScreen,
};
use ratatui::backend::CrosstermBackend;
use ratatui::layout::{Constraint, Direction, Layout, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span, Text};
use ratatui::widgets::{Block, Borders, Gauge, Paragraph};
use ratatui::{Frame, Terminal};
use std::io;
use std::time::Duration;

const LOGO: &str = r#"████████╗ ██████╗ ███╗   ██╗ ██████╗ ██╗   ██╗███████╗████████╗██╗   ██╗██████╗ ███████╗██████╗
╚══██╔══╝██╔═══██╗████╗  ██║██╔════╝ ██║   ██║██╔════╝╚══██╔══╝╚██╗ ██╔╝██╔══██╗██╔════╝██╔══██╗
   ██║   ██║   ██║██╔██╗ ██║██║  ███╗██║   ██║█████╗     ██║    ╚████╔╝ ██████╔╝█████╗  ██║  ██║
   ██║   ██║   ██║██║╚██╗██║██║   ██║██║   ██║██╔══╝     ██║     ╚██╔╝  ██╔═══╝ ██╔══╝  ██║  ██║
   ██║   ╚██████╔╝██║ ╚████║╚██████╔╝╚██████╔╝███████╗   ██║      ██║   ██║     ███████╗██████╔╝
   ╚═╝    ╚═════╝ ╚═╝  ╚═══╝ ╚═════╝  ╚═════╝ ╚══════╝   ╚═╝      ╚═╝   ╚═╝     ╚══════╝╚═════╝

                              ░▒▓  S P E A K .  T Y P E .  R E P E A T .  ▓▒░"#;
const LOGO_HEIGHT: u16 = 8;

const ACTIVATION_LABELS: [&str; 2] = crate::config::ACTIVATION_MODE_LABELS;
const STARTUP_LABELS: [&str; 2] = crate::config::STARTUP_LABELS;
const OVERLAY_ENABLED_LABELS: [&str; 2] = ["Disabled", "Enabled"];

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum StepKind {
    Model,
    Downloading,
    InferenceBackend,
    Microphone,
    Activation,
    Shortcut,
    Output,
    OutputBackend,
    Startup,
    OverlayEnabled,
    OverlayPosition,
    OverlayStyle,
    OverlayStreaming,
    Confirm,
}

enum ControlFlow {
    Continue,
    Finish(SetupOutcome),
}

pub(super) fn run(capabilities: Capabilities) -> anyhow::Result<SetupOutcome> {
    let config = Config::reload()?;
    let mut state = ConsoleState::new(config, capabilities);

    enable_raw_mode()?;
    let mut stdout = io::stdout();
    execute!(stdout, EnterAlternateScreen)?;
    let backend = CrosstermBackend::new(stdout);
    let terminal = Terminal::new(backend)?;
    let mut guard = TerminalGuard { terminal };

    let outcome = state.run_loop(&mut guard.terminal);
    drop(guard);
    outcome
}

struct TerminalGuard {
    terminal: Terminal<CrosstermBackend<io::Stdout>>,
}

impl Drop for TerminalGuard {
    fn drop(&mut self) {
        let _ = disable_raw_mode();
        let _ = execute!(self.terminal.backend_mut(), LeaveAlternateScreen);
    }
}

struct ConsoleState {
    config: Config,
    capabilities: Capabilities,
    step: StepKind,
    model_values: Vec<String>,
    model_selection: usize,
    inference_backend_values: Vec<&'static str>,
    inference_backend_selection: usize,
    mic_selection: usize,
    activation_selection: usize,
    output_labels: Vec<String>,
    output_selection: usize,
    backend_values: Vec<String>,
    backend_selection: usize,
    startup_selection: usize,
    overlay_enabled_selection: usize,
    overlay_position_selection: usize,
    overlay_style_selection: usize,
    overlay_streaming_selection: usize,
    shortcut_input: String,
    shortcut_handle: Option<ReconfigureHandle>,
    shortcut_feedback: Option<Result<String, String>>,
    error: Option<String>,
    mic_monitor: audio::MicMonitor,
    download_queue: Vec<ModelRequirement>,
    download_active_label: Option<String>,
    download_handle: Option<ProvisionHandle>,
    download_log: Vec<(String, Result<crate::model::DownloadOutcome, String>)>,
    downloading_needed: bool,
}

impl ConsoleState {
    fn new(config: Config, capabilities: Capabilities) -> Self {
        let model_values: Vec<String> = crate::catalog::ENTRIES
            .iter()
            .map(|entry| entry.id.to_string())
            .collect();
        let model_selection = model_values
            .iter()
            .position(|name| name == &config.model.active_model)
            .unwrap_or(0);
        // Only choices that can work on this build and host are selectable,
        // with one exception: a configured backend that can't run here stays
        // in the list so the step opens on it and never silently replaces the
        // saved pin with `auto`. It is shown as unavailable.
        let mut inference_backend_values: Vec<&'static str> = capabilities
            .inference_backends
            .iter()
            .filter(|choice| choice.unavailable.is_none())
            .map(|choice| choice.name)
            .collect();
        if let Some(configured) = capabilities.inference_backends.iter().find(|choice| {
            choice.name == config.model.preferred_backend && choice.unavailable.is_some()
        }) {
            inference_backend_values.push(configured.name);
        }
        if inference_backend_values.is_empty() {
            inference_backend_values.push("auto");
        }
        let inference_backend_selection = inference_backend_values
            .iter()
            .position(|name| *name == config.model.preferred_backend)
            .unwrap_or(0);
        let mic_selection = capabilities.microphone_index(&config.audio.microphone);
        let activation_selection = usize::from(config.activation.mode == ActivationMode::Toggle);
        let output_labels = capabilities.output_labels();
        let output_selection =
            usize::from(config.output.method == OutputMethod::Type && output_labels.len() > 1);
        let backend_values = capabilities.typing_backend_values();
        let backend_selection = backend_values
            .iter()
            .position(|backend| backend == &config.output.typing_backend)
            .unwrap_or(0);
        let startup_selection = usize::from(config.startup.autostart);
        let overlay_enabled_selection = usize::from(config.overlay.enabled);
        let overlay_position_selection = OVERLAY_POSITION_VALUES
            .iter()
            .position(|value| *value == config.overlay.position)
            .unwrap_or(2);
        let overlay_style_selection = OVERLAY_STYLE_VALUES
            .iter()
            .position(|value| *value == config.overlay.style)
            .unwrap_or(0);
        let overlay_streaming_selection = usize::from(config.overlay.streaming_indicator);
        let shortcut_input = config.activation.keybind.clone();

        Self {
            config,
            capabilities,
            step: StepKind::Model,
            model_values,
            model_selection,
            inference_backend_values,
            inference_backend_selection,
            mic_selection,
            activation_selection,
            output_labels,
            output_selection,
            backend_values,
            backend_selection,
            startup_selection,
            overlay_enabled_selection,
            overlay_position_selection,
            overlay_style_selection,
            overlay_streaming_selection,
            shortcut_input,
            shortcut_handle: None,
            shortcut_feedback: None,
            error: None,
            mic_monitor: audio::MicMonitor::new(),
            download_queue: Vec::new(),
            download_active_label: None,
            download_handle: None,
            download_log: Vec::new(),
            downloading_needed: false,
        }
    }

    fn backend_unavailable(&self, name: &str) -> bool {
        self.capabilities
            .inference_backends
            .iter()
            .any(|choice| choice.name == name && choice.unavailable.is_some())
    }

    fn run_loop(
        &mut self,
        terminal: &mut Terminal<CrosstermBackend<io::Stdout>>,
    ) -> anyhow::Result<SetupOutcome> {
        loop {
            self.refresh_mic_monitor();
            self.refresh_downloads();
            self.refresh_shortcut_reconfigure();
            terminal.draw(|frame| self.render(frame))?;
            if event::poll(Duration::from_millis(66))? {
                if let Event::Key(key) = event::read()? {
                    if key.kind == KeyEventKind::Press {
                        match self.handle_key(key)? {
                            ControlFlow::Continue => {}
                            ControlFlow::Finish(SetupOutcome::Saved) => {
                                crate::autostart::save_configuration(&self.config)?;
                                return Ok(SetupOutcome::Saved);
                            }
                            ControlFlow::Finish(SetupOutcome::Cancelled) => {
                                return Ok(SetupOutcome::Cancelled);
                            }
                        }
                    }
                }
            }
        }
    }

    fn handle_key(&mut self, key: KeyEvent) -> anyhow::Result<ControlFlow> {
        if key.code == KeyCode::Char('c') && key.modifiers.contains(KeyModifiers::CONTROL) {
            return Ok(ControlFlow::Finish(SetupOutcome::Cancelled));
        }
        match self.step {
            StepKind::Shortcut => self.handle_shortcut_key(key),
            StepKind::Confirm => self.handle_confirm_key(key),
            StepKind::Downloading => self.handle_downloading_key(key),
            _ => self.handle_list_key(key),
        }
    }

    fn downloads_finished(&self) -> bool {
        self.download_handle.is_none() && self.download_queue.is_empty()
    }

    fn handle_downloading_key(&mut self, key: KeyEvent) -> anyhow::Result<ControlFlow> {
        match key.code {
            KeyCode::Char('q') => return Ok(ControlFlow::Finish(SetupOutcome::Cancelled)),
            KeyCode::Esc | KeyCode::Left | KeyCode::Char('h') => return Ok(self.retreat()),
            KeyCode::Enter | KeyCode::Right | KeyCode::Char('l') if self.downloads_finished() => {
                return self.advance();
            }
            _ => {}
        }
        Ok(ControlFlow::Continue)
    }

    fn handle_list_key(&mut self, key: KeyEvent) -> anyhow::Result<ControlFlow> {
        match key.code {
            KeyCode::Char('q') => return Ok(ControlFlow::Finish(SetupOutcome::Cancelled)),
            KeyCode::Up | KeyCode::Char('k') => self.move_selection(-1),
            KeyCode::Down | KeyCode::Char('j') => self.move_selection(1),
            KeyCode::Enter | KeyCode::Right | KeyCode::Char('l') => return self.advance(),
            KeyCode::Esc | KeyCode::Left | KeyCode::Char('h') => return Ok(self.retreat()),
            _ => {}
        }
        Ok(ControlFlow::Continue)
    }

    fn handle_shortcut_key(&mut self, key: KeyEvent) -> anyhow::Result<ControlFlow> {
        if self.shortcut_handle.is_some() {
            // The background thread driving the native dialog isn't
            // cancellable, but leaving the step is still allowed - same
            // permissiveness `handle_downloading_key` already gives an
            // in-flight download. The thread finishes on its own; its
            // result is simply never polled again.
            return match key.code {
                KeyCode::Esc => Ok(self.retreat()),
                _ => Ok(ControlFlow::Continue),
            };
        }
        if key.code == KeyCode::Char('r') && key.modifiers.contains(KeyModifiers::CONTROL) {
            self.start_shortcut_reconfigure();
            return Ok(ControlFlow::Continue);
        }
        match key.code {
            KeyCode::Enter => return self.advance(),
            KeyCode::Esc => return Ok(self.retreat()),
            KeyCode::Backspace => {
                self.shortcut_input.pop();
            }
            KeyCode::Char(c) => {
                self.shortcut_input.push(c);
            }
            _ => {}
        }
        Ok(ControlFlow::Continue)
    }

    fn handle_confirm_key(&mut self, key: KeyEvent) -> anyhow::Result<ControlFlow> {
        Ok(match key.code {
            KeyCode::Enter | KeyCode::Char('y') | KeyCode::Char('Y') => {
                ControlFlow::Finish(SetupOutcome::Saved)
            }
            KeyCode::Char('n') | KeyCode::Char('N') | KeyCode::Char('q') => {
                ControlFlow::Finish(SetupOutcome::Cancelled)
            }
            KeyCode::Esc | KeyCode::Left => self.retreat(),
            _ => ControlFlow::Continue,
        })
    }

    fn move_selection(&mut self, delta: i32) {
        let len = self.current_options_len() as i32;
        if len == 0 {
            return;
        }
        let previous = *self.current_index_mut();
        let next = (previous as i32 + delta).clamp(0, len - 1) as usize;
        *self.current_index_mut() = next;
        if self.step == StepKind::Microphone && next != previous {
            self.defer_mic_monitor();
        }
    }

    fn current_options_len(&self) -> usize {
        match self.step {
            StepKind::Model => self.model_values.len(),
            StepKind::InferenceBackend => self.inference_backend_values.len(),
            StepKind::Microphone => self.capabilities.microphones.len(),
            StepKind::Activation => ACTIVATION_LABELS.len(),
            StepKind::Output => self.output_labels.len(),
            StepKind::OutputBackend => self.backend_values.len(),
            StepKind::Startup => STARTUP_LABELS.len(),
            StepKind::OverlayEnabled => OVERLAY_ENABLED_LABELS.len(),
            StepKind::OverlayPosition => OVERLAY_POSITION_VALUES.len(),
            StepKind::OverlayStyle => OVERLAY_STYLE_VALUES.len(),
            StepKind::OverlayStreaming => OVERLAY_STREAMING_LABELS.len(),
            StepKind::Shortcut | StepKind::Confirm | StepKind::Downloading => 0,
        }
    }

    fn current_index_mut(&mut self) -> &mut usize {
        match self.step {
            StepKind::Model => &mut self.model_selection,
            StepKind::InferenceBackend => &mut self.inference_backend_selection,
            StepKind::Microphone => &mut self.mic_selection,
            StepKind::Activation => &mut self.activation_selection,
            StepKind::Output => &mut self.output_selection,
            StepKind::OutputBackend => &mut self.backend_selection,
            StepKind::Startup => &mut self.startup_selection,
            StepKind::OverlayEnabled => &mut self.overlay_enabled_selection,
            StepKind::OverlayPosition => &mut self.overlay_position_selection,
            StepKind::OverlayStyle => &mut self.overlay_style_selection,
            StepKind::OverlayStreaming => &mut self.overlay_streaming_selection,
            StepKind::Shortcut | StepKind::Confirm | StepKind::Downloading => {
                unreachable!("no list selection for this step")
            }
        }
    }

    fn has_backend_step(&self) -> bool {
        self.output_selection == 1 && !self.capabilities.typing_backends.is_empty()
    }

    /// The overlay's position/streaming sub-steps are only worth configuring
    /// once the overlay itself is enabled - mirrors `has_backend_step`'s
    /// pattern of skipping a sub-step whose parent choice ruled it out.
    fn overlay_options_shown(&self) -> bool {
        self.overlay_enabled_selection == 1
    }

    /// The full ordered list of steps this run would actually visit, given
    /// the current conditional choices (model download queue, output
    /// backend, overlay enabled). Deriving `next_step`/`prev_step`/
    /// `step_index`/`step_total` from one sequence keeps them from drifting
    /// out of sync as conditional steps are added.
    fn step_sequence(&self) -> Vec<StepKind> {
        use StepKind::*;
        let mut steps = vec![Model];
        if self.downloading_needed {
            steps.push(Downloading);
        }
        steps.push(InferenceBackend);
        steps.push(Microphone);
        steps.push(Activation);
        steps.push(Shortcut);
        steps.push(Output);
        if self.has_backend_step() {
            steps.push(OutputBackend);
        }
        steps.push(Startup);
        steps.push(OverlayEnabled);
        if self.overlay_options_shown() {
            steps.push(OverlayPosition);
            steps.push(OverlayStyle);
            steps.push(OverlayStreaming);
        }
        steps.push(Confirm);
        steps
    }

    fn next_step(&self) -> Option<StepKind> {
        let sequence = self.step_sequence();
        let index = sequence.iter().position(|step| *step == self.step)?;
        sequence.get(index + 1).copied()
    }

    fn prev_step(&self) -> Option<StepKind> {
        let sequence = self.step_sequence();
        let index = sequence.iter().position(|step| *step == self.step)?;
        index.checked_sub(1).and_then(|i| sequence.get(i).copied())
    }

    fn advance(&mut self) -> anyhow::Result<ControlFlow> {
        match self.step {
            StepKind::Model => {
                self.config.model.active_model = self.model_values[self.model_selection].clone();
                self.download_queue = super::model_requirements(&self.config)
                    .into_iter()
                    .filter(|requirement| !requirement.already_present)
                    .collect();
                self.download_log.clear();
                self.downloading_needed = !self.download_queue.is_empty();
                if self.downloading_needed {
                    self.start_next_download();
                }
            }
            StepKind::InferenceBackend => {
                let selected = self.inference_backend_values[self.inference_backend_selection];
                if !self.backend_unavailable(selected) {
                    self.config.model.preferred_backend = selected.to_string();
                }
            }
            StepKind::Microphone => {
                self.config.audio.microphone =
                    self.capabilities.microphones[self.mic_selection].0.clone();
            }
            StepKind::Activation => {
                self.config.activation.mode = if self.activation_selection == 0 {
                    ActivationMode::Hold
                } else {
                    ActivationMode::Toggle
                };
            }
            StepKind::Shortcut => {
                let shortcut = self.shortcut_input.trim().to_string();
                match crate::activation::portal_trigger(&shortcut) {
                    Ok(_) => {
                        // Only invalidate a known-real binding (learned from
                        // a completed reconfigure, see
                        // `refresh_shortcut_reconfigure`) when the typed
                        // value actually changed - otherwise leave it so the
                        // Confirm screen keeps showing the real trigger
                        // instead of reverting to "untested".
                        if shortcut != self.config.activation.keybind {
                            self.config.activation.keybind_status = "untested".to_string();
                        }
                        self.config.activation.keybind = shortcut;
                    }
                    Err(err) => {
                        self.error = Some(format!("Invalid shortcut: {err}"));
                        return Ok(ControlFlow::Continue);
                    }
                }
            }
            StepKind::Output => {
                if self.output_selection == 0 {
                    self.config.output.method = OutputMethod::None;
                    self.config.output.typing_backend = "auto".to_string();
                } else {
                    self.config.output.method = OutputMethod::Type;
                }
            }
            StepKind::OutputBackend => {
                self.config.output.typing_backend =
                    self.backend_values[self.backend_selection].clone();
            }
            StepKind::Startup => {
                self.config.startup.autostart = self.startup_selection == 1;
                self.config.validate()?;
            }
            StepKind::OverlayEnabled => {
                self.config.overlay.enabled = self.overlay_enabled_selection == 1;
            }
            StepKind::OverlayPosition => {
                self.config.overlay.position =
                    OVERLAY_POSITION_VALUES[self.overlay_position_selection].to_string();
            }
            StepKind::OverlayStyle => {
                self.config.overlay.style =
                    OVERLAY_STYLE_VALUES[self.overlay_style_selection].to_string();
            }
            StepKind::OverlayStreaming => {
                self.config.overlay.streaming_indicator = self.overlay_streaming_selection == 1;
            }
            StepKind::Downloading => {}
            StepKind::Confirm => unreachable!("confirm handled separately"),
        }
        self.error = None;
        if let Some(next) = self.next_step() {
            self.step = next;
            self.sync_mic_monitor();
        }
        Ok(ControlFlow::Continue)
    }

    fn retreat(&mut self) -> ControlFlow {
        match self.prev_step() {
            Some(step) => {
                self.step = step;
                self.error = None;
                self.sync_mic_monitor();
                ControlFlow::Continue
            }
            None => ControlFlow::Finish(SetupOutcome::Cancelled),
        }
    }

    /// Live VU meter follows whichever microphone is currently highlighted in the
    /// list, not just the one last confirmed, so switching the selection is audible
    /// feedback before the user commits to it.
    fn sync_mic_monitor(&mut self) {
        if self.step != StepKind::Microphone {
            self.mic_monitor.stop();
            return;
        }
        let device_name = self.capabilities.microphones[self.mic_selection].0.clone();
        self.mic_monitor.sync(self.mic_selection, &device_name);
    }

    fn defer_mic_monitor(&mut self) {
        self.mic_monitor.defer_restart();
    }

    fn refresh_mic_monitor(&mut self) {
        if self.mic_monitor.poll() {
            self.sync_mic_monitor();
        }
    }

    fn start_next_download(&mut self) {
        if self.download_queue.is_empty() {
            self.download_active_label = None;
            return;
        }
        let requirement = self.download_queue.remove(0);
        self.download_active_label = Some(requirement.label.clone());
        self.download_handle = Some(super::provision_model_async(requirement));
    }

    fn refresh_downloads(&mut self) {
        let Some(handle) = &self.download_handle else {
            return;
        };
        let finished = handle.result.lock().ok().and_then(|mut guard| guard.take());
        let Some(outcome) = finished else {
            return;
        };
        let label = self.download_active_label.take().unwrap_or_default();
        self.download_log.push((label, outcome));
        self.download_handle = None;
        self.start_next_download();
    }

    /// Starts the portal's native "press your new shortcut" dialog in the
    /// background (`setup::reconfigure_shortcut_async`) - the only way to
    /// actually change the "activation" shortcut once it has ever been
    /// bound before, since the desktop ignores `preferred_trigger` after
    /// that (see `activation::bind_activation_shortcut`'s doc comment).
    fn start_shortcut_reconfigure(&mut self) {
        if self.shortcut_handle.is_some() {
            return;
        }
        let shortcut = self.shortcut_input.trim().to_string();
        if let Err(err) = crate::activation::portal_trigger(&shortcut) {
            self.error = Some(format!("Invalid shortcut: {err}"));
            return;
        }
        self.error = None;
        self.shortcut_feedback = None;
        self.shortcut_handle = Some(super::reconfigure_shortcut_async(shortcut));
    }

    /// Polled every render tick while a reconfigure dialog is in flight,
    /// same shape as `refresh_downloads`/`refresh_mic_monitor` above.
    fn refresh_shortcut_reconfigure(&mut self) {
        let Some(handle) = &self.shortcut_handle else {
            return;
        };
        let finished = handle.result.lock().ok().and_then(|mut guard| guard.take());
        let Some(outcome) = finished else {
            return;
        };
        self.shortcut_handle = None;
        if let Ok(trigger_description) = &outcome {
            self.config.activation.keybind_status = trigger_description.clone();
        }
        self.shortcut_feedback = Some(outcome);
    }

    fn step_title(&self) -> &'static str {
        match self.step {
            StepKind::Model => "Speech model",
            StepKind::Downloading => "Fetching speech model",
            StepKind::InferenceBackend => "Inference backend",
            StepKind::Microphone => "Microphone",
            StepKind::Activation => "Activation",
            StepKind::Shortcut => "Shortcut",
            StepKind::Output => "Transcript output",
            StepKind::OutputBackend => "Typing backend",
            StepKind::Startup => "Startup",
            StepKind::OverlayEnabled => "Overlay",
            StepKind::OverlayPosition => "Overlay position",
            StepKind::OverlayStyle => "Overlay style",
            StepKind::OverlayStreaming => "Overlay streaming",
            StepKind::Confirm => "Review",
        }
    }

    fn step_index(&self) -> usize {
        let sequence = self.step_sequence();
        sequence
            .iter()
            .position(|step| *step == self.step)
            .map(|index| index + 1)
            .unwrap_or(1)
    }

    fn step_total(&self) -> usize {
        self.step_sequence().len()
    }

    fn render(&self, frame: &mut Frame) {
        let area = frame.area();
        let chunks = Layout::default()
            .direction(Direction::Vertical)
            .constraints([
                Constraint::Length(LOGO_HEIGHT),
                Constraint::Length(1),
                Constraint::Min(5),
                Constraint::Length(1),
            ])
            .split(area);

        frame.render_widget(
            Paragraph::new(Text::styled(
                LOGO,
                Style::default()
                    .fg(Color::Cyan)
                    .add_modifier(Modifier::BOLD),
            )),
            chunks[0],
        );
        frame.render_widget(
            Paragraph::new(format!(
                "Step {} of {}: {}",
                self.step_index(),
                self.step_total(),
                self.step_title()
            )),
            chunks[1],
        );
        self.render_body(frame, chunks[2]);

        let footer = if let Some(error) = &self.error {
            Line::from(Span::styled(error.clone(), Style::default().fg(Color::Red)))
        } else {
            Line::from(self.hint_text())
        };
        frame.render_widget(Paragraph::new(footer), chunks[3]);
    }

    fn hint_text(&self) -> &'static str {
        match self.step {
            StepKind::Shortcut if self.shortcut_handle.is_some() => {
                "Waiting for the system shortcut dialog...  Esc back"
            }
            StepKind::Shortcut => {
                "Type key or modifiers+key (e.g. Ctrl+Shift+Space, F13)  Ctrl+R set via system dialog  Enter confirm  Esc back"
            }
            StepKind::Confirm => "Enter/y save  n/q discard  Esc back",
            StepKind::Downloading if !self.downloads_finished() => "Fetching...  Esc back  q quit",
            StepKind::Downloading => "Enter continue  Esc back  q quit",
            _ => "↑/↓ choose  Enter continue  Esc back  q quit",
        }
    }

    fn render_body(&self, frame: &mut Frame, area: Rect) {
        match self.step {
            StepKind::Model => frame.render_widget(
                list_paragraph(
                    &self.model_values,
                    self.model_selection,
                    self.step_title(),
                    area.height,
                ),
                area,
            ),
            StepKind::Downloading => self.render_downloading(frame, area),
            StepKind::InferenceBackend => self.render_inference_backend(frame, area),
            StepKind::Microphone => self.render_microphone(frame, area),
            StepKind::Activation => frame.render_widget(
                list_paragraph(
                    &ACTIVATION_LABELS.map(String::from),
                    self.activation_selection,
                    self.step_title(),
                    area.height,
                ),
                area,
            ),
            StepKind::Shortcut => self.render_shortcut(frame, area),
            StepKind::Output => frame.render_widget(
                list_paragraph(
                    &self.output_labels,
                    self.output_selection,
                    self.step_title(),
                    area.height,
                ),
                area,
            ),
            StepKind::OutputBackend => frame.render_widget(
                list_paragraph(
                    &self.backend_values,
                    self.backend_selection,
                    self.step_title(),
                    area.height,
                ),
                area,
            ),
            StepKind::Startup => frame.render_widget(
                list_paragraph(
                    &STARTUP_LABELS.map(String::from),
                    self.startup_selection,
                    self.step_title(),
                    area.height,
                ),
                area,
            ),
            StepKind::OverlayEnabled => frame.render_widget(
                list_paragraph(
                    &OVERLAY_ENABLED_LABELS.map(String::from),
                    self.overlay_enabled_selection,
                    self.step_title(),
                    area.height,
                ),
                area,
            ),
            StepKind::OverlayPosition => frame.render_widget(
                list_paragraph(
                    &OVERLAY_POSITION_VALUES.map(String::from),
                    self.overlay_position_selection,
                    self.step_title(),
                    area.height,
                ),
                area,
            ),
            StepKind::OverlayStyle => frame.render_widget(
                list_paragraph(
                    &OVERLAY_STYLE_LABELS.map(String::from),
                    self.overlay_style_selection,
                    self.step_title(),
                    area.height,
                ),
                area,
            ),
            StepKind::OverlayStreaming => frame.render_widget(
                list_paragraph(
                    &OVERLAY_STREAMING_LABELS.map(String::from),
                    self.overlay_streaming_selection,
                    self.step_title(),
                    area.height,
                ),
                area,
            ),
            StepKind::Confirm => self.render_confirm(frame, area),
        }
    }

    fn render_downloading(&self, frame: &mut Frame, area: Rect) {
        let mut lines: Vec<Line> = self
            .download_log
            .iter()
            .map(|(label, outcome)| match outcome {
                Ok(outcome) => Line::from(Span::styled(
                    format!("  {label}: {}", super::outcome_label(*outcome)),
                    Style::default().fg(Color::Green),
                )),
                Err(error) => Line::from(Span::styled(
                    format!("  {label}: failed - {error}"),
                    Style::default().fg(Color::Red),
                )),
            })
            .collect();
        if let Some(label) = &self.download_active_label {
            lines.push(Line::from(format!("> Fetching {label}...")));
        } else if self.downloads_finished() {
            lines.push(Line::from(Span::styled(
                "All required files are ready.",
                Style::default().fg(Color::Green),
            )));
        }

        let chunks = Layout::default()
            .direction(Direction::Vertical)
            .constraints([Constraint::Min(3), Constraint::Length(3)])
            .split(area);
        frame.render_widget(
            Paragraph::new(lines).block(
                Block::default()
                    .borders(Borders::ALL)
                    .title(self.step_title()),
            ),
            chunks[0],
        );

        let (downloaded, total) = self
            .download_handle
            .as_ref()
            .and_then(|handle| handle.progress.lock().ok().map(|guard| *guard))
            .unwrap_or((0, 0));
        let ratio = if total == 0 {
            0.0
        } else {
            (downloaded as f64 / total as f64).clamp(0.0, 1.0)
        };
        let gauge = Gauge::default()
            .block(Block::default().borders(Borders::ALL).title("Progress"))
            .gauge_style(Style::default().fg(Color::Cyan))
            .ratio(ratio);
        frame.render_widget(gauge, chunks[1]);
    }

    fn render_inference_backend(&self, frame: &mut Frame, area: Rect) {
        // A configured backend that can't run here is shown in the list above
        // (marked unavailable); the panel below only explains the rest.
        let unavailable: Vec<Line> = self
            .capabilities
            .inference_backends
            .iter()
            .filter(|choice| !self.inference_backend_values.contains(&choice.name))
            .filter_map(|choice| {
                choice.unavailable.as_ref().map(|reason| {
                    Line::from(Span::styled(
                        format!("  {}: {reason}", choice.name),
                        Style::default().fg(Color::DarkGray),
                    ))
                })
            })
            .collect();
        let chunks = Layout::default()
            .direction(Direction::Vertical)
            .constraints([
                Constraint::Min(3),
                Constraint::Length(if unavailable.is_empty() {
                    0
                } else {
                    unavailable.len() as u16 + 2
                }),
            ])
            .split(area);

        let labels: Vec<String> = self
            .inference_backend_values
            .iter()
            .map(|name| {
                let label = crate::inference::backend_preference_label(name);
                if self.backend_unavailable(name) {
                    format!("{label} (unavailable here)")
                } else {
                    label.to_string()
                }
            })
            .collect();
        frame.render_widget(
            list_paragraph(
                &labels,
                self.inference_backend_selection,
                self.step_title(),
                chunks[0].height,
            ),
            chunks[0],
        );
        if !unavailable.is_empty() {
            frame.render_widget(
                Paragraph::new(unavailable).block(
                    Block::default()
                        .borders(Borders::ALL)
                        .title("Not available here"),
                ),
                chunks[1],
            );
        }
    }

    fn render_microphone(&self, frame: &mut Frame, area: Rect) {
        let chunks = Layout::default()
            .direction(Direction::Vertical)
            .constraints([Constraint::Min(3), Constraint::Length(3)])
            .split(area);

        let labels: Vec<String> = self
            .capabilities
            .microphones
            .iter()
            .map(|(_, label)| label.clone())
            .collect();
        frame.render_widget(
            list_paragraph(
                &labels,
                self.mic_selection,
                self.step_title(),
                chunks[0].height,
            ),
            chunks[0],
        );

        if let Some(error) = self.mic_monitor.error() {
            frame.render_widget(
                Paragraph::new(Span::styled(
                    format!("Unavailable: {error}"),
                    Style::default().fg(Color::Red),
                ))
                .block(Block::default().borders(Borders::ALL).title("Input level")),
                chunks[1],
            );
            return;
        }

        let level = self.mic_monitor.level();
        let ratio = level_to_ratio(level);
        let color = if ratio > 0.85 {
            Color::Red
        } else if ratio > 0.5 {
            Color::Yellow
        } else {
            Color::Green
        };
        let gauge = Gauge::default()
            .block(Block::default().borders(Borders::ALL).title("Input level"))
            .gauge_style(Style::default().fg(color))
            .ratio(ratio);
        frame.render_widget(gauge, chunks[1]);
    }

    fn render_shortcut(&self, frame: &mut Frame, area: Rect) {
        let mut lines = vec![Line::from(format!("Shortcut: {}_", self.shortcut_input))];
        if self.config.activation.keybind_status != "untested" {
            lines.push(Line::from(format!(
                "Currently bound: {}",
                self.config.activation.keybind_status
            )));
        }
        lines.push(Line::from(""));
        if self.shortcut_handle.is_some() {
            lines.push(Line::from(Span::styled(
                "A system dialog is open - press your new shortcut there now.",
                Style::default().fg(Color::Yellow),
            )));
        } else if let Some(feedback) = &self.shortcut_feedback {
            match feedback {
                Ok(trigger_description) => lines.push(Line::from(Span::styled(
                    format!("Shortcut bound: {trigger_description}"),
                    Style::default().fg(Color::Green),
                ))),
                Err(error) => lines.push(Line::from(Span::styled(
                    format!("Reconfigure failed: {error}"),
                    Style::default().fg(Color::Red),
                ))),
            }
        } else {
            lines.push(Line::from(
                "Type a key, optionally with modifiers (Ctrl, Alt, Shift, Super), e.g. \
                 Ctrl+Shift+Space, or a bare key such as F13 or Alt_R (Right Alt) - \
                 only used the first time this shortcut is ever bound.",
            ));
            lines.push(Line::from(
                "Already bound before? Ctrl+R opens your desktop's own shortcut dialog so you \
                 can set the real trigger.",
            ));
        }
        frame.render_widget(
            Paragraph::new(lines).block(
                Block::default()
                    .borders(Borders::ALL)
                    .title(self.step_title()),
            ),
            area,
        );
    }

    fn render_confirm(&self, frame: &mut Frame, area: Rect) {
        let lines = vec![
            Line::from(format!("Model:       {}", self.config.model.active_model)),
            Line::from(format!(
                "Backend:     {}",
                crate::inference::backend_preference_label(&self.config.model.preferred_backend)
            )),
            Line::from(format!("Microphone:  {}", self.config.audio.microphone)),
            Line::from(format!(
                "Activation:  {} with {}",
                self.config.activation.mode,
                if self.config.activation.keybind_status == "untested" {
                    self.config.activation.keybind.as_str()
                } else {
                    self.config.activation.keybind_status.as_str()
                }
            )),
            Line::from(format!("Output:      {}", self.config.output.method)),
            Line::from(format!(
                "Autostart:   {}",
                if self.config.startup.autostart {
                    "yes"
                } else {
                    "no"
                }
            )),
            Line::from(format!(
                "Overlay:     {}",
                if self.config.overlay.enabled {
                    format!(
                        "enabled, {}, {} style, {}",
                        self.config.overlay.position,
                        self.config.overlay.style,
                        if self.config.overlay.streaming_indicator {
                            "streaming waveform"
                        } else {
                            "simple pulse"
                        }
                    )
                } else {
                    "disabled".to_string()
                }
            )),
            Line::from(""),
            Line::from("Write this configuration?"),
        ];
        frame.render_widget(
            Paragraph::new(lines).block(
                Block::default()
                    .borders(Borders::ALL)
                    .title(self.step_title()),
            ),
            area,
        );
    }
}

fn list_paragraph<'a>(
    labels: &[String],
    selected: usize,
    title: &'a str,
    height: u16,
) -> Paragraph<'a> {
    let lines: Vec<Line> = labels
        .iter()
        .enumerate()
        .map(|(index, label)| {
            let marker = if index == selected { "> " } else { "  " };
            let text = format!("{marker}{label}");
            let style = if index == selected {
                Style::default()
                    .fg(Color::Cyan)
                    .add_modifier(Modifier::BOLD)
            } else {
                Style::default()
            };
            Line::from(Span::styled(text, style))
        })
        .collect();
    let visible_rows = usize::from(height.saturating_sub(2)).max(1);
    let scroll = selected.saturating_sub(visible_rows - 1) as u16;
    Paragraph::new(lines)
        .block(Block::default().borders(Borders::ALL).title(title))
        .scroll((scroll, 0))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::{Arc, Mutex};

    fn no_mic_capabilities() -> Capabilities {
        Capabilities {
            microphones: vec![(
                "default".to_string(),
                "System default microphone".to_string(),
            )],
            typing_backends: Vec::new(),
            inference_backends: Vec::new(),
        }
    }

    #[test]
    fn completed_download_records_outcome_and_starts_the_next_queued_requirement() {
        let mut state = ConsoleState::new(Config::default(), no_mic_capabilities());
        state.step = StepKind::Downloading;
        state.downloading_needed = true;
        state.download_active_label = Some("first-model".to_string());
        state.download_handle = Some(ProvisionHandle {
            progress: Arc::new(Mutex::new((10, 10))),
            result: Arc::new(Mutex::new(Some(Ok(crate::model::DownloadOutcome::Fresh)))),
        });
        state.download_queue = vec![crate::setup::ModelRequirement {
            label: "second-model".to_string(),
            already_present: false,
            id: "whisper-small-q5_k_m".to_string(),
        }];

        state.refresh_downloads();

        assert_eq!(
            state.download_log,
            vec![(
                "first-model".to_string(),
                Ok(crate::model::DownloadOutcome::Fresh)
            )]
        );
        assert_eq!(
            state.download_active_label,
            Some("second-model".to_string())
        );
        assert!(state.download_queue.is_empty());
        assert!(!state.downloads_finished());
    }

    #[test]
    fn download_failure_is_recorded_and_enter_waits_for_the_queue_to_drain() {
        let mut state = ConsoleState::new(Config::default(), no_mic_capabilities());
        state.step = StepKind::Downloading;
        state.downloading_needed = true;
        state.download_active_label = Some("flaky-model".to_string());
        state.download_handle = Some(ProvisionHandle {
            progress: Arc::new(Mutex::new((0, 0))),
            result: Arc::new(Mutex::new(Some(Err("network unreachable".to_string())))),
        });

        let enter = KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE);
        assert!(!state.downloads_finished());
        state.handle_downloading_key(enter).unwrap();
        assert_eq!(
            state.step,
            StepKind::Downloading,
            "Enter must not skip past an in-flight download"
        );

        state.refresh_downloads();
        assert!(state.downloads_finished());
        assert_eq!(
            state.download_log,
            vec![(
                "flaky-model".to_string(),
                Err("network unreachable".to_string())
            )]
        );

        state.handle_downloading_key(enter).unwrap();
        assert_eq!(
            state.step,
            StepKind::InferenceBackend,
            "a failed download is non-fatal - the user can still continue setup"
        );
    }

    #[test]
    fn overlay_steps_are_skipped_when_the_overlay_is_disabled() {
        let mut state = ConsoleState::new(Config::default(), no_mic_capabilities());
        state.step = StepKind::OverlayEnabled;
        state.overlay_enabled_selection = 0;

        state.advance().unwrap();
        assert!(!state.config.overlay.enabled);
        assert_eq!(
            state.step,
            StepKind::Confirm,
            "disabling the overlay should skip straight to Confirm, not Position/Streaming"
        );
    }

    #[test]
    fn enabling_the_overlay_walks_through_position_style_and_streaming_before_confirm() {
        let mut state = ConsoleState::new(Config::default(), no_mic_capabilities());
        state.step = StepKind::OverlayEnabled;
        state.overlay_enabled_selection = 1;
        state.advance().unwrap();
        assert_eq!(state.step, StepKind::OverlayPosition);

        state.overlay_position_selection = OVERLAY_POSITION_VALUES
            .iter()
            .position(|value| *value == "bottom-left")
            .unwrap();
        state.advance().unwrap();
        assert_eq!(state.config.overlay.position, "bottom-left");
        assert_eq!(state.step, StepKind::OverlayStyle);

        state.overlay_style_selection = OVERLAY_STYLE_VALUES
            .iter()
            .position(|value| *value == "pill")
            .unwrap();
        state.advance().unwrap();
        assert_eq!(state.config.overlay.style, "pill");
        assert_eq!(state.step, StepKind::OverlayStreaming);

        state.overlay_streaming_selection = 1;
        state.advance().unwrap();
        assert!(state.config.overlay.streaming_indicator);
        assert_eq!(state.step, StepKind::Confirm);
    }

    #[test]
    fn retreating_from_confirm_returns_to_streaming_when_overlay_is_enabled_and_to_overlay_enabled_otherwise(
    ) {
        let mut enabled_state = ConsoleState::new(Config::default(), no_mic_capabilities());
        enabled_state.step = StepKind::Confirm;
        enabled_state.overlay_enabled_selection = 1;
        assert_eq!(enabled_state.prev_step(), Some(StepKind::OverlayStreaming));

        let mut disabled_state = ConsoleState::new(Config::default(), no_mic_capabilities());
        disabled_state.step = StepKind::Confirm;
        disabled_state.overlay_enabled_selection = 0;
        assert_eq!(disabled_state.prev_step(), Some(StepKind::OverlayEnabled));
    }

    #[test]
    fn leaving_the_model_step_skips_downloading_when_nothing_is_missing() {
        let mut state = ConsoleState::new(Config::default(), no_mic_capabilities());
        state.step = StepKind::Model;
        // The real download queue only ever contains entries whose files are
        // missing (see `model_requirements`); emulate "nothing missing" by
        // starting from an already-empty queue rather than touching the
        // filesystem or network in this test.
        state.downloading_needed = false;

        assert_eq!(state.next_step(), Some(StepKind::InferenceBackend));
    }

    fn capabilities_with_backends(backends: &[(&'static str, Option<&str>)]) -> Capabilities {
        let mut capabilities = no_mic_capabilities();
        capabilities.inference_backends = backends
            .iter()
            .map(|(name, reason)| crate::inference::BackendChoice {
                name,
                unavailable: reason.map(String::from),
            })
            .collect();
        capabilities
    }

    #[test]
    fn inference_backend_step_offers_only_usable_backends_and_saves_the_choice() {
        let capabilities = capabilities_with_backends(&[
            ("auto", None),
            ("cpu", None),
            ("vulkan", None),
            (
                "cuda",
                Some("this build was compiled without the `gpu-cuda` feature"),
            ),
        ]);
        let mut state = ConsoleState::new(Config::default(), capabilities);
        assert_eq!(
            state.inference_backend_values,
            vec!["auto", "cpu", "vulkan"]
        );
        assert_eq!(state.inference_backend_selection, 0);

        state.step = StepKind::InferenceBackend;
        state.move_selection(1);
        state.advance().unwrap();
        assert_eq!(state.config.model.preferred_backend, "cpu");
        assert_eq!(state.step, StepKind::Microphone);
        assert_eq!(state.prev_step(), Some(StepKind::InferenceBackend));
    }

    fn capabilities_with_unavailable_cuda() -> Capabilities {
        capabilities_with_backends(&[
            ("auto", None),
            ("cpu", None),
            (
                "cuda",
                Some("no usable cuda device or driver was found on this host"),
            ),
        ])
    }

    #[test]
    fn a_configured_backend_that_is_unavailable_here_stays_selected_and_is_not_overwritten() {
        let mut config = Config::default();
        config.model.preferred_backend = "cuda".to_string();
        let mut state = ConsoleState::new(config, capabilities_with_unavailable_cuda());
        assert_eq!(
            state.inference_backend_values[state.inference_backend_selection],
            "cuda"
        );

        // Stepping through the backend step without changing anything must
        // leave the saved pin alone rather than replacing it with `auto`.
        state.step = StepKind::InferenceBackend;
        state.advance().unwrap();
        assert_eq!(state.config.model.preferred_backend, "cuda");

        // Choosing a backend that does work here still saves normally.
        let mut config = Config::default();
        config.model.preferred_backend = "cuda".to_string();
        let mut state = ConsoleState::new(config, capabilities_with_unavailable_cuda());
        state.step = StepKind::InferenceBackend;
        state.move_selection(-1);
        state.advance().unwrap();
        assert_eq!(state.config.model.preferred_backend, "cpu");

        let mut config = Config::default();
        config.model.preferred_backend = "cpu".to_string();
        let state = ConsoleState::new(
            config,
            capabilities_with_backends(&[("auto", None), ("cpu", None)]),
        );
        assert_eq!(
            state.inference_backend_values[state.inference_backend_selection],
            "cpu"
        );
    }

    #[test]
    fn rapid_mic_navigation_defers_preview_restart_until_selection_settles() {
        let capabilities = Capabilities {
            microphones: vec![
                ("test-mic-1".to_string(), "Test microphone 1".to_string()),
                ("test-mic-2".to_string(), "Test microphone 2".to_string()),
                ("test-mic-3".to_string(), "Test microphone 3".to_string()),
            ],
            typing_backends: Vec::new(),
            inference_backends: Vec::new(),
        };
        let mut state = ConsoleState::new(Config::default(), capabilities);
        state.step = StepKind::Microphone;
        state.mic_monitor.simulate_active_preview(0);

        state.move_selection(1);
        state.move_selection(1);

        assert_eq!(state.mic_selection, 2);
        assert_eq!(state.mic_monitor.active_index(), None);
        assert!(state.mic_monitor.restart_pending());
    }

    #[test]
    fn async_stream_error_is_routed_into_the_mic_error_panel_instead_of_the_terminal() {
        let capabilities = Capabilities {
            microphones: vec![("test-mic-1".to_string(), "Test microphone 1".to_string())],
            typing_backends: Vec::new(),
            inference_backends: Vec::new(),
        };
        let mut state = ConsoleState::new(Config::default(), capabilities);
        state.step = StepKind::Microphone;
        state.mic_monitor.simulate_active_preview(0);

        // Simulate what AudioRecorder's error_callback does from its background
        // audio thread when a device disconnects mid-stream.
        state.mic_monitor.inject_stream_error("Device disconnected");

        state.refresh_mic_monitor();

        assert_eq!(state.mic_monitor.error(), Some("Device disconnected"));
        assert!(!state.mic_monitor.has_open_stream());
        assert!(!state.mic_monitor.has_pending_stream_error());
    }

    #[test]
    fn switching_mic_selection_discards_the_previous_device_stream_error() {
        let capabilities = Capabilities {
            microphones: vec![
                ("test-mic-1".to_string(), "Test microphone 1".to_string()),
                ("test-mic-2".to_string(), "Test microphone 2".to_string()),
            ],
            typing_backends: Vec::new(),
            inference_backends: Vec::new(),
        };
        let mut state = ConsoleState::new(Config::default(), capabilities);
        state.step = StepKind::Microphone;
        state.mic_monitor.simulate_active_preview(0);

        // Device 0 reports an async error from its background audio thread, but
        // the user navigates to device 1 before refresh_mic_monitor() drains it.
        state
            .mic_monitor
            .inject_stream_error("Device 0 disconnected");
        state.move_selection(1);

        assert_eq!(state.mic_selection, 1);
        assert!(!state.mic_monitor.has_pending_stream_error());

        state.refresh_mic_monitor();

        assert_eq!(state.mic_monitor.error(), None);
    }

    #[test]
    fn logo_preserves_the_chosen_block_shadow_spacing() {
        let lines: Vec<_> = LOGO.lines().collect();

        assert_eq!(lines.len(), usize::from(LOGO_HEIGHT));
        assert!(lines[2].starts_with("   ██║"));
        assert!(lines[5].starts_with("   ╚═╝"));
        assert!(lines[6].is_empty());
        assert!(lines[7].starts_with("                              ░▒▓"));
    }

    #[test]
    fn completed_reconfigure_stores_the_real_trigger_and_clears_the_handle() {
        let mut state = ConsoleState::new(Config::default(), no_mic_capabilities());
        state.step = StepKind::Shortcut;
        state.shortcut_handle = Some(ReconfigureHandle {
            result: Arc::new(Mutex::new(Some(Ok("Ctrl + Shift + Space".to_string())))),
        });

        state.refresh_shortcut_reconfigure();

        assert!(state.shortcut_handle.is_none());
        assert_eq!(
            state.config.activation.keybind_status,
            "Ctrl + Shift + Space"
        );
        assert_eq!(
            state.shortcut_feedback,
            Some(Ok("Ctrl + Shift + Space".to_string()))
        );
    }

    #[test]
    fn failed_reconfigure_reports_the_error_and_leaves_keybind_status_untouched() {
        let mut state = ConsoleState::new(Config::default(), no_mic_capabilities());
        state.step = StepKind::Shortcut;
        let previous_status = state.config.activation.keybind_status.clone();
        state.shortcut_handle = Some(ReconfigureHandle {
            result: Arc::new(Mutex::new(Some(Err("timed out waiting for the shortcut \
                dialog"
                .to_string())))),
        });

        state.refresh_shortcut_reconfigure();

        assert!(state.shortcut_handle.is_none());
        assert_eq!(state.config.activation.keybind_status, previous_status);
        assert_eq!(
            state.shortcut_feedback,
            Some(Err("timed out waiting for the shortcut dialog".to_string()))
        );
    }

    #[test]
    fn advancing_past_an_unchanged_shortcut_preserves_a_known_real_binding() {
        let mut state = ConsoleState::new(Config::default(), no_mic_capabilities());
        state.step = StepKind::Shortcut;
        // Simulate a completed reconfigure: the typed field still reads the
        // original default, but the real bound trigger is now known.
        state.config.activation.keybind_status = "Super + O".to_string();

        state.advance().unwrap();

        assert_eq!(state.config.activation.keybind_status, "Super + O");
    }

    #[test]
    fn advancing_with_an_edited_shortcut_invalidates_the_previously_known_binding() {
        let mut state = ConsoleState::new(Config::default(), no_mic_capabilities());
        state.step = StepKind::Shortcut;
        state.config.activation.keybind_status = "Super + O".to_string();
        state.shortcut_input = "Ctrl+Shift+Space".to_string();

        state.advance().unwrap();

        assert_eq!(state.config.activation.keybind_status, "untested");
        assert_eq!(state.config.activation.keybind, "Ctrl+Shift+Space");
    }

    #[test]
    fn a_pending_reconfigure_blocks_every_shortcut_key_except_escape() {
        let mut state = ConsoleState::new(Config::default(), no_mic_capabilities());
        state.step = StepKind::Shortcut;
        state.shortcut_handle = Some(ReconfigureHandle {
            result: Arc::new(Mutex::new(None)),
        });

        let enter = KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE);
        state.handle_shortcut_key(enter).unwrap();
        assert_eq!(
            state.step,
            StepKind::Shortcut,
            "Enter must not advance while the native dialog is still open"
        );

        let esc = KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE);
        let flow = state.handle_shortcut_key(esc).unwrap();
        assert!(matches!(flow, ControlFlow::Continue));
        assert_eq!(
            state.step,
            StepKind::Activation,
            "Esc still retreats even with a reconfigure in flight, same as Downloading"
        );
    }
}
