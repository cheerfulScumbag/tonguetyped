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
//! The home screen is two stacked panels with one selection cursor: a
//! fixed Settings panel listing the eight configuration areas (Model,
//! Microphone, Activation, Shortcut, Transcript output, Typing backend,
//! Startup, Overlay) with their current values, and a Commands panel derived
//! from `crate::cli::Cli`'s clap metadata (`Cli::command().get_subcommands()`)
//! with the directional recording commands intentionally omitted in favor of
//! Toggle. `model` and `autostart` are CLI commands but live in the Settings
//! panel rather than the Commands panel, so each configuration area has
//! exactly one home-screen entry point.

mod logo;
mod screens;

use screens::selection_style;

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
use ratatui::layout::{Constraint, Direction, Layout, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Gauge, Paragraph};
use ratatui::{Frame, Terminal};
use std::cell::Cell;
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

/// The eight configuration areas the home screen's Settings panel lists, in
/// display order.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum SettingId {
    Model,
    Microphone,
    Activation,
    Shortcut,
    TranscriptOutput,
    TypingBackend,
    Startup,
    Overlay,
}

impl SettingId {
    const ALL: [SettingId; 8] = [
        SettingId::Model,
        SettingId::Microphone,
        SettingId::Activation,
        SettingId::Shortcut,
        SettingId::TranscriptOutput,
        SettingId::TypingBackend,
        SettingId::Startup,
        SettingId::Overlay,
    ];

    fn label(self) -> &'static str {
        match self {
            SettingId::Model => "Model",
            SettingId::Microphone => "Microphone",
            SettingId::Activation => "Activation",
            SettingId::Shortcut => "Shortcut",
            SettingId::TranscriptOutput => "Transcript output",
            SettingId::TypingBackend => "Typing backend",
            SettingId::Startup => "Startup",
            SettingId::Overlay => "Overlay",
        }
    }
}

/// Eight settings rows plus the panel's two border rows.
const SETTINGS_PANEL_HEIGHT: u16 = 10;

/// The dashboard's Commands panel, derived from the exact same clap metadata
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

enum Screen {
    Home,
    Info {
        title: String,
        lines: Vec<OutputLine>,
    },
    Model(screens::ModelScreen),
    Autostart(screens::AutostartScreen),
    Daemon(screens::DaemonScreen),
    Microphone(screens::MicrophoneScreen),
    Activation(screens::ActivationScreen),
    Shortcut(screens::ShortcutScreen),
    TranscriptOutput(screens::OutputScreen),
    TypingBackend(screens::TypingBackendScreen),
    Overlay(screens::OverlayScreen),
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
    command_items: Vec<HomeItem>,
    home_selected: usize,
    pending: Option<PendingAction>,
    shortcut_test: Option<(String, setup::ShortcutTestHandle)>,
    shortcut_dialog: Option<(String, setup::ReconfigureHandle)>,
    should_quit: bool,
    daemon_status_cache: Cell<Option<(bool, Instant)>>,
}

/// How often the home screen's status strip re-probes daemon liveness.
/// `status_strip_line` renders on every tick of `run_loop`'s 66ms ticker, but
/// the probe it displays (`commands::daemon_socket_exists`) opens a real
/// `UnixStream` connection against the daemon's control socket - re-running
/// that on every redraw would make the daemon's accept loop spawn a task per
/// frame purely to paint a status string, so the cached result is reused
/// until it goes stale.
const DAEMON_STATUS_REFRESH: Duration = Duration::from_secs(1);

impl App {
    fn new(config: Config) -> Self {
        Self {
            config,
            capabilities: Capabilities::discover_or_default(),
            screen: Screen::Home,
            command_items: home_items(),
            home_selected: 0,
            pending: None,
            shortcut_test: None,
            shortcut_dialog: None,
            should_quit: false,
            daemon_status_cache: Cell::new(None),
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
        self.poll_shortcut_handles();
    }

    fn poll_shortcut_handles(&mut self) {
        let test_outcome = self.shortcut_test.as_ref().and_then(|(keybind, handle)| {
            handle
                .result
                .lock()
                .ok()
                .and_then(|mut guard| guard.take())
                .map(|outcome| (keybind.clone(), outcome))
        });
        if let Some((keybind, outcome)) = test_outcome {
            self.shortcut_test = None;
            self.apply_shortcut_test_outcome(&keybind, outcome);
        }

        let dialog_outcome = self.shortcut_dialog.as_ref().and_then(|(keybind, handle)| {
            handle
                .result
                .lock()
                .ok()
                .and_then(|mut guard| guard.take())
                .map(|outcome| (keybind.clone(), outcome))
        });
        if let Some((keybind, outcome)) = dialog_outcome {
            self.shortcut_dialog = None;
            self.apply_shortcut_dialog_outcome(&keybind, outcome);
        }
    }

    fn apply_shortcut_test_outcome(
        &mut self,
        submitted_keybind: &str,
        outcome: Result<activation::ShortcutTestOutcome, String>,
    ) {
        let Screen::Shortcut(screen) = &mut self.screen else {
            return;
        };
        if screen.input.trim() != submitted_keybind {
            return;
        }
        match outcome {
            Ok(activation::ShortcutTestOutcome::Pressed) => {
                // The desktop only listened for presses after actually
                // binding the shortcut, so a detected press is strong enough
                // to persist the typed value, same as the console's
                // shortcut step.
                let keybind = screen.input.trim().to_string();
                if keybind != self.config.activation.keybind {
                    self.config.activation.keybind_status = "untested".to_string();
                }
                self.config.activation.keybind = keybind;
                screen.status = match self.config.save() {
                    Ok(()) => screens::ShortcutStatus::Message {
                        text: "shortcut press detected - the binding works".to_string(),
                        is_error: false,
                    },
                    Err(error) => screens::ShortcutStatus::Message {
                        text: format!(
                            "shortcut press detected, but saving the configuration failed: {error}"
                        ),
                        is_error: true,
                    },
                };
            }
            Ok(activation::ShortcutTestOutcome::TimedOut) => {
                screen.status = screens::ShortcutStatus::Message {
                    text: "no shortcut press detected within 15s".to_string(),
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
        submitted_keybind: &str,
        outcome: Result<String, String>,
    ) {
        let Screen::Shortcut(screen) = &mut self.screen else {
            return;
        };
        if screen.input.trim() != submitted_keybind {
            return;
        }
        match outcome {
            Ok(trigger_description) => {
                self.config.activation.keybind = screen.input.trim().to_string();
                self.config.activation.keybind_status = trigger_description.clone();
                screen.status = match self.config.save() {
                    Ok(()) => screens::ShortcutStatus::Message {
                        text: format!("Shortcut bound: {trigger_description}"),
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
        match &self.screen {
            Screen::Home => self.handle_home_key(key, terminal)?,
            Screen::Info { .. } => self.handle_info_key(key),
            Screen::Model(_) => self.handle_model_key(key),
            Screen::Autostart(_) => self.handle_autostart_key(key),
            Screen::Daemon(_) => self.handle_daemon_key(key),
            Screen::Microphone(_) => self.handle_microphone_key(key),
            Screen::Activation(_) => self.handle_activation_key(key),
            Screen::Shortcut(_) => self.handle_shortcut_key(key),
            Screen::TranscriptOutput(_) => self.handle_output_key(key),
            Screen::TypingBackend(_) => self.handle_typing_backend_key(key),
            Screen::Overlay(_) => self.handle_overlay_key(key),
        }
        Ok(())
    }

    fn move_home_selection(&mut self, delta: i32) {
        let len = (SettingId::ALL.len() + self.command_items.len()) as i32;
        if len == 0 {
            return;
        }
        let next = (self.home_selected as i32 + delta).clamp(0, len - 1) as usize;
        self.home_selected = next;
    }

    fn handle_home_key(
        &mut self,
        key: KeyEvent,
        terminal: &mut Terminal<Backend>,
    ) -> anyhow::Result<()> {
        match key.code {
            KeyCode::Char('q') => self.should_quit = true,
            KeyCode::Up | KeyCode::Char('k') => self.move_home_selection(-1),
            KeyCode::Down | KeyCode::Char('j') => self.move_home_selection(1),
            KeyCode::Enter if self.pending.is_none() => {
                let selected = self.home_selected;
                self.open_home_row(selected, terminal)?;
            }
            _ => {}
        }
        Ok(())
    }

    fn open_home_row(
        &mut self,
        index: usize,
        terminal: &mut Terminal<Backend>,
    ) -> anyhow::Result<()> {
        if index < SettingId::ALL.len() {
            self.open_setting(SettingId::ALL[index]);
            return Ok(());
        }
        let name = self.command_items[index - SettingId::ALL.len()]
            .name
            .clone();
        self.dispatch_command(&name, terminal)
    }

    fn open_setting(&mut self, setting: SettingId) {
        self.screen = match setting {
            SettingId::Model => Screen::Model(screens::ModelScreen::new(&self.config)),
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
            SettingId::Startup => Screen::Autostart(screens::AutostartScreen::new(&self.config)),
            SettingId::Overlay => Screen::Overlay(screens::OverlayScreen::new(&self.config)),
        };
    }

    /// Saves the current config after a settings screen mutated it in place,
    /// reporting the outcome in whichever screen's result line is current -
    /// so every settings screen gives the same success/error feedback.
    fn save_settings_config(&mut self) {
        let result = self.config.save().map_err(|error| error.to_string());
        match &mut self.screen {
            Screen::Microphone(screen) => screen.result = Some(result),
            Screen::Activation(screen) => screen.result = Some(result),
            Screen::TranscriptOutput(screen) => screen.result = Some(result),
            Screen::TypingBackend(screen) => screen.result = Some(result),
            Screen::Overlay(screen) => screen.result = Some(result),
            _ => {}
        }
    }

    fn handle_microphone_key(&mut self, key: KeyEvent) {
        match key.code {
            KeyCode::Char('q') => self.should_quit = true,
            KeyCode::Esc => self.screen = Screen::Home,
            KeyCode::Up | KeyCode::Char('k') => {
                if let Screen::Microphone(screen) = &mut self.screen {
                    screen.move_selection(-1);
                }
            }
            KeyCode::Down | KeyCode::Char('j') => {
                if let Screen::Microphone(screen) = &mut self.screen {
                    screen.move_selection(1);
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
        match key.code {
            KeyCode::Char('q') => self.should_quit = true,
            KeyCode::Esc => self.screen = Screen::Home,
            KeyCode::Up | KeyCode::Char('k') => {
                if let Screen::Activation(screen) = &mut self.screen {
                    screen.move_selection(-1);
                }
            }
            KeyCode::Down | KeyCode::Char('j') => {
                if let Screen::Activation(screen) = &mut self.screen {
                    screen.move_selection(1);
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

    fn handle_output_key(&mut self, key: KeyEvent) {
        match key.code {
            KeyCode::Char('q') => self.should_quit = true,
            KeyCode::Esc => self.screen = Screen::Home,
            KeyCode::Up | KeyCode::Char('k') => {
                if let Screen::TranscriptOutput(screen) = &mut self.screen {
                    screen.move_selection(-1);
                }
            }
            KeyCode::Down | KeyCode::Char('j') => {
                if let Screen::TranscriptOutput(screen) = &mut self.screen {
                    screen.move_selection(1);
                }
            }
            KeyCode::Enter => {
                if let Screen::TranscriptOutput(screen) = &mut self.screen {
                    screen.apply(&mut self.config);
                }
                self.save_settings_config();
            }
            _ => {}
        }
    }

    fn handle_typing_backend_key(&mut self, key: KeyEvent) {
        match key.code {
            KeyCode::Char('q') => self.should_quit = true,
            KeyCode::Esc => self.screen = Screen::Home,
            KeyCode::Up | KeyCode::Char('k') => {
                if let Screen::TypingBackend(screen) = &mut self.screen {
                    screen.move_selection(-1);
                }
            }
            KeyCode::Down | KeyCode::Char('j') => {
                if let Screen::TypingBackend(screen) = &mut self.screen {
                    screen.move_selection(1);
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
        match key.code {
            KeyCode::Char('q') => self.should_quit = true,
            KeyCode::Esc => self.screen = Screen::Home,
            KeyCode::Up | KeyCode::Char('k') => {
                if let Screen::Overlay(screen) = &mut self.screen {
                    screen.move_selection(-1, &self.config);
                }
            }
            KeyCode::Down | KeyCode::Char('j') => {
                if let Screen::Overlay(screen) = &mut self.screen {
                    screen.move_selection(1, &self.config);
                }
            }
            KeyCode::Enter => {
                if let Screen::Overlay(screen) = &mut self.screen {
                    screen.apply(&mut self.config);
                }
                self.save_settings_config();
            }
            _ => {}
        }
    }

    /// The Shortcut screen owns raw text input, so `q` and `j`/`k` are
    /// literal keybinding characters here (same as the setup console's
    /// shortcut step) - only the arrow keys, Escape, Enter, Ctrl+R and
    /// Ctrl+C are commands, and everything is inert while the portal test or
    /// native dialog is in flight except Escape.
    fn handle_shortcut_key(&mut self, key: KeyEvent) {
        let busy = matches!(&self.screen, Screen::Shortcut(screen) if screen.is_busy());
        if busy {
            if key.code == KeyCode::Esc {
                self.shortcut_test = None;
                self.shortcut_dialog = None;
                self.screen = Screen::Home;
            }
            return;
        }
        match key.code {
            KeyCode::Esc => self.screen = Screen::Home,
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
            KeyCode::Backspace => {
                if let Screen::Shortcut(screen) = &mut self.screen {
                    screen.input.pop();
                }
            }
            KeyCode::Char('r') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                self.start_shortcut_action(Some(1));
            }
            KeyCode::Enter => self.start_shortcut_action(None),
            KeyCode::Char(c) => {
                if let Screen::Shortcut(screen) = &mut self.screen {
                    screen.input.push(c);
                }
            }
            _ => {}
        }
    }

    fn start_shortcut_action(&mut self, action: Option<usize>) {
        let Screen::Shortcut(screen) = &mut self.screen else {
            return;
        };
        let shortcut = screen.input.trim().to_string();
        if let Err(error) = activation::portal_trigger(&shortcut) {
            screen.status = screens::ShortcutStatus::Message {
                text: format!("Invalid shortcut: {error}"),
                is_error: true,
            };
            return;
        }
        match action.unwrap_or(screen.selected_action) {
            0 => {
                screen.status = screens::ShortcutStatus::Testing;
                self.shortcut_test = Some((shortcut.clone(), setup::shortcut_test_async(shortcut)));
            }
            1 => {
                screen.status = screens::ShortcutStatus::DialogOpen;
                self.shortcut_dialog = Some((
                    shortcut.clone(),
                    setup::reconfigure_shortcut_async(shortcut),
                ));
            }
            _ => {}
        }
    }

    fn handle_info_key(&mut self, key: KeyEvent) {
        match key.code {
            KeyCode::Char('q') => self.should_quit = true,
            KeyCode::Esc | KeyCode::Enter => self.screen = Screen::Home,
            _ => {}
        }
    }

    fn handle_model_key(&mut self, key: KeyEvent) {
        match key.code {
            KeyCode::Char('q') => self.should_quit = true,
            KeyCode::Esc => self.screen = Screen::Home,
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
            KeyCode::Enter if self.pending.is_none() => {
                if let Screen::Model(screen) = &self.screen {
                    let id = screen.selected_id().to_string();
                    self.start_model_activation(id);
                }
            }
            _ => {}
        }
    }

    fn handle_autostart_key(&mut self, key: KeyEvent) {
        match key.code {
            KeyCode::Char('q') => self.should_quit = true,
            KeyCode::Esc => self.screen = Screen::Home,
            KeyCode::Up | KeyCode::Char('k') => {
                if let Screen::Autostart(screen) = &mut self.screen {
                    screen.move_selection(-1);
                }
            }
            KeyCode::Down | KeyCode::Char('j') => {
                if let Screen::Autostart(screen) = &mut self.screen {
                    screen.move_selection(1);
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
        match key.code {
            KeyCode::Char('q') => self.should_quit = true,
            KeyCode::Esc => self.screen = Screen::Home,
            KeyCode::Up | KeyCode::Char('k') => {
                if let Screen::Daemon(screen) = &mut self.screen {
                    screen.move_selection(-1);
                }
            }
            KeyCode::Down | KeyCode::Char('j') => {
                if let Screen::Daemon(screen) = &mut self.screen {
                    screen.move_selection(1);
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
                let keybind = self.config.activation.keybind.clone();
                self.spawn_pending(
                    "Shortcut test - press it now",
                    shortcut_test_task(keybind),
                    None,
                );
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

    fn render(&self, frame: &mut Frame) {
        let area = frame.area();
        match &self.screen {
            Screen::Home => self.render_home(frame, area),
            Screen::Info { title, lines } => render_info(frame, area, title, lines),
            Screen::Model(screen) => self.render_model(frame, area, screen),
            Screen::Autostart(screen) => self.render_autostart(frame, area, screen),
            Screen::Daemon(screen) => self.render_daemon(frame, area, screen),
            Screen::Microphone(screen) => self.render_microphone(frame, area, screen),
            Screen::Activation(screen) => self.render_activation(frame, area, screen),
            Screen::Shortcut(screen) => self.render_shortcut(frame, area, screen),
            Screen::TranscriptOutput(screen) => self.render_output(frame, area, screen),
            Screen::TypingBackend(screen) => self.render_typing_backend(frame, area, screen),
            Screen::Overlay(screen) => self.render_overlay(frame, area, screen),
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
                Constraint::Min(3),
                Constraint::Length(1),
            ])
            .split(area);

        frame.render_widget(Paragraph::new(logo_lines), chunks[0]);
        frame.render_widget(Paragraph::new(self.status_strip_line()), chunks[1]);
        frame.render_widget(self.settings_panel_widget(), chunks[2]);
        frame.render_widget(self.commands_panel_widget(chunks[3].height), chunks[3]);

        let footer = match &self.pending {
            Some(pending) => Line::from(Span::styled(
                format!("Working: {}...", pending.title),
                Style::default().fg(Color::Yellow),
            )),
            None => Line::from("↑/↓ navigate  Enter select  q quit"),
        };
        frame.render_widget(Paragraph::new(footer), chunks[4]);
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
            SettingId::Microphone => self
                .capabilities
                .microphone_label(&self.config.audio.microphone),
            SettingId::Activation => self.config.activation.mode.to_string(),
            SettingId::Shortcut => {
                if self.config.activation.keybind_status == "untested" {
                    self.config.activation.keybind.clone()
                } else {
                    self.config.activation.keybind_status.clone()
                }
            }
            SettingId::TranscriptOutput => match self.config.output.method {
                OutputMethod::None => "keep in TongueTyped".to_string(),
                OutputMethod::Type => "type into the focused application".to_string(),
            },
            SettingId::TypingBackend => self.config.output.typing_backend.clone(),
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
                        if self.config.overlay.streaming_indicator {
                            "streaming waveform"
                        } else {
                            "simple pulse"
                        }
                    )
                }
            }
        }
    }

    /// The Settings panel: every row is `"> "`/`"  "`-prefixed, the selected
    /// row cyan+bold, the value column aligned after an 18-wide name.
    fn settings_panel_widget(&self) -> Paragraph<'static> {
        let lines: Vec<Line> = SettingId::ALL
            .iter()
            .enumerate()
            .map(|(index, setting)| {
                let selected = index == self.home_selected;
                let marker = if selected { "> " } else { "  " };
                let text = format!(
                    "{marker}{:<18} {}",
                    setting.label(),
                    self.setting_value(*setting)
                );
                Line::from(Span::styled(text, selection_style(selected)))
            })
            .collect();
        Paragraph::new(lines).block(Block::default().borders(Borders::ALL).title("Settings"))
    }

    /// The Commands panel, sharing one selection cursor with the Settings
    /// panel above it: command row `i` is home-selected when
    /// `home_selected == SettingId::ALL.len() + i`.
    fn commands_panel_widget(&self, height: u16) -> Paragraph<'static> {
        let lines: Vec<Line> = self
            .command_items
            .iter()
            .enumerate()
            .map(|(index, item)| {
                let selected = self.home_selected == SettingId::ALL.len() + index;
                let marker = if selected { "> " } else { "  " };
                let text = format!("{marker}{:<14} {}", item.name, item.about);
                Line::from(Span::styled(text, selection_style(selected)))
            })
            .collect();
        let visible_rows = usize::from(height.saturating_sub(2)).max(1);
        let scroll = if self.home_selected >= SettingId::ALL.len() {
            (self.home_selected - SettingId::ALL.len())
                .saturating_sub(visible_rows.saturating_sub(1)) as u16
        } else {
            0
        };
        Paragraph::new(lines)
            .block(Block::default().borders(Borders::ALL).title("Commands"))
            .scroll((scroll, 0))
    }

    fn render_model(&self, frame: &mut Frame, area: Rect, screen: &screens::ModelScreen) {
        let chunks = Layout::default()
            .direction(Direction::Vertical)
            .constraints([
                Constraint::Min(5),
                Constraint::Length(3),
                Constraint::Length(1),
            ])
            .split(area);

        frame.render_widget(screen.list_widget(chunks[0].height), chunks[0]);

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
                frame.render_widget(Block::default().borders(Borders::ALL), chunks[1]);
            }
        }

        frame.render_widget(
            Paragraph::new("↑/↓ choose  Enter activate  Esc back  q quit"),
            chunks[2],
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
        frame.render_widget(screen.list_widget(), chunks[0]);
        let status = match &self.pending {
            Some(pending) => Line::from(Span::styled(
                format!("Working: {}...", pending.title),
                Style::default().fg(Color::Yellow),
            )),
            None => Line::from(""),
        };
        frame.render_widget(Paragraph::new(status), chunks[1]);
        frame.render_widget(
            Paragraph::new("↑/↓ choose  Enter run  Esc back  q quit"),
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
        frame.render_widget(screen.list_widget(), chunks[0]);
        frame.render_widget(Paragraph::new(screen.result_line()), chunks[1]);
        frame.render_widget(
            Paragraph::new("↑/↓ choose  Enter apply  Esc back  q quit"),
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
        frame.render_widget(screen.list_widget(), chunks[0]);
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
            Paragraph::new("↑/↓ choose  Enter apply  Esc back  q quit"),
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
        frame.render_widget(screen.list_widget(), chunks[0]);
        frame.render_widget(Paragraph::new(screen.result_line()), chunks[1]);
        frame.render_widget(
            Paragraph::new("↑/↓ choose  Enter apply  Esc back  q quit"),
            chunks[2],
        );
    }

    fn render_shortcut(&self, frame: &mut Frame, area: Rect, screen: &screens::ShortcutScreen) {
        let chunks = Layout::default()
            .direction(Direction::Vertical)
            .constraints([Constraint::Min(6), Constraint::Length(1)])
            .split(area);
        frame.render_widget(
            screen.body(&self.config.activation.keybind_status),
            chunks[0],
        );
        frame.render_widget(
            Paragraph::new(
                "Type to edit  ↑/↓ choose action  Enter run  Ctrl+R system dialog  Esc back",
            ),
            chunks[1],
        );
    }

    fn render_output(&self, frame: &mut Frame, area: Rect, screen: &screens::OutputScreen) {
        let chunks = Layout::default()
            .direction(Direction::Vertical)
            .constraints([
                Constraint::Min(3),
                Constraint::Length(1),
                Constraint::Length(1),
            ])
            .split(area);
        frame.render_widget(screen.list_widget(), chunks[0]);
        frame.render_widget(Paragraph::new(screen.result_line()), chunks[1]);
        frame.render_widget(
            Paragraph::new("↑/↓ choose  Enter apply  Esc back  q quit"),
            chunks[2],
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
            ])
            .split(area);
        frame.render_widget(screen.list_widget(), chunks[0]);
        frame.render_widget(
            Paragraph::new("Only used when transcripts are typed into the focused application."),
            chunks[1],
        );
        frame.render_widget(Paragraph::new(screen.result_line()), chunks[2]);
        frame.render_widget(
            Paragraph::new("↑/↓ choose  Enter apply  Esc back  q quit"),
            chunks[3],
        );
    }

    fn render_overlay(&self, frame: &mut Frame, area: Rect, screen: &screens::OverlayScreen) {
        let chunks = Layout::default()
            .direction(Direction::Vertical)
            .constraints([
                Constraint::Min(3),
                Constraint::Length(1),
                Constraint::Length(1),
            ])
            .split(area);
        frame.render_widget(screen.list_widget(&self.config), chunks[0]);
        frame.render_widget(Paragraph::new(screen.result_line()), chunks[1]);
        frame.render_widget(
            Paragraph::new("↑/↓ choose  Enter change  Esc back  q quit"),
            chunks[2],
        );
    }
}

fn render_info(frame: &mut Frame, area: Rect, title: &str, lines: &[OutputLine]) {
    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([Constraint::Min(3), Constraint::Length(1)])
        .split(area);

    let rendered: Vec<Line> = lines
        .iter()
        .map(|output| {
            let style = if output.is_error {
                Style::default().fg(Color::Red)
            } else {
                Style::default()
            };
            Line::from(Span::styled(output.text.clone(), style))
        })
        .collect();
    frame.render_widget(
        Paragraph::new(rendered).block(
            Block::default()
                .borders(Borders::ALL)
                .title(title.to_string()),
        ),
        chunks[0],
    );
    frame.render_widget(Paragraph::new("Esc/Enter back  q quit"), chunks[1]);
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

async fn shortcut_test_task(keybind: String) -> Vec<OutputLine> {
    match activation::test_shortcut_binding(&keybind).await {
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

#[cfg(test)]
mod tests {
    use super::*;

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
    fn settings_rows_are_the_captains_eight_areas_in_order() {
        let labels: Vec<&str> = SettingId::ALL
            .iter()
            .map(|setting| setting.label())
            .collect();
        assert_eq!(
            labels,
            vec![
                "Model",
                "Microphone",
                "Activation",
                "Shortcut",
                "Transcript output",
                "Typing backend",
                "Startup",
                "Overlay",
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
    fn a_stale_shortcut_action_result_is_never_applied_to_a_different_binding() {
        use crate::activation::ShortcutTestOutcome;

        // `save()` validates before it writes, so an out-of-range recording
        // limit makes it fail before it can touch the real config file; the
        // assertions below are about the in-memory config only.
        let mut config = Config::default();
        config.transcription.max_recording_seconds = 0;
        let mut app = App::new(config);

        // On the Shortcut screen, a test for Super+X is in flight; the user
        // then leaves with Esc (allowed - the background action just finishes
        // unobserved).
        app.open_setting(SettingId::Shortcut);
        if let Screen::Shortcut(screen) = &mut app.screen {
            screen.input = "Super+X".to_string();
            screen.status = screens::ShortcutStatus::Testing;
        }
        app.shortcut_test = Some((
            "Super+X".to_string(),
            setup::ShortcutTestHandle {
                result: Arc::new(Mutex::new(None)),
            },
        ));
        app.handle_shortcut_key(KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE));

        assert!(
            app.shortcut_test.is_none(),
            "leaving the Shortcut screen with an action in flight must drop its handle"
        );

        // Even if a result for Super+X were delivered after re-entering and
        // editing to Super+Z, it must be discarded rather than binding a
        // keybind that was never tested.
        app.open_setting(SettingId::Shortcut);
        if let Screen::Shortcut(screen) = &mut app.screen {
            screen.input = "Super+Z".to_string();
        }
        app.shortcut_test = Some((
            "Super+X".to_string(),
            setup::ShortcutTestHandle {
                result: Arc::new(Mutex::new(Some(Ok(ShortcutTestOutcome::Pressed)))),
            },
        ));
        app.poll_shortcut_handles();

        assert_eq!(
            app.config.activation.keybind, "Super+O",
            "a stale test result must never bind a keybind that was never tested"
        );
        assert!(
            app.shortcut_test.is_none(),
            "a delivered result must clear the handle"
        );
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
