use super::{Capabilities, SetupOutcome};
use crate::audio;
use crate::config::{ActivationMode, Config, OutputMethod};
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
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

const MIC_PREVIEW_SETTLE_TIME: Duration = Duration::from_millis(250);
const LOGO: &str = r#"████████╗ ██████╗ ███╗   ██╗ ██████╗ ██╗   ██╗███████╗████████╗██╗   ██╗██████╗ ███████╗██████╗
╚══██╔══╝██╔═══██╗████╗  ██║██╔════╝ ██║   ██║██╔════╝╚══██╔══╝╚██╗ ██╔╝██╔══██╗██╔════╝██╔══██╗
   ██║   ██║   ██║██╔██╗ ██║██║  ███╗██║   ██║█████╗     ██║    ╚████╔╝ ██████╔╝█████╗  ██║  ██║
   ██║   ██║   ██║██║╚██╗██║██║   ██║██║   ██║██╔══╝     ██║     ╚██╔╝  ██╔═══╝ ██╔══╝  ██║  ██║
   ██║   ╚██████╔╝██║ ╚████║╚██████╔╝╚██████╔╝███████╗   ██║      ██║   ██║     ███████╗██████╔╝
   ╚═╝    ╚═════╝ ╚═╝  ╚═══╝ ╚═════╝  ╚═════╝ ╚══════╝   ╚═╝      ╚═╝   ╚═╝     ╚══════╝╚═════╝

                              ░▒▓  S P E A K .  T Y P E .  R E P E A T .  ▓▒░"#;
const LOGO_HEIGHT: u16 = 8;

const ACTIVATION_LABELS: [&str; 2] = [
    "Hold the shortcut while speaking",
    "Press once to start and again to stop",
];
const STARTUP_LABELS: [&str; 2] = ["Start manually", "Start TongueTyped when you sign in"];

#[derive(Clone, Copy, PartialEq, Eq)]
enum StepKind {
    Model,
    Microphone,
    Activation,
    Shortcut,
    Output,
    OutputBackend,
    Startup,
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
    mic_selection: usize,
    activation_selection: usize,
    output_labels: Vec<String>,
    output_selection: usize,
    backend_values: Vec<String>,
    backend_selection: usize,
    startup_selection: usize,
    shortcut_input: String,
    error: Option<String>,
    mic_level: Arc<Mutex<f32>>,
    mic_recorder: Option<audio::AudioRecorder>,
    mic_recorder_index: Option<usize>,
    mic_restart_at: Option<Instant>,
    mic_error: Option<String>,
    mic_stream_error: Arc<Mutex<Option<String>>>,
}

impl ConsoleState {
    fn new(config: Config, capabilities: Capabilities) -> Self {
        let model_values: Vec<String> = crate::model::ModelCatalog::model_names()
            .iter()
            .map(|name| (*name).to_string())
            .collect();
        let model_selection = model_values
            .iter()
            .position(|name| name == &config.model.selected)
            .unwrap_or(0);
        let mic_selection = capabilities.microphone_index(&config.audio.microphone);
        let activation_selection = usize::from(config.activation.mode == ActivationMode::Toggle);
        let output_labels = output_labels(&capabilities);
        let output_selection =
            usize::from(config.output.method == OutputMethod::Type && output_labels.len() > 1);
        let mut backend_values = vec!["auto".to_string()];
        backend_values.extend(capabilities.typing_backends.iter().cloned());
        let backend_selection = backend_values
            .iter()
            .position(|backend| backend == &config.output.typing_backend)
            .unwrap_or(0);
        let startup_selection = usize::from(config.startup.autostart);
        let shortcut_input = config.activation.keybind.clone();

        Self {
            config,
            capabilities,
            step: StepKind::Model,
            model_values,
            model_selection,
            mic_selection,
            activation_selection,
            output_labels,
            output_selection,
            backend_values,
            backend_selection,
            startup_selection,
            shortcut_input,
            error: None,
            mic_level: Arc::new(Mutex::new(0.0)),
            mic_recorder: None,
            mic_recorder_index: None,
            mic_restart_at: None,
            mic_error: None,
            mic_stream_error: Arc::new(Mutex::new(None)),
        }
    }

    fn run_loop(
        &mut self,
        terminal: &mut Terminal<CrosstermBackend<io::Stdout>>,
    ) -> anyhow::Result<SetupOutcome> {
        loop {
            self.refresh_mic_monitor();
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
            _ => self.handle_list_key(key),
        }
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
            StepKind::Microphone => self.capabilities.microphones.len(),
            StepKind::Activation => ACTIVATION_LABELS.len(),
            StepKind::Output => self.output_labels.len(),
            StepKind::OutputBackend => self.backend_values.len(),
            StepKind::Startup => STARTUP_LABELS.len(),
            StepKind::Shortcut | StepKind::Confirm => 0,
        }
    }

    fn current_index_mut(&mut self) -> &mut usize {
        match self.step {
            StepKind::Model => &mut self.model_selection,
            StepKind::Microphone => &mut self.mic_selection,
            StepKind::Activation => &mut self.activation_selection,
            StepKind::Output => &mut self.output_selection,
            StepKind::OutputBackend => &mut self.backend_selection,
            StepKind::Startup => &mut self.startup_selection,
            StepKind::Shortcut | StepKind::Confirm => {
                unreachable!("no list selection for this step")
            }
        }
    }

    fn has_backend_step(&self) -> bool {
        self.output_selection == 1 && !self.capabilities.typing_backends.is_empty()
    }

    fn next_step(&self) -> Option<StepKind> {
        use StepKind::*;
        Some(match self.step {
            Model => Microphone,
            Microphone => Activation,
            Activation => Shortcut,
            Shortcut => Output,
            Output => {
                if self.has_backend_step() {
                    OutputBackend
                } else {
                    Startup
                }
            }
            OutputBackend => Startup,
            Startup => Confirm,
            Confirm => return None,
        })
    }

    fn prev_step(&self) -> Option<StepKind> {
        use StepKind::*;
        Some(match self.step {
            Model => return None,
            Microphone => Model,
            Activation => Microphone,
            Shortcut => Activation,
            Output => Shortcut,
            OutputBackend => Output,
            Startup => {
                if self.has_backend_step() {
                    OutputBackend
                } else {
                    Output
                }
            }
            Confirm => Startup,
        })
    }

    fn advance(&mut self) -> anyhow::Result<ControlFlow> {
        match self.step {
            StepKind::Model => {
                self.config.model.selected = self.model_values[self.model_selection].clone();
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
                        self.config.activation.keybind = shortcut;
                        self.config.activation.keybind_status = "untested".to_string();
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
        self.mic_restart_at = None;
        if self.step != StepKind::Microphone {
            self.mic_recorder = None;
            self.mic_recorder_index = None;
            self.mic_error = None;
            return;
        }
        if self.mic_recorder_index == Some(self.mic_selection) {
            return;
        }
        self.mic_recorder = None;
        if let Ok(mut level) = self.mic_level.lock() {
            *level = 0.0;
        }
        self.mic_error = None;
        if let Ok(mut guard) = self.mic_stream_error.lock() {
            *guard = None;
        }
        let device_name = self.capabilities.microphones[self.mic_selection].0.clone();
        let level = self.mic_level.clone();
        let callback: audio::LevelCallback = Arc::new(move |value| {
            if let Ok(mut guard) = level.lock() {
                *guard = value;
            }
        });
        let stream_error = self.mic_stream_error.clone();
        let error_callback: audio::ErrorCallback = Arc::new(move |message| {
            if let Ok(mut guard) = stream_error.lock() {
                *guard = Some(message);
            }
        });
        match audio::AudioRecorder::new(&device_name, 16_000, Some(callback), Some(error_callback))
        {
            Ok(mut recorder) => match recorder.start() {
                Ok(()) => self.mic_recorder = Some(recorder),
                Err(err) => self.mic_error = Some(err.to_string()),
            },
            Err(err) => self.mic_error = Some(err.to_string()),
        }
        self.mic_recorder_index = Some(self.mic_selection);
    }

    fn defer_mic_monitor(&mut self) {
        self.mic_recorder = None;
        self.mic_recorder_index = None;
        self.mic_error = None;
        if let Ok(mut level) = self.mic_level.lock() {
            *level = 0.0;
        }
        self.mic_restart_at = Some(Instant::now() + MIC_PREVIEW_SETTLE_TIME);
    }

    fn refresh_mic_monitor(&mut self) {
        let stream_error = self.mic_stream_error.lock().ok().and_then(|mut guard| guard.take());
        if let Some(message) = stream_error {
            self.mic_error = Some(message);
            self.mic_recorder = None;
        }
        if self
            .mic_restart_at
            .is_some_and(|deadline| Instant::now() >= deadline)
        {
            self.sync_mic_monitor();
        }
    }

    fn step_title(&self) -> &'static str {
        match self.step {
            StepKind::Model => "Speech model",
            StepKind::Microphone => "Microphone",
            StepKind::Activation => "Activation",
            StepKind::Shortcut => "Shortcut",
            StepKind::Output => "Transcript output",
            StepKind::OutputBackend => "Typing backend",
            StepKind::Startup => "Startup",
            StepKind::Confirm => "Review",
        }
    }

    fn step_index(&self) -> usize {
        match self.step {
            StepKind::Model => 1,
            StepKind::Microphone => 2,
            StepKind::Activation => 3,
            StepKind::Shortcut => 4,
            StepKind::Output => 5,
            StepKind::OutputBackend => 6,
            StepKind::Startup => {
                if self.has_backend_step() {
                    7
                } else {
                    6
                }
            }
            StepKind::Confirm => {
                if self.has_backend_step() {
                    8
                } else {
                    7
                }
            }
        }
    }

    fn step_total(&self) -> usize {
        if self.has_backend_step() {
            8
        } else {
            7
        }
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
            StepKind::Shortcut => {
                "Type modifiers+key (e.g. Ctrl+Shift+Space)  Enter confirm  Esc back"
            }
            StepKind::Confirm => "Enter/y save  n/q discard  Esc back",
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
            StepKind::Confirm => self.render_confirm(frame, area),
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

        if let Some(error) = &self.mic_error {
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

        let level = self.mic_level.lock().map(|guard| *guard).unwrap_or(0.0);
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
        let lines = vec![
            Line::from(format!("Shortcut: {}_", self.shortcut_input)),
            Line::from(""),
            Line::from(
                "Combine modifiers (Ctrl, Alt, Shift, Super) with a key, e.g. Ctrl+Shift+Space.",
            ),
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

    fn render_confirm(&self, frame: &mut Frame, area: Rect) {
        let lines = vec![
            Line::from(format!("Model:       {}", self.config.model.selected)),
            Line::from(format!("Microphone:  {}", self.config.audio.microphone)),
            Line::from(format!(
                "Activation:  {} with {}",
                self.config.activation.mode, self.config.activation.keybind
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

fn output_labels(capabilities: &Capabilities) -> Vec<String> {
    let mut labels = vec!["Keep transcripts in TongueTyped".to_string()];
    if !capabilities.typing_backends.is_empty() {
        labels.push("Type into the focused application".to_string());
    }
    labels
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

fn level_to_ratio(level: f32) -> f64 {
    if level <= 0.0 {
        return 0.0;
    }
    let db = 20.0 * level.log10();
    ((db + 60.0) / 60.0).clamp(0.0, 1.0) as f64
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn silence_maps_to_empty_gauge() {
        assert_eq!(level_to_ratio(0.0), 0.0);
    }

    #[test]
    fn full_scale_maps_to_full_gauge() {
        assert_eq!(level_to_ratio(1.0), 1.0);
    }

    #[test]
    fn quiet_signal_is_between_bounds() {
        let ratio = level_to_ratio(0.01);
        assert!(ratio > 0.0 && ratio < 1.0);
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
        };
        let mut state = ConsoleState::new(Config::default(), capabilities);
        state.step = StepKind::Microphone;
        state.mic_recorder_index = Some(0);

        state.move_selection(1);
        state.move_selection(1);

        assert_eq!(state.mic_selection, 2);
        assert_eq!(state.mic_recorder_index, None);
        assert!(state.mic_restart_at.is_some());
    }

    #[test]
    fn async_stream_error_is_routed_into_the_mic_error_panel_instead_of_the_terminal() {
        let capabilities = Capabilities {
            microphones: vec![("test-mic-1".to_string(), "Test microphone 1".to_string())],
            typing_backends: Vec::new(),
        };
        let mut state = ConsoleState::new(Config::default(), capabilities);
        state.step = StepKind::Microphone;
        state.mic_recorder_index = Some(0);

        // Simulate what AudioRecorder's error_callback does from its background
        // audio thread when a device disconnects mid-stream.
        *state.mic_stream_error.lock().unwrap() = Some("Device disconnected".to_string());

        state.refresh_mic_monitor();

        assert_eq!(state.mic_error, Some("Device disconnected".to_string()));
        assert!(state.mic_recorder.is_none());
        assert!(state.mic_stream_error.lock().unwrap().is_none());
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
}
