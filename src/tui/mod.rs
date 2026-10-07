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
//! The command list on the first page is derived from `crate::cli::Cli`'s
//! clap metadata (`Cli::command().get_subcommands()`), with the directional
//! recording commands intentionally omitted in favor of Toggle.

mod logo;
mod screens;

use crate::cli::Cli;
use crate::commands::{self, OutputLine};
use crate::config::Config;
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

/// The dashboard's first-page command list, derived from the exact same
/// clap metadata `--help` renders - see this module's doc comment. The
/// implicit `help` meta-subcommand clap adds is excluded; it has no useful
/// standalone dashboard action. `start`/`stop` (single-shot recording
/// start/stop) are also excluded: the dashboard offers `toggle` and
/// `cancel` for recording control, and `start`/`stop` remain reachable only
/// as the public CLI commands documented in `--help`.
fn home_items() -> Vec<HomeItem> {
    Cli::command()
        .get_subcommands()
        .filter(|command| !matches!(command.get_name(), "help" | "start" | "stop"))
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
}

struct PendingAction {
    title: &'static str,
    handle: JoinHandle<Vec<OutputLine>>,
    progress: Option<Arc<Mutex<(u64, u64)>>>,
}

struct App {
    config: Config,
    screen: Screen,
    home_items: Vec<HomeItem>,
    home_selected: usize,
    pending: Option<PendingAction>,
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
            screen: Screen::Home,
            home_items: home_items(),
            home_selected: 0,
            pending: None,
            should_quit: false,
            daemon_status_cache: Cell::new(None),
        }
    }

    async fn run_loop(&mut self, terminal: &mut Terminal<Backend>) -> anyhow::Result<()> {
        let mut events = EventStream::new();
        let mut ticker = tokio::time::interval(Duration::from_millis(66));

        loop {
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
        }
        Ok(())
    }

    fn move_home_selection(&mut self, delta: i32) {
        let len = self.home_items.len() as i32;
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
                let name = self.home_items[self.home_selected].name.clone();
                self.dispatch_home_action(&name, terminal)?;
            }
            _ => {}
        }
        Ok(())
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

    fn dispatch_home_action(
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
                self.spawn_pending("Shortcut test", shortcut_test_task(keybind), None);
            }
            "autostart" => {
                self.screen = Screen::Autostart(screens::AutostartScreen::new(&self.config));
            }
            "model" => {
                self.screen = Screen::Model(screens::ModelScreen::new(&self.config));
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
        }
    }

    fn render_home(&self, frame: &mut Frame, area: Rect) {
        let (logo_lines, logo_height) = logo::centered_logo(area.width);
        let chunks = Layout::default()
            .direction(Direction::Vertical)
            .constraints([
                Constraint::Length(logo_height),
                Constraint::Length(1),
                Constraint::Min(3),
                Constraint::Length(1),
            ])
            .split(area);

        frame.render_widget(Paragraph::new(logo_lines), chunks[0]);
        frame.render_widget(Paragraph::new(self.status_strip_line()), chunks[1]);
        frame.render_widget(self.home_list_widget(chunks[2].height), chunks[2]);

        let footer = match &self.pending {
            Some(pending) => Line::from(Span::styled(
                format!("Working: {}...", pending.title),
                Style::default().fg(Color::Yellow),
            )),
            None => Line::from("↑/↓ navigate  Enter select  q quit"),
        };
        frame.render_widget(Paragraph::new(footer), chunks[3]);
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

    fn home_list_widget(&self, height: u16) -> Paragraph<'static> {
        let lines: Vec<Line> = self
            .home_items
            .iter()
            .enumerate()
            .map(|(index, item)| {
                let marker = if index == self.home_selected {
                    "> "
                } else {
                    "  "
                };
                let text = format!("{marker}{:<14} {}", item.name, item.about);
                let style = if index == self.home_selected {
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
        let scroll = self
            .home_selected
            .saturating_sub(visible_rows.saturating_sub(1)) as u16;
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
        None => vec![OutputLine {
            text: "shortcut binding available".to_string(),
            is_error: false,
        }],
        Some(error) => vec![OutputLine {
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
    fn home_items_cover_every_real_cli_command_in_declared_order_excluding_help_start_stop() {
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
                "autostart",
                "model",
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
}
