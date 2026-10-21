//! The interactive dashboard opened by running `tonguetyped` with no
//! subcommand. Every action here calls into the same functions the plain CLI
//! commands use (`crate::commands`, `crate::doctor`, `crate::activation`,
//! `crate::autostart`, `crate::setup`) - nothing here reimplements command
//! logic or shells out to the installed binary. The one exception is
//! `daemon`, which is spawned as a separate OS process via
//! `crate::commands::spawn_daemon` because the daemon is a long-running
//! service that must outlive this dashboard session (see that function's
//! doc comment).
//!
//! The home screen is one Settings panel with one selection cursor, grouped
//! into four labeled sections (Language & model, Capture & activation, Output,
//! Behaviour) listing the fifteen configuration areas (Model, Inference
//! backend, Translation, Decoding, Text processing, Microphone, Activation,
//! Shortcut, Transcript output, Typing backend, Transcript folder, History
//! retention, Startup, Overlay, Media pause) with their current values, plus a
//! Commands row. The Commands row opens a dedicated Commands screen derived
//! from `crate::cli::Cli`'s clap metadata (`Cli::command().get_subcommands()`)
//! with the directional recording commands intentionally omitted in favor of
//! Toggle. `model` and `autostart` are CLI commands but live in the Settings
//! panel rather than the Commands screen, so each configuration area has
//! exactly one home-screen entry point.

mod logo;
mod screens;

use screens::{pad_selected_row, selection_list, selection_style};

use crate::cli::Cli;
use crate::commands::{self, OutputLine};
use crate::config::{Config, OutputMethod};
use crate::setup::Capabilities;
use crate::{activation, doctor, ipc, setup};
use clap::CommandFactory;
use crossterm::event::{Event, EventStream, KeyCode, KeyEvent, KeyEventKind, KeyModifiers};
use crossterm::execute;
use crossterm::terminal::{
    disable_raw_mode, enable_raw_mode, EnterAlternateScreen, LeaveAlternateScreen,
};
use futures_util::StreamExt;
use ratatui::backend::CrosstermBackend;
use ratatui::layout::{Constraint, Direction, Layout, Margin, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::canvas::{Canvas, Line as CanvasLine, Points};
use ratatui::widgets::{Block, Borders, Gauge, Paragraph, Wrap};
use ratatui::{Frame, Terminal};
use std::cell::{Cell, RefCell};
use std::io;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};
use tokio::task::JoinHandle;

type Backend = CrosstermBackend<io::Stdout>;

pub async fn run() -> anyhow::Result<()> {
    install_panic_hook();
    enable_raw_mode()?;
    let mut stdout = io::stdout();
    execute!(stdout, EnterAlternateScreen)?;
    let backend = CrosstermBackend::new(stdout);
    let terminal = Terminal::new(backend)?;
    let mut guard = TerminalGuard {
        terminal: Some(terminal),
        saved_stderr: redirect_stderr_to_devnull(),
    };

    let config = Config::load()?;
    let mut app = App::new(config);
    let result = app
        .run_loop(guard.terminal.as_mut().expect("terminal present"))
        .await;
    drop(guard);
    result
}

/// Redirects the process's real stderr file descriptor to `/dev/null` for
/// the dashboard's lifetime, restored by `TerminalGuard`'s `Drop`. Rust's own
/// `tracing` output is separately sent to `io::sink()` (see `main.rs`), but
/// `transcribe-cpp`'s underlying C/C++ GGUF loader logs straight to the
/// process's stderr fd on backend-fallback/load-failure paths - exactly the
/// kind of thing the Doctor and Model screens trigger - and that bypasses
/// `tracing` entirely, so only an OS-level fd swap catches it. Returns the
/// duplicated original fd to restore later, or `None` if the redirect
/// couldn't be set up (dashboard still runs; native library logging would
/// then visibly interleave with the display, same as before this fix).
fn redirect_stderr_to_devnull() -> Option<std::os::fd::RawFd> {
    use std::os::fd::IntoRawFd;
    // SAFETY: `dup`/`dup2`/`close` are called with fds this process owns
    // (stderr, and a freshly opened /dev/null), per their standard contract.
    unsafe {
        let saved = libc::dup(libc::STDERR_FILENO);
        if saved < 0 {
            return None;
        }
        let Ok(devnull) = std::fs::OpenOptions::new().write(true).open("/dev/null") else {
            libc::close(saved);
            return None;
        };
        let devnull_fd = devnull.into_raw_fd();
        let result = libc::dup2(devnull_fd, libc::STDERR_FILENO);
        libc::close(devnull_fd);
        if result < 0 {
            libc::close(saved);
            return None;
        }
        Some(saved)
    }
}

fn restore_stderr(saved: std::os::fd::RawFd) {
    // SAFETY: `saved` was produced by `libc::dup` in
    // `redirect_stderr_to_devnull` and not yet closed.
    unsafe {
        libc::dup2(saved, libc::STDERR_FILENO);
        libc::close(saved);
    }
}

/// Restores the terminal on every exit path, including an unwinding panic -
/// `Drop` runs during unwind, so a raw-mode/alternate-screen terminal is
/// never left corrupted even if a screen's render or action handler panics.
struct TerminalGuard {
    terminal: Option<Terminal<Backend>>,
    saved_stderr: Option<std::os::fd::RawFd>,
}

impl Drop for TerminalGuard {
    fn drop(&mut self) {
        if let Some(terminal) = &mut self.terminal {
            let _ = disable_raw_mode();
            let _ = execute!(terminal.backend_mut(), LeaveAlternateScreen);
        }
        if let Some(saved) = self.saved_stderr.take() {
            restore_stderr(saved);
        }
    }
}

/// Also restores the terminal before the default panic message prints, so a
/// crash is readable instead of being swallowed by raw mode / the alternate
/// screen - belt-and-suspenders alongside `TerminalGuard`, which still
/// catches any exit path this hook does not (e.g. a panic during the hook
/// itself, or process abort).
fn install_panic_hook() {
    let original = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        let _ = disable_raw_mode();
        let _ = execute!(io::stdout(), LeaveAlternateScreen);
        original(info);
    }));
}

struct HomeItem {
    name: String,
    about: String,
}

/// The fifteen configuration areas the home screen's Settings panel lists, in
/// display order.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum SettingId {
    Model,
    InferenceBackend,
    Translation,
    Decoding,
    TextProcessing,
    Microphone,
    Activation,
    Shortcut,
    TranscriptOutput,
    TypingBackend,
    TranscriptFolder,
    HistoryRetention,
    Startup,
    Overlay,
    MediaPause,
}

impl SettingId {
    const ALL: [SettingId; 15] = [
        SettingId::Model,
        SettingId::InferenceBackend,
        SettingId::Translation,
        SettingId::Decoding,
        SettingId::TextProcessing,
        SettingId::Microphone,
        SettingId::Activation,
        SettingId::Shortcut,
        SettingId::TranscriptOutput,
        SettingId::TypingBackend,
        SettingId::TranscriptFolder,
        SettingId::HistoryRetention,
        SettingId::Startup,
        SettingId::Overlay,
        SettingId::MediaPause,
    ];

    fn label(self) -> &'static str {
        match self {
            SettingId::Model => "Model",
            SettingId::InferenceBackend => "Inference backend",
            SettingId::Translation => "Translation",
            SettingId::Decoding => "Decoding",
            SettingId::TextProcessing => "Text processing",
            SettingId::Microphone => "Microphone",
            SettingId::Activation => "Activation",
            SettingId::Shortcut => "Shortcut",
            SettingId::TranscriptOutput => "Transcript output",
            SettingId::TypingBackend => "Typing backend",
            SettingId::TranscriptFolder => "Transcript folder",
            SettingId::HistoryRetention => "History retention",
            SettingId::Startup => "Startup",
            SettingId::Overlay => "Overlay",
            SettingId::MediaPause => "Media pause",
        }
    }
}

/// The Settings panel's four labeled groups, in `SettingId::ALL` order (so the
/// section headers are purely visual and the home selection index still maps
/// one-to-one onto `SettingId::ALL`, then the Commands row).
const SETTING_SECTIONS: [(&str, &[SettingId]); 4] = [
    (
        "Language & model",
        &[
            SettingId::Model,
            SettingId::InferenceBackend,
            SettingId::Translation,
            SettingId::Decoding,
            SettingId::TextProcessing,
        ],
    ),
    (
        "Capture & activation",
        &[
            SettingId::Microphone,
            SettingId::Activation,
            SettingId::Shortcut,
        ],
    ),
    (
        "Output",
        &[
            SettingId::TranscriptOutput,
            SettingId::TypingBackend,
            SettingId::TranscriptFolder,
            SettingId::HistoryRetention,
        ],
    ),
    (
        "App",
        &[
            SettingId::Startup,
            SettingId::Overlay,
            SettingId::MediaPause,
        ],
    ),
];

/// Selectable Settings rows (fifteen configuration areas plus the Commands
/// row that opens the Commands screen).
const HOME_ROW_COUNT: usize = SettingId::ALL.len() + 1;

/// Fifteen settings rows, the four section headers, and the Commands row, plus
/// the panel's two border rows.
const SETTINGS_PANEL_HEIGHT: u16 = 15 + 4 + 1 + 2;

/// The dashboard's Commands screen, derived from the exact same clap metadata
/// `--help` renders - see this module's doc comment. The implicit `help`
/// meta-subcommand clap adds is excluded; it has no useful standalone
/// dashboard action. `start`/`stop` (single-shot recording start/stop) are
/// also excluded: the dashboard offers `toggle` and `cancel` for recording
/// control, and `start`/`stop` remain reachable only as the public CLI
/// commands documented in `--help`. `model` and `autostart` exist as CLI
/// commands but are excluded here because their settings live in the
/// Settings panel (exactly one home-screen entry point per area).
fn home_items() -> Vec<HomeItem> {
    Cli::command()
        .get_subcommands()
        .filter(|command| {
            !matches!(
                command.get_name(),
                "help" | "start" | "stop" | "model" | "autostart"
            )
            // `transcribe` takes a file-path argument, so it is a terminal
            // command rather than a dashboard action; there is no home-screen
            // handler for it.
            && command.get_name() != "transcribe"
        })
        .map(|command| HomeItem {
            name: command.get_name().to_string(),
            about: command
                .get_about()
                .map(|about| about.to_string())
                .unwrap_or_default(),
        })
        .collect()
}

#[derive(Clone, Copy)]
enum InfoReturn {
    Deck,
    Home,
}

enum Screen {
    Deck,
    Home,
    Commands,
    Info {
        title: String,
        lines: Vec<OutputLine>,
    },
    Model(screens::ModelScreen),
    InferenceBackend(screens::BackendScreen),
    Autostart(screens::AutostartScreen),
    Daemon(screens::DaemonScreen),
    Microphone(screens::MicrophoneScreen),
    Activation(screens::ActivationScreen),
    Translation(screens::TranslationScreen),
    TextProcessing(screens::TextProcessingScreen),
    Shortcut(screens::ShortcutScreen),
    TranscriptOutput(screens::OutputScreen),
    TypingBackend(screens::TypingBackendScreen),
    Overlay(screens::OverlayScreen),
    MediaPause(screens::MediaPauseScreen),
    TextFields(screens::TextFieldsScreen),
}

struct PendingAction {
    title: &'static str,
    handle: JoinHandle<Vec<OutputLine>>,
    progress: Option<Arc<Mutex<(u64, u64)>>>,
}

struct App {
    config: Config,
    capabilities: Capabilities,
    screen: Screen,
    deck_selected: usize,
    deck_model_selected: usize,
    deck_model_filter: usize,
    deck_output: screens::OutputScreen,
    deck_history: screens::TextFieldsScreen,
    deck_setup_selected: usize,
    deck_wave_motion: Cell<bool>,
    deck_wave_phase: Cell<f32>,
    splash_until: Cell<Option<Instant>>,
    info_return: InfoReturn,
    command_items: Vec<HomeItem>,
    home_selected: usize,
    /// Cursor on the dedicated Commands screen, separate from the home cursor.
    command_selected: usize,
    pending: Option<PendingAction>,
    shortcut_test: Option<setup::ShortcutTestHandle>,
    shortcut_dialog: Option<setup::ReconfigureHandle>,
    should_quit: bool,
    daemon_status_cache: Cell<Option<(bool, Instant)>>,
    /// Scroll offset (in wrapped rows) for the current `Screen::Info` pane.
    /// `render_info` clamps it to the wrapped content height each frame, so
    /// the key handler can simply increment/decrement it and a transcript
    /// taller than the pane stays fully reachable.
    info_scroll: Cell<u16>,
    /// Cached rasterisation of the overlay style shown on the Overlay screen.
    /// `render_overlay` runs on every 66ms tick, and the offline rasteriser
    /// allocates and paints the style's full-resolution surface (1920x1080 for
    /// `border`) each call, so the pixels are rendered once and reused until the
    /// previewed style, look, or waveform shape changes.
    overlay_preview: RefCell<Option<OverlayPreview>>,
}

/// One cached overlay preview frame: the rasterised pixels plus the inputs that
/// produced them, so `render_overlay` can tell when it must re-render.
struct OverlayPreview {
    style: String,
    look: String,
    shape: String,
    width: u32,
    height: u32,
    pixels: Vec<u8>,
}

/// How often the home screen's status strip re-probes daemon liveness.
/// `status_strip_line` renders on every tick of `run_loop`'s 66ms ticker, but
/// the probe it displays (`commands::daemon_socket_exists`) opens a real
/// `UnixStream` connection against the daemon's control socket - re-running
/// that on every redraw would make the daemon's accept loop spawn a task per
/// frame purely to paint a status string, so the cached result is reused
/// until it goes stale.
const DAEMON_STATUS_REFRESH: Duration = Duration::from_secs(1);

/// The Overlay screen's preview band: its total height in rows including the
/// border and title. Kept small so the preview reads as a preview rather than
/// dominating the screen the way the old side panel did.
const PREVIEW_BAND_HEIGHT: u16 = 8;

/// The deck pane sits to the right of the one-column gutter, so its left edge
/// is already separated and only the other three sides are bordered. That
/// keeps the pane content at the approved 60 columns at 80x24 and 94 at
/// 120x40 instead of losing a column to a second separator.
const PANE_BORDERS: Borders = Borders::TOP.union(Borders::RIGHT).union(Borders::BOTTOM);

impl App {
    fn new(config: Config) -> Self {
        let capabilities = Capabilities::discover_or_default();
        let rows = commands::model_rows(&config);
        let active_model = rows.iter().position(|row| row.active).unwrap_or(0);
        let deck_output = screens::OutputScreen::new(&capabilities, &config);
        let deck_history = screens::TextFieldsScreen::history_retention(&config);
        Self {
            config,
            capabilities,
            screen: Screen::Deck,
            deck_selected: 0,
            deck_model_selected: active_model,
            deck_model_filter: 0,
            deck_output,
            deck_history,
            deck_setup_selected: 0,
            deck_wave_motion: Cell::new(false),
            deck_wave_phase: Cell::new(0.0),
            splash_until: Cell::new(Some(Instant::now() + Duration::from_millis(700))),
            info_return: InfoReturn::Home,
            command_items: home_items(),
            home_selected: 0,
            command_selected: 0,
            pending: None,
            shortcut_test: None,
            shortcut_dialog: None,
            should_quit: false,
            daemon_status_cache: Cell::new(None),
            info_scroll: Cell::new(0),
            overlay_preview: RefCell::new(None),
        }
    }

    async fn run_loop(&mut self, terminal: &mut Terminal<Backend>) -> anyhow::Result<()> {
        let mut events = EventStream::new();
        let mut ticker = tokio::time::interval(Duration::from_millis(66));

        loop {
            self.refresh_screens();
            terminal.draw(|frame| self.render(frame))?;

            // Computed before `pending_done` borrows `self.pending` mutably,
            // so the guard check and that borrow never overlap.
            let has_pending = self.pending.is_some();
            let pending_done = async {
                match &mut self.pending {
                    Some(pending) => (&mut pending.handle).await,
                    None => std::future::pending().await,
                }
            };

            tokio::select! {
                _ = ticker.tick() => {}
                maybe_event = events.next() => {
                    if let Some(Ok(event)) = maybe_event {
                        self.handle_event(event, terminal)?;
                    }
                }
                result = pending_done, if has_pending => {
                    self.apply_pending_result(result);
                }
            }

            if self.should_quit {
                return Ok(());
            }
        }
    }

    fn apply_pending_result(&mut self, result: Result<Vec<OutputLine>, tokio::task::JoinError>) {
        let title = self
            .pending
            .take()
            .map(|pending| pending.title)
            .unwrap_or("Action");
        let lines = match result {
            Ok(lines) => lines,
            Err(error) => vec![OutputLine {
                text: format!("internal error: {error}"),
                is_error: true,
            }],
        };
        self.info_scroll.set(0);
        self.remember_info_return();
        self.screen = Screen::Info {
            title: title.to_string(),
            lines,
        };
        // A background action may have written config.toml itself
        // (`commands::activate_model`'s `commands::select_model` is the one
        // such writer). Refresh the in-memory config so a later
        // `save_settings_config` serializes the on-disk state instead of
        // clobbering it with a stale copy.
        if let Ok(reloaded) = Config::load() {
            self.config = reloaded;
        }
    }

    /// Per-frame maintenance for screens with background work: the
    /// microphone preview's settle/restart cycle, and the Shortcut screen's
    /// in-flight portal test/dialog. Called every tick, before drawing, so
    /// results surface in the next frame - the same shape the setup console
    /// uses for its own background handles.
    fn refresh_screens(&mut self) {
        if let Screen::Microphone(screen) = &mut self.screen {
            screen.poll_preview();
        }
        if matches!(self.screen, Screen::Deck) && self.deck_wave_motion.get() {
            let phase = (self.deck_wave_phase.get() + 0.09) % std::f32::consts::TAU;
            self.deck_wave_phase.set(phase);
        }
        self.poll_shortcut_handles();
    }

    fn render_splash(&self, frame: &mut Frame, area: Rect) {
        let lines = logo::splash_lines(area.width);
        frame.render_widget(
            Paragraph::new(lines).style(Style::default().bg(Color::Rgb(3, 8, 20))),
            area,
        );
    }

    fn poll_shortcut_handles(&mut self) {
        let test_outcome = self
            .shortcut_test
            .as_ref()
            .and_then(|handle| handle.result.lock().ok().and_then(|mut guard| guard.take()));
        if let Some(outcome) = test_outcome {
            self.shortcut_test = None;
            self.apply_shortcut_test_outcome(outcome);
        }

        let dialog_outcome = self
            .shortcut_dialog
            .as_ref()
            .and_then(|handle| handle.result.lock().ok().and_then(|mut guard| guard.take()));
        if let Some(outcome) = dialog_outcome {
            self.shortcut_dialog = None;
            self.apply_shortcut_dialog_outcome(outcome);
        }
    }

    fn apply_shortcut_test_outcome(
        &mut self,
        outcome: Result<activation::ShortcutTestOutcome, String>,
    ) {
        let Screen::Shortcut(screen) = &mut self.screen else {
            return;
        };
        match outcome {
            Ok(activation::ShortcutTestOutcome::Pressed) => {
                screen.status = screens::ShortcutStatus::Message {
                    text: "shortcut press detected - the binding works".to_string(),
                    is_error: false,
                };
            }
            Ok(activation::ShortcutTestOutcome::TimedOut) => {
                screen.status = screens::ShortcutStatus::Message {
                    text: "no shortcut press detected within 15s - set one with the system \
                           dialog, then try again"
                        .to_string(),
                    is_error: true,
                };
            }
            Err(error) => {
                screen.status = screens::ShortcutStatus::Message {
                    text: format!("shortcut binding test failed: {error}"),
                    is_error: true,
                };
            }
        }
    }

    fn apply_shortcut_dialog_outcome(
        &mut self,
        outcome: Result<activation::ReconfigureOutcome, String>,
    ) {
        let Screen::Shortcut(screen) = &mut self.screen else {
            return;
        };
        match outcome {
            Ok(activation::ReconfigureOutcome::Changed {
                trigger_description,
            }) => {
                self.config.activation.keybind = activation::keybind_label(&trigger_description);
                screen.status = match self.config.save() {
                    Ok(()) => screens::ShortcutStatus::Message {
                        text: format!(
                            "Shortcut bound: {}",
                            activation::keybind_label(&trigger_description)
                        ),
                        is_error: false,
                    },
                    Err(error) => screens::ShortcutStatus::Message {
                        text: format!(
                            "Shortcut bound, but saving the configuration failed: {error}"
                        ),
                        is_error: true,
                    },
                };
            }
            // Not a failure: the user closed the desktop's settings window
            // without applying anything, or kept the binding on purpose.
            Ok(activation::ReconfigureOutcome::Unchanged {
                trigger_description,
            }) => {
                screen.status = screens::ShortcutStatus::Message {
                    text: format!(
                        "No change was made - the shortcut is still {}",
                        activation::keybind_display(&trigger_description)
                    ),
                    is_error: false,
                };
            }
            Err(error) => {
                screen.status = screens::ShortcutStatus::Message {
                    text: format!("Reconfigure failed: {error}"),
                    is_error: true,
                };
            }
        }
    }

    fn handle_event(
        &mut self,
        event: Event,
        terminal: &mut Terminal<Backend>,
    ) -> anyhow::Result<()> {
        if let Event::Key(key) = event {
            if key.kind == KeyEventKind::Press {
                self.handle_key(key, terminal)?;
            }
        }
        Ok(())
    }

    fn handle_key(
        &mut self,
        key: KeyEvent,
        terminal: &mut Terminal<Backend>,
    ) -> anyhow::Result<()> {
        if key.code == KeyCode::Char('c') && key.modifiers.contains(KeyModifiers::CONTROL) {
            self.should_quit = true;
            return Ok(());
        }
        if self.splash_until.get().is_some() {
            self.splash_until.set(None);
            return Ok(());
        }
        match &self.screen {
            Screen::Deck => self.handle_deck_key(key, terminal)?,
            Screen::Home => self.handle_home_key(key)?,
            Screen::Commands => self.handle_commands_key(key, terminal)?,
            Screen::Info { .. } => self.handle_info_key(key),
            Screen::Model(_) => self.handle_model_key(key),
            Screen::InferenceBackend(_) => self.handle_inference_backend_key(key),
            Screen::Autostart(_) => self.handle_autostart_key(key),
            Screen::Daemon(_) => self.handle_daemon_key(key),
            Screen::Microphone(_) => self.handle_microphone_key(key),
            Screen::Activation(_) => self.handle_activation_key(key),
            Screen::Translation(_) => self.handle_translation_key(key),
            Screen::TextProcessing(_) => self.handle_text_processing_key(key),
            Screen::Shortcut(_) => self.handle_shortcut_key(key),
            Screen::TranscriptOutput(_) => self.handle_output_key(key),
            Screen::TypingBackend(_) => self.handle_typing_backend_key(key),
            Screen::Overlay(_) => self.handle_overlay_key(key),
            Screen::MediaPause(_) => self.handle_media_pause_key(key),
            Screen::TextFields(_) => self.handle_text_fields_key(key),
        }
        Ok(())
    }

    fn move_home_selection(&mut self, delta: i32) {
        let len = HOME_ROW_COUNT as i32;
        let next = (self.home_selected as i32 + delta).clamp(0, len - 1) as usize;
        self.home_selected = next;
    }

    /// The dedicated Commands screen: `↑`/`↓` (and `←`/`→`) move the cursor,
    /// `Enter` runs the highlighted command, and `Esc`/`Backspace` return home.
    fn handle_commands_key(
        &mut self,
        key: KeyEvent,
        terminal: &mut Terminal<Backend>,
    ) -> anyhow::Result<()> {
        if Self::is_back_key(key.code) {
            self.screen = Screen::Home;
            return Ok(());
        }
        match key.code {
            KeyCode::Char('q') => self.should_quit = true,
            KeyCode::Up
            | KeyCode::Down
            | KeyCode::Left
            | KeyCode::Right
            | KeyCode::Char('k')
            | KeyCode::Char('j')
            | KeyCode::Char('h')
            | KeyCode::Char('l') => {
                let delta = Self::step_delta(key.code).unwrap_or(0);
                let len = self.command_items.len() as i32;
                if len > 0 {
                    self.command_selected =
                        (self.command_selected as i32 + delta).clamp(0, len - 1) as usize;
                }
            }
            KeyCode::Enter if self.pending.is_none() => {
                let name = self.command_items[self.command_selected].name.clone();
                self.dispatch_command(&name, terminal)?;
            }
            _ => {}
        }
        Ok(())
    }

    fn handle_home_key(&mut self, key: KeyEvent) -> anyhow::Result<()> {
        match key.code {
            KeyCode::Char('q') => self.should_quit = true,
            KeyCode::Esc | KeyCode::Left | KeyCode::Backspace => self.screen = Screen::Deck,
            KeyCode::Up | KeyCode::Char('k') => self.move_home_selection(-1),
            KeyCode::Down | KeyCode::Char('j') => self.move_home_selection(1),
            KeyCode::Enter | KeyCode::Right | KeyCode::Char('l') if self.pending.is_none() => {
                let selected = self.home_selected;
                self.open_home_row(selected);
            }
            _ => {}
        }
        Ok(())
    }

    fn handle_deck_key(
        &mut self,
        key: KeyEvent,
        terminal: &mut Terminal<Backend>,
    ) -> anyhow::Result<()> {
        match key.code {
            KeyCode::Char('q') => {
                self.should_quit = true;
                return Ok(());
            }
            KeyCode::Char('s') => {
                self.deck_selected = 0;
                return Ok(());
            }
            KeyCode::Char('m') => {
                self.deck_selected = 1;
                self.deck_model_selected = self
                    .deck_model_selected
                    .min(self.deck_model_rows().len().saturating_sub(1));
                return Ok(());
            }
            KeyCode::Char('o') => {
                self.deck_selected = 2;
                return Ok(());
            }
            KeyCode::Char('h') => {
                self.deck_selected = 3;
                self.deck_history = screens::TextFieldsScreen::history_retention(&self.config);
                return Ok(());
            }
            KeyCode::Char('p') => {
                self.deck_selected = 4;
                return Ok(());
            }
            KeyCode::Char('w') => {
                self.deck_wave_motion.set(!self.deck_wave_motion.get());
                return Ok(());
            }
            KeyCode::Char('?') => {
                self.remember_info_return();
                self.screen = Screen::Info {
                    title: "Keys".to_string(),
                    lines: vec![
                        OutputLine {
                            text: "↑↓ move   s m o h p jump   enter open   ? help   q quit"
                                .to_string(),
                            is_error: false,
                        },
                        OutputLine {
                            text: "Speak: t toggle recording   c cancel   w wave on/off"
                                .to_string(),
                            is_error: false,
                        },
                        OutputLine {
                            text: "Models: a/d activate or download   / filter   enter activate"
                                .to_string(),
                            is_error: false,
                        },
                        OutputLine {
                            text: "Output/History: space toggles history   enter saves".to_string(),
                            is_error: false,
                        },
                        OutputLine {
                            text: "Setup: enter guided setup   g settings   c commands".to_string(),
                            is_error: false,
                        },
                        OutputLine {
                            text: "No mouse required.".to_string(),
                            is_error: false,
                        },
                    ],
                };
                return Ok(());
            }
            _ => {}
        }

        match self.deck_selected {
            0 => match key.code {
                KeyCode::Up | KeyCode::Char('k') => {
                    self.deck_selected = self.deck_selected.saturating_sub(1);
                }
                KeyCode::Down | KeyCode::Char('j') => {
                    self.deck_selected = (self.deck_selected + 1).min(4);
                }
                KeyCode::Enter if self.pending.is_none() => {
                    self.dispatch_command("toggle", terminal)?;
                }
                KeyCode::Char('t') if self.pending.is_none() => {
                    self.dispatch_command("toggle", terminal)?;
                }
                KeyCode::Char('c') if self.pending.is_none() => {
                    self.dispatch_command("cancel", terminal)?;
                }
                _ => {}
            },
            1 => match key.code {
                KeyCode::Up | KeyCode::Char('k') => {
                    self.deck_model_selected = self.deck_model_selected.saturating_sub(1);
                }
                KeyCode::Down | KeyCode::Char('j') => {
                    let len = self.deck_model_rows().len();
                    self.deck_model_selected =
                        (self.deck_model_selected + 1).min(len.saturating_sub(1));
                }
                KeyCode::Enter | KeyCode::Char('a') | KeyCode::Char('d')
                    if self.pending.is_none() =>
                {
                    let rows = self.deck_model_rows();
                    if let Some(row) =
                        rows.get(self.deck_model_selected.min(rows.len().saturating_sub(1)))
                    {
                        self.start_model_activation(row.id.to_string());
                    }
                }
                KeyCode::Char('/') => {
                    self.deck_model_filter = (self.deck_model_filter + 1) % 3;
                    self.deck_model_selected = 0;
                }
                _ => {}
            },
            2 => match key.code {
                KeyCode::Up | KeyCode::Char('k') => {
                    self.deck_output.move_selection(-1, &self.config);
                }
                KeyCode::Down | KeyCode::Char('j') => {
                    self.deck_output.move_selection(1, &self.config);
                }
                KeyCode::Left => {
                    if !self.deck_output.step(-1, &mut self.config) {
                        self.deck_output.move_selection(-1, &self.config);
                    }
                }
                KeyCode::Right | KeyCode::Char('l') => {
                    if !self.deck_output.step(1, &mut self.config) {
                        self.deck_output.move_selection(1, &self.config);
                    }
                }
                KeyCode::Enter if self.pending.is_none() => {
                    let result = self.deck_output.apply(&mut self.config);
                    if result.is_ok() {
                        self.deck_output.result =
                            Some(self.config.save().map_err(|error| error.to_string()));
                    } else {
                        self.deck_output.result = Some(result);
                    }
                }
                KeyCode::Char(' ') if self.pending.is_none() => {
                    self.config.history.enabled = !self.config.history.enabled;
                    self.deck_output.result =
                        Some(self.config.save().map_err(|error| error.to_string()));
                }
                _ => {}
            },
            3 => {
                if key.code == KeyCode::Char(' ') {
                    if self.pending.is_none() {
                        self.config.history.enabled = !self.config.history.enabled;
                        self.deck_history =
                            screens::TextFieldsScreen::history_retention(&self.config);
                        self.deck_history.result =
                            Some(self.config.save().map_err(|error| error.to_string()));
                    }
                    return Ok(());
                }
                if key.code == KeyCode::Enter {
                    if self.pending.is_none() {
                        let result = self.deck_history.apply(&mut self.config);
                        if result.is_ok() {
                            self.deck_history.result =
                                Some(self.config.save().map_err(|error| error.to_string()));
                        } else {
                            self.deck_history.result = Some(result);
                        }
                    }
                    return Ok(());
                }
                if self.deck_history.disabled {
                    return Ok(());
                }
                let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);
                match key.code {
                    KeyCode::Up | KeyCode::BackTab => self.deck_history.move_selection(-1),
                    KeyCode::Down | KeyCode::Tab => self.deck_history.move_selection(1),
                    KeyCode::Left => self.deck_history.selected_field_mut().move_left(),
                    KeyCode::Right => self.deck_history.selected_field_mut().move_right(),
                    KeyCode::Home => self.deck_history.selected_field_mut().home(),
                    KeyCode::End => self.deck_history.selected_field_mut().end(),
                    KeyCode::Backspace => self.deck_history.selected_field_mut().backspace(),
                    KeyCode::Char(c) if !ctrl => self.deck_history.selected_field_mut().insert(c),
                    _ => {}
                }
            }
            _ => match key.code {
                KeyCode::Up | KeyCode::Char('k') => {
                    self.deck_setup_selected = self.deck_setup_selected.saturating_sub(1);
                }
                KeyCode::Down | KeyCode::Char('j') => {
                    self.deck_setup_selected = (self.deck_setup_selected + 1).min(4);
                }
                KeyCode::Enter => {
                    self.dispatch_command("setup", terminal)?;
                }
                KeyCode::Char('g') => {
                    self.screen = Screen::Home;
                }
                KeyCode::Char('c') => {
                    self.screen = Screen::Commands;
                }
                _ => {}
            },
        }
        Ok(())
    }

    fn render_deck(&self, frame: &mut Frame, area: Rect) {
        let header = logo::bubble_header(
            self.daemon_running_cached(),
            &self.config.model.active_model,
        );
        let chunks = Layout::default()
            .direction(Direction::Vertical)
            .constraints([
                Constraint::Length(logo::BUBBLE_HEIGHT),
                Constraint::Min(0),
                Constraint::Length(2),
            ])
            .split(area);
        frame.render_widget(Paragraph::new(header), chunks[0]);

        let rail_width = if area.width >= 120 { 24 } else { 18 };
        let body = Layout::default()
            .direction(Direction::Horizontal)
            .constraints([
                Constraint::Length(rail_width),
                Constraint::Length(1),
                Constraint::Min(0),
            ])
            .split(chunks[1]);
        frame.render_widget(
            Block::default().style(Style::default().bg(Color::Rgb(32, 12, 48))),
            body[1],
        );
        self.render_deck_rail(frame, body[0]);
        self.render_deck_pane(frame, body[2]);

        let footer = match &self.pending {
            Some(pending) => Line::from(Span::styled(
                format!("Working: {}...", pending.title),
                Style::default().fg(Color::Yellow),
            )),
            None => Line::from(match self.deck_selected {
                0 => "↑↓ move   enter start   t toggle   w wave   ? help   q quit",
                1 => "↑↓ model   enter activate   d download   / filter   ? help   q quit",
                2 => "↑↓ row   ←/→ change   enter save   space history   ? help   q quit",
                3 => "↑↓ field   type edit   enter save   space on/off   ? help   q quit",
                _ => "↑↓ chapter   enter setup   g settings   c commands   q quit",
            }),
        };
        let status = match self.deck_selected {
            0 => "5 destinations · single letter jumps · no mouse required",
            1 => "live = streaming partial text · final = one transcript · / cycles filter",
            2 => "space toggles transcript history · the pane saves on enter",
            3 => "space toggles history · numeric fields save on enter",
            _ => "guided setup is five chapters · settings and commands stay reachable",
        };
        frame.render_widget(
            Paragraph::new(vec![
                footer,
                Line::from(Span::styled(status, Style::default().fg(Color::DarkGray))),
            ]),
            chunks[2],
        );
    }

    fn render_deck_rail(&self, frame: &mut Frame, area: Rect) {
        let labels = [
            ("SPEAK", "hold / toggle"),
            ("MODELS", "download / activate"),
            ("OUTPUT", "deliver text"),
            ("HISTORY", "local retention"),
            ("SETUP", "guided configuration"),
        ];
        let selected = Style::default()
            .fg(Color::Rgb(182, 255, 182))
            .bg(Color::Rgb(50, 0, 77))
            .add_modifier(Modifier::BOLD);
        let accent = Style::default()
            .fg(Color::Rgb(0, 255, 0))
            .add_modifier(Modifier::BOLD);
        let muted = Style::default().fg(Color::DarkGray);
        let inner_width = usize::from(area.width.saturating_sub(2));
        let show_desc = area.height >= 20;
        let mut lines: Vec<Line> = Vec::new();
        for (index, (label, description)) in labels.iter().enumerate() {
            let is_selected = index == self.deck_selected;
            let marker = if is_selected { "▸ " } else { "  " };
            let mut spans = Vec::new();
            if is_selected {
                let text = format!("{marker}{label}");
                let used = marker.len() + label.len();
                spans.push(Span::styled(text, selected));
                if used < inner_width {
                    spans.push(Span::styled(" ".repeat(inner_width - used), selected));
                }
            } else {
                spans.push(Span::raw(marker));
                spans.push(Span::styled(label[0..1].to_string(), accent));
                spans.push(Span::styled(label[1..].to_string(), muted));
            }
            lines.push(Line::from(spans));
            if show_desc {
                let mut desc = vec![Span::styled(
                    format!("  {description}"),
                    if is_selected { selected } else { muted },
                )];
                let used = 2 + description.len();
                if is_selected && used < inner_width {
                    desc.push(Span::styled(" ".repeat(inner_width - used), selected));
                }
                lines.push(Line::from(desc));
                lines.push(Line::from(""));
            }
        }
        lines.push(Line::from(""));
        let (daemon_text, daemon_style) = if self.daemon_running_cached() {
            ("running", Style::default().fg(Color::Green))
        } else {
            ("stopped", Style::default().fg(Color::Red))
        };
        lines.push(Line::from(vec![
            Span::styled("  daemon ", muted),
            Span::styled(daemon_text, daemon_style),
        ]));
        lines.push(Line::from(vec![
            Span::styled("  model  ", muted),
            Span::styled(
                self.config.model.active_model.clone(),
                Style::default().fg(Color::Cyan),
            ),
        ]));
        let block = Block::default()
            .borders(Borders::ALL)
            .title(Line::from(Span::styled(
                "LIVE",
                Style::default().fg(Color::DarkGray),
            )));
        frame.render_widget(Paragraph::new(lines).block(block), area);
    }

    fn render_deck_pane(&self, frame: &mut Frame, area: Rect) {
        match self.deck_selected {
            0 => self.render_deck_speak(frame, area),
            1 => self.render_deck_models(frame, area),
            2 => self.render_deck_output(frame, area),
            3 => self.render_deck_history(frame, area),
            _ => self.render_deck_setup(frame, area),
        }
    }

    fn render_deck_speak(&self, frame: &mut Frame, area: Rect) {
        let accent = Style::default()
            .fg(Color::Rgb(0, 255, 0))
            .add_modifier(Modifier::BOLD);
        let muted = Style::default().fg(Color::DarkGray);
        let text = Style::default().fg(Color::Gray);
        let (daemon_text, daemon_style) = if self.daemon_running_cached() {
            ("running", Style::default().fg(Color::Green))
        } else {
            ("stopped", Style::default().fg(Color::Red))
        };
        let level = self.deck_wave_level();
        let lines = vec![
            Line::from(vec![Span::styled("● ", daemon_style), Span::raw("idle")]),
            Line::from(""),
            Line::from(Span::styled(
                "hold the shortcut, or press once to toggle",
                text,
            )),
            Line::from(vec![
                Span::styled("mic ", muted),
                Span::styled(format!("{:>3}%", (level * 100.0).round() as u32), accent),
                Span::styled(" simulated", muted),
            ]),
            Line::from(""),
            Line::from(vec![
                Span::styled("daemon ", muted),
                Span::styled(daemon_text, daemon_style),
                Span::styled(" · model ", muted),
                Span::styled(
                    self.config.model.active_model.clone(),
                    Style::default().fg(Color::Cyan),
                ),
            ]),
            Line::from(""),
            Line::from(vec![
                Span::styled("enter", accent),
                Span::raw(" start   "),
                Span::styled("c", accent),
                Span::raw(" cancel   "),
                Span::styled("w", accent),
                Span::raw(if self.deck_wave_motion.get() {
                    " wave on"
                } else {
                    " wave off"
                }),
            ]),
        ];
        if self.deck_selected == 0 {
            self.render_wave_field(
                frame,
                area.inner(Margin {
                    vertical: 1,
                    horizontal: 1,
                }),
            );
        }
        let block = Block::default()
            .borders(PANE_BORDERS)
            .title(Line::from(Span::styled("SPEAK", accent)));
        frame.render_widget(Paragraph::new(lines).block(block), area);
    }

    fn deck_model_rows(&self) -> Vec<commands::ModelRow> {
        commands::model_rows(&self.config)
            .into_iter()
            .filter(|row| match self.deck_model_filter {
                1 => row.info.is_english_only(),
                2 => row.installed,
                _ => true,
            })
            .collect()
    }

    fn deck_model_status(row: &commands::ModelRow) -> &'static str {
        if row.active {
            "active"
        } else if row.installed {
            "installed"
        } else {
            "download"
        }
    }

    fn render_deck_models(&self, frame: &mut Frame, area: Rect) {
        let accent = Style::default()
            .fg(Color::Rgb(0, 255, 0))
            .add_modifier(Modifier::BOLD);
        let selected_style = Style::default()
            .fg(Color::Rgb(182, 255, 182))
            .bg(Color::Rgb(50, 0, 77))
            .add_modifier(Modifier::BOLD);
        let muted = Style::default().fg(Color::DarkGray);
        let text = Style::default().fg(Color::Gray);
        let rows = self.deck_model_rows();
        if rows.is_empty() {
            let block = Block::default()
                .borders(PANE_BORDERS)
                .title(Line::from(Span::styled("MODELS", accent)));
            frame.render_widget(
                Paragraph::new("No models match this filter.")
                    .style(text)
                    .block(block),
                area,
            );
            return;
        }
        if let Some(progress) = self
            .pending
            .as_ref()
            .and_then(|pending| pending.progress.clone())
        {
            let (downloaded, total) = progress.lock().map(|guard| *guard).unwrap_or((0, 0));
            let ratio = if total == 0 {
                0.0
            } else {
                (downloaded as f64 / total as f64).clamp(0.0, 1.0)
            };
            let chunks = Layout::default()
                .direction(Direction::Vertical)
                .constraints([Constraint::Min(3), Constraint::Length(2)])
                .split(area);
            let block = Block::default()
                .borders(PANE_BORDERS)
                .title(Line::from(Span::styled("DOWNLOAD", accent)));
            frame.render_widget(
                Gauge::default()
                    .block(block)
                    .gauge_style(Style::default().fg(Color::Rgb(0, 255, 0)))
                    .ratio(ratio),
                chunks[0],
            );
            frame.render_widget(
                Paragraph::new(vec![
                    Line::from(Span::styled(
                        format!(
                            "{} / {}",
                            commands::human_size(downloaded),
                            commands::human_size(total)
                        ),
                        text,
                    )),
                    Line::from(Span::styled("sha-256 checked after the download", muted)),
                ]),
                chunks[1],
            );
            return;
        }

        let selected = self.deck_model_selected.min(rows.len() - 1);
        let visible: usize = if area.height >= 26 { 10 } else { 5 };
        let start = selected.saturating_sub(visible.saturating_sub(1));
        let end = (start + visible).min(rows.len());
        let inner_width = usize::from(area.width.saturating_sub(1));
        let mut lines: Vec<Line> = Vec::new();
        let header = if area.width >= 90 {
            format!(
                " {:>2} {:<28} {:<6} {:>7} {:<5} {:<8} {:<10}",
                "#", "MODEL", "QUANT", "SIZE", "LANG", "STREAM", "STATUS"
            )
        } else {
            format!(
                " {:>2} {:<24} {:>7} {:<5} {:<9}",
                "#", "MODEL", "SIZE", "STREAM", "STATUS"
            )
        };
        lines.push(Line::from(Span::styled(header, muted)));
        for (index, row) in rows.iter().enumerate().take(end).skip(start) {
            let is_selected = index == selected;
            let marker = if is_selected { "▸" } else { " " };
            let stream = if row.info.supports_streaming() {
                "live"
            } else {
                "final"
            };
            let status = Self::deck_model_status(row);
            let size = commands::human_size(row.size_bytes);
            let id = row.id.chars().take(28).collect::<String>();
            let mut spans: Vec<Span> = Vec::new();
            if is_selected {
                let row_text = if area.width >= 90 {
                    format!(
                        "{marker}{:>2} {:<28} {:<6} {:>7} {:<5} {:<8} {:<10}",
                        index + 1,
                        id,
                        row.quant,
                        size,
                        row.info.language_summary(),
                        stream,
                        status
                    )
                } else {
                    format!(
                        "{marker}{:>2} {:<24} {:>7} {:<5} {:<9}",
                        index + 1,
                        id,
                        size,
                        stream,
                        status
                    )
                };
                let used = row_text.chars().count();
                spans.push(Span::styled(row_text, selected_style));
                if used < inner_width {
                    spans.push(Span::styled(" ".repeat(inner_width - used), selected_style));
                }
            } else {
                spans.push(Span::styled(format!("{marker}{:>2} ", index + 1), accent));
                if area.width >= 90 {
                    spans.push(Span::raw(format!(
                        "{:<28} {:<6} {:>7} ",
                        id, row.quant, size
                    )));
                    spans.push(Span::styled(
                        format!("{:<5} ", row.info.language_summary()),
                        muted,
                    ));
                } else {
                    spans.push(Span::raw(format!("{:<24} {:>7} ", id, size)));
                }
                let stream_width = if area.width >= 90 { 8 } else { 5 };
                spans.push(Span::styled(
                    format!("{stream:<stream_width$} "),
                    if stream == "live" {
                        Style::default().fg(Color::Green)
                    } else {
                        muted
                    },
                ));
                spans.push(Span::styled(status, muted));
            }
            lines.push(Line::from(spans));
        }
        let above = start;
        let more = rows.len() - end;
        if above > 0 || more > 0 {
            let mut parts = Vec::new();
            if above > 0 {
                parts.push(format!("↑ {above} above"));
            }
            if more > 0 {
                parts.push(format!("↓ {more} more"));
            }
            lines.push(Line::from(Span::styled(parts.join("   "), muted)));
        }
        lines.push(Line::from(Span::styled(
            "─".repeat(inner_width),
            Style::default().fg(Color::DarkGray),
        )));
        let row = &rows[selected];
        lines.push(Line::from(vec![
            Span::styled("selected ", muted),
            Span::styled(row.id.to_string(), Style::default().fg(Color::Cyan)),
            Span::styled(
                format!(
                    " · {} · {}",
                    row.quant,
                    commands::human_size(row.size_bytes)
                ),
                text,
            ),
        ]));
        lines.push(Line::from(Span::styled(
            format!(
                "{} · {}",
                row.info.language_summary(),
                if row.info.supports_streaming() {
                    "live partial text"
                } else {
                    "final transcript only"
                }
            ),
            text,
        )));
        lines.push(Line::from(vec![
            Span::styled(
                if row.installed {
                    "installed"
                } else {
                    "not installed"
                },
                muted,
            ),
            Span::styled("   ", muted),
            Span::styled("a", accent),
            Span::raw(" activate   "),
            Span::styled("d", accent),
            Span::raw(" download   "),
            Span::styled("/", accent),
            Span::raw(" filter"),
        ]));
        let block = Block::default()
            .borders(PANE_BORDERS)
            .title(Line::from(Span::styled("MODELS", accent)));
        frame.render_widget(Paragraph::new(lines).block(block), area);
    }

    fn render_deck_output(&self, frame: &mut Frame, area: Rect) {
        let accent = Style::default()
            .fg(Color::Rgb(0, 255, 0))
            .add_modifier(Modifier::BOLD);
        let inner_width = usize::from(area.width.saturating_sub(1));
        let mut lines = self.deck_output.deck_lines(&self.config, inner_width);
        lines.push(Line::from(""));
        lines.push(self.deck_output.warning_line());
        lines.push(self.deck_output.result_line());
        let block = Block::default()
            .borders(PANE_BORDERS)
            .title(Line::from(Span::styled("OUTPUT", accent)));
        frame.render_widget(Paragraph::new(lines).block(block), area);
    }

    fn render_deck_history(&self, frame: &mut Frame, area: Rect) {
        let accent = Style::default()
            .fg(Color::Rgb(0, 255, 0))
            .add_modifier(Modifier::BOLD);
        let muted = Style::default().fg(Color::DarkGray);
        let inner_width = usize::from(area.width.saturating_sub(1));
        let mut lines = self.deck_history.deck_lines(inner_width);
        lines.push(Line::from(""));
        lines.push(Line::from(Span::styled(
            self.deck_history.hint_line(),
            muted,
        )));
        lines.push(self.deck_history.result_line("History retention saved."));
        let block = Block::default()
            .borders(PANE_BORDERS)
            .title(Line::from(Span::styled("HISTORY", accent)));
        frame.render_widget(Paragraph::new(lines).block(block), area);
    }

    fn render_deck_setup(&self, frame: &mut Frame, area: Rect) {
        let accent = Style::default()
            .fg(Color::Rgb(0, 255, 0))
            .add_modifier(Modifier::BOLD);
        let selected_style = Style::default()
            .fg(Color::Rgb(182, 255, 182))
            .bg(Color::Rgb(50, 0, 77))
            .add_modifier(Modifier::BOLD);
        let muted = Style::default().fg(Color::DarkGray);
        let text = Style::default().fg(Color::Gray);
        let chapters = [
            ("1", "Model", "choose or download"),
            ("2", "Output", "delivery"),
            ("3", "History", "local retention"),
            ("4", "App", "startup / overlay / media"),
            ("5", "Review", "apply"),
        ];
        let inner_width = usize::from(area.width.saturating_sub(1));
        let mut lines = Vec::new();
        for (index, (number, title, description)) in chapters.iter().enumerate() {
            let is_selected = index == self.deck_setup_selected.min(chapters.len() - 1);
            let marker = if is_selected { "▸ " } else { "  " };
            let label = format!("{marker}{number}  {title}");
            if is_selected {
                let used = label.chars().count();
                let mut spans = vec![Span::styled(label, selected_style)];
                if used < inner_width {
                    spans.push(Span::styled(" ".repeat(inner_width - used), selected_style));
                }
                lines.push(Line::from(spans));
                let desc = format!("   {description}");
                let used = desc.chars().count();
                let mut spans = vec![Span::styled(desc, selected_style)];
                if used < inner_width {
                    spans.push(Span::styled(" ".repeat(inner_width - used), selected_style));
                }
                lines.push(Line::from(spans));
            } else {
                lines.push(Line::from(vec![
                    Span::raw(marker),
                    Span::styled(number.to_string(), accent),
                    Span::raw(format!("  {title}")),
                ]));
                lines.push(Line::from(Span::styled(format!("   {description}"), muted)));
            }
        }
        lines.push(Line::from(""));
        lines.push(Line::from(Span::styled(
            "enter guided setup   g settings   c commands",
            text,
        )));
        let block = Block::default()
            .borders(PANE_BORDERS)
            .title(Line::from(Span::styled("SETUP", accent)));
        frame.render_widget(Paragraph::new(lines).block(block), area);
    }

    /// The Speak pane's simulated mic level. It is deterministic in the wave
    /// phase, so it animates only while the user has motion on and stays a
    /// readable steady value when the wave is paused.
    fn deck_wave_level(&self) -> f64 {
        let phase = f64::from(self.deck_wave_phase.get());
        let slow = (phase * 0.55).sin();
        let flutter = (phase * 2.7).sin() * 0.12;
        (0.62 + slow * 0.22 + flutter).clamp(0.0, 1.0)
    }

    fn render_wave_field(&self, frame: &mut Frame, area: Rect) {
        if area.width < 20 || area.height < 5 {
            return;
        }
        let rows = 8usize;
        let cols = 16usize;
        let phase = f64::from(self.deck_wave_phase.get());
        let level = self.deck_wave_level();
        let amplitude = 0.05 + level * 0.09;
        let canvas = Canvas::default()
            .x_bounds([-1.6, 1.6])
            .y_bounds([-1.0, 1.0])
            .paint(|ctx| {
                let line = Color::Rgb(18, 70, 140);
                for row in 0..rows {
                    let z = row as f64 / (rows - 1) as f64;
                    let base = -0.7 + z * 1.2;
                    for col in 0..cols - 1 {
                        let x1 = col as f64 / (cols - 1) as f64 * 3.2 - 1.6;
                        let x2 = (col + 1) as f64 / (cols - 1) as f64 * 3.2 - 1.6;
                        let wave = |x: f64| {
                            (x * 2.4 + z * 3.0 + phase).sin() * amplitude * (1.0 - z * 0.5)
                        };
                        ctx.draw(&CanvasLine {
                            x1,
                            y1: base + wave(x1),
                            x2,
                            y2: base + wave(x2),
                            color: line,
                        });
                    }
                }
                for col in 0..cols {
                    let x = col as f64 / (cols - 1) as f64 * 3.2 - 1.6;
                    for row in 0..rows - 1 {
                        let z1 = row as f64 / (rows - 1) as f64;
                        let z2 = (row + 1) as f64 / (rows - 1) as f64;
                        let y1 = -0.7
                            + z1 * 1.2
                            + (x * 2.4 + z1 * 3.0 + phase).sin() * amplitude * (1.0 - z1 * 0.5);
                        let y2 = -0.7
                            + z2 * 1.2
                            + (x * 2.4 + z2 * 3.0 + phase).sin() * amplitude * (1.0 - z2 * 0.5);
                        ctx.draw(&CanvasLine {
                            x1: x,
                            y1,
                            x2: x,
                            y2,
                            color: line,
                        });
                    }
                }
                let stars = [
                    (-1.3, 0.82),
                    (-0.7, 0.55),
                    (0.2, 0.9),
                    (0.9, 0.64),
                    (1.4, 0.35),
                ];
                ctx.draw(&Points {
                    coords: &stars,
                    color: Color::Rgb(70, 150, 230),
                });
            });
        frame.render_widget(canvas, area);
    }

    fn open_home_row(&mut self, index: usize) {
        if index < SettingId::ALL.len() {
            self.open_setting(SettingId::ALL[index]);
        } else {
            self.screen = Screen::Commands;
        }
    }

    fn open_setting(&mut self, setting: SettingId) {
        self.screen = match setting {
            SettingId::Model => Screen::Model(screens::ModelScreen::new(
                &self.config,
                crate::inference::backend_choices(),
            )),
            SettingId::InferenceBackend => Screen::InferenceBackend(screens::BackendScreen::new(
                &self.config,
                crate::inference::backend_choices(),
            )),
            SettingId::Translation => {
                Screen::Translation(screens::TranslationScreen::new(&self.config))
            }
            SettingId::Decoding => {
                Screen::TextFields(screens::TextFieldsScreen::decoding(&self.config))
            }
            SettingId::TextProcessing => {
                Screen::TextProcessing(screens::TextProcessingScreen::new(&self.config))
            }
            SettingId::Microphone => {
                let mut screen = screens::MicrophoneScreen::new(&self.capabilities, &self.config);
                screen.start_preview();
                Screen::Microphone(screen)
            }
            SettingId::Activation => {
                Screen::Activation(screens::ActivationScreen::new(&self.config))
            }
            SettingId::Shortcut => Screen::Shortcut(screens::ShortcutScreen::new(&self.config)),
            SettingId::TranscriptOutput => Screen::TranscriptOutput(screens::OutputScreen::new(
                &self.capabilities,
                &self.config,
            )),
            SettingId::TypingBackend => Screen::TypingBackend(screens::TypingBackendScreen::new(
                &self.capabilities,
                &self.config,
            )),
            SettingId::TranscriptFolder => {
                Screen::TextFields(screens::TextFieldsScreen::transcript_folder(&self.config))
            }
            SettingId::HistoryRetention => {
                Screen::TextFields(screens::TextFieldsScreen::history_retention(&self.config))
            }
            SettingId::Startup => Screen::Autostart(screens::AutostartScreen::new(&self.config)),
            SettingId::Overlay => Screen::Overlay(screens::OverlayScreen::new(&self.config)),
            SettingId::MediaPause => {
                Screen::MediaPause(screens::MediaPauseScreen::new(&self.config))
            }
        };
    }

    /// Saves the current config after a settings screen mutated it in place,
    /// reporting the outcome in whichever screen's result line is current -
    /// so every settings screen gives the same success/error feedback. Returns
    /// whether the write succeeded, so a leave handler can stay on the screen
    /// (with the error visible) instead of navigating away from a failed save.
    fn save_settings_config(&mut self) -> bool {
        let result = self.config.save().map_err(|error| error.to_string());
        let saved = result.is_ok();
        match &mut self.screen {
            Screen::Microphone(screen) => screen.result = Some(result),
            Screen::Activation(screen) => screen.result = Some(result),
            Screen::Translation(screen) => screen.result = Some(result),
            Screen::TextProcessing(screen) => screen.result = Some(result),
            Screen::TranscriptOutput(screen) => screen.result = Some(result),
            Screen::TypingBackend(screen) => screen.result = Some(result),
            Screen::Overlay(screen) => screen.result = Some(result),
            Screen::MediaPause(screen) => screen.result = Some(result),
            Screen::TextFields(screen) => screen.result = Some(result),
            _ => {}
        }
        saved
    }

    /// Leaves the current settings screen, committing it first so backing out
    /// saves exactly like `Enter` does. A screen whose form is invalid stays put
    /// with the error showing, and a failed config write also keeps the screen
    /// open, so backing out never silently discards input or hides a failure.
    ///
    /// The screen maps "commit on leave" to its own pending value: the value
    /// rows of a radio list (Activation, Microphone, ...) commit the highlighted
    /// option, the Overlay steppers commit their pending style/position/waveform,
    /// and the text forms run their own validation. Screens whose `apply` is a
    /// heavy one-shot action (Model, Inference backend, Daemon, Shortcut) and the
    /// Transcript output screen, whose rows apply individually on `Enter`, commit
    /// nothing here and only persist what was already applied.
    fn commit_and_leave_settings(&mut self) {
        let leave = match &mut self.screen {
            Screen::Microphone(screen) => {
                screen.apply(&mut self.config);
                true
            }
            Screen::Activation(screen) => {
                screen.apply(&mut self.config);
                true
            }
            Screen::Translation(screen) => {
                screen.apply(&mut self.config);
                true
            }
            Screen::MediaPause(screen) => {
                screen.apply(&mut self.config);
                true
            }
            Screen::TypingBackend(screen) => {
                screen.apply(&mut self.config);
                true
            }
            Screen::Overlay(screen) => {
                screen.commit(&mut self.config);
                true
            }
            Screen::Autostart(screen) => {
                let target = screen.selected == 1;
                match crate::autostart::update(target) {
                    Ok(()) => {
                        self.config.startup.autostart = target;
                        true
                    }
                    Err(error) => {
                        screen.result = Some(Err(error.to_string()));
                        false
                    }
                }
            }
            Screen::TextFields(screen) => match screen.apply(&mut self.config) {
                Ok(()) => true,
                Err(message) => {
                    screen.result = Some(Err(message));
                    false
                }
            },
            Screen::TextProcessing(screen) => match screen.apply(&mut self.config) {
                Ok(()) => true,
                Err(message) => {
                    screen.result = Some(Err(message));
                    false
                }
            },
            _ => true,
        };
        if leave && self.save_settings_config() {
            self.screen = Screen::Home;
        }
    }

    /// The `↑`/`←`/`k`/`h` step is `-1`, the `↓`/`→`/`j`/`l` step is `1`, and
    /// anything else is `None`. Text screens never call this for the letter
    /// aliases, because `h`/`j`/`k`/`l` are literal characters there.
    fn step_delta(code: KeyCode) -> Option<i32> {
        match code {
            KeyCode::Up | KeyCode::Left | KeyCode::Char('k') | KeyCode::Char('h') => Some(-1),
            KeyCode::Down | KeyCode::Right | KeyCode::Char('j') | KeyCode::Char('l') => Some(1),
            _ => None,
        }
    }

    /// Whether this key leaves a settings screen: `Esc` everywhere, and
    /// `Backspace` too on the list screens (the text screens keep `Backspace`
    /// for deleting a character, and their handler tests `Esc` alone).
    fn is_back_key(code: KeyCode) -> bool {
        matches!(code, KeyCode::Esc | KeyCode::Backspace)
    }

    fn handle_microphone_key(&mut self, key: KeyEvent) {
        if Self::is_back_key(key.code) {
            self.commit_and_leave_settings();
            return;
        }
        match key.code {
            KeyCode::Char('q') => self.should_quit = true,
            KeyCode::Up
            | KeyCode::Down
            | KeyCode::Left
            | KeyCode::Right
            | KeyCode::Char('k')
            | KeyCode::Char('j')
            | KeyCode::Char('h')
            | KeyCode::Char('l') => {
                let delta = Self::step_delta(key.code).unwrap_or(0);
                if let Screen::Microphone(screen) = &mut self.screen {
                    screen.move_selection(delta);
                }
            }
            KeyCode::Enter => {
                if let Screen::Microphone(screen) = &mut self.screen {
                    screen.apply(&mut self.config);
                }
                self.save_settings_config();
            }
            _ => {}
        }
    }

    fn handle_activation_key(&mut self, key: KeyEvent) {
        if Self::is_back_key(key.code) {
            self.commit_and_leave_settings();
            return;
        }
        match key.code {
            KeyCode::Char('q') => self.should_quit = true,
            KeyCode::Up
            | KeyCode::Down
            | KeyCode::Left
            | KeyCode::Right
            | KeyCode::Char('k')
            | KeyCode::Char('j')
            | KeyCode::Char('h')
            | KeyCode::Char('l') => {
                let delta = Self::step_delta(key.code).unwrap_or(0);
                if let Screen::Activation(screen) = &mut self.screen {
                    screen.move_selection(delta);
                }
            }
            KeyCode::Enter => {
                if let Screen::Activation(screen) = &mut self.screen {
                    screen.apply(&mut self.config);
                }
                self.save_settings_config();
            }
            _ => {}
        }
    }

    fn handle_translation_key(&mut self, key: KeyEvent) {
        if Self::is_back_key(key.code) {
            self.commit_and_leave_settings();
            return;
        }
        match key.code {
            KeyCode::Char('q') => self.should_quit = true,
            KeyCode::Up
            | KeyCode::Down
            | KeyCode::Left
            | KeyCode::Right
            | KeyCode::Char('k')
            | KeyCode::Char('j')
            | KeyCode::Char('h')
            | KeyCode::Char('l') => {
                let delta = Self::step_delta(key.code).unwrap_or(0);
                if let Screen::Translation(screen) = &mut self.screen {
                    screen.move_selection(delta);
                }
            }
            KeyCode::Enter => {
                if let Screen::Translation(screen) = &mut self.screen {
                    screen.apply(&mut self.config);
                }
                self.save_settings_config();
            }
            _ => {}
        }
    }

    /// The "Text processing" screen: Up/Down/Tab move between rows, the
    /// editing keys act on the selected text row, Enter flips a toggle row (or
    /// just saves a text row) and applies the whole form. `q` types a literal
    /// `q` on a text row since the fields accept free text; Ctrl+C quits.
    fn handle_text_processing_key(&mut self, key: KeyEvent) {
        // Escape commits and leaves. `Backspace` stays a text edit here,
        // because the replacement and command rows accept free text.
        if key.code == KeyCode::Esc {
            self.commit_and_leave_settings();
            return;
        }
        if key.code == KeyCode::Enter {
            let Screen::TextProcessing(screen) = &mut self.screen else {
                return;
            };
            let previous = (screen.spoken_punctuation, screen.remove_fillers);
            screen.toggle_selected();
            let result = screen.apply(&mut self.config);
            if result.is_err() {
                screen.spoken_punctuation = previous.0;
                screen.remove_fillers = previous.1;
            }
            let saved = result.is_ok();
            screen.result = Some(result);
            if saved {
                self.save_settings_config();
            }
            return;
        }
        let Screen::TextProcessing(screen) = &mut self.screen else {
            return;
        };
        let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);
        match key.code {
            KeyCode::Up | KeyCode::BackTab => screen.move_selection(-1),
            KeyCode::Down | KeyCode::Tab => screen.move_selection(1),
            KeyCode::Left => {
                if let Some(field) = screen.selected_text_field_mut() {
                    field.move_left();
                }
            }
            KeyCode::Right => {
                if let Some(field) = screen.selected_text_field_mut() {
                    field.move_right();
                }
            }
            KeyCode::Home => {
                if let Some(field) = screen.selected_text_field_mut() {
                    field.home();
                }
            }
            KeyCode::End => {
                if let Some(field) = screen.selected_text_field_mut() {
                    field.end();
                }
            }
            KeyCode::Backspace => {
                if let Some(field) = screen.selected_text_field_mut() {
                    field.backspace();
                }
            }
            KeyCode::Char(c) if !ctrl => {
                if let Some(field) = screen.selected_text_field_mut() {
                    field.insert(c);
                }
            }
            _ => {}
        }
    }

    fn handle_output_key(&mut self, key: KeyEvent) {
        if Self::is_back_key(key.code) {
            self.commit_and_leave_settings();
            return;
        }
        match key.code {
            KeyCode::Char('q') => self.should_quit = true,
            KeyCode::Up | KeyCode::Char('k') => {
                if let Screen::TranscriptOutput(screen) = &mut self.screen {
                    screen.move_selection(-1, &self.config);
                }
            }
            KeyCode::Down | KeyCode::Char('j') => {
                if let Screen::TranscriptOutput(screen) = &mut self.screen {
                    screen.move_selection(1, &self.config);
                }
            }
            // On the option rows `←`/`→` steps the value both ways (the paste
            // keys cycle, "after text" flips); on the method rows it moves the
            // highlight, like `↑`/`↓`.
            KeyCode::Left | KeyCode::Right | KeyCode::Char('h') | KeyCode::Char('l') => {
                let delta = Self::step_delta(key.code).unwrap_or(0);
                if let Screen::TranscriptOutput(screen) = &mut self.screen {
                    if !screen.step(delta, &mut self.config) {
                        screen.move_selection(delta, &self.config);
                    }
                }
            }
            KeyCode::Enter => {
                if let Screen::TranscriptOutput(screen) = &mut self.screen {
                    if let Err(message) = screen.apply(&mut self.config) {
                        // Nothing changed, so skip the save (and its success
                        // feedback): the result line explains the refusal.
                        screen.result = Some(Err(message));
                        return;
                    }
                }
                self.save_settings_config();
            }
            _ => {}
        }
    }

    fn handle_typing_backend_key(&mut self, key: KeyEvent) {
        if Self::is_back_key(key.code) {
            self.commit_and_leave_settings();
            return;
        }
        match key.code {
            KeyCode::Char('q') => self.should_quit = true,
            KeyCode::Up
            | KeyCode::Down
            | KeyCode::Left
            | KeyCode::Right
            | KeyCode::Char('k')
            | KeyCode::Char('j')
            | KeyCode::Char('h')
            | KeyCode::Char('l') => {
                let delta = Self::step_delta(key.code).unwrap_or(0);
                if let Screen::TypingBackend(screen) = &mut self.screen {
                    screen.move_selection(delta);
                }
            }
            KeyCode::Enter => {
                if let Screen::TypingBackend(screen) = &mut self.screen {
                    screen.apply(&mut self.config);
                }
                self.save_settings_config();
            }
            _ => {}
        }
    }

    fn handle_overlay_key(&mut self, key: KeyEvent) {
        if Self::is_back_key(key.code) {
            self.commit_and_leave_settings();
            return;
        }
        match key.code {
            KeyCode::Char('q') => self.should_quit = true,
            KeyCode::Up | KeyCode::Char('k') => {
                if let Screen::Overlay(screen) = &mut self.screen {
                    screen.move_selection(-1);
                }
            }
            KeyCode::Down | KeyCode::Char('j') => {
                if let Screen::Overlay(screen) = &mut self.screen {
                    screen.move_selection(1);
                }
            }
            // Step the highlighted value both ways; the preview follows the
            // pending value, so it redraws before anything is committed.
            KeyCode::Left | KeyCode::Right | KeyCode::Char('h') | KeyCode::Char('l') => {
                let delta = Self::step_delta(key.code).unwrap_or(0);
                if let Screen::Overlay(screen) = &mut self.screen {
                    screen.step(delta);
                }
            }
            KeyCode::Enter => {
                if let Screen::Overlay(screen) = &mut self.screen {
                    screen.commit(&mut self.config);
                }
                self.save_settings_config();
            }
            _ => {}
        }
    }

    fn handle_media_pause_key(&mut self, key: KeyEvent) {
        if Self::is_back_key(key.code) {
            self.commit_and_leave_settings();
            return;
        }
        match key.code {
            KeyCode::Char('q') => self.should_quit = true,
            KeyCode::Up
            | KeyCode::Down
            | KeyCode::Left
            | KeyCode::Right
            | KeyCode::Char('k')
            | KeyCode::Char('j')
            | KeyCode::Char('h')
            | KeyCode::Char('l') => {
                let delta = Self::step_delta(key.code).unwrap_or(0);
                if let Screen::MediaPause(screen) = &mut self.screen {
                    screen.move_selection(delta);
                }
            }
            KeyCode::Enter => {
                if let Screen::MediaPause(screen) = &mut self.screen {
                    screen.apply(&mut self.config);
                }
                self.save_settings_config();
            }
            _ => {}
        }
    }

    /// The text-field screens ("Decoding", "Transcript folder", "History
    /// retention"):
    /// Up/Down/Tab move between fields, the editing keys act on the selected
    /// field, and Enter runs the screen's `apply`. `q` types a literal `q`
    /// here instead of quitting, since the fields accept free text; Ctrl+C
    /// still quits from any screen.
    fn handle_text_fields_key(&mut self, key: KeyEvent) {
        // Escape commits and leaves. `Backspace` stays a text edit on this
        // screen because the fields accept free text.
        if key.code == KeyCode::Esc {
            self.commit_and_leave_settings();
            return;
        }
        if matches!(&self.screen, Screen::TextFields(screen) if screen.disabled) {
            // The history fields are hidden while keeping transcripts is off.
            // Escape already left above; every other key is inert so a hidden
            // field can never change.
            return;
        }
        if key.code == KeyCode::Enter {
            let result = match &self.screen {
                Screen::TextFields(screen) => screen.apply(&mut self.config),
                _ => return,
            };
            let saved = result.is_ok();
            if let Screen::TextFields(screen) = &mut self.screen {
                screen.result = Some(result);
            }
            if saved {
                self.save_settings_config();
            }
            return;
        }
        let Screen::TextFields(screen) = &mut self.screen else {
            return;
        };
        let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);
        match key.code {
            KeyCode::Up | KeyCode::BackTab => screen.move_selection(-1),
            KeyCode::Down | KeyCode::Tab => screen.move_selection(1),
            KeyCode::Left => screen.selected_field_mut().move_left(),
            KeyCode::Right => screen.selected_field_mut().move_right(),
            KeyCode::Home => screen.selected_field_mut().home(),
            KeyCode::End => screen.selected_field_mut().end(),
            KeyCode::Backspace => screen.selected_field_mut().backspace(),
            KeyCode::Char(c) if !ctrl => screen.selected_field_mut().insert(c),
            _ => {}
        }
    }

    /// The Shortcut screen has no editable text: the binding always comes
    /// from the desktop's own dialog, so the arrow keys, Enter, Ctrl+R,
    /// Escape/Backspace and `q` are the only commands. While the system dialog
    /// is in flight, Escape/Backspace leaves, Enter keeps the current binding,
    /// and Ctrl+R reopens the dialog - so a window closed with no change is
    /// never a dead end. While the portal test runs, only Escape/Backspace
    /// does anything.
    fn handle_shortcut_key(&mut self, key: KeyEvent) {
        if let Screen::Shortcut(screen) = &self.screen {
            match &screen.status {
                screens::ShortcutStatus::DialogOpen => {
                    match key.code {
                        // Stop waiting and settle on whatever is bound now,
                        // rather than blocking until the reconfigure timeout.
                        KeyCode::Enter => self.keep_current_shortcut(),
                        KeyCode::Esc | KeyCode::Backspace => {
                            self.cancel_shortcut_dialog();
                            self.screen = Screen::Home;
                        }
                        KeyCode::Char('r') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                            self.cancel_shortcut_dialog();
                            self.start_shortcut_action(Some(0));
                        }
                        _ => {}
                    }
                    return;
                }
                screens::ShortcutStatus::Testing => {
                    if matches!(key.code, KeyCode::Esc | KeyCode::Backspace) {
                        self.shortcut_test = None;
                        self.screen = Screen::Home;
                    }
                    return;
                }
                _ => {}
            }
        }
        match key.code {
            KeyCode::Char('q') => self.should_quit = true,
            KeyCode::Esc | KeyCode::Backspace => self.screen = Screen::Home,
            KeyCode::Up => {
                if let Screen::Shortcut(screen) = &mut self.screen {
                    screen.move_selection(-1);
                }
            }
            KeyCode::Down => {
                if let Screen::Shortcut(screen) = &mut self.screen {
                    screen.move_selection(1);
                }
            }
            KeyCode::Char('r') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                self.start_shortcut_action(Some(0));
            }
            KeyCode::Enter => self.start_shortcut_action(None),
            _ => {}
        }
    }

    /// Stops the in-flight reconfigure wait, if any. The background thread
    /// settles on its own (`Unchanged`) once the cancellation lands; its late
    /// result is never polled again.
    fn cancel_shortcut_dialog(&mut self) {
        if let Some(handle) = self.shortcut_dialog.take() {
            handle.cancel();
        }
    }

    /// The user kept the current binding after the desktop's settings window
    /// was closed with no change: stop waiting and settle on it.
    fn keep_current_shortcut(&mut self) {
        self.cancel_shortcut_dialog();
        let trigger_description = self.config.activation.keybind.clone();
        self.apply_shortcut_dialog_outcome(Ok(activation::ReconfigureOutcome::Unchanged {
            trigger_description,
        }));
    }

    fn start_shortcut_action(&mut self, action: Option<usize>) {
        let Screen::Shortcut(screen) = &mut self.screen else {
            return;
        };
        match action.unwrap_or(screen.selected_action) {
            0 => {
                screen.status = screens::ShortcutStatus::DialogOpen;
                self.shortcut_dialog = Some(setup::reconfigure_shortcut_async());
            }
            1 => {
                screen.status = screens::ShortcutStatus::Testing;
                self.shortcut_test = Some(setup::shortcut_test_async());
            }
            _ => {}
        }
    }

    fn handle_info_key(&mut self, key: KeyEvent) {
        match key.code {
            KeyCode::Char('q') => self.should_quit = true,
            KeyCode::Esc | KeyCode::Backspace | KeyCode::Enter => {
                self.screen = match self.info_return {
                    InfoReturn::Deck => Screen::Deck,
                    InfoReturn::Home => Screen::Home,
                }
            }
            KeyCode::Up | KeyCode::Char('k') => self.adjust_info_scroll(-1),
            KeyCode::Down | KeyCode::Char('j') => self.adjust_info_scroll(1),
            KeyCode::PageUp => self.adjust_info_scroll(-5),
            KeyCode::PageDown => self.adjust_info_scroll(5),
            KeyCode::Home => self.info_scroll.set(0),
            KeyCode::End => self.info_scroll.set(u16::MAX),
            _ => {}
        }
    }

    /// `render_info` clamps `info_scroll` to the wrapped content height every
    /// frame, so moving it without knowing the pane geometry here cannot
    /// strand the view past the end.
    fn adjust_info_scroll(&self, delta: i32) {
        let current = self.info_scroll.get();
        let next = if delta >= 0 {
            current.saturating_add(delta.unsigned_abs() as u16)
        } else {
            current.saturating_sub(delta.unsigned_abs() as u16)
        };
        self.info_scroll.set(next);
    }

    fn handle_model_key(&mut self, key: KeyEvent) {
        if Self::is_back_key(key.code) {
            self.screen = Screen::Home;
            return;
        }
        match key.code {
            KeyCode::Char('q') => self.should_quit = true,
            KeyCode::Up | KeyCode::Char('k') => {
                if let Screen::Model(screen) = &mut self.screen {
                    screen.move_selection(-1);
                }
            }
            KeyCode::Down | KeyCode::Char('j') => {
                if let Screen::Model(screen) = &mut self.screen {
                    screen.move_selection(1);
                }
            }
            KeyCode::Tab | KeyCode::BackTab => {
                if let Screen::Model(screen) = &mut self.screen {
                    screen.toggle_focus();
                }
            }
            KeyCode::Enter if self.pending.is_none() => {
                if let Screen::Model(screen) = &self.screen {
                    match screen.focus {
                        screens::ModelFocus::Catalog => {
                            let id = screen.selected_id().to_string();
                            self.start_model_activation(id);
                        }
                        screens::ModelFocus::Backend => {
                            if let Some(name) = screen.selected_backend() {
                                self.spawn_pending(
                                    "Backend change",
                                    backend_activation_task(name),
                                    None,
                                );
                            }
                        }
                    }
                }
            }
            _ => {}
        }
    }

    /// The dedicated "Inference backend" settings screen: picks a backend with
    /// the same up/down/Esc shape as the other settings screens, then runs the
    /// Model screen backend panel's activate-and-confirm flow (save, reload a
    /// running daemon, re-run diagnostics) so the change takes effect at once.
    fn handle_inference_backend_key(&mut self, key: KeyEvent) {
        if Self::is_back_key(key.code) {
            self.screen = Screen::Home;
            return;
        }
        match key.code {
            KeyCode::Char('q') => self.should_quit = true,
            KeyCode::Up
            | KeyCode::Down
            | KeyCode::Left
            | KeyCode::Right
            | KeyCode::Char('k')
            | KeyCode::Char('j')
            | KeyCode::Char('h')
            | KeyCode::Char('l') => {
                let delta = Self::step_delta(key.code).unwrap_or(0);
                if let Screen::InferenceBackend(screen) = &mut self.screen {
                    screen.move_selection(delta);
                }
            }
            KeyCode::Enter if self.pending.is_none() => {
                if let Screen::InferenceBackend(screen) = &self.screen {
                    if let Some(name) = screen.selected_backend() {
                        self.spawn_pending("Backend change", backend_activation_task(name), None);
                    }
                }
            }
            _ => {}
        }
    }

    fn handle_autostart_key(&mut self, key: KeyEvent) {
        if Self::is_back_key(key.code) {
            self.commit_and_leave_settings();
            return;
        }
        match key.code {
            KeyCode::Char('q') => self.should_quit = true,
            KeyCode::Up
            | KeyCode::Down
            | KeyCode::Left
            | KeyCode::Right
            | KeyCode::Char('k')
            | KeyCode::Char('j')
            | KeyCode::Char('h')
            | KeyCode::Char('l') => {
                let delta = Self::step_delta(key.code).unwrap_or(0);
                if let Screen::Autostart(screen) = &mut self.screen {
                    screen.move_selection(delta);
                }
            }
            KeyCode::Enter => {
                let target = match &self.screen {
                    Screen::Autostart(screen) => Some(screen.selected == 1),
                    _ => None,
                };
                if let Some(target) = target {
                    let result = crate::autostart::update(target);
                    if result.is_ok() {
                        self.config.startup.autostart = target;
                    }
                    if let Screen::Autostart(screen) = &mut self.screen {
                        screen.result = Some(result.map_err(|error| error.to_string()));
                    }
                }
            }
            _ => {}
        }
    }

    fn handle_daemon_key(&mut self, key: KeyEvent) {
        if Self::is_back_key(key.code) {
            self.screen = Screen::Home;
            return;
        }
        match key.code {
            KeyCode::Char('q') => self.should_quit = true,
            KeyCode::Up
            | KeyCode::Down
            | KeyCode::Left
            | KeyCode::Right
            | KeyCode::Char('k')
            | KeyCode::Char('j')
            | KeyCode::Char('h')
            | KeyCode::Char('l') => {
                let delta = Self::step_delta(key.code).unwrap_or(0);
                if let Screen::Daemon(screen) = &mut self.screen {
                    screen.move_selection(delta);
                }
            }
            KeyCode::Enter if self.pending.is_none() => {
                if let Screen::Daemon(screen) = &self.screen {
                    match screen.selected_action() {
                        "Start" => self.spawn_pending("Daemon", daemon_task(), None),
                        "Stop" => self.spawn_pending("Daemon", daemon_stop_task(), None),
                        "Restart" => self.spawn_pending("Daemon", daemon_restart_task(), None),
                        other => unreachable!("daemon screen has no handler for {other:?}"),
                    }
                }
            }
            _ => {}
        }
    }

    fn dispatch_command(
        &mut self,
        name: &str,
        terminal: &mut Terminal<Backend>,
    ) -> anyhow::Result<()> {
        match name {
            "setup" => self.run_setup_console(terminal)?,
            "daemon" => self.screen = Screen::Daemon(screens::DaemonScreen::new()),
            "toggle" => self.spawn_ipc_pending("Toggle", ipc::Request::Toggle),
            "cancel" => self.spawn_ipc_pending("Cancel", ipc::Request::Cancel),
            "status" => self.spawn_ipc_pending("Status", ipc::Request::Status),
            "reload" => self.spawn_ipc_pending("Reload", ipc::Request::ReloadConfig),
            "last-result" => self.spawn_ipc_pending("Last result", ipc::Request::GetLastResult),
            "doctor" => {
                let config = self.config.clone();
                self.spawn_pending("Doctor", doctor_task(config), None);
            }
            "shortcut-test" => {
                self.spawn_pending("Shortcut test - press it now", shortcut_test_task(), None);
            }
            other => unreachable!("dashboard home has no handler for CLI command {other:?}"),
        }
        Ok(())
    }

    fn spawn_ipc_pending(&mut self, title: &'static str, request: ipc::Request) {
        self.spawn_pending(title, ipc_task(request), None);
    }

    fn spawn_pending<F>(
        &mut self,
        title: &'static str,
        future: F,
        progress: Option<Arc<Mutex<(u64, u64)>>>,
    ) where
        F: std::future::Future<Output = Vec<OutputLine>> + Send + 'static,
    {
        let handle = tokio::spawn(future);
        self.pending = Some(PendingAction {
            title,
            handle,
            progress,
        });
    }

    fn start_model_activation(&mut self, id: String) {
        let progress = Arc::new(Mutex::new((0u64, 0u64)));
        self.spawn_pending(
            "Model activation",
            model_activation_task(id, progress.clone()),
            Some(progress),
        );
    }

    /// Runs the pre-existing setup console (`setup::run_console`) by
    /// temporarily leaving the dashboard's own alternate screen - terminals
    /// track one alternate-screen buffer, not a stack, so re-entering it
    /// without first leaving would strand the dashboard on the primary
    /// screen once the console exits.
    fn run_setup_console(&mut self, terminal: &mut Terminal<Backend>) -> anyhow::Result<()> {
        disable_raw_mode()?;
        execute!(terminal.backend_mut(), LeaveAlternateScreen)?;

        let outcome = setup::run_console();

        enable_raw_mode()?;
        execute!(terminal.backend_mut(), EnterAlternateScreen)?;
        terminal.clear()?;

        if let Ok(reloaded) = Config::load() {
            self.config = reloaded;
        }
        if let Err(error) = outcome {
            self.info_scroll.set(0);
            self.remember_info_return();
            self.screen = Screen::Info {
                title: "Setup".to_string(),
                lines: vec![OutputLine {
                    text: error.to_string(),
                    is_error: true,
                }],
            };
        }
        Ok(())
    }

    /// The Info pane returns to whichever screen created it: the Deck keeps
    /// its own help and action results, while Home (including its settings and
    /// Commands screens) keeps its own.
    fn remember_info_return(&mut self) {
        self.info_return = match &self.screen {
            Screen::Deck => InfoReturn::Deck,
            _ => InfoReturn::Home,
        };
    }

    fn render(&self, frame: &mut Frame) {
        let area = frame.area();
        if let Some(until) = self.splash_until.get() {
            if Instant::now() < until {
                self.render_splash(frame, area);
                return;
            }
            self.splash_until.set(None);
        }
        match &self.screen {
            Screen::Deck => self.render_deck(frame, area),
            Screen::Home => self.render_home(frame, area),
            Screen::Commands => self.render_commands(frame, area),
            Screen::Info { title, lines } => {
                render_info(frame, area, title, lines, &self.info_scroll)
            }
            Screen::Model(screen) => self.render_model(frame, area, screen),
            Screen::InferenceBackend(screen) => self.render_inference_backend(frame, area, screen),
            Screen::Autostart(screen) => self.render_autostart(frame, area, screen),
            Screen::Daemon(screen) => self.render_daemon(frame, area, screen),
            Screen::Microphone(screen) => self.render_microphone(frame, area, screen),
            Screen::Activation(screen) => self.render_activation(frame, area, screen),
            Screen::Translation(screen) => self.render_translation(frame, area, screen),
            Screen::TextProcessing(screen) => self.render_text_processing(frame, area, screen),
            Screen::Shortcut(screen) => self.render_shortcut(frame, area, screen),
            Screen::TranscriptOutput(screen) => self.render_output(frame, area, screen),
            Screen::TypingBackend(screen) => self.render_typing_backend(frame, area, screen),
            Screen::Overlay(screen) => self.render_overlay(frame, area, screen),
            Screen::MediaPause(screen) => self.render_media_pause(frame, area, screen),
            Screen::TextFields(screen) => self.render_text_fields(frame, area, screen),
        }
    }

    fn render_home(&self, frame: &mut Frame, area: Rect) {
        let (logo_lines, logo_height) = logo::centered_logo(area.width);
        let chunks = Layout::default()
            .direction(Direction::Vertical)
            .constraints([
                Constraint::Length(logo_height),
                Constraint::Length(1),
                Constraint::Length(SETTINGS_PANEL_HEIGHT),
                Constraint::Min(0),
                Constraint::Length(1),
            ])
            .split(area);

        frame.render_widget(Paragraph::new(logo_lines), chunks[0]);
        frame.render_widget(Paragraph::new(self.status_strip_line()), chunks[1]);
        frame.render_widget(
            self.settings_panel_widget(chunks[2].width.saturating_sub(2) as usize),
            chunks[2],
        );
        if self.config.history.enabled {
            frame.render_widget(
                Paragraph::new(Self::history_notice_line()).wrap(Wrap { trim: true }),
                chunks[3],
            );
        }

        let footer = match &self.pending {
            Some(pending) => Line::from(Span::styled(
                format!("Working: {}...", pending.title),
                Style::default().fg(Color::Yellow),
            )),
            None => Line::from("↑/↓ move   Enter/→ open   q quit"),
        };
        frame.render_widget(Paragraph::new(footer), chunks[4]);
    }

    /// Shown on the home screen while transcripts are being saved, so an
    /// upgraded configuration that kept `enabled = true` says where the new
    /// opt-out lives instead of silently continuing to fill the folder.
    fn history_notice_line() -> Line<'static> {
        Line::from(Span::styled(
            "Saving transcripts is on. Turn it off in Transcript output > Save transcript history.",
            Style::default().fg(Color::Yellow),
        ))
    }

    /// The dedicated Commands screen, reached from the Settings panel's
    /// Commands row. It lists the same clap-derived commands the home screen
    /// used to stack below the Settings panel, with the standard list grammar.
    fn render_commands(&self, frame: &mut Frame, area: Rect) {
        let rows: Vec<String> = self
            .command_items
            .iter()
            .map(|item| format!("{:<14} {}", item.name, item.about))
            .collect();
        let chunks = Layout::default()
            .direction(Direction::Vertical)
            .constraints([Constraint::Min(3), Constraint::Length(1)])
            .split(area);
        frame.render_widget(
            selection_list(
                "Home \u{203a} Commands",
                &rows,
                self.command_selected,
                chunks[0].width.saturating_sub(2) as usize,
            ),
            chunks[0],
        );
        let footer = match &self.pending {
            Some(pending) => Line::from(Span::styled(
                format!("Working: {}...", pending.title),
                Style::default().fg(Color::Yellow),
            )),
            None => Line::from("↑/↓ row   Enter run   Esc/\u{232b} back   q quit"),
        };
        frame.render_widget(Paragraph::new(footer), chunks[1]);
    }

    fn daemon_running_cached(&self) -> bool {
        let now = Instant::now();
        if let Some((running, checked_at)) = self.daemon_status_cache.get() {
            if now.duration_since(checked_at) < DAEMON_STATUS_REFRESH {
                return running;
            }
        }
        let running = commands::daemon_socket_exists();
        self.daemon_status_cache.set(Some((running, now)));
        running
    }

    fn status_strip_line(&self) -> Line<'static> {
        let daemon_running = self.daemon_running_cached();
        let (daemon_text, daemon_color) = if daemon_running {
            ("running", Color::Green)
        } else {
            ("not running", Color::Red)
        };
        Line::from(vec![
            Span::raw("Daemon: "),
            Span::styled(daemon_text, Style::default().fg(daemon_color)),
            Span::raw("   Model: "),
            Span::styled(
                self.config.model.active_model.clone(),
                Style::default()
                    .fg(Color::Cyan)
                    .add_modifier(Modifier::BOLD),
            ),
        ])
    }

    /// The current value shown next to a Settings-panel row, grounded in the
    /// same config fields the corresponding settings screen edits.
    fn setting_value(&self, setting: SettingId) -> String {
        match setting {
            SettingId::Model => self.config.model.active_model.clone(),
            SettingId::InferenceBackend => self.config.model.preferred_backend.clone(),
            SettingId::Translation => self.config.transcription.translate_label().to_string(),
            SettingId::Decoding => self.config.transcription.decoding_summary(),
            SettingId::TextProcessing => self.config.text.processing_summary(),
            SettingId::Microphone => self
                .capabilities
                .microphone_label(&self.config.audio.microphone),
            SettingId::Activation => self.config.activation.mode.to_string(),
            SettingId::Shortcut => activation::keybind_display(&self.config.activation.keybind),
            SettingId::TranscriptOutput => {
                let method = match self.config.output.method {
                    OutputMethod::None => "do not deliver",
                    OutputMethod::Type => "type into the focused application",
                    OutputMethod::Clipboard => "copy to the clipboard",
                    OutputMethod::Paste => "paste into the focused application",
                };
                let mut value = method.to_string();
                if self.config.output.method == OutputMethod::Paste {
                    value.push_str(&format!(" with {}", self.config.output.paste_keys));
                }
                if self.config.output.auto_submit && self.config.output.method.inserts_text() {
                    value.push_str(", then Enter");
                }
                value
            }
            SettingId::TypingBackend => self.config.output.typing_backend.clone(),
            SettingId::TranscriptFolder => self.config.history.folder_summary(),
            SettingId::HistoryRetention => {
                if self.config.history.enabled {
                    self.config.history.retention_summary()
                } else {
                    "history off".to_string()
                }
            }
            SettingId::Startup => {
                if self.config.startup.autostart {
                    "start when you sign in".to_string()
                } else {
                    "start manually".to_string()
                }
            }
            SettingId::Overlay => {
                if !self.config.overlay.enabled {
                    "disabled".to_string()
                } else {
                    format!(
                        "enabled, {}, {}, {}",
                        self.config.overlay.position,
                        self.config.overlay.style,
                        screens::overlay_look_summary(&self.config)
                    )
                }
            }
            SettingId::MediaPause => {
                if self.config.audio.pause_media {
                    "pause while dictating".to_string()
                } else {
                    "keep playing".to_string()
                }
            }
        }
    }

    /// The Settings panel: four labeled groups whose headers are dim and not
    /// selectable, then a single `Commands` row that opens the dedicated
    /// Commands screen. Every selectable row is `"> "`/`"  "`-prefixed, drawn as
    /// a full-width highlight bar when selected, with the value column aligned
    /// after an 18-wide name.
    fn settings_panel_widget(&self, width: usize) -> Paragraph<'static> {
        let header_style = Style::default().fg(Color::DarkGray);
        let mut lines: Vec<Line> = Vec::new();
        let mut selectable = 0usize;
        for (section, settings) in SETTING_SECTIONS {
            lines.push(Line::from(Span::styled(
                format!("  {section}"),
                header_style,
            )));
            for setting in settings {
                let selected = selectable == self.home_selected;
                let marker = if selected { "> " } else { "  " };
                let text = format!(
                    "{marker}{:<18} {}",
                    setting.label(),
                    self.setting_value(*setting)
                );
                let mut spans = vec![Span::styled(text, selection_style(selected))];
                pad_selected_row(&mut spans, selected, width);
                lines.push(Line::from(spans));
                selectable += 1;
            }
        }
        let selected = self.home_selected == SettingId::ALL.len();
        let marker = if selected { "> " } else { "  " };
        let text = format!(
            "{marker}{:<18} {}",
            "Commands", "setup, daemon, doctor, status, ..."
        );
        let mut spans = vec![Span::styled(text, selection_style(selected))];
        pad_selected_row(&mut spans, selected, width);
        lines.push(Line::from(spans));
        Paragraph::new(lines).block(Block::default().borders(Borders::ALL).title("Settings"))
    }

    fn render_model(&self, frame: &mut Frame, area: Rect, screen: &screens::ModelScreen) {
        let chunks = Layout::default()
            .direction(Direction::Vertical)
            .constraints([
                Constraint::Min(5),
                Constraint::Length(screen.backend_widget_height()),
                Constraint::Length(4),
                Constraint::Length(1),
            ])
            .split(area);

        frame.render_widget(
            screen.list_widget(chunks[0].width.saturating_sub(2) as usize, chunks[0].height),
            chunks[0],
        );
        frame.render_widget(
            screen.backend_widget(chunks[1].width.saturating_sub(2) as usize),
            chunks[1],
        );
        let chunks = &chunks[1..];

        match &self.pending {
            Some(pending) if pending.progress.is_some() => {
                let progress = pending.progress.as_ref().unwrap();
                let (downloaded, total) = progress.lock().map(|guard| *guard).unwrap_or((0, 0));
                let ratio = if total == 0 {
                    0.0
                } else {
                    (downloaded as f64 / total as f64).clamp(0.0, 1.0)
                };
                let gauge = Gauge::default()
                    .block(Block::default().borders(Borders::ALL).title("Activating"))
                    .gauge_style(Style::default().fg(Color::Cyan))
                    .ratio(ratio);
                frame.render_widget(gauge, chunks[1]);
            }
            Some(pending) => {
                frame.render_widget(
                    Paragraph::new(format!("Working: {}...", pending.title))
                        .block(Block::default().borders(Borders::ALL)),
                    chunks[1],
                );
            }
            None => {
                frame.render_widget(screen.selected_detail_widget(), chunks[1]);
            }
        }

        frame.render_widget(
            Paragraph::new(
                "↑/↓ row   Tab switch list   Enter activate   Esc/\u{232b} back   q quit",
            ),
            chunks[2],
        );
    }

    fn render_inference_backend(
        &self,
        frame: &mut Frame,
        area: Rect,
        screen: &screens::BackendScreen,
    ) {
        let chunks = Layout::default()
            .direction(Direction::Vertical)
            .constraints([Constraint::Min(4), Constraint::Length(1)])
            .split(area);
        frame.render_widget(
            screen.list_widget(&self.config, chunks[0].width.saturating_sub(2) as usize),
            chunks[0],
        );
        frame.render_widget(
            Paragraph::new("↑/↓ row   Enter apply   Esc/\u{232b} back   q quit"),
            chunks[1],
        );
    }

    fn render_daemon(&self, frame: &mut Frame, area: Rect, screen: &screens::DaemonScreen) {
        let chunks = Layout::default()
            .direction(Direction::Vertical)
            .constraints([
                Constraint::Min(4),
                Constraint::Length(1),
                Constraint::Length(1),
            ])
            .split(area);
        frame.render_widget(
            screen.list_widget(chunks[0].width.saturating_sub(2) as usize),
            chunks[0],
        );
        let status = match &self.pending {
            Some(pending) => Line::from(Span::styled(
                format!("Working: {}...", pending.title),
                Style::default().fg(Color::Yellow),
            )),
            None => Line::from(""),
        };
        frame.render_widget(Paragraph::new(status), chunks[1]);
        frame.render_widget(
            Paragraph::new("↑/↓ row   Enter run   Esc/\u{232b} back   q quit"),
            chunks[2],
        );
    }

    fn render_autostart(&self, frame: &mut Frame, area: Rect, screen: &screens::AutostartScreen) {
        let chunks = Layout::default()
            .direction(Direction::Vertical)
            .constraints([
                Constraint::Min(4),
                Constraint::Length(1),
                Constraint::Length(1),
            ])
            .split(area);
        frame.render_widget(
            screen.list_widget(chunks[0].width.saturating_sub(2) as usize),
            chunks[0],
        );
        frame.render_widget(Paragraph::new(screen.result_line()), chunks[1]);
        frame.render_widget(
            Paragraph::new("↑/↓ row   ←/→ change   Enter save   Esc/\u{232b} back   q quit"),
            chunks[2],
        );
    }

    fn render_microphone(&self, frame: &mut Frame, area: Rect, screen: &screens::MicrophoneScreen) {
        let chunks = Layout::default()
            .direction(Direction::Vertical)
            .constraints([
                Constraint::Min(3),
                Constraint::Length(3),
                Constraint::Length(1),
                Constraint::Length(1),
            ])
            .split(area);
        frame.render_widget(
            screen.list_widget(chunks[0].width.saturating_sub(2) as usize),
            chunks[0],
        );
        match screen.error() {
            Some(error) => frame.render_widget(
                Paragraph::new(Span::styled(
                    format!("Unavailable: {error}"),
                    Style::default().fg(Color::Red),
                ))
                .block(Block::default().borders(Borders::ALL).title("Input level")),
                chunks[1],
            ),
            None => frame.render_widget(screen.gauge(), chunks[1]),
        }
        frame.render_widget(Paragraph::new(screen.result_line()), chunks[2]);
        frame.render_widget(
            Paragraph::new("↑/↓ row   ←/→ change   Enter save   Esc/\u{232b} back   q quit"),
            chunks[3],
        );
    }

    fn render_activation(&self, frame: &mut Frame, area: Rect, screen: &screens::ActivationScreen) {
        let chunks = Layout::default()
            .direction(Direction::Vertical)
            .constraints([
                Constraint::Min(3),
                Constraint::Length(1),
                Constraint::Length(1),
            ])
            .split(area);
        frame.render_widget(
            screen.list_widget(chunks[0].width.saturating_sub(2) as usize),
            chunks[0],
        );
        frame.render_widget(Paragraph::new(screen.result_line()), chunks[1]);
        frame.render_widget(
            Paragraph::new("↑/↓ row   ←/→ change   Enter save   Esc/\u{232b} back   q quit"),
            chunks[2],
        );
    }

    fn render_translation(
        &self,
        frame: &mut Frame,
        area: Rect,
        screen: &screens::TranslationScreen,
    ) {
        let chunks = Layout::default()
            .direction(Direction::Vertical)
            .constraints([
                Constraint::Min(3),
                Constraint::Length(1),
                Constraint::Length(1),
                Constraint::Length(1),
            ])
            .split(area);
        frame.render_widget(
            screen.list_widget(chunks[0].width.saturating_sub(2) as usize),
            chunks[0],
        );
        frame.render_widget(
            Paragraph::new("Translation needs a multilingual model; .en models always transcribe."),
            chunks[1],
        );
        frame.render_widget(Paragraph::new(screen.result_line()), chunks[2]);
        frame.render_widget(
            Paragraph::new("↑/↓ row   ←/→ change   Enter save   Esc/\u{232b} back   q quit"),
            chunks[3],
        );
    }

    fn render_text_processing(
        &self,
        frame: &mut Frame,
        area: Rect,
        screen: &screens::TextProcessingScreen,
    ) {
        let chunks = Layout::default()
            .direction(Direction::Vertical)
            .constraints([
                Constraint::Min(6),
                Constraint::Length(1),
                Constraint::Length(1),
                Constraint::Length(1),
            ])
            .split(area);
        frame.render_widget(
            screen.list_widget(chunks[0].width.saturating_sub(2) as usize),
            chunks[0],
        );
        frame.render_widget(
            Paragraph::new(
                "Replacements: 'from => to' separated by ';'; escape ';', '>', and '\\' with '\\'. Filler removal is English-only.",
            ),
            chunks[1],
        );
        frame.render_widget(Paragraph::new(screen.result_line()), chunks[2]);
        frame.render_widget(
            Paragraph::new("↑/↓ row   ←/→ edit   Enter save   Esc back   Ctrl+C quit"),
            chunks[3],
        );
    }

    fn render_media_pause(
        &self,
        frame: &mut Frame,
        area: Rect,
        screen: &screens::MediaPauseScreen,
    ) {
        let chunks = Layout::default()
            .direction(Direction::Vertical)
            .constraints([
                Constraint::Min(3),
                Constraint::Length(1),
                Constraint::Length(1),
            ])
            .split(area);
        frame.render_widget(
            screen.list_widget(chunks[0].width.saturating_sub(2) as usize),
            chunks[0],
        );
        frame.render_widget(Paragraph::new(screen.result_line()), chunks[1]);
        frame.render_widget(
            Paragraph::new("↑/↓ row   ←/→ change   Enter save   Esc/\u{232b} back   q quit"),
            chunks[2],
        );
    }

    fn render_shortcut(&self, frame: &mut Frame, area: Rect, screen: &screens::ShortcutScreen) {
        let chunks = Layout::default()
            .direction(Direction::Vertical)
            .constraints([Constraint::Min(6), Constraint::Length(1)])
            .split(area);
        frame.render_widget(
            screen.body(
                &activation::keybind_display(&self.config.activation.keybind),
                chunks[0].width.saturating_sub(2) as usize,
            ),
            chunks[0],
        );
        frame.render_widget(Paragraph::new(screen.footer()), chunks[1]);
    }

    fn render_output(&self, frame: &mut Frame, area: Rect, screen: &screens::OutputScreen) {
        let chunks = Layout::default()
            .direction(Direction::Vertical)
            .constraints([
                Constraint::Min(3),
                Constraint::Length(1),
                Constraint::Length(1),
                Constraint::Length(1),
            ])
            .split(area);
        frame.render_widget(
            screen.list_widget(&self.config, chunks[0].width.saturating_sub(2) as usize),
            chunks[0],
        );
        frame.render_widget(Paragraph::new(screen.warning_line()), chunks[1]);
        frame.render_widget(Paragraph::new(screen.result_line()), chunks[2]);
        frame.render_widget(
            Paragraph::new("↑/↓ row   ←/→ change   Enter choose   Esc/\u{232b} back   q quit"),
            chunks[3],
        );
    }

    fn render_typing_backend(
        &self,
        frame: &mut Frame,
        area: Rect,
        screen: &screens::TypingBackendScreen,
    ) {
        let chunks = Layout::default()
            .direction(Direction::Vertical)
            .constraints([
                Constraint::Min(3),
                Constraint::Length(1),
                Constraint::Length(1),
                Constraint::Length(1),
                Constraint::Length(1),
            ])
            .split(area);
        frame.render_widget(
            screen.list_widget(chunks[0].width.saturating_sub(2) as usize),
            chunks[0],
        );
        frame.render_widget(
            Paragraph::new("Only used when transcripts are typed into the focused application."),
            chunks[1],
        );
        frame.render_widget(Paragraph::new(screen.warning_line()), chunks[2]);
        frame.render_widget(Paragraph::new(screen.result_line()), chunks[3]);
        frame.render_widget(
            Paragraph::new("↑/↓ row   ←/→ change   Enter save   Esc/\u{232b} back   q quit"),
            chunks[4],
        );
    }

    fn render_overlay(&self, frame: &mut Frame, area: Rect, screen: &screens::OverlayScreen) {
        // The preview follows the *pending* values the cursor is editing, not
        // the saved config, so it redraws on every `←`/`→` step before anything
        // is committed. It runs as a modest band below the list, and draws the
        // overlay inside a mini screen so the placement is visible too.
        let (list_area, preview_area) = if screen.enabled {
            let rows = Layout::default()
                .direction(Direction::Vertical)
                .constraints([Constraint::Min(3), Constraint::Length(PREVIEW_BAND_HEIGHT)])
                .split(area);
            (rows[0], Some(rows[1]))
        } else {
            (area, None)
        };

        let chunks = Layout::default()
            .direction(Direction::Vertical)
            .constraints([
                Constraint::Min(3),
                Constraint::Length(1),
                Constraint::Length(1),
            ])
            .split(list_area);
        frame.render_widget(
            screen.list_widget(chunks[0].width.saturating_sub(2) as usize),
            chunks[0],
        );
        frame.render_widget(Paragraph::new(screen.result_line()), chunks[1]);
        frame.render_widget(
            Paragraph::new("↑/↓ row   ←/→ change   Enter save   Esc/\u{232b} back   q quit"),
            chunks[2],
        );

        if let Some(preview_area) = preview_area {
            let title = format!(
                "Preview \u{00b7} {} \u{00b7} {}",
                screens::overlay_style_label(&screen.style),
                screen.position
            );
            let block = Block::default().borders(Borders::ALL).title(title);
            let inner = block.inner(preview_area);
            frame.render_widget(block, preview_area);
            // A one-cell horizontal inset so the drawn shape reads as sitting
            // inside the screen rather than touching its frame.
            let stage = inner.inner(ratatui::layout::Margin::new(1, 0));
            let lines = self.overlay_preview_lines(
                &screen.style,
                &screen.look,
                &screen.waveform_shape,
                stage.width,
                stage.height,
            );
            let cols = lines.iter().map(|line| line.width()).max().unwrap_or(0) as u16;
            let rows = lines.len() as u16;
            let placed = placed_rect(stage, cols, rows, &screen.position);
            frame.render_widget(Paragraph::new(lines), placed);
        }
    }

    /// The preview cells for the selected overlay style, reusing the cached
    /// rasterisation when the style, look, and waveform shape are unchanged so the
    /// dashboard does not re-render the full-resolution frame every tick. The
    /// raster is fitted uniformly into the `max_cols` x `max_rows` box at its
    /// true aspect ratio (see `preview_cell_size`), so wide styles stay wide
    /// and square styles stay square instead of stretching to fill the box.
    fn overlay_preview_lines(
        &self,
        style: &str,
        look: &str,
        shape: &str,
        max_cols: u16,
        max_rows: u16,
    ) -> Vec<Line<'static>> {
        let mut cache = self.overlay_preview.borrow_mut();
        let stale = match cache.as_ref() {
            Some(preview) => {
                preview.style != style || preview.look != look || preview.shape != shape
            }
            None => true,
        };
        if stale {
            let (width, height, pixels) = crate::overlay::render_frame_pixels(
                style,
                Some("recording"),
                0.0,
                look,
                shape,
                &[],
            );
            *cache = Some(OverlayPreview {
                style: style.to_string(),
                look: look.to_string(),
                shape: shape.to_string(),
                width,
                height,
                pixels,
            });
        }
        let preview = cache.as_ref().expect("preview populated above");
        let (cols, rows) = preview_cell_size(preview.width, preview.height, max_cols, max_rows);
        preview_lines_from_pixels(preview.width, preview.height, &preview.pixels, cols, rows)
    }

    fn render_text_fields(
        &self,
        frame: &mut Frame,
        area: Rect,
        screen: &screens::TextFieldsScreen,
    ) {
        let chunks = Layout::default()
            .direction(Direction::Vertical)
            .constraints([
                Constraint::Min(4),
                Constraint::Length(1),
                Constraint::Length(1),
                Constraint::Length(1),
            ])
            .split(area);
        frame.render_widget(
            screen.list_widget(chunks[0].width.saturating_sub(2) as usize),
            chunks[0],
        );
        frame.render_widget(Paragraph::new(screen.hint_line()), chunks[1]);
        frame.render_widget(
            Paragraph::new(screen.result_line(&format!("{} saved.", screen.title))),
            chunks[2],
        );
        frame.render_widget(
            Paragraph::new("↑/↓ row   ←/→ edit   Enter save   Esc back   Ctrl+C quit"),
            chunks[3],
        );
    }
}

/// The Info/result pane shared by every command that reports back to the
/// dashboard (last-result, status, doctor, model/backend activation, setup
/// errors). Its content is arbitrary command output - a transcript can be one
/// very long line - so it must wrap rather than let the widget clip anything
/// past the pane edge, and it must scroll so wrapped content taller than the
/// pane stays reachable.
fn render_info(
    frame: &mut Frame,
    area: Rect,
    title: &str,
    lines: &[OutputLine],
    scroll: &Cell<u16>,
) {
    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([Constraint::Min(3), Constraint::Length(1)])
        .split(area);

    let rendered = rendered_output_lines(lines);

    // Clamp the persistent offset to the wrapped content height. The count is
    // measured without the block so `line_count` uses this exact inner width -
    // the rendered `Paragraph`'s text area after its borders - keeping the
    // clamp consistent with how many rows actually get drawn.
    let inner_width = chunks[0].width.saturating_sub(2).max(1);
    let inner_height = chunks[0].height.saturating_sub(2);
    let wrapped_rows = Paragraph::new(rendered.clone())
        .wrap(Wrap { trim: false })
        .line_count(inner_width) as u16;
    let offset = scroll.get().min(wrapped_rows.saturating_sub(inner_height));
    scroll.set(offset);

    frame.render_widget(
        Paragraph::new(rendered)
            .block(
                Block::default()
                    .borders(Borders::ALL)
                    .title(title.to_string()),
            )
            .wrap(Wrap { trim: false })
            .scroll((offset, 0)),
        chunks[0],
    );
    frame.render_widget(
        Paragraph::new("↑/↓ scroll   Esc/\u{232b}/Enter back   q quit"),
        chunks[1],
    );
}

fn rendered_output_lines(lines: &[OutputLine]) -> Vec<Line<'static>> {
    lines
        .iter()
        .map(|output| {
            let style = if output.is_error {
                Style::default().fg(Color::Red)
            } else {
                Style::default()
            };
            Line::from(Span::styled(output.text.clone(), style))
        })
        .collect()
}

/// Fits a rasterised overlay frame of `pixels_width` x `pixels_height` image
/// pixels into a `max_cols` x `max_rows` terminal-cell box at its true aspect
/// ratio, returning the cell dimensions to render into.
///
/// The preview draws two image pixels per row inside a half-block cell, and a
/// terminal cell is roughly twice as tall as it is wide, so a uniformly scaled
/// raster satisfies `cols / (rows * 2) == pixels_width / pixels_height`. Limiting
/// the row count by the available width before the available height keeps that
/// ratio instead of stretching each style to fill the box on both axes, which
/// is what turned the wide `pill` and `long-pill` styles into tall blobs.
fn preview_cell_size(
    pixels_width: u32,
    pixels_height: u32,
    max_cols: u16,
    max_rows: u16,
) -> (u16, u16) {
    if pixels_width == 0 || pixels_height == 0 || max_cols == 0 || max_rows == 0 {
        return (0, 0);
    }
    let aspect = pixels_width as f64 / pixels_height as f64;
    let rows = ((max_cols as f64 / (2.0 * aspect)).floor() as u16).clamp(1, max_rows);
    let cols = (2.0 * aspect * rows as f64).round() as u16;
    (cols.clamp(1, max_cols), rows)
}

/// Rasterises an overlay style into terminal preview cells, fitted into a
/// `max_cols` x `max_rows` box. Shared with the setup wizard's Overlay steps so
/// both show the same picture. Rasterising per call is fine for the wizard's
/// event-driven redraw; the dashboard caches this in `OverlayPreview`.
pub(crate) fn overlay_preview_cells(
    style: &str,
    look: &str,
    shape: &str,
    max_cols: u16,
    max_rows: u16,
) -> Vec<Line<'static>> {
    let (width, height, pixels) =
        crate::overlay::render_frame_pixels(style, Some("recording"), 0.0, look, shape, &[]);
    let (cols, rows) = preview_cell_size(width, height, max_cols, max_rows);
    preview_lines_from_pixels(width, height, &pixels, cols, rows)
}

/// Places a `width` x `height` box inside `area` at the overlay position named
/// by `position` (one of `overlay::POSITION_VALUES`), so the preview shows the
/// overlay landing where the real one will. An unknown name centres the box.
pub(crate) fn placed_rect(area: Rect, width: u16, height: u16, position: &str) -> Rect {
    let width = width.min(area.width);
    let height = height.min(area.height);
    let left = area.x;
    let right = area.x + area.width - width;
    let center_x = area.x + (area.width - width) / 2;
    let top = area.y;
    let bottom = area.y + area.height - height;
    let center_y = area.y + (area.height - height) / 2;
    let (x, y) = match position {
        "top-left" => (left, top),
        "top" => (center_x, top),
        "top-right" => (right, top),
        "center" => (center_x, center_y),
        "bottom-left" => (left, bottom),
        "bottom" => (center_x, bottom),
        "bottom-right" => (right, bottom),
        _ => (center_x, center_y),
    };
    Rect {
        x,
        y,
        width,
        height,
    }
}

/// Downsamples a rasterised overlay frame (from the overlay's own offline
/// rasteriser, so the preview matches what the compositor shows at the style's
/// recording state) into `rows` x `cols` terminal half-block cells for the
/// Overlay screen. Each cell carries two vertical image pixels (`▀` with the
/// upper pixel as the foreground and the lower as the background), which any
/// truecolour terminal renders without a graphics protocol.
fn preview_lines_from_pixels(
    pixels_width: u32,
    pixels_height: u32,
    pixels: &[u8],
    cols: u16,
    rows: u16,
) -> Vec<Line<'static>> {
    if pixels_width == 0 || pixels_height == 0 || cols == 0 || rows == 0 {
        return Vec::new();
    }
    let sample_rows = rows as usize * 2;
    let mut lines = Vec::with_capacity(rows as usize);
    for row in 0..rows as usize {
        let mut spans = Vec::with_capacity(cols as usize);
        for col in 0..cols as usize {
            let top = preview_sample(
                pixels,
                pixels_width,
                pixels_height,
                col,
                cols as usize,
                row * 2,
                sample_rows,
            );
            let bottom = preview_sample(
                pixels,
                pixels_width,
                pixels_height,
                col,
                cols as usize,
                row * 2 + 1,
                sample_rows,
            );
            spans.push(preview_span(top, bottom));
        }
        lines.push(Line::from(spans));
    }
    lines
}

/// Samples one image pixel for the preview cell at (`col`, `sample_row`) of a
/// `cols` x `sample_rows` grid, returning straight RGB and whether the pixel is
/// opaque enough to draw. The rasteriser's buffer is premultiplied BGRA over a
/// transparent surface, so the premultiplied channels are already the colour to
/// show against a dark terminal.
fn preview_sample(
    pixels: &[u8],
    width: u32,
    height: u32,
    col: usize,
    cols: usize,
    sample_row: usize,
    sample_rows: usize,
) -> (u8, u8, u8, bool) {
    let x = (col as u64 * width as u64 / cols as u64).min(width as u64 - 1) as u32;
    let y = (sample_row as u64 * height as u64 / sample_rows as u64).min(height as u64 - 1) as u32;
    let index = ((y * width + x) * 4) as usize;
    let pixel = &pixels[index..index + 4];
    (pixel[2], pixel[1], pixel[0], pixel[3] > 40)
}

fn preview_span(top: (u8, u8, u8, bool), bottom: (u8, u8, u8, bool)) -> Span<'static> {
    let fg = |pixel: (u8, u8, u8, bool)| Style::default().fg(Color::Rgb(pixel.0, pixel.1, pixel.2));
    match (top.3, bottom.3) {
        (false, false) => Span::raw(" "),
        (true, false) => Span::styled("▀", fg(top)),
        (false, true) => Span::styled("▄", fg(bottom)),
        (true, true) => Span::styled("▀", fg(top).bg(Color::Rgb(bottom.0, bottom.1, bottom.2))),
    }
}

async fn ipc_task(request: ipc::Request) -> Vec<OutputLine> {
    match commands::send_ipc(request).await {
        Ok(response) => commands::format_response_lines(&response),
        Err(error) => vec![OutputLine {
            text: error.to_string(),
            is_error: true,
        }],
    }
}

async fn doctor_task(config: Config) -> Vec<OutputLine> {
    match doctor::run_doctor(&config).await {
        Ok(report) => commands::format_doctor_lines(&report),
        Err(error) => vec![OutputLine {
            text: error.to_string(),
            is_error: true,
        }],
    }
}

async fn shortcut_test_task() -> Vec<OutputLine> {
    match activation::test_shortcut_binding().await {
        Ok(activation::ShortcutTestOutcome::Pressed) => vec![OutputLine {
            text: "shortcut press detected - the binding works".to_string(),
            is_error: false,
        }],
        Ok(activation::ShortcutTestOutcome::TimedOut) => vec![OutputLine {
            text: "no shortcut press detected within 15s".to_string(),
            is_error: true,
        }],
        Err(error) => vec![OutputLine {
            text: format!("shortcut binding test failed: {error}"),
            is_error: true,
        }],
    }
}

/// Launches the daemon (if not already running) and waits up to 30s for its
/// control socket to answer - bounded so a slow first-run model download
/// doesn't block the dashboard forever; a timeout is reported as "still
/// starting", not a failure, since the launch itself succeeded.
async fn daemon_task() -> Vec<OutputLine> {
    if commands::daemon_socket_exists() {
        if let Ok(response) = commands::send_ipc(ipc::Request::Status).await {
            let mut lines = vec![OutputLine {
                text: "daemon already running".to_string(),
                is_error: false,
            }];
            lines.extend(commands::format_response_lines(&response));
            return lines;
        }
    }
    if let Err(error) = commands::spawn_daemon() {
        return vec![OutputLine {
            text: format!("failed to launch daemon: {error}"),
            is_error: true,
        }];
    }
    wait_for_daemon_ready("started").await
}

/// Gracefully stops the running daemon, mirroring `tonguetyped daemon stop`.
async fn daemon_stop_task() -> Vec<OutputLine> {
    match commands::stop_daemon().await {
        Ok(()) => vec![OutputLine {
            text: "daemon stopped".to_string(),
            is_error: false,
        }],
        Err(error) => vec![OutputLine {
            text: error.to_string(),
            is_error: true,
        }],
    }
}

/// Stops the running daemon (if any) and launches a fresh one, mirroring
/// `tonguetyped daemon restart`. `commands::restart_daemon` already waits for
/// the old instance lock to be released before spawning the replacement, so
/// this only needs to wait for the new instance to come up.
async fn daemon_restart_task() -> Vec<OutputLine> {
    if let Err(error) = commands::restart_daemon().await {
        return vec![OutputLine {
            text: format!("failed to restart daemon: {error}"),
            is_error: true,
        }];
    }
    wait_for_daemon_ready("restarted").await
}

/// Polls for the daemon's control socket to come up and answer a status
/// request, bounded to 30s - shared by the start and restart actions, which
/// both launch a fresh daemon process and need to wait for it the same way.
async fn wait_for_daemon_ready(verb: &str) -> Vec<OutputLine> {
    for _ in 0..100 {
        tokio::time::sleep(Duration::from_millis(300)).await;
        if commands::daemon_socket_exists() {
            if let Ok(response) = commands::send_ipc(ipc::Request::Status).await {
                let mut lines = vec![OutputLine {
                    text: format!("daemon {verb}"),
                    is_error: false,
                }];
                lines.extend(commands::format_response_lines(&response));
                return lines;
            }
        }
    }
    vec![OutputLine {
        text: "daemon launch requested; it may still be starting (e.g. downloading its \
               model) - check status again shortly"
            .to_string(),
        is_error: false,
    }]
}

async fn model_activation_task(id: String, progress: Arc<Mutex<(u64, u64)>>) -> Vec<OutputLine> {
    let callback: crate::model::ProgressCallback = Arc::new(move |downloaded, total| {
        if let Ok(mut guard) = progress.lock() {
            *guard = (downloaded, total);
        }
    });
    match commands::activate_model(&id, Some(callback)).await {
        Ok(activation) => {
            let mut lines = vec![OutputLine {
                text: format!("download: {}", setup::outcome_label(activation.download)),
                is_error: false,
            }];
            match &activation.daemon_reload {
                None => lines.push(OutputLine {
                    text: "daemon: not running (config saved; applies on next start)".to_string(),
                    is_error: false,
                }),
                Some(Ok(())) => lines.push(OutputLine {
                    text: "daemon: reloaded".to_string(),
                    is_error: false,
                }),
                Some(Err(error)) => lines.push(OutputLine {
                    text: format!("daemon reload failed: {error}"),
                    is_error: true,
                }),
            }
            let succeeded = activation.succeeded();
            lines.extend(commands::format_doctor_lines(&activation.confirmation));
            lines.insert(
                0,
                if succeeded {
                    OutputLine {
                        text: format!("model '{id}' activated"),
                        is_error: false,
                    }
                } else {
                    OutputLine {
                        text: format!("model '{id}' NOT fully activated - see details below"),
                        is_error: true,
                    }
                },
            );
            lines
        }
        Err(error) => vec![OutputLine {
            text: error.to_string(),
            is_error: true,
        }],
    }
}

async fn backend_activation_task(name: &'static str) -> Vec<OutputLine> {
    match commands::activate_backend(name).await {
        Ok(activation) => {
            let succeeded = activation.succeeded();
            let mut lines = vec![if succeeded {
                OutputLine {
                    text: format!("inference backend '{name}' active"),
                    is_error: false,
                }
            } else {
                OutputLine {
                    text: format!(
                        "inference backend '{name}' saved but NOT confirmed - see details below"
                    ),
                    is_error: true,
                }
            }];
            lines.push(match &activation.daemon_reload {
                None => OutputLine {
                    text: "daemon: not running (config saved; applies on next start)".to_string(),
                    is_error: false,
                },
                Some(Ok(())) => OutputLine {
                    text: "daemon: reloaded".to_string(),
                    is_error: false,
                },
                Some(Err(error)) => OutputLine {
                    text: format!("daemon reload failed: {error}"),
                    is_error: true,
                },
            });
            lines.extend(commands::format_doctor_lines(&activation.confirmation));
            lines
        }
        Err(error) => vec![OutputLine {
            text: format!("{error:#}"),
            is_error: true,
        }],
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn deck_renders_at_80x24_and_100x38() {
        use ratatui::backend::TestBackend;
        for (width, height) in [(80, 24), (120, 40), (100, 38), (40, 24)] {
            let backend = TestBackend::new(width, height);
            let mut terminal = Terminal::new(backend).unwrap();
            let mut app = App::new(Config::default());
            app.splash_until.set(None);
            for selected in 0..5 {
                app.deck_selected = selected;
                terminal.draw(|frame| app.render(frame)).unwrap();
            }
        }
    }

    #[test]
    fn overlay_preview_draws_a_grid_of_half_block_cells() {
        // Every style must render a full grid; the badge preview centres an
        // opaque disc, so at least one cell carries a foreground colour while
        // the transparency around it stays blank.
        for style in crate::overlay::STYLE_VALUES {
            let (width, height, pixels) = crate::overlay::render_frame_pixels(
                style,
                Some("recording"),
                0.0,
                "plain",
                "bars",
                &[],
            );
            let (cols, rows) = preview_cell_size(width, height, 24, 8);
            let lines = preview_lines_from_pixels(width, height, &pixels, cols, rows);
            assert_eq!(
                lines.len(),
                rows as usize,
                "{style}: one line per preview row"
            );
            for line in &lines {
                assert_eq!(
                    line.spans.len(),
                    cols as usize,
                    "{style}: one cell per column"
                );
            }
            let painted = lines
                .iter()
                .flat_map(|line| &line.spans)
                .filter(|span| span.style.fg.is_some())
                .count();
            assert!(painted > 0, "{style}: the preview should paint something");
        }
    }

    #[test]
    fn overlay_preview_keeps_each_styles_aspect_ratio() {
        // The preview band is wide and short, so a uniformly fitted raster must
        // still let a wide style render wider than it is tall. The `pill` raster
        // is ~2:1; the old side panel stretched it into a tall blob instead.
        let (width, height, _) = crate::overlay::render_frame_pixels(
            "pill",
            Some("recording"),
            0.0,
            "plain",
            "bars",
            &[],
        );
        let (cols, rows) = preview_cell_size(width, height, 40, 6);
        assert!(
            cols > rows,
            "the pill preview should be wider than tall, got {cols} cols x {rows} rows"
        );

        // A square raster (`badge`) maps to a physically square region: its cell
        // box is twice as wide as it is tall because a half-block cell is 2:1.
        let (width, height, _) = crate::overlay::render_frame_pixels(
            "badge",
            Some("recording"),
            0.0,
            "plain",
            "bars",
            &[],
        );
        let (cols, rows) = preview_cell_size(width, height, 40, 6);
        assert_eq!(cols, 2 * rows, "a square style must render 2:1 in cells");

        // A very wide raster (`long-pill`, 8:1) must still render much wider
        // than it is tall rather than filling the band vertically.
        let (width, height, _) = crate::overlay::render_frame_pixels(
            "long-pill",
            Some("recording"),
            0.0,
            "plain",
            "bars",
            &[],
        );
        let (cols, rows) = preview_cell_size(width, height, 40, 6);
        assert!(
            cols > 2 * rows,
            "long-pill should stay wide, got {cols} x {rows}"
        );
    }

    #[test]
    fn home_items_cover_every_real_cli_command_in_declared_order_excluding_help_start_stop_and_settings_areas(
    ) {
        let names: Vec<String> = home_items().into_iter().map(|item| item.name).collect();
        assert_eq!(
            names,
            vec![
                "setup",
                "daemon",
                "toggle",
                "cancel",
                "status",
                "reload",
                "last-result",
                "doctor",
                "shortcut-test",
            ]
        );
    }

    #[test]
    fn settings_rows_are_the_captains_areas_in_order() {
        let labels: Vec<&str> = SettingId::ALL
            .iter()
            .map(|setting| setting.label())
            .collect();
        assert_eq!(
            labels,
            vec![
                "Model",
                "Inference backend",
                "Translation",
                "Decoding",
                "Text processing",
                "Microphone",
                "Activation",
                "Shortcut",
                "Transcript output",
                "Typing backend",
                "Transcript folder",
                "History retention",
                "Startup",
                "Overlay",
                "Media pause",
            ]
        );
    }

    #[test]
    fn every_home_item_has_a_non_empty_description_matching_help_text() {
        for item in home_items() {
            assert!(
                !item.about.is_empty(),
                "command {} has no --help description",
                item.name
            );
        }
    }

    #[test]
    fn shortcut_dialog_result_stores_the_desktop_reported_binding() {
        // `save()` validates before it writes, so an out-of-range recording
        // limit makes it fail before it can touch the real config file; the
        // assertion below is about the in-memory config only.
        let mut config = Config::default();
        config.transcription.max_recording_seconds = 0;
        let mut app = App::new(config);

        app.open_setting(SettingId::Shortcut);
        app.shortcut_dialog = Some(setup::ReconfigureHandle::for_test(Arc::new(Mutex::new(
            Some(Ok(activation::ReconfigureOutcome::Changed {
                trigger_description: "Meta+O".to_string(),
            })),
        ))));
        app.poll_shortcut_handles();

        assert!(
            app.shortcut_dialog.is_none(),
            "a delivered dialog result must clear the handle"
        );
        assert_eq!(
            app.config.activation.keybind, "Super+O",
            "the dialog's report is stored in the app's own wording"
        );
    }

    #[test]
    fn keeping_the_current_shortcut_settles_without_waiting_out_the_dialog() {
        let mut config = Config::default();
        config.activation.keybind = "Super+O".to_string();
        let mut app = App::new(config);

        app.open_setting(SettingId::Shortcut);
        if let Screen::Shortcut(screen) = &mut app.screen {
            screen.status = screens::ShortcutStatus::DialogOpen;
        }
        app.shortcut_dialog = Some(setup::ReconfigureHandle::for_test(Arc::new(Mutex::new(
            None,
        ))));

        app.handle_shortcut_key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE));

        assert!(
            app.shortcut_dialog.is_none(),
            "keeping the current binding must stop the wait"
        );
        assert_eq!(
            app.config.activation.keybind, "Super+O",
            "a no-change settle must not touch the binding"
        );
        let Screen::Shortcut(screen) = &app.screen else {
            panic!("keeping the current binding stays on the Shortcut screen");
        };
        match &screen.status {
            screens::ShortcutStatus::Message { text, is_error } => {
                assert!(!*is_error, "no change is not a failure");
                assert!(text.contains("No change was made"), "{text}");
            }
            _ => panic!("expected a settled no-change message"),
        }
    }

    #[test]
    fn backspace_is_back_from_the_shortcut_in_flight_states() {
        for dialog_open in [true, false] {
            let mut app = App::new(Config::default());
            app.open_setting(SettingId::Shortcut);
            if let Screen::Shortcut(screen) = &mut app.screen {
                screen.status = if dialog_open {
                    screens::ShortcutStatus::DialogOpen
                } else {
                    screens::ShortcutStatus::Testing
                };
            }
            if dialog_open {
                app.shortcut_dialog = Some(setup::ReconfigureHandle::for_test(Arc::new(
                    Mutex::new(None),
                )));
            } else {
                app.shortcut_test = Some(setup::ShortcutTestHandle {
                    result: Arc::new(Mutex::new(None)),
                });
            }

            app.handle_shortcut_key(KeyEvent::new(KeyCode::Backspace, KeyModifiers::NONE));

            assert!(
                matches!(app.screen, Screen::Home),
                "Backspace must leave the Shortcut screen while its dialog or test is in flight"
            );
            assert!(
                app.shortcut_dialog.is_none() && app.shortcut_test.is_none(),
                "leaving must stop any in-flight shortcut wait"
            );
        }
    }

    #[test]
    fn shortcut_testing_footer_names_only_the_back_key_it_handles() {
        let mut app = App::new(Config::default());
        app.open_setting(SettingId::Shortcut);
        if let Screen::Shortcut(screen) = &mut app.screen {
            screen.status = screens::ShortcutStatus::Testing;
        }

        let Screen::Shortcut(screen) = &app.screen else {
            panic!("the Shortcut screen must stay open");
        };
        let footer = screen.footer();
        assert!(
            footer.contains("Esc") && footer.contains('\u{232b}'),
            "the testing legend must still name back: {footer}"
        );
        for ignored in ["Enter", "Ctrl+R", "\u{2191}/\u{2193}"] {
            assert!(
                !footer.contains(ignored),
                "the testing legend must not advertise {ignored} while it is ignored: {footer}"
            );
        }
    }

    #[test]
    fn a_passed_test_never_fabricates_a_binding() {
        use crate::activation::ShortcutTestOutcome;

        let mut app = App::new(Config::default());
        app.open_setting(SettingId::Shortcut);
        app.shortcut_test = Some(setup::ShortcutTestHandle {
            result: Arc::new(Mutex::new(Some(Ok(ShortcutTestOutcome::Pressed)))),
        });
        app.poll_shortcut_handles();

        assert!(
            app.config.activation.keybind.is_empty(),
            "a detected press confirms the existing binding but never invents one"
        );
        assert!(app.shortcut_test.is_none());
    }

    #[test]
    fn a_settings_save_after_model_activation_does_not_revert_the_active_model() {
        let _guard = crate::config::XDG_CONFIG_HOME_LOCK
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let root = std::env::temp_dir().join(format!(
            "tonguetyped-tui-model-reload-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir_all(root.join("tonguetyped")).unwrap();

        let previous = std::env::var_os("XDG_CONFIG_HOME");
        std::env::set_var("XDG_CONFIG_HOME", &root);

        let initial = Config::default();
        initial.save().unwrap();
        let mut app = App::new(initial);

        // `commands::activate_model` saves a different model to config.toml
        // behind the dashboard's back (via `commands::select_model`) before
        // the model-activation background action reports completion.
        let activated = "whisper-tiny-q5_k_m";
        let mut behind_the_back = Config::default();
        behind_the_back.model.active_model = activated.to_string();
        behind_the_back.save().unwrap();

        app.apply_pending_result(Ok(Vec::new()));
        assert_eq!(
            app.config.model.active_model, activated,
            "the dashboard must refresh its in-memory config after a background write"
        );

        // Any later settings save must persist the activated model rather than
        // reverting it to the pre-activation value.
        app.save_settings_config();
        assert_eq!(
            Config::load().unwrap().model.active_model,
            activated,
            "a settings save must not clobber the model activated behind the dashboard's back"
        );

        match previous {
            Some(value) => std::env::set_var("XDG_CONFIG_HOME", value),
            None => std::env::remove_var("XDG_CONFIG_HOME"),
        }
        std::fs::remove_dir_all(root).unwrap();
    }
}
