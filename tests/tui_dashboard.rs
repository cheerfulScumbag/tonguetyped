//! Real-pseudo-terminal behavior tests for the dashboard opened by a bare
//! `tonguetyped` invocation (no subcommand). These drive the actual compiled
//! binary through a PTY exactly as a human terminal session would - no
//! source-grepping, no calling internal functions directly - because the
//! requirement under test is the terminal *experience*: what is actually
//! drawn, and how real key bytes change it.
//!
//! Every test that allocates a real PTY (`portable_pty::native_pty_system`)
//! is named with a `pty_` prefix - `flake.nix`'s `packages.default` skips
//! them (`cargoTestFlags = ["--" "--skip" "pty_"]`) because the Nix build
//! sandbox's `checkPhase` has no usable pty/tty subsystem for a nested
//! process to be spawned into (`spawn_command` fails there with ENOENT even
//! though the same binary builds and `openpty` itself succeeds); `nix
//! develop -c cargo test` and plain `cargo test` both run them normally and
//! are the real coverage for this file.

use portable_pty::{native_pty_system, Child, CommandBuilder, MasterPty, PtySize};
use std::io::{Read, Write};
use std::os::fd::FromRawFd;
use std::process::{Child as StdChild, Command as StdCommand, Stdio};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};
use tonguetyped::history::HistoryStore;

fn sandbox_root(tag: &str) -> std::path::PathBuf {
    std::env::temp_dir().join(format!(
        "tt-tui-dashboard-{tag}-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ))
}

/// Builds an isolated XDG sandbox (no real user config/data/runtime dirs
/// touched) and pre-seeds the default catalog model as "already installed"
/// (an empty stub file, same trick `tests/daemon_startup.rs` uses) so tests
/// that activate or confirm a model never attempt a real network download.
struct Sandbox {
    root: std::path::PathBuf,
    config_home: std::path::PathBuf,
    data_home: std::path::PathBuf,
    runtime_dir: std::path::PathBuf,
    bin: std::path::PathBuf,
    /// The compositor the spawned binary sees. KDE by default, so a helper-less
    /// sandbox exercises the "install dotool" warning; a test that needs the
    /// native libei backend *unavailable* pins a non-KDE/GNOME desktop instead.
    desktop: &'static str,
}

impl Sandbox {
    fn new(tag: &str) -> Self {
        Self::on(tag, "KDE")
    }

    fn on(tag: &str, desktop: &'static str) -> Self {
        let root = sandbox_root(tag);
        let config_home = root.join("config");
        let data_home = root.join("data");
        let runtime_dir = root.join("runtime");
        let bin = root.join("bin");
        std::fs::create_dir_all(config_home.join("tonguetyped")).unwrap();
        std::fs::create_dir_all(data_home.join("tonguetyped/models")).unwrap();
        std::fs::create_dir_all(&runtime_dir).unwrap();
        std::fs::create_dir_all(&bin).unwrap();
        let entry = tonguetyped::catalog::find(tonguetyped::catalog::DEFAULT_MODEL_ID).unwrap();
        std::fs::write(
            data_home.join("tonguetyped/models").join(entry.filename),
            [],
        )
        .unwrap();
        Self {
            root,
            config_home,
            data_home,
            runtime_dir,
            bin,
            desktop,
        }
    }

    fn apply_env(&self, cmd: &mut CommandBuilder) {
        cmd.env("XDG_CONFIG_HOME", &self.config_home);
        cmd.env("XDG_DATA_HOME", &self.data_home);
        cmd.env("XDG_RUNTIME_DIR", &self.runtime_dir);
        // Isolate typing-helper detection from the host machine: only the
        // sandbox's own bin directory is on PATH, and the session type is
        // pinned to Wayland so the X11-only `enigo` backend can never sneak
        // in on a developer's desktop. The compositor is pinned to KDE so the
        // install warning names dotool (wtype cannot work on KWin); tests that
        // need a typing helper drop a stub into `bin` via
        // `install_typing_helper`.
        cmd.env("PATH", &self.bin);
        cmd.env("XDG_SESSION_TYPE", "wayland");
        cmd.env("XDG_CURRENT_DESKTOP", self.desktop);
        cmd.env("XDG_SESSION_DESKTOP", self.desktop);
        cmd.env("DESKTOP_SESSION", self.desktop);
        cmd.env("NO_COLOR", "1");
        cmd.env("DBUS_SESSION_BUS_ADDRESS", "unix:path=/nonexistent");
    }

    /// Puts an executable stub on the sandbox PATH so `helper_self_test`
    /// finds a working typing helper of the given name.
    fn install_typing_helper(&self, name: &str) {
        let helper = self.bin.join(name);
        std::fs::write(&helper, "#!/bin/sh\nexit 0\n").unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&helper, std::fs::Permissions::from_mode(0o755)).unwrap();
        }
    }

    fn config_contents(&self) -> String {
        std::fs::read_to_string(self.config_home.join("tonguetyped/config.toml")).unwrap()
    }

    fn write_config(&self, contents: &str) {
        std::fs::write(self.config_home.join("tonguetyped/config.toml"), contents).unwrap();
    }

    fn socket_path(&self) -> std::path::PathBuf {
        self.runtime_dir.join("tonguetyped/control.sock")
    }

    /// Seeds the history database the daemon reads its "last result" from, so
    /// a test can drive a real `last-result` without recording anything.
    fn seed_history(&self, transcript: &str) {
        let path = self.data_home.join("tonguetyped/history.db");
        let store = HistoryStore::new(&path).unwrap();
        store.insert(transcript, None, "auto").unwrap();
    }
}

/// Runs the daemon as a separate process against a sandbox and waits for its
/// control socket, mirroring `tests/daemon_startup.rs`. Killed on drop.
struct Daemon(StdChild);

impl Drop for Daemon {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

fn start_daemon(sandbox: &Sandbox) -> Daemon {
    let mut child = StdCommand::new(env!("CARGO_BIN_EXE_tonguetyped"))
        .arg("daemon")
        .env("XDG_CONFIG_HOME", &sandbox.config_home)
        .env("XDG_DATA_HOME", &sandbox.data_home)
        .env("XDG_RUNTIME_DIR", &sandbox.runtime_dir)
        .env("DBUS_SESSION_BUS_ADDRESS", "unix:path=/nonexistent")
        .stdout(Stdio::null())
        .stderr(Stdio::piped())
        .spawn()
        .expect("spawn daemon");
    let socket = sandbox.socket_path();
    let deadline = Instant::now() + Duration::from_secs(10);
    while !socket.exists() {
        if child.try_wait().unwrap().is_some() {
            let mut stderr = String::new();
            child
                .stderr
                .take()
                .unwrap()
                .read_to_string(&mut stderr)
                .unwrap();
            panic!("daemon exited before opening its control socket: {stderr}");
        }
        assert!(
            Instant::now() < deadline,
            "daemon did not open its control socket"
        );
        std::thread::sleep(Duration::from_millis(20));
    }
    Daemon(child)
}

impl Drop for Sandbox {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.root);
    }
}

/// `vt100::Parser` turns the raw, diffed escape-sequence stream a real
/// terminal application emits (cursor moves, partial-cell rewrites on every
/// redraw) into an actual 2D screen grid - naively stripping ANSI codes and
/// concatenating bytes is not enough to read "the current screen" from a
/// ratatui app, since ratatui only rewrites the cells that changed between
/// frames and jumps the cursor directly between them.
struct Session {
    master: Box<dyn MasterPty + Send>,
    writer: Box<dyn Write + Send>,
    parser: Arc<Mutex<vt100::Parser>>,
    child: Box<dyn Child + Send + Sync>,
}

impl Session {
    fn spawn(sandbox: &Sandbox, cols: u16, rows: u16) -> Self {
        let pty_system = native_pty_system();
        let pair = pty_system
            .openpty(PtySize {
                rows,
                cols,
                pixel_width: 0,
                pixel_height: 0,
            })
            .expect("openpty");
        let mut cmd = CommandBuilder::new(env!("CARGO_BIN_EXE_tonguetyped"));
        sandbox.apply_env(&mut cmd);
        let child = pair.slave.spawn_command(cmd).expect("spawn dashboard");
        drop(pair.slave);
        let mut reader = pair.master.try_clone_reader().expect("clone reader");
        let writer = pair.master.take_writer().expect("take writer");
        // `Terminal::clear` asks the terminal for its cursor position and waits
        // for the report. A real terminal answers; this parser does not, so
        // answer it here - otherwise any screen that clears after suspending
        // (the setup console) blocks forever inside the dashboard.
        let reporter_fd = unsafe { libc::dup(pair.master.as_raw_fd().expect("pty master fd")) };
        assert!(
            reporter_fd >= 0,
            "duplicate the pty master for cursor reports"
        );
        let mut reporter = unsafe { std::fs::File::from_raw_fd(reporter_fd) };
        let parser = Arc::new(Mutex::new(vt100::Parser::new(rows, cols, 0)));
        let parser_for_thread = parser.clone();
        std::thread::spawn(move || {
            let mut chunk = [0u8; 4096];
            let mut tail: Vec<u8> = Vec::new();
            loop {
                match reader.read(&mut chunk) {
                    Ok(0) | Err(_) => break,
                    Ok(n) => {
                        let bytes = &chunk[..n];
                        parser_for_thread.lock().unwrap().process(bytes);
                        tail.extend_from_slice(bytes);
                        while let Some(position) =
                            tail.windows(4).position(|window| window == b"\x1b[6n")
                        {
                            tail.drain(..position + 4);
                            let _ = reporter.write_all(b"\x1b[1;1R");
                            let _ = reporter.flush();
                        }
                        if tail.len() > 8 {
                            tail.drain(..tail.len() - 8);
                        }
                    }
                }
            }
        });
        Session {
            master: pair.master,
            writer,
            parser,
            child,
        }
    }

    fn send(&mut self, bytes: &[u8]) {
        self.writer.write_all(bytes).unwrap();
        self.writer.flush().unwrap();
    }

    /// The current full screen contents, reconstructed from the real
    /// terminal state (not a raw byte concatenation).
    fn visible_text(&self) -> String {
        self.parser.lock().unwrap().screen().contents()
    }

    /// The character drawn in one cell of the real 2D terminal grid, so a test
    /// can assert where a border lands rather than only which text is present.
    fn char_at(&self, row: u16, col: u16) -> char {
        let parser = self.parser.lock().unwrap();
        parser
            .screen()
            .cell(row, col)
            .and_then(|cell| cell.contents().chars().next())
            .unwrap_or(' ')
    }

    /// Waits for one terminal cell to reach the expected state, so a resize
    /// assertion does not race the app's next repaint.
    fn wait_for_cell(
        &self,
        row: u16,
        col: u16,
        expected: impl Fn(char) -> bool,
        timeout: Duration,
    ) -> char {
        let deadline = Instant::now() + timeout;
        loop {
            let cell = self.char_at(row, col);
            if expected(cell) {
                return cell;
            }
            assert!(
                Instant::now() < deadline,
                "cell ({row}, {col}) stayed {cell:?}"
            );
            std::thread::sleep(Duration::from_millis(50));
        }
    }

    fn wait_for(&self, needle: &str, timeout: Duration) -> String {
        let deadline = Instant::now() + timeout;
        loop {
            let snapshot = self.visible_text();
            if snapshot.contains(needle) {
                return snapshot;
            }
            if Instant::now() >= deadline {
                panic!("timed out waiting for {needle:?}; current screen:\n{snapshot}");
            }
            std::thread::sleep(Duration::from_millis(50));
        }
    }

    fn resize(&self, cols: u16, rows: u16) {
        self.master
            .resize(PtySize {
                rows,
                cols,
                pixel_width: 0,
                pixel_height: 0,
            })
            .expect("resize pty");
        self.parser.lock().unwrap().set_size(rows, cols);
    }

    fn quit_and_wait(mut self) {
        self.send(b"q");
        let deadline = Instant::now() + Duration::from_secs(5);
        loop {
            if let Ok(Some(_)) = self.child.try_wait() {
                return;
            }
            if Instant::now() >= deadline {
                let _ = self.child.kill();
                panic!("dashboard did not exit after 'q'");
            }
            std::thread::sleep(Duration::from_millis(50));
        }
    }
}

const KEY_UP: &[u8] = b"\x1b[A";
const KEY_DOWN: &[u8] = b"\x1b[B";
const KEY_LEFT: &[u8] = b"\x1b[D";
const KEY_RIGHT: &[u8] = b"\x1b[C";
const KEY_ENTER: &[u8] = b"\r";
const KEY_ESC: &[u8] = b"\x1b";
const KEY_TAB: &[u8] = b"\t";
const KEY_BACKSPACE: &[u8] = b"\x7f";
const KEY_PAGE_DOWN: &[u8] = b"\x1b[6~";

const SETTING_LABELS: [&str; 15] = [
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
];

const COMMAND_NAMES: [&str; 9] = [
    "setup",
    "daemon",
    "toggle",
    "cancel",
    "status",
    "reload",
    "last-result",
    "doctor",
    "shortcut-test",
];

/// Opens the Settings panel from the split-deck home. The old dashboard
/// opened directly on Settings; the approved split deck starts on the rail.
/// Tests that need the settings list call this first.
fn open_settings(session: &mut Session) {
    let deadline = Instant::now() + Duration::from_secs(10);
    let screen = loop {
        let text = session.visible_text();
        if text.contains("Settings") || text.contains("5 destinations") {
            break text;
        }
        assert!(
            Instant::now() < deadline,
            "could not reach Settings; current screen:\n{text}"
        );
        std::thread::sleep(Duration::from_millis(50));
    };
    if screen.contains("Settings") {
        return;
    }
    session.send(b"p");
    session.wait_for("\u{25b8} SETUP", Duration::from_secs(3));
    session.send(b"g");
    session.wait_for("Settings", Duration::from_secs(3));
}

/// Home index of the Settings row labelled `label`.
fn setting_row(label: &str) -> usize {
    SETTING_LABELS
        .iter()
        .position(|candidate| *candidate == label)
        .unwrap()
}

/// The home Settings panel now ends with a single "Commands" row that opens the
/// dedicated Commands screen; this is its selection index (after the fifteen
/// configuration rows).
const FIRST_COMMAND_ROW: usize = SETTING_LABELS.len();

/// Opens the dedicated Commands screen from Home (the Settings panel's last
/// row) and waits for its title.
fn open_commands(session: &mut Session) {
    open_settings(session);
    for _ in 0..=SETTING_LABELS.len() {
        session.send(KEY_UP);
    }
    for _ in 0..SETTING_LABELS.len() {
        session.send(KEY_DOWN);
    }
    session.wait_for("> Commands", Duration::from_secs(3));
    session.send(KEY_ENTER);
    session.wait_for("Home \u{203a} Commands", Duration::from_secs(3));
}

/// Opens the Commands screen and moves the cursor onto command `name`. It
/// clamps to the first row before stepping down, so it does not depend on the
/// cursor's starting position.
fn select_command(session: &mut Session, name: &str) {
    open_commands(session);
    let index = COMMAND_NAMES
        .iter()
        .position(|candidate| *candidate == name)
        .unwrap_or_else(|| panic!("{name:?} is not a command"));
    for _ in 0..COMMAND_NAMES.len() {
        session.send(KEY_UP);
    }
    session.wait_for("> setup", Duration::from_secs(3));
    for _ in 0..index {
        session.send(KEY_DOWN);
    }
    session.wait_for(&format!("> {name}"), Duration::from_secs(3));
}

/// Moves the home selection from the first Settings row ("Model") down to
/// the row labeled `label`, and waits until it is drawn selected.
fn select_setting(session: &mut Session, label: &str) -> String {
    open_settings(session);
    for _ in 0..SETTING_LABELS.len() {
        session.send(KEY_UP);
    }
    let row = SETTING_LABELS
        .iter()
        .position(|candidate| *candidate == label)
        .unwrap_or_else(|| panic!("{label:?} is not a Settings row"));
    for _ in 0..row {
        session.send(KEY_DOWN);
    }
    session.wait_for(&format!("> {label}"), Duration::from_secs(3))
}

/// Waits until the dashboard has written `needle` into the sandbox's
/// config.toml. Used where two saves of the same screen produce the same
/// on-screen result line, so the result text alone cannot mark the second.
fn wait_for_config(sandbox: &Sandbox, needle: &str, timeout: Duration) {
    let deadline = Instant::now() + timeout;
    loop {
        let contents = sandbox.config_contents();
        if contents.contains(needle) {
            return;
        }
        assert!(
            Instant::now() < deadline,
            "config never contained {needle:?}:\n{contents}"
        );
        std::thread::sleep(Duration::from_millis(20));
    }
}

#[test]
fn pty_bare_invocation_opens_the_dashboard_with_settings_and_every_command_on_a_normal_terminal() {
    let sandbox = Sandbox::new("home");
    // Tall enough for the grouped Settings panel (fifteen rows, four section
    // headers, the Commands row) with no scrolling.
    let mut session = Session::spawn(&sandbox, 100, 38);

    // The signed-off redesign groups the Settings rows into four labeled
    // sections and moves the CLI commands onto their own screen, reached from
    // the Settings panel's "Commands" row. The grouped panel must still fit one
    // normal terminal with no pagination.
    open_settings(&mut session);
    let screen = session.visible_text();
    assert!(
        screen.contains("████████╗"),
        "a 100-column terminal should fit the full block-art logo:\n{screen}"
    );
    for label in SETTING_LABELS {
        assert!(
            screen.contains(label),
            "home screen is missing settings row {label:?}; got:\n{screen}"
        );
    }
    for section in ["Language & model", "Capture & activation", "Output", "App"] {
        assert!(
            screen.contains(section),
            "home screen is missing section header {section:?}:\n{screen}"
        );
    }
    assert!(
        screen.contains("Commands") && screen.contains("setup, daemon, doctor"),
        "the Settings panel should end with a Commands row:\n{screen}"
    );
    // The Settings panel shows real current values, not placeholders.
    assert!(
        screen.contains("Model              whisper-small-q5_k_m"),
        "the Model row should show the active catalog model:\n{screen}"
    );
    assert!(
        screen.contains("Inference backend  auto"),
        "the Inference backend row should show the configured preference:\n{screen}"
    );
    assert!(
        screen.contains("Overlay            enabled, top-right, badge, waveform"),
        "the Overlay row should summarize the real config:\n{screen}"
    );
    assert!(
        screen.contains("Transcript folder  disabled (history off)")
            && screen.contains("History retention  history off"),
        "a fresh install must show transcript saving as off:\n{screen}"
    );
    assert!(
        screen.contains("Daemon:"),
        "missing status strip:\n{screen}"
    );

    // The commands live on their own screen now; every one must still be there.
    open_commands(&mut session);
    let commands = session.visible_text();
    for name in COMMAND_NAMES {
        assert!(
            commands.contains(name),
            "Commands screen is missing command {name:?}; got:\n{commands}"
        );
    }
    assert!(
        !commands.contains("start          Start a new recording"),
        "start/stop stay hidden from the Commands screen:\n{commands}"
    );

    session.quit_and_wait();
}

#[test]
fn pty_home_notice_points_an_upgraded_history_setting_at_the_opt_out() {
    let sandbox = Sandbox::new("history-notice");
    sandbox.write_config("[history]\nenabled = true\n");
    let mut session = Session::spawn(&sandbox, 100, 40);
    open_settings(&mut session);

    let home = session.wait_for("Saving transcripts is on", Duration::from_secs(5));
    assert!(
        home.contains("Transcript output > Save transcript history"),
        "the notice must say where the opt-out lives:\n{home}"
    );

    session.quit_and_wait();
}

/// A fresh install (no config file) starts with saving off, shows no opt-out
/// notice, and exposes the "Save transcript history" toggle on the Transcript
/// output screen.
#[test]
fn pty_fresh_install_defaults_history_off_and_shows_the_toggle() {
    let sandbox = Sandbox::new("fresh-history-off");
    let mut session = Session::spawn(&sandbox, 100, 40);

    open_settings(&mut session);
    let home = session.visible_text();
    assert!(
        !home.contains("Saving transcripts is on"),
        "a fresh install must not claim transcripts are being saved:\n{home}"
    );
    assert!(
        home.contains("Transcript folder  disabled (history off)"),
        "a fresh install must summarize saving as off:\n{home}"
    );

    select_setting(&mut session, "Transcript output");
    session.send(KEY_ENTER);
    let screen = session.wait_for("Save transcript history", Duration::from_secs(3));
    assert!(
        screen.contains("[ ] do not deliver"),
        "the history toggle must be visible and off:\n{screen}"
    );

    session.send(KEY_ESC);
    session.wait_for("> Transcript output", Duration::from_secs(3));
    session.quit_and_wait();
}

/// Turning the toggle off persists to disk and survives stopping and starting
/// the daemon again.
#[test]
fn pty_opting_out_of_history_persists_across_a_daemon_restart() {
    // Short tag: the daemon's control-socket path must stay under SUN_LEN.
    let sandbox = Sandbox::new("optout");
    sandbox.write_config("[history]\nenabled = true\n");
    let daemon = start_daemon(&sandbox);
    let mut session = Session::spawn(&sandbox, 100, 40);
    open_settings(&mut session);
    session.wait_for("Saving transcripts is on", Duration::from_secs(5));

    select_setting(&mut session, "Transcript output");
    session.send(KEY_ENTER);
    session.wait_for("Save transcript history", Duration::from_secs(3));
    for _ in 0..4 {
        session.send(KEY_DOWN);
    }
    session.send(KEY_RIGHT);
    session.wait_for("[ ] do not deliver", Duration::from_secs(3));
    session.send(KEY_ENTER);
    session.wait_for("Transcript output saved.", Duration::from_secs(3));
    session.send(KEY_ESC);
    session.wait_for("> Transcript output", Duration::from_secs(3));
    session.quit_and_wait();

    let config: toml::Value = toml::from_str(&sandbox.config_contents()).unwrap();
    assert_eq!(
        config["history"]["enabled"].as_bool(),
        Some(false),
        "opting out must be saved: {config:?}"
    );

    drop(daemon);
    let daemon = start_daemon(&sandbox);
    let mut session = Session::spawn(&sandbox, 100, 40);
    open_settings(&mut session);
    let home = session.visible_text();
    assert!(
        !home.contains("Saving transcripts is on"),
        "the restart must not bring the opt-out notice back:\n{home}"
    );
    assert!(
        home.contains("Transcript folder  disabled (history off)"),
        "the restart must load the saved opt-out:\n{home}"
    );
    session.quit_and_wait();
    drop(daemon);
}

#[test]
fn pty_home_selection_walks_the_settings_then_opens_the_commands_screen() {
    let sandbox = Sandbox::new("cross-panel-nav");
    let mut session = Session::spawn(&sandbox, 100, 40);
    open_settings(&mut session);

    // "Model" (first settings row) is selected by default.
    let initial = session.visible_text();
    assert!(
        initial.contains("> Model"),
        "expected Model selected:\n{initial}"
    );

    // Walking down past every settings row lands on the single "Commands" row.
    for _ in 0..FIRST_COMMAND_ROW {
        session.send(KEY_DOWN);
    }
    let commands_selected = session.wait_for("> Commands", Duration::from_secs(3));
    assert!(
        !commands_selected.contains("> Model"),
        "only one row should carry the selection marker:\n{commands_selected}"
    );

    // Enter opens the dedicated Commands screen, starting on the first command.
    session.send(KEY_ENTER);
    let screen = session.wait_for("> setup", Duration::from_secs(3));
    assert!(
        screen.contains("Commands"),
        "the Commands screen should render:\n{screen}"
    );

    session.quit_and_wait();
}

#[test]
fn non_terminal_bare_invocation_fails_fast_instead_of_hanging() {
    // No subcommand, piped (non-TTY) stdio: the dashboard cannot open a real
    // terminal UI, so it must report a clear error and exit promptly rather
    // than hang or silently succeed - this is the only observable behavior
    // change versus the old clap error for a non-interactive bare
    // invocation, since both error out, just for a different reason.
    let sandbox = Sandbox::new("noninteractive");
    let mut cmd = std::process::Command::new(env!("CARGO_BIN_EXE_tonguetyped"));
    cmd.env("XDG_CONFIG_HOME", &sandbox.config_home)
        .env("XDG_DATA_HOME", &sandbox.data_home)
        .env("XDG_RUNTIME_DIR", &sandbox.runtime_dir)
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped());
    let child = cmd.spawn().unwrap();
    let output = child.wait_with_output().unwrap();
    assert!(!output.status.success());
    assert!(output.stdout.is_empty());
}

#[test]
fn pty_arrow_keys_move_the_home_selection_marker_and_escape_returns_to_the_deck() {
    let sandbox = Sandbox::new("nav");
    let mut session = Session::spawn(&sandbox, 100, 32);
    open_settings(&mut session);

    // "Model" (first settings row) is selected by default.
    let initial = session.visible_text();
    assert!(
        initial.contains("> Model"),
        "expected Model selected:\n{initial}"
    );

    let after_down = select_setting(&mut session, "Activation");
    assert!(
        !after_down.contains("> Model")
            && !after_down.contains("> Inference backend")
            && !after_down.contains("> Microphone"),
        "only one row should carry the selection marker:\n{after_down}"
    );

    session.send(KEY_UP);
    session.wait_for("> Microphone", Duration::from_secs(3));

    // Escape returns to the split deck instead of quitting.
    session.send(KEY_ESC);
    let deck = session.wait_for("LIVE", Duration::from_secs(3));
    assert!(
        session.child.try_wait().unwrap().is_none(),
        "Esc must not quit the dashboard"
    );
    assert!(
        deck.contains("SPEAK"),
        "Esc from Settings should return to the split deck:\n{deck}"
    );

    session.quit_and_wait();
}

#[test]
fn pty_logo_recenters_for_wide_and_narrow_terminals_without_clipping() {
    let sandbox = Sandbox::new("resize");
    let mut session = Session::spawn(&sandbox, 140, 32);
    let deck = session.wait_for("5 destinations", Duration::from_secs(5));
    assert!(
        deck.contains("TONGUETYPED"),
        "the split deck should show the Bubble header wordmark:\n{deck}"
    );
    open_settings(&mut session);
    let wide = session.wait_for("Commands", Duration::from_secs(5));
    assert!(
        wide.contains("████████╗"),
        "a wide (140-col) terminal should show the full block-art logo:\n{wide}"
    );

    session.resize(40, 32);
    let narrow = session.wait_for("TONGUETYPED", Duration::from_secs(3));
    assert!(
        !narrow.contains("████████╗"),
        "a narrow (40-col) terminal must fall back to the compact wordmark, not clip the block \
         art:\n{narrow}"
    );

    session.resize(140, 32);
    let wide_again = session.wait_for("████████╗", Duration::from_secs(3));
    assert!(
        !wide_again.contains("TONGUETYPED"),
        "widening back out should restore the full logo, not keep the compact one:\n{wide_again}"
    );

    session.quit_and_wait();
}

#[test]
fn pty_splash_shows_the_chafa_bubble_then_the_deck() {
    let sandbox = Sandbox::new("splash");
    let session = Session::spawn(&sandbox, 80, 24);
    let splash = session.wait_for("press any key", Duration::from_secs(2));
    assert!(
        splash.contains("TONGUETYPED"),
        "the splash should show the chafa bubble and wordmark:\n{splash}"
    );
    let deck = session.wait_for("5 destinations", Duration::from_secs(3));
    assert!(
        deck.contains("LIVE") && deck.contains("SPEAK"),
        "the splash should give way to the split deck:\n{deck}"
    );
    session.quit_and_wait();
}

#[test]
fn pty_splash_consumes_the_dismissal_key() {
    let sandbox = Sandbox::new("splash-key");
    let mut session = Session::spawn(&sandbox, 80, 24);
    session.wait_for("press any key", Duration::from_secs(2));
    session.send(b"q");
    let deck = session.wait_for("5 destinations", Duration::from_secs(3));
    assert!(
        session.child.try_wait().unwrap().is_none(),
        "'q' must only dismiss the splash, not quit the dashboard"
    );
    assert!(
        deck.contains("SPEAK"),
        "the splash should give way to the deck after one key:\n{deck}"
    );
    session.quit_and_wait();
}

#[test]
fn pty_split_deck_shows_the_bubble_header_and_single_letter_rail() {
    let sandbox = Sandbox::new("deck");
    let mut session = Session::spawn(&sandbox, 80, 24);
    let deck = session.wait_for("5 destinations", Duration::from_secs(5));
    assert!(
        deck.contains("TONGUETYPED"),
        "missing Bubble wordmark:\n{deck}"
    );
    for destination in ["SPEAK", "MODELS", "OUTPUT", "HISTORY", "SETUP"] {
        assert!(
            deck.contains(destination),
            "missing rail destination {destination:?}:\n{deck}"
        );
    }
    assert!(
        deck.contains("enter start") && deck.contains("↑↓ move"),
        "the footer should keep the single-letter key map:\n{deck}"
    );
    assert!(
        deck.contains("mic") && deck.contains("simulated"),
        "the Speak pane should show its simulated mic level:\n{deck}"
    );
    let footer = deck
        .lines()
        .find(|line| line.contains("q quit"))
        .expect("the deck footer should be drawn");
    assert!(
        !footer.contains('[') && !footer.contains(']'),
        "the deck footer must not use bracketed keycaps:\n{footer}"
    );
    let rail = deck
        .lines()
        .find(|line| line.contains("\u{25b8} SPEAK"))
        .expect("the selected rail row should be drawn");
    assert!(
        !rail.contains('[') && !rail.contains(']'),
        "the deck rail must not use bracketed keycaps:\n{rail}"
    );
    // 80x24: 18-column rail + 1-column gutter + 60-column pane + 1-column
    // right margin. The body border row sits under the seven-row header.
    assert_ne!(
        session.char_at(7, 78),
        ' ',
        "the pane's right border should land in column 78 at 80x24"
    );
    assert_eq!(
        session.char_at(7, 79),
        ' ',
        "a one-column right margin should follow the pane at 80x24"
    );

    // 120x40: 24-column rail + 1-column gutter + 94-column pane + 1-column
    // right margin.
    session.resize(120, 40);
    let border = session.wait_for_cell(7, 118, |cell| cell != ' ', Duration::from_secs(3));
    assert_ne!(
        border, ' ',
        "the pane's right border should land in column 118 at 120x40"
    );
    assert_eq!(
        session.char_at(7, 119),
        ' ',
        "a one-column right margin should follow the pane at 120x40"
    );
    session.resize(80, 24);
    session.wait_for_cell(7, 79, |cell| cell == ' ', Duration::from_secs(3));
    assert_ne!(
        session.char_at(7, 78),
        ' ',
        "the pane's right border should land back in column 78 at 80x24"
    );

    session.send(b"p");
    session.wait_for("\u{25b8} SETUP", Duration::from_secs(3));
    session.send(b"g");
    session.wait_for("Settings", Duration::from_secs(3));

    session.quit_and_wait();
}

#[test]
fn pty_deck_action_result_returns_to_the_deck() {
    let sandbox = Sandbox::new("deck-info-return");
    let mut session = Session::spawn(&sandbox, 80, 24);
    session.wait_for("5 destinations", Duration::from_secs(5));

    session.send(KEY_ENTER);
    let result = session.wait_for("daemon is not running", Duration::from_secs(5));
    assert!(
        result.contains("Toggle"),
        "expected the Toggle action's title:\n{result}"
    );

    session.send(KEY_ESC);
    let deck = session.wait_for("5 destinations", Duration::from_secs(3));
    assert!(
        deck.contains("SPEAK"),
        "Esc from a Deck action result should return to the Deck:\n{deck}"
    );

    session.quit_and_wait();
}

#[test]
fn pty_deck_commands_return_to_the_deck() {
    let sandbox = Sandbox::new("deck-commands-return");
    let mut session = Session::spawn(&sandbox, 80, 24);
    session.wait_for("5 destinations", Duration::from_secs(5));

    session.send(b"p");
    session.wait_for("\u{25b8} SETUP", Duration::from_secs(3));
    session.send(b"c");
    session.wait_for("Home \u{203a} Commands", Duration::from_secs(3));

    session.send(KEY_ESC);
    let deck = session.wait_for("LIVE", Duration::from_secs(3));
    assert!(
        deck.contains("SPEAK"),
        "Esc from Commands opened on the Deck should return to the Deck:\n{deck}"
    );

    session.quit_and_wait();
}

#[test]
fn pty_deck_commands_detail_and_result_screens_return_to_the_deck() {
    let sandbox = Sandbox::new("deck-commands-detail-return");
    let mut session = Session::spawn(&sandbox, 80, 24);
    session.wait_for("5 destinations", Duration::from_secs(5));

    let select_deck_command = |session: &mut Session, name: &str| {
        session.send(b"p");
        session.wait_for("\u{25b8} SETUP", Duration::from_secs(3));
        session.send(b"c");
        session.wait_for("Home \u{203a} Commands", Duration::from_secs(3));
        for _ in 0..COMMAND_NAMES.len() {
            session.send(KEY_UP);
        }
        session.wait_for("> setup", Duration::from_secs(3));
        let index = COMMAND_NAMES
            .iter()
            .position(|candidate| *candidate == name)
            .unwrap();
        for _ in 0..index {
            session.send(KEY_DOWN);
        }
        session.wait_for(&format!("> {name}"), Duration::from_secs(3));
    };

    select_deck_command(&mut session, "daemon");
    session.send(KEY_ENTER);
    session.wait_for("Restart", Duration::from_secs(3));
    session.send(KEY_ESC);
    let deck = session.wait_for("LIVE", Duration::from_secs(3));
    assert!(
        deck.contains("SPEAK"),
        "Esc from a daemon screen opened on the Deck should return to the Deck:\n{deck}"
    );

    select_deck_command(&mut session, "status");
    session.send(KEY_ENTER);
    let result = session.wait_for("daemon is not running", Duration::from_secs(5));
    assert!(
        result.contains("Status"),
        "expected the Status action's title:\n{result}"
    );
    session.send(KEY_ESC);
    let deck = session.wait_for("LIVE", Duration::from_secs(3));
    assert!(
        deck.contains("SPEAK"),
        "Esc from a command result opened on the Deck should return to the Deck:\n{deck}"
    );

    session.quit_and_wait();
}

#[test]
fn pty_deck_action_result_under_help_returns_to_the_deck() {
    let sandbox = Sandbox::new("dh");
    let socket = sandbox.socket_path();
    std::fs::create_dir_all(socket.parent().unwrap()).unwrap();
    let listener = std::os::unix::net::UnixListener::bind(&socket).unwrap();
    std::thread::spawn(move || {
        for stream in listener.incoming().flatten() {
            std::thread::sleep(Duration::from_secs(3));
            drop(stream);
        }
    });

    let mut session = Session::spawn(&sandbox, 80, 24);
    session.wait_for("5 destinations", Duration::from_secs(5));

    session.send(KEY_ENTER);
    session.wait_for("Working: Toggle...", Duration::from_secs(3));
    session.send(b"?");
    let help = session.wait_for("Keys", Duration::from_secs(3));
    assert!(
        help.contains("Speak: t toggle"),
        "the help overlay should open over the pending action:\n{help}"
    );

    let deadline = Instant::now() + Duration::from_secs(8);
    let mut result = session.visible_text();
    while result.contains("Keys") && Instant::now() < deadline {
        std::thread::sleep(Duration::from_millis(50));
        result = session.visible_text();
    }
    assert!(
        result.contains("Toggle") && !result.contains("Keys"),
        "the pending result should replace the help overlay:\n{result}"
    );

    session.send(KEY_ESC);
    let deck = session.wait_for("LIVE", Duration::from_secs(3));
    assert!(
        deck.contains("SPEAK"),
        "Esc on a result that replaced Deck help should return to the Deck:\n{deck}"
    );

    session.quit_and_wait();
}

#[test]
fn pty_deck_panes_show_models_output_history_and_setup() {
    let sandbox = Sandbox::new("deck-panes");
    let mut session = Session::spawn(&sandbox, 80, 24);
    session.wait_for("5 destinations", Duration::from_secs(5));

    session.send(b"m");
    let models = session.wait_for("STREAM", Duration::from_secs(3));
    assert!(
        models.contains("STREAM") && (models.contains("live") || models.contains("final")),
        "the model pane should show the stream column:\n{models}"
    );
    assert!(
        models.contains("more") || models.contains("above"),
        "the model pane should show the more-below cue:\n{models}"
    );
    assert!(
        models.contains("activate") && models.contains("download"),
        "the model pane should show its actions:\n{models}"
    );

    session.send(b"o");
    let output = session.wait_for("Save transcript history", Duration::from_secs(3));
    assert!(
        output.contains("Save transcript history") || output.contains("Do not deliver"),
        "the output pane should show the delivery choices:\n{output}"
    );

    session.send(b"h");
    let history = session.wait_for("History is off", Duration::from_secs(3));
    assert!(
        history.contains("History is off") || history.contains("Maximum entries"),
        "the history pane should show the history state:\n{history}"
    );
    session.send(b" ");
    let history_on = session.wait_for("Maximum entries", Duration::from_secs(3));
    assert!(
        history_on.contains("Maximum age"),
        "turning history on should reveal both fields:\n{history_on}"
    );

    session.send(b"p");
    let setup = session.wait_for("Review", Duration::from_secs(3));
    assert!(
        setup.contains("Model") && setup.contains("Review"),
        "the setup pane should show the five chapters:\n{setup}"
    );

    session.quit_and_wait();
}

#[test]
fn pty_deck_output_step_saves_without_enter() {
    let sandbox = Sandbox::new("deck-step-save");
    let mut session = Session::spawn(&sandbox, 80, 24);
    session.wait_for("5 destinations", Duration::from_secs(5));

    session.send(b"o");
    session.wait_for("Save transcript history", Duration::from_secs(3));
    for _ in 0..4 {
        session.send(KEY_DOWN);
    }
    session.send(KEY_RIGHT);
    session.wait_for("Transcript output saved.", Duration::from_secs(3));

    let config: toml::Value = toml::from_str(&sandbox.config_contents()).unwrap();
    assert_eq!(
        config["history"]["enabled"].as_bool(),
        Some(true),
        "stepping the history toggle must persist it before a reload: {config:?}"
    );

    session.quit_and_wait();
}

#[test]
fn pty_deck_history_form_resyncs_after_a_background_action_reloads_config() {
    let sandbox = Sandbox::new("dhs");
    sandbox.write_config("[history]\nenabled = true\nmax_entries = 1000\nmax_age_days = 30\n");
    // A daemon socket that accepts the Toggle request and holds it open, so
    // the Deck is still showing the History pane while the action is pending.
    let socket = sandbox.socket_path();
    std::fs::create_dir_all(socket.parent().unwrap()).unwrap();
    let listener = std::os::unix::net::UnixListener::bind(&socket).unwrap();
    let handle = std::thread::spawn(move || {
        if let Ok((stream, _)) = listener.accept() {
            std::thread::sleep(Duration::from_secs(3));
            drop(stream);
        }
    });

    let mut session = Session::spawn(&sandbox, 80, 24);
    session.wait_for("5 destinations", Duration::from_secs(5));

    session.send(KEY_ENTER);
    session.wait_for("Working: Toggle...", Duration::from_secs(3));
    session.send(b"h");
    let history = session.wait_for("Maximum entries", Duration::from_secs(3));
    assert!(
        history.contains("1000"),
        "the Deck History pane should start from the saved limit:\n{history}"
    );

    // Another process (e.g. the CLI or setup console) lowers the persisted
    // limit while the Deck action is still in flight. When the action result
    // reloads config, the Deck's History form must follow it instead of later
    // writing the stale 1000 back over the file.
    sandbox.write_config("[history]\nenabled = true\nmax_entries = 500\nmax_age_days = 30\n");

    session.wait_for("Connection reset by peer", Duration::from_secs(10));
    session.send(KEY_ESC);
    let deck = session.wait_for("Maximum entries", Duration::from_secs(3));
    assert!(
        deck.contains("500") && !deck.contains("1000"),
        "returning to the Deck must resync the History form from config:\n{deck}"
    );

    session.send(KEY_ENTER);
    session.wait_for("History retention saved.", Duration::from_secs(3));
    let config: toml::Value = toml::from_str(&sandbox.config_contents()).unwrap();
    assert_eq!(
        config["history"]["max_entries"].as_integer(),
        Some(500),
        "saving the Deck History pane must not overwrite the reloaded limit: {config:?}"
    );

    handle.join().unwrap();
    session.quit_and_wait();
}

#[test]
fn pty_deck_output_selection_resyncs_after_settings_change_the_method() {
    let sandbox = Sandbox::new("deck-output-resync");
    sandbox.install_typing_helper("wtype");
    sandbox.write_config("[output]\nmethod = \"none\"\ntyping_backend = \"wtype\"\n");
    let mut session = Session::spawn(&sandbox, 100, 40);
    session.wait_for("5 destinations", Duration::from_secs(5));

    // Change the saved method in the full Settings screen, as the finding's
    // Deck -> g -> Transcript output path does.
    open_settings(&mut session);
    select_setting(&mut session, "Transcript output");
    session.send(KEY_ENTER);
    session.wait_for("Do not deliver", Duration::from_secs(3));
    session.send(KEY_DOWN);
    session.wait_for(
        "> Type into the focused application",
        Duration::from_secs(3),
    );
    session.send(KEY_ENTER);
    session.wait_for("Transcript output saved.", Duration::from_secs(3));
    let saved: toml::Value = toml::from_str(&sandbox.config_contents()).unwrap();
    assert_eq!(
        saved["output"]["method"].as_str(),
        Some("type"),
        "the settings screen should have saved the new method: {saved:?}"
    );

    // Returning to the Deck must resync the Output pane. If it keeps the
    // startup selection ("none"), Enter applies that stale row: it reverts
    // the method and resets typing_backend to "auto".
    session.send(KEY_ESC);
    session.wait_for("Settings", Duration::from_secs(3));
    session.send(KEY_ESC);
    session.wait_for("enter setup", Duration::from_secs(3));
    session.send(b"o");
    session.wait_for("Save transcript history", Duration::from_secs(3));
    session.send(KEY_ENTER);
    session.wait_for("Transcript output saved.", Duration::from_secs(3));

    let config: toml::Value = toml::from_str(&sandbox.config_contents()).unwrap();
    assert_eq!(
        config["output"]["method"].as_str(),
        Some("type"),
        "the Deck Output pane must not write back the method it was built with: {config:?}"
    );
    assert_eq!(
        config["output"]["typing_backend"].as_str(),
        Some("wtype"),
        "applying a synced row must not reset the typing backend: {config:?}"
    );

    session.quit_and_wait();
}

#[test]
fn pty_setup_console_preserves_an_external_config_rewrite() {
    let sandbox = Sandbox::new("setup-presave");
    sandbox.install_typing_helper("wtype");
    sandbox.write_config("[output]\nmethod = \"type\"\ntyping_backend = \"wtype\"\n");
    let mut session = Session::spawn(&sandbox, 100, 40);
    session.wait_for("5 destinations", Duration::from_secs(5));

    // Another terminal (or dashboard) rewrites the config while this
    // dashboard is open. Launching the setup console and leaving it untouched
    // must not overwrite it with this process's startup snapshot.
    sandbox.write_config("[output]\nmethod = \"none\"\ntyping_backend = \"wtype\"\n");

    session.send(b"p");
    session.wait_for("Review", Duration::from_secs(3));
    session.send(KEY_ENTER);
    session.wait_for("VOX // ", Duration::from_secs(15));
    session.send(b"q");
    // Re-entering the alternate screen clears it; nudge the Deck to redraw a
    // changed pane so the assertion below reads the resumed dashboard.
    session.send(b"s");
    session.wait_for("5 destinations", Duration::from_secs(10));

    let config: toml::Value = toml::from_str(&sandbox.config_contents()).unwrap();
    assert_eq!(
        config["output"]["method"].as_str(),
        Some("none"),
        "launching and leaving setup untouched must keep the newer on-disk config: {config:?}"
    );

    // The Output pane must also follow the config the console reloaded, not
    // the method this dashboard started with.
    session.send(b"o");
    session.wait_for("Save transcript history", Duration::from_secs(3));
    session.send(KEY_ENTER);
    session.wait_for("Transcript output saved.", Duration::from_secs(3));

    let config: toml::Value = toml::from_str(&sandbox.config_contents()).unwrap();
    assert_eq!(
        config["output"]["method"].as_str(),
        Some("none"),
        "the Deck Output pane must use the config reloaded after setup: {config:?}"
    );

    session.quit_and_wait();
}

#[test]
fn pty_deck_models_download_key_installs_without_activating() {
    let sandbox = Sandbox::new("deck-download");
    for entry in tonguetyped::catalog::ENTRIES {
        std::fs::write(
            sandbox
                .data_home
                .join("tonguetyped/models")
                .join(entry.filename),
            [],
        )
        .unwrap();
    }
    let mut session = Session::spawn(&sandbox, 100, 32);
    session.wait_for("5 destinations", Duration::from_secs(5));

    session.send(b"m");
    session.wait_for("STREAM", Duration::from_secs(3));
    // The default (active) model is the initial cursor; move to an installed
    // model that is not active, so an activation would be observable.
    session.send(KEY_DOWN);
    session.send(b"d");

    let result = session.wait_for("already installed", Duration::from_secs(10));
    assert!(
        result.contains("Model download"),
        "the download key should report a download result:\n{result}"
    );
    assert!(
        !result.contains("activated"),
        "the download key must not claim or perform activation:\n{result}"
    );
    assert!(
        sandbox.config_contents().contains(&format!(
            "active_model = \"{}\"",
            tonguetyped::catalog::DEFAULT_MODEL_ID
        )),
        "the download key must not select the downloaded model:\n{}",
        sandbox.config_contents()
    );

    session.send(KEY_ESC);
    let deck = session.wait_for("LIVE", Duration::from_secs(3));
    assert!(
        deck.contains("SPEAK"),
        "Esc on a download result should return to the Deck:\n{deck}"
    );

    session.quit_and_wait();
}

#[test]
fn pty_selecting_an_ipc_action_without_a_running_daemon_shows_an_actionable_error() {
    let sandbox = Sandbox::new("action-feedback");
    let mut session = Session::spawn(&sandbox, 100, 32);
    open_settings(&mut session);

    // Run "status" from the dedicated Commands screen. No daemon is running in
    // this sandbox, so the dashboard must show a visible, actionable error -
    // not hang, not crash, not silently do nothing.
    select_command(&mut session, "status");
    session.send(KEY_ENTER);

    let result = session.wait_for("daemon is not running", Duration::from_secs(5));
    assert!(
        result.contains("Status"),
        "expected the Status action's title:\n{result}"
    );

    // Esc/Enter return to Home from the result screen.
    session.send(KEY_ESC);
    let back_home = session.wait_for("> Commands", Duration::from_secs(3));
    assert!(
        !back_home.contains("daemon is not running"),
        "returning to Home should clear the prior result screen:\n{back_home}"
    );

    session.quit_and_wait();
}

#[test]
fn pty_commands_screen_shows_in_flight_feedback_while_a_command_runs() {
    // The tag must stay short: the daemon stand-in binds a Unix-domain socket
    // under the sandbox, and the path must fit under SUN_LEN (~108 bytes) even
    // with Nix's already-long TMPDIR.
    let sandbox = Sandbox::new("ci");

    // A daemon stand-in that accepts the connection and never answers keeps a
    // dispatched IPC command pending indefinitely, so the screen can be
    // observed *while* the command is in flight rather than after it settles.
    let socket = sandbox.socket_path();
    std::fs::create_dir_all(socket.parent().unwrap()).unwrap();
    let listener = std::os::unix::net::UnixListener::bind(&socket).unwrap();
    std::thread::spawn(move || {
        let mut held = Vec::new();
        for stream in listener.incoming() {
            match stream {
                Ok(stream) => held.push(stream),
                Err(_) => break,
            }
        }
    });

    let mut session = Session::spawn(&sandbox, 100, 32);
    open_settings(&mut session);

    select_command(&mut session, "status");
    session.send(KEY_ENTER);

    // Dispatching the command must be acknowledged: the Working line replaces
    // the static legend while the command runs, and the screen stays on
    // Commands (it has not jumped to the result yet).
    let in_flight = session.wait_for("Working: Status...", Duration::from_secs(3));
    assert!(
        in_flight.contains("Home \u{203a} Commands"),
        "the in-flight indicator must show on the Commands screen:\n{in_flight}"
    );
    assert!(
        !in_flight.contains("Enter run"),
        "the static legend should give way to the Working line while a command runs:\n{in_flight}"
    );

    session.quit_and_wait();
}

#[test]
fn pty_daemon_home_item_opens_a_start_stop_restart_screen_instead_of_starting_immediately() {
    let sandbox = Sandbox::new("daemon-screen");
    let mut session = Session::spawn(&sandbox, 100, 32);
    open_settings(&mut session);

    // "daemon" opens a sub-screen offering Start/Stop/Restart, not immediately
    // launching the daemon the way the old single-action binding did.
    select_command(&mut session, "daemon");
    session.send(KEY_ENTER);
    let screen = session.wait_for("Restart", Duration::from_secs(3));
    assert!(
        screen.contains("Start") && screen.contains("Stop") && screen.contains("Restart"),
        "daemon screen should list all three actions:\n{screen}"
    );
    assert!(
        !screen.contains("Daemon: running"),
        "opening the daemon screen must not start anything by itself:\n{screen}"
    );

    // Esc from the daemon screen with no action run yet must return to Home
    // without side effects.
    session.send(KEY_ESC);
    session.wait_for("> Commands", Duration::from_secs(3));

    // Reopen the daemon screen (selection resets to "Start"), move down once
    // to select "Stop", and run it. No daemon is running in this sandbox, so
    // this must surface the same actionable error the CLI's `daemon stop`
    // would, not hang or silently do nothing.
    select_command(&mut session, "daemon");
    session.send(KEY_ENTER);
    session.wait_for("Restart", Duration::from_secs(3));
    session.send(KEY_DOWN);
    let selected = session.wait_for("> Stop", Duration::from_secs(3));
    assert!(
        selected.contains("> Stop"),
        "expected Stop selected:\n{selected}"
    );
    session.send(KEY_ENTER);
    let result = session.wait_for("daemon is not running", Duration::from_secs(5));
    assert!(
        result.contains("Daemon"),
        "expected the Daemon action's title:\n{result}"
    );

    // Esc from the result screen returns to Home.
    session.send(KEY_ESC);
    session.wait_for("> Commands", Duration::from_secs(3));

    session.quit_and_wait();
}

#[test]
fn pty_model_screen_lists_the_catalog_and_activation_reports_confirmed_failure_for_a_fake_file() {
    // The sandbox's pre-seeded model file is an empty stub, not a real GGUF,
    // so activation's own confirmation step (re-running diagnostics) must
    // fail to load it - this exercises the full "download/verify, save,
    // reload, confirm, and report failure without claiming activation" flow
    // deterministically and offline, through the real TUI, rather than
    // requiring a multi-gigabyte network download in a test.
    let sandbox = Sandbox::new("model-activation");
    let mut session = Session::spawn(&sandbox, 100, 40);
    open_settings(&mut session);

    // "Model" is the first Settings row and is already selected.
    session.wait_for("> Model", Duration::from_secs(3));
    session.send(KEY_ENTER);

    let catalog_screen = session.wait_for("MODEL ID", Duration::from_secs(5));
    assert!(
        catalog_screen.contains(tonguetyped::catalog::DEFAULT_MODEL_ID),
        "model screen should list the default catalog entry:\n{catalog_screen}"
    );

    // The default model is already the active/selected row; activate it.
    session.send(KEY_ENTER);

    let outcome = session.wait_for("NOT fully activated", Duration::from_secs(20));
    assert!(
        outcome.contains("Model activation"),
        "expected the model activation result title:\n{outcome}"
    );
    assert!(
        outcome.contains("download:"),
        "activation result should report the download outcome:\n{outcome}"
    );

    session.quit_and_wait();
}

#[test]
fn pty_activation_setting_changes_mode_and_persists_it() {
    let sandbox = Sandbox::new("activation-setting");
    let mut session = Session::spawn(&sandbox, 100, 32);
    open_settings(&mut session);

    select_setting(&mut session, "Activation");
    session.send(KEY_ENTER);

    session.wait_for("> Hold the shortcut while speaking", Duration::from_secs(3));
    session.send(KEY_DOWN);
    session.send(KEY_ENTER);
    let saved = session.wait_for("Activation mode saved.", Duration::from_secs(3));
    assert!(
        saved.contains("> Press once to start and again to stop"),
        "the new mode should stay highlighted after applying:\n{saved}"
    );

    // Reopening the screen shows the new mode selected, and the same choice
    // was written to config.toml.
    session.send(KEY_ESC);
    open_settings(&mut session);
    session.send(KEY_ENTER);
    session.wait_for(
        "> Press once to start and again to stop",
        Duration::from_secs(3),
    );
    assert!(sandbox.config_contents().contains("mode = \"toggle\""));

    session.quit_and_wait();
}

#[test]
fn pty_backspace_and_left_right_work_on_a_settings_screen() {
    // The menu redesign adds Backspace as a second way out (Escape is no longer
    // the only one) and makes `←`/`→` move the highlight, so a change is one
    // key away and commits on leave.
    let sandbox = Sandbox::new("backspace-nav");
    let mut session = Session::spawn(&sandbox, 100, 32);
    open_settings(&mut session);

    select_setting(&mut session, "Activation");
    session.send(KEY_ENTER);
    let opened = session.wait_for("> Hold the shortcut while speaking", Duration::from_secs(3));
    assert!(
        opened.contains("Home \u{203a} Activation"),
        "a settings screen should carry its breadcrumb:\n{opened}"
    );

    // Right moves the highlight to the second option; Backspace commits it and
    // returns home - no Enter, no Escape.
    session.send(KEY_RIGHT);
    session.wait_for(
        "> Press once to start and again to stop",
        Duration::from_secs(3),
    );
    session.send(KEY_BACKSPACE);
    let home = session.wait_for("> Activation", Duration::from_secs(3));
    assert!(
        home.contains("toggle"),
        "backing out should commit the highlighted mode:\n{home}"
    );
    assert!(sandbox.config_contents().contains("mode = \"toggle\""));

    session.quit_and_wait();
}

#[test]
fn pty_overlay_setting_steps_position_both_ways_and_persists_on_leave() {
    let sandbox = Sandbox::new("overlay-setting");
    let mut session = Session::spawn(&sandbox, 100, 40);
    open_settings(&mut session);

    // Overlay is a Settings row.
    for _ in 0..setting_row("Overlay") {
        session.send(KEY_DOWN);
    }
    session.wait_for("> Overlay", Duration::from_secs(3));
    session.send(KEY_ENTER);

    // The screen opens on the enable stepper; every value is a `‹ value ›`
    // stepper, not a plain text value.
    let opened = session.wait_for("\u{2039} top-right \u{203a}", Duration::from_secs(3));
    assert!(
        opened.contains("\u{2039} Enabled")
            && opened.contains("2/2")
            && opened.contains("Badge \u{203a}")
            && opened.contains("\u{2039} Waveform"),
        "the Overlay screen should show all current values as steppers:\n{opened}"
    );

    // Move to the Position row and step right: the value changes immediately,
    // instead of Enter cycling forward only.
    session.send(KEY_DOWN);
    session.wait_for("> Position", Duration::from_secs(3));
    session.send(KEY_RIGHT);
    let stepped = session.wait_for("\u{2039} center \u{203a}", Duration::from_secs(3));
    assert!(
        stepped.contains("4/7"),
        "the counter should show the position moved one step:\n{stepped}"
    );

    // Left steps back to the saved value - the reverse Enter never offered.
    session.send(KEY_LEFT);
    let back = session.wait_for("\u{2039} top-right \u{203a}", Duration::from_secs(3));
    assert!(
        back.contains("3/7"),
        "stepping left should walk back to the saved value:\n{back}"
    );

    // Step right again and back out: leaving commits the pending value.
    session.send(KEY_RIGHT);
    session.wait_for("\u{2039} center \u{203a}", Duration::from_secs(3));
    session.send(KEY_ESC);
    let home = session.wait_for("enabled, center, badge, waveform", Duration::from_secs(3));
    assert!(
        home.contains("> Overlay"),
        "selection should return to the Overlay row:\n{home}"
    );
    assert!(
        sandbox.config_contents().contains("position = \"center\""),
        "backing out should commit the pending position"
    );

    session.quit_and_wait();
}

#[test]
fn pty_overlay_disabled_hides_sub_rows_until_enabled() {
    let sandbox = Sandbox::new("overlay-disabled");
    std::fs::write(
        sandbox.config_home.join("tonguetyped/config.toml"),
        "[overlay]\nenabled = false\n",
    )
    .unwrap();
    let mut session = Session::spawn(&sandbox, 100, 40);
    open_settings(&mut session);

    let home = session.wait_for("Overlay            disabled", Duration::from_secs(5));
    assert!(
        home.contains("Overlay            disabled"),
        "the summary row should reflect the disabled overlay:\n{home}"
    );
    for _ in 0..setting_row("Overlay") {
        session.send(KEY_DOWN);
    }
    session.wait_for("> Overlay", Duration::from_secs(3));
    session.send(KEY_ENTER);

    // Disabled state: only the enable stepper, no configuration sub-rows.
    let disabled = session.wait_for("Disabled \u{203a}", Duration::from_secs(3));
    assert!(
        disabled.contains("1/2")
            && !disabled.contains("Position")
            && !disabled.contains("Waveform"),
        "a disabled overlay must show only the enable stepper:\n{disabled}"
    );

    // Step the enable row right: the three configuration steppers appear and
    // the preview band opens with them.
    session.send(KEY_RIGHT);
    let enabled = session.wait_for("Position", Duration::from_secs(3));
    assert!(
        enabled.contains("\u{2039} Enabled") && enabled.contains("Preview"),
        "enabling should reveal the configuration rows and the preview:\n{enabled}"
    );

    // Leaving commits the pending enable state.
    session.send(KEY_ESC);
    open_settings(&mut session);
    assert!(sandbox.config_contents().contains("enabled = true"));

    session.quit_and_wait();
}

#[test]
fn pty_transcript_folder_setting_persists_an_absolute_path() {
    let sandbox = Sandbox::new("folder-setting");
    let mut session = Session::spawn(&sandbox, 100, 40);
    open_settings(&mut session);

    // Saving transcripts is opt-in, so turn it on from the Transcript output
    // screen before the folder is edited. The history row sits after the four
    // method rows for the default "Do not deliver" method.
    select_setting(&mut session, "Transcript output");
    session.send(KEY_ENTER);
    session.wait_for("Transcript output", Duration::from_secs(3));
    for _ in 0..4 {
        session.send(KEY_DOWN);
    }
    session.wait_for("Save transcript history", Duration::from_secs(3));
    session.send(KEY_RIGHT);
    session.wait_for("[x] history only", Duration::from_secs(3));
    session.send(KEY_ENTER);
    session.wait_for("Transcript output saved.", Duration::from_secs(3));
    session.send(KEY_ESC);
    session.wait_for("> Transcript output", Duration::from_secs(3));
    session.send(KEY_DOWN);
    session.send(KEY_DOWN);
    session.wait_for("> Transcript folder", Duration::from_secs(3));
    session.send(KEY_ENTER);

    let opened = session.wait_for("Folder", Duration::from_secs(3));
    assert!(
        opened.contains("Leave empty to disable")
            && opened.contains("keeping transcripts is on"),
        "the folder screen should explain that empty disables it and that it needs keeping on:\n{opened}"
    );

    // A short absolute path, so the home row it later renders is never clipped.
    let target = "/tmp/tonguetyped-tt-folder";
    session.send(target.as_bytes());
    session.wait_for(target, Duration::from_secs(3));
    session.send(KEY_ENTER);
    let saved = session.wait_for("Transcript folder saved.", Duration::from_secs(3));
    assert!(saved.contains(target), "{saved}");

    let config = sandbox.config_contents();
    assert!(config.contains("enabled = true"), "{config}");
    assert!(
        config.contains(&format!("transcript_folder = \"{target}\"")),
        "{config}"
    );

    session.send(KEY_ESC);
    // Wait on the Home-specific selection marker: the target path is also on
    // the folder screen, so waiting for it alone would race the redraw.
    let home = session.wait_for("> Transcript folder", Duration::from_secs(3));
    assert!(
        home.contains(target),
        "the Home row should show the saved folder:\n{home}"
    );

    session.quit_and_wait();
}

#[test]
fn pty_decoding_setting_persists_the_prompt_and_overrides_and_rejects_bad_numbers() {
    let sandbox = Sandbox::new("decoding-setting");
    let mut session = Session::spawn(&sandbox, 100, 40);
    open_settings(&mut session);
    let home = session.visible_text();
    assert!(
        home.contains("Decoding           model defaults"),
        "an untouched config should read as model defaults:\n{home}"
    );

    select_setting(&mut session, "Decoding");
    session.send(KEY_ENTER);
    let opened = session.wait_for("Vocabulary prompt", Duration::from_secs(3));
    assert!(
        opened.contains("model default (0.2)") && opened.contains("Whisper models only"),
        "empty knobs should show their model defaults:\n{opened}"
    );

    session.send(b"TongueTyped, Wayland");
    session.send(KEY_TAB);
    session.send(b"hot");
    session.send(KEY_ENTER);
    session.wait_for(
        "Temperature must be between 0 and 1, or empty for the model default.",
        Duration::from_secs(3),
    );
    assert!(
        !sandbox.config_contents().contains("TongueTyped, Wayland"),
        "a rejected form must not be saved"
    );

    for _ in 0..3 {
        session.send(KEY_BACKSPACE);
    }
    session.send(b"0.3");
    session.send(KEY_ENTER);
    session.wait_for("Decoding saved.", Duration::from_secs(3));

    let config = sandbox.config_contents();
    assert!(
        config.contains("initial_prompt = \"TongueTyped, Wayland\""),
        "{config}"
    );
    assert!(config.contains("temperature = 0.3"), "{config}");
    assert!(!config.contains("no_speech_threshold"), "{config}");

    session.send(KEY_ESC);
    let home = session.wait_for("> Decoding", Duration::from_secs(3));
    assert!(
        home.contains("vocabulary prompt, 1 override"),
        "the Home row should summarize the saved decoding settings:\n{home}"
    );

    session.quit_and_wait();
}

#[test]
fn pty_translation_setting_persists_the_choice() {
    let sandbox = Sandbox::new("translation-setting");
    let mut session = Session::spawn(&sandbox, 100, 40);
    open_settings(&mut session);

    select_setting(&mut session, "Translation");
    session.send(KEY_ENTER);
    session.wait_for(
        "> Transcribe in the spoken language",
        Duration::from_secs(3),
    );
    session.send(KEY_DOWN);
    session.send(KEY_ENTER);
    session.wait_for("Translation saved.", Duration::from_secs(3));
    assert!(sandbox.config_contents().contains("translate = true"));

    session.send(KEY_ESC);
    let home = session.wait_for("> Translation", Duration::from_secs(3));
    assert!(
        home.contains("Translate speech into English"),
        "the Home row should show the saved choice:\n{home}"
    );

    session.quit_and_wait();
}

#[test]
fn pty_text_processing_screen_toggles_passes_and_persists_replacements() {
    let sandbox = Sandbox::new("text-processing-setting");
    let mut session = Session::spawn(&sandbox, 100, 40);
    open_settings(&mut session);

    select_setting(&mut session, "Text processing");
    session.send(KEY_ENTER);
    let opened = session.wait_for("Spoken punctuation", Duration::from_secs(3));
    assert!(
        opened.contains("Replacements")
            && opened.contains("Spoken punctuation")
            && opened.contains("Remove filler words")
            && opened.contains("Cleanup command"),
        "the screen should list every text-processing setting:\n{opened}"
    );

    // Row 1 (Spoken punctuation): Enter flips it on and saves the form.
    session.send(KEY_DOWN);
    session.send(KEY_ENTER);
    wait_for_config(
        &sandbox,
        "spoken_punctuation = true",
        Duration::from_secs(3),
    );

    // Row 2 (Remove filler words): flip it on too.
    session.send(KEY_DOWN);
    session.send(KEY_ENTER);
    wait_for_config(&sandbox, "remove_fillers = true", Duration::from_secs(3));

    // Row 0 (Replacements): type an entry into the empty field and save.
    session.send(KEY_UP);
    session.send(KEY_UP);
    session.send(b"vox type => Voxtype");
    session.send(KEY_ENTER);
    wait_for_config(
        &sandbox,
        "\"vox type\" = \"Voxtype\"",
        Duration::from_secs(3),
    );

    session.send(KEY_ESC);
    let home = session.wait_for("> Text processing", Duration::from_secs(3));
    assert!(
        home.contains("1 replacement, spoken punctuation, filler removal"),
        "the Home row should summarize the saved settings:\n{home}"
    );
    session.quit_and_wait();
}

#[test]
fn pty_text_processing_screen_round_trips_replacement_separator_and_backslash() {
    let sandbox = Sandbox::new("text-processing-round-trip");
    // A hand-written table whose value contains the form's own separator (`;`)
    // and whose key contains a backslash - the exact shapes the escaped
    // single-line form exists to protect. Applying an unrelated screen save
    // must not silently rewrite them.
    sandbox.write_config("[text.replacements]\n\"a\" = \"x; b => y\"\n\"c\\\\d\" = \"\\\\;\"\n");
    let mut session = Session::spawn(&sandbox, 100, 40);
    open_settings(&mut session);

    select_setting(&mut session, "Text processing");
    session.send(KEY_ENTER);
    session.wait_for("Spoken punctuation", Duration::from_secs(3));

    // Enter on the Replacements row (the default selection) applies and saves
    // the whole form without changing it.
    session.send(KEY_ENTER);
    session.wait_for("Text processing saved.", Duration::from_secs(3));

    let contents = sandbox.config_contents();
    assert!(
        contents.contains("x; b => y"),
        "a value containing ';' must round-trip intact:\n{contents}"
    );
    assert!(
        contents.contains(r"c\d") && contents.contains(r"\;"),
        "a key with a backslash and a value of '\\;' must round-trip intact:\n{contents}"
    );
    session.send(KEY_ESC);
    session.wait_for("> Text processing", Duration::from_secs(3));
    session.quit_and_wait();
}

#[test]
fn pty_text_processing_screen_rejects_invalid_input_without_saving() {
    let sandbox = Sandbox::new("text-processing-invalid");
    sandbox.write_config("");
    let mut session = Session::spawn(&sandbox, 100, 40);
    open_settings(&mut session);

    select_setting(&mut session, "Text processing");
    session.send(KEY_ENTER);
    session.wait_for("Spoken punctuation", Duration::from_secs(3));

    // A replacement entry without the '=>' arrow is rejected on Enter, and
    // nothing is saved.
    session.send(b"no arrow here");
    session.send(KEY_ENTER);
    let error = session.wait_for("Each replacement must be", Duration::from_secs(3));
    assert!(error.contains("from => to"), "{error}");
    assert!(!sandbox.config_contents().contains("no arrow here"));

    // Leaving commits now too, so an invalid form keeps you on the screen with
    // the error instead of silently discarding the input.
    session.send(KEY_ESC);
    let still_open = session.wait_for("Each replacement must be", Duration::from_secs(3));
    assert!(
        still_open.contains("Home \u{203a} Text processing"),
        "an invalid form must keep the screen open on leave:\n{still_open}"
    );
    assert!(!sandbox.config_contents().contains("no arrow here"));

    // Clear the bad replacement: leaving now saves and returns home.
    for _ in 0.."no arrow here".len() {
        session.send(KEY_BACKSPACE);
    }
    session.send(KEY_ESC);
    session.wait_for("> Text processing", Duration::from_secs(3));
    session.send(KEY_ENTER);
    session.wait_for("Spoken punctuation", Duration::from_secs(3));

    // A zero cleanup timeout is rejected as well.
    for _ in 0..4 {
        session.send(KEY_DOWN);
    }
    session.send(KEY_BACKSPACE);
    session.send(KEY_BACKSPACE);
    session.send(b"0");
    session.send(KEY_ENTER);
    let error = session.wait_for("Timeout must be at least 1 second", Duration::from_secs(3));
    assert!(error.contains("Timeout"), "{error}");
    assert!(!sandbox
        .config_contents()
        .contains("post_process_timeout_seconds = 0"));

    // Restore a valid timeout so leaving commits and returns home.
    session.send(KEY_BACKSPACE);
    session.send(b"10");
    session.send(KEY_ESC);
    session.wait_for("> Text processing", Duration::from_secs(3));
    session.quit_and_wait();
}

#[test]
fn pty_history_retention_setting_persists_both_limits() {
    let sandbox = Sandbox::new("retention-setting");
    let mut session = Session::spawn(&sandbox, 100, 40);
    open_settings(&mut session);

    // Retention is only editable while saving transcripts is on. Enable it
    // from the Transcript output screen's history row.
    select_setting(&mut session, "Transcript output");
    session.send(KEY_ENTER);
    session.wait_for("Transcript output", Duration::from_secs(3));
    for _ in 0..4 {
        session.send(KEY_DOWN);
    }
    session.wait_for("Save transcript history", Duration::from_secs(3));
    session.send(KEY_RIGHT);
    session.wait_for("[x] history only", Duration::from_secs(3));
    session.send(KEY_ENTER);
    session.wait_for("Transcript output saved.", Duration::from_secs(3));
    session.send(KEY_ESC);
    session.wait_for("> Transcript output", Duration::from_secs(3));
    session.send(KEY_DOWN);
    session.send(KEY_DOWN);
    session.send(KEY_DOWN);
    session.wait_for("> History retention", Duration::from_secs(3));
    session.send(KEY_ENTER);

    let opened = session.wait_for("Maximum entries", Duration::from_secs(3));
    assert!(
        opened.contains("Maximum age (days)") && opened.contains("30"),
        "the retention screen should start on the current limits:\n{opened}"
    );

    // Clear the prefilled "100" and enter 42, then move to the age field,
    // clear "30", and enter 12.
    for _ in 0..3 {
        session.send(KEY_BACKSPACE);
    }
    session.send(b"42");
    session.wait_for("42", Duration::from_secs(3));
    session.send(KEY_TAB);
    for _ in 0..2 {
        session.send(KEY_BACKSPACE);
    }
    session.send(b"12");
    session.wait_for("12", Duration::from_secs(3));

    session.send(KEY_ENTER);
    session.wait_for("History retention saved.", Duration::from_secs(3));

    let config = sandbox.config_contents();
    assert!(config.contains("max_entries = 42"), "{config}");
    assert!(config.contains("max_age_days = 12"), "{config}");

    session.send(KEY_ESC);
    let home = session.wait_for("42 entries, 12 days", Duration::from_secs(3));
    assert!(
        home.contains("> History retention"),
        "selection should return to the History retention row:\n{home}"
    );

    session.quit_and_wait();
}

#[test]
fn pty_shortcut_screen_offers_the_system_dialog_and_reports_portal_failures_inline() {
    let sandbox = Sandbox::new("shortcut-setting");
    let mut session = Session::spawn(&sandbox, 100, 40);
    open_settings(&mut session);

    select_setting(&mut session, "Shortcut");
    session.send(KEY_ENTER);

    // No app-chosen default: with nothing bound the screen says so and the
    // only way to set a key is the desktop's own dialog.
    session.wait_for("Currently bound: (none set yet)", Duration::from_secs(3));
    let screen = session.wait_for("Set shortcut via system dialog", Duration::from_secs(3));
    assert!(
        !screen.contains("Super+O"),
        "the Shortcut screen must never show an app-chosen default:\n{screen}"
    );

    // Ctrl+R opens the system dialog: the sandbox has no session bus, so the
    // portal failure must surface inline and the screen must stay usable.
    session.send(b"\x12");
    let failed = session.wait_for("Reconfigure failed", Duration::from_secs(10));
    assert!(
        failed.contains("Shortcut"),
        "the failure should render inside the Shortcut panel:\n{failed}"
    );

    // The screen remains navigable afterwards.
    session.send(KEY_ESC);
    open_settings(&mut session);
    session.quit_and_wait();
}

#[test]
fn pty_text_processing_toggle_does_not_stick_after_a_failed_save() {
    let sandbox = Sandbox::new("text-processing-toggle-revert");
    sandbox.write_config("");
    let mut session = Session::spawn(&sandbox, 100, 40);
    open_settings(&mut session);

    select_setting(&mut session, "Text processing");
    session.send(KEY_ENTER);
    session.wait_for("Spoken punctuation", Duration::from_secs(3));

    // Leave an invalid replacement in the form, then press Enter on the Spoken
    // punctuation row: the whole form fails to apply, so the toggle flip must
    // not stick.
    session.send(b"no arrow here");
    session.send(KEY_DOWN);
    session.send(KEY_ENTER);
    let error = session.wait_for("Each replacement must be", Duration::from_secs(3));
    assert!(error.contains("from => to"), "{error}");
    assert!(!sandbox
        .config_contents()
        .contains("spoken_punctuation = true"));

    // Clear the bad replacement, then Enter on the same toggle row again. A
    // stale flip from the failed save would turn spoken punctuation back off
    // here; the correct behavior is that this Enter turns it on and saves it.
    session.send(KEY_UP);
    for _ in 0.."no arrow here".len() {
        session.send(KEY_BACKSPACE);
    }
    session.send(KEY_DOWN);
    session.send(KEY_ENTER);
    wait_for_config(
        &sandbox,
        "spoken_punctuation = true",
        Duration::from_secs(3),
    );

    session.send(KEY_ESC);
    session.wait_for("> Text processing", Duration::from_secs(3));
    session.quit_and_wait();
}

#[test]
fn pty_microphone_screen_lists_devices_with_a_level_panel() {
    let sandbox = Sandbox::new("microphone-setting");
    let mut session = Session::spawn(&sandbox, 100, 40);
    open_settings(&mut session);

    select_setting(&mut session, "Microphone");
    session.send(KEY_ENTER);

    // Whether or not a real capture device opens in this environment, the
    // screen must show the device list and the input-level panel (a live
    // gauge, or the recorder's error message inside that panel).
    let screen = session.wait_for("Input level", Duration::from_secs(10));
    assert!(
        screen.contains("System default microphone"),
        "the microphone list should render:\n{screen}"
    );

    session.send(KEY_ENTER);
    session.wait_for(
        "Saved - used for the next recording.",
        Duration::from_secs(3),
    );

    session.quit_and_wait();
}

#[test]
fn pty_transcript_output_and_typing_backend_screens_apply_and_persist() {
    let sandbox = Sandbox::new("output-setting");
    sandbox.install_typing_helper("wtype");
    let mut session = Session::spawn(&sandbox, 100, 40);
    open_settings(&mut session);

    select_setting(&mut session, "Transcript output");
    session.send(KEY_ENTER);
    let screen = session.wait_for("Type into the focused application", Duration::from_secs(3));
    assert!(
        screen.contains("Do not deliver"),
        "both choices must be listed:\n{screen}"
    );
    assert!(
        !screen.contains("No typing helper found"),
        "a working helper must not warn:\n{screen}"
    );

    session.send(KEY_DOWN);
    session.send(KEY_ENTER);
    session.wait_for("Transcript output saved.", Duration::from_secs(3));
    assert!(sandbox.config_contents().contains("method = \"type\""));

    // Typing backend is the sixth Settings row.
    session.send(KEY_ESC);
    open_settings(&mut session);
    session.send(KEY_DOWN);
    session.wait_for("> Typing backend", Duration::from_secs(3));
    session.send(KEY_ENTER);
    let backend_screen = session.wait_for("> auto", Duration::from_secs(3));
    assert!(
        backend_screen.contains("wtype"),
        "the detected helper must be listed:\n{backend_screen}"
    );
    assert!(
        !backend_screen.contains("No typing helper found"),
        "a detected helper must not warn:\n{backend_screen}"
    );
    session.send(KEY_DOWN);
    session.send(KEY_ENTER);
    session.wait_for("Typing backend saved.", Duration::from_secs(3));
    assert!(sandbox
        .config_contents()
        .contains("typing_backend = \"wtype\""));

    session.quit_and_wait();
}

#[test]
fn pty_transcript_output_configures_paste_and_enter() {
    let sandbox = Sandbox::new("output-paste");
    sandbox.install_typing_helper("wtype");
    sandbox.install_typing_helper("wl-copy");
    let mut session = Session::spawn(&sandbox, 100, 40);
    open_settings(&mut session);
    // Locate the row from `SETTING_LABELS` rather than assuming a fixed
    // offset: the list grows when the base branch adds Settings areas, and
    // this test runs against the merged tree.
    let output_row = SETTING_LABELS
        .iter()
        .position(|label| *label == "Transcript output")
        .expect("Transcript output must be a Settings row");
    for _ in 0..output_row {
        session.send(KEY_DOWN);
    }
    session.wait_for("> Transcript output", Duration::from_secs(3));
    session.send(KEY_ENTER);
    session.wait_for("Paste into the focused application", Duration::from_secs(3));

    // Choosing paste reveals its two options.
    for _ in 0..3 {
        session.send(KEY_DOWN);
    }
    session.wait_for(
        "> Paste into the focused application",
        Duration::from_secs(3),
    );
    session.send(KEY_ENTER);
    let screen = session.wait_for(
        "Paste into the focused application (current)",
        Duration::from_secs(3),
    );
    assert!(screen.contains("[ ] nothing"), "{screen}");
    assert!(
        screen.contains("Ctrl+V (most applications) \u{203a}")
            && screen.contains("1/2")
            && !screen.contains("\u{2039} Ctrl+V"),
        "the paste keys should render as a stepper with its left arrow dimmed at the start:\n{screen}"
    );

    // `→` on the "After text" row flips it; `→` on the paste-keys row steps the
    // key set, and `←` steps it back - both directions, no forward-only cycle.
    session.send(KEY_DOWN);
    session.wait_for(">   After text", Duration::from_secs(3));
    session.send(KEY_RIGHT);
    session.wait_for("[x] press Enter", Duration::from_secs(3));
    session.send(KEY_DOWN);
    session.wait_for(">   Paste keys", Duration::from_secs(3));
    session.send(KEY_RIGHT);
    let screen = session.wait_for("Shift+Insert (terminals)", Duration::from_secs(3));
    assert!(
        screen.contains("Paste into the focused application (current)"),
        "the saved method stays marked while the cursor is on an option:\n{screen}"
    );
    assert!(
        screen.contains("\u{2039} Shift+Insert (terminals)") && screen.contains("2/2"),
        "at the last paste-key value the counter should read 2/2:\n{screen}"
    );
    assert!(
        !screen.contains("Shift+Insert (terminals) \u{203a}"),
        "at the last paste-key value the right arrow should be dimmed:\n{screen}"
    );
    session.send(KEY_LEFT);
    session.wait_for("Ctrl+V (most applications)", Duration::from_secs(3));
    session.send(KEY_RIGHT);
    session.wait_for("Shift+Insert (terminals)", Duration::from_secs(3));

    // Enter on an option row only saves; it must not cycle the value, which
    // would wrap Shift+Insert back to Ctrl+V.
    session.send(KEY_ENTER);
    session.wait_for("Shift+Insert (terminals)", Duration::from_secs(3));

    // Stepping right at the last value clamps instead of wrapping to the
    // first: backing out must keep Shift+Insert, not revert to Ctrl+V.
    session.send(KEY_RIGHT);

    // Leaving commits the option rows; the Settings panel summarizes the choice.
    session.send(KEY_ESC);
    session.wait_for(
        "paste into the focused application with Shift+Insert, then Enter",
        Duration::from_secs(3),
    );
    let config = sandbox.config_contents();
    assert!(config.contains("method = \"paste\""), "{config}");
    assert!(config.contains("auto_submit = true"), "{config}");
    assert!(config.contains("paste_keys = \"shift-insert\""), "{config}");

    session.quit_and_wait();
}

#[test]
fn pty_transcript_output_explains_a_missing_helper_and_refuses_typing() {
    // The sandbox PATH has no typing helper and the session is pinned to a
    // Wayland compositor that is neither wlroots, KDE, nor GNOME (weston), so
    // no backend can type: this is the exact "no helper installed" state the
    // warning exists for, and it must name dotool (wtype cannot work without
    // the wlroots virtual-keyboard protocol and the native libei backend is
    // KDE/GNOME only).
    let sandbox = Sandbox::on("output-no-helper", "weston");
    let mut session = Session::spawn(&sandbox, 100, 40);
    open_settings(&mut session);

    select_setting(&mut session, "Transcript output");
    session.send(KEY_ENTER);

    // The type choice stays listed, and highlighting it shows the warning
    // naming what to install.
    let screen = session.wait_for("Type into the focused application", Duration::from_secs(3));
    assert!(
        screen.contains("Do not deliver"),
        "the type choice must not be dropped silently:\n{screen}"
    );
    session.send(KEY_DOWN);
    let screen = session.wait_for(
        "> Type into the focused application",
        Duration::from_secs(3),
    );
    assert!(
        screen.contains("install dotool"),
        "the screen must say what to install:\n{screen}"
    );

    // Choosing it is refused with an explanation, nothing is saved, and the
    // install warning stays visible above the refusal.
    session.send(KEY_ENTER);
    let refused = session.wait_for("Cannot apply", Duration::from_secs(3));
    assert!(
        refused.contains("install dotool"),
        "the install warning must stay visible next to the refusal:\n{refused}"
    );
    assert!(
        !refused.contains("Transcript output saved."),
        "a refused choice must not report a save:\n{refused}"
    );

    // Keep still applies and persists normally.
    session.send(KEY_UP);
    session.send(KEY_ENTER);
    session.wait_for("Transcript output saved.", Duration::from_secs(3));
    assert!(sandbox.config_contents().contains("method = \"none\""));

    // The Typing backend screen explains the empty helper list too.
    session.send(KEY_ESC);
    open_settings(&mut session);
    session.send(KEY_DOWN);
    session.wait_for("> Typing backend", Duration::from_secs(3));
    session.send(KEY_ENTER);
    let backend_screen = session.wait_for("> auto", Duration::from_secs(3));
    assert!(
        backend_screen.contains("No typing helper found"),
        "the empty helper list must be explained:\n{backend_screen}"
    );
    session.send(KEY_ENTER);
    session.wait_for("Typing backend saved.", Duration::from_secs(3));

    session.quit_and_wait();
}

#[test]
fn pty_startup_setting_reuses_the_existing_autostart_toggle() {
    let sandbox = Sandbox::new("startup-setting");
    let mut session = Session::spawn(&sandbox, 100, 40);
    open_settings(&mut session);

    select_setting(&mut session, "Startup");
    session.send(KEY_ENTER);

    session.wait_for("> Start manually", Duration::from_secs(3));
    session.send(KEY_DOWN);
    session.send(KEY_ENTER);
    session.wait_for("Startup setting saved.", Duration::from_secs(3));
    assert!(sandbox
        .config_home
        .join("autostart/tonguetyped.desktop")
        .exists());
    assert!(sandbox.config_contents().contains("autostart = true"));

    session.quit_and_wait();
}

#[test]
fn pty_inference_backend_setting_saves_and_reloads_like_the_model_screen() {
    // The dedicated "Inference backend" Settings row reuses the Model screen's
    // backend list and its activate-and-confirm flow: choosing a backend saves
    // it, reloads a running daemon if there is one, and reports the outcome.
    let sandbox = Sandbox::new("inference-backend-setting");
    let mut session = Session::spawn(&sandbox, 100, 40);
    open_settings(&mut session);
    let home = session.visible_text();
    assert!(
        home.contains("Inference backend  auto"),
        "the row should show the configured preference:\n{home}"
    );

    // "Inference backend" is the second Settings row, right after Model.
    session.send(KEY_DOWN);
    session.wait_for("> Inference backend", Duration::from_secs(3));
    session.send(KEY_ENTER);

    let opened = session.wait_for("Auto (tries CUDA", Duration::from_secs(5));
    assert!(
        opened.contains("> Auto (tries CUDA"),
        "the current preference should be selected:\n{opened}"
    );
    assert!(
        opened.contains("CPU"),
        "the always-available CPU backend must be listed:\n{opened}"
    );

    // Move from "auto" onto the always-available "cpu" and apply: the same
    // activate-and-confirm flow as the Model screen's backend panel saves it
    // and reports the outcome (no daemon running here, so it applies on next
    // start rather than reloading).
    session.send(KEY_DOWN);
    session.wait_for("> CPU", Duration::from_secs(3));
    session.send(KEY_ENTER);

    let outcome = session.wait_for("daemon: not running", Duration::from_secs(20));
    assert!(
        outcome.contains("Backend change"),
        "expected the backend change result title:\n{outcome}"
    );
    assert!(
        outcome.contains("backend pref:   cpu"),
        "diagnostics should report the pinned backend:\n{outcome}"
    );
    assert!(sandbox
        .config_contents()
        .contains("preferred_backend = \"cpu\""));

    // Returning home shows the new value and keeps the row selected.
    session.send(KEY_ESC);
    let home = session.wait_for("> Inference backend", Duration::from_secs(3));
    assert!(
        home.contains("Inference backend  cpu"),
        "the Home row should show the saved preference:\n{home}"
    );

    session.quit_and_wait();
}

#[test]
fn pty_model_screen_saves_a_pinned_inference_backend_and_reports_it() {
    // Same stub-model trick as the activation test above: pinning the
    // always-available CPU backend must save the setting, then report that
    // the stub still can't be loaded on it rather than claiming success.
    let sandbox = Sandbox::new("backend-selection");
    let mut session = Session::spawn(&sandbox, 100, 40);
    open_settings(&mut session);

    // "Model" is the first Settings row and is already selected.
    session.wait_for("> Model", Duration::from_secs(3));
    session.send(KEY_ENTER);

    // Wait on the catalog title, not "Inference backend": that string is now
    // also the home Settings row's label, so waiting on it would race the
    // screen opening.
    let screen = session.wait_for("MODEL ID", Duration::from_secs(5));
    assert!(
        screen.contains("Auto (tries CUDA"),
        "backend panel should list the auto choice:\n{screen}"
    );

    // Tab moves the selection from the catalog to the backend list, where
    // only the usable backends are selectable: auto, then cpu.
    session.send(KEY_TAB);
    session.send(KEY_DOWN);
    session.wait_for("> CPU", Duration::from_secs(3));
    session.send(KEY_ENTER);

    let outcome = session.wait_for("NOT confirmed", Duration::from_secs(20));
    assert!(
        outcome.contains("Backend change"),
        "expected the backend change result title:\n{outcome}"
    );
    assert!(
        outcome.contains("backend pref:   cpu"),
        "diagnostics should report the pinned backend:\n{outcome}"
    );
    let config = std::fs::read_to_string(sandbox.config_home.join("tonguetyped/config.toml"))
        .expect("config saved");
    assert!(
        config.contains("preferred_backend = \"cpu\""),
        "config should pin the CPU backend:\n{config}"
    );

    session.quit_and_wait();
}

/// One repeated phrase long enough that a handful of copies overflows a line
/// in a 100-column pane; the `ZZZTAILZZZ` sentinel marks the very end.
const TRANSCRIPT_PHRASE: &str = "alpha bravo charlie delta echo foxtrot golf hotel india juliet ";

/// Seeds a long single-line transcript into the daemon's history DB, starts a
/// daemon against the sandbox, and drives the dashboard to its `last-result`
/// Info screen - the real dashboard -> daemon -> last-result path with no
/// recording. The sandbox and daemon are returned so they outlive the session
/// for the test's duration.
///
/// The tag must stay short: PTY tests that start a daemon need a Unix-domain
/// socket path under SUN_LEN (~108 bytes), which the longer human-readable
/// tags the other sandboxes use would exceed under Nix's already-long TMPDIR.
fn open_last_result(tag: &str, transcript: &str) -> (Sandbox, Daemon, Session) {
    let sandbox = Sandbox::new(tag);
    sandbox.write_config(
        "[audio]\nfeedback_sounds = false\n[transcription]\nvad_enabled = false\n\
         [history]\nenabled = true\n",
    );
    sandbox.seed_history(transcript);
    let daemon = start_daemon(&sandbox);

    let mut session = Session::spawn(&sandbox, 100, 32);
    open_settings(&mut session);
    select_command(&mut session, "last-result");
    session.send(KEY_ENTER);
    session.wait_for("Last result", Duration::from_secs(10));
    (sandbox, daemon, session)
}

#[test]
fn pty_last_result_wraps_a_long_transcript_instead_of_truncating_it() {
    // A real transcript is a single long line. The Info pane must wrap it so
    // its tail stays on screen instead of being clipped at the pane edge - the
    // exact regression.
    let transcript = format!("{}ZZZTAILZZZ", TRANSCRIPT_PHRASE.repeat(4));
    assert!(
        transcript.chars().count() > 200,
        "the fixture must be a genuinely long single line"
    );
    let (_sandbox, _daemon, session) = open_last_result("lr", &transcript);

    // The tail is only visible if the line wrapped rather than being clipped.
    let wrapped = session.wait_for("ZZZTAILZZZ", Duration::from_secs(5));
    assert!(
        wrapped.contains("result:   alpha bravo charlie"),
        "the head of the wrapped transcript should stay readable:\n{wrapped}"
    );
    assert!(
        wrapped.contains("juliet ZZZTAILZZZ"),
        "the wrapped tail should stay readable:\n{wrapped}"
    );

    session.quit_and_wait();
}

#[test]
fn pty_last_result_can_be_scrolled_to_text_taller_than_the_pane() {
    // A transcript that wraps to more rows than the pane is tall must remain
    // reachable by scrolling, not just wrapped off the bottom.
    let transcript = format!("{}ZZZTAILZZZ", TRANSCRIPT_PHRASE.repeat(80));
    let (_sandbox, _daemon, mut session) = open_last_result("lrs", &transcript);

    let initial = session.visible_text();
    assert!(
        initial.contains("result:   alpha bravo charlie"),
        "the top of the transcript should be on screen first:\n{initial}"
    );
    assert!(
        !initial.contains("ZZZTAILZZZ"),
        "the tail should start below the fold:\n{initial}"
    );

    for _ in 0..40 {
        session.send(KEY_PAGE_DOWN);
    }
    let scrolled = session.wait_for("ZZZTAILZZZ", Duration::from_secs(5));
    assert!(
        scrolled.contains("juliet ZZZTAILZZZ"),
        "scrolling should bring the tail into view:\n{scrolled}"
    );

    session.quit_and_wait();
}

#[test]
fn pty_media_pause_setting_defaults_off_and_persists_when_enabled() {
    let sandbox = Sandbox::new("media-pause-setting");
    let mut session = Session::spawn(&sandbox, 100, 40);
    open_settings(&mut session);
    let home = session.visible_text();
    assert!(
        home.contains("Media pause        keep playing"),
        "media pause should default to off:\n{home}"
    );

    for _ in 0..setting_row("Media pause") {
        session.send(KEY_DOWN);
    }
    session.wait_for("> Media pause", Duration::from_secs(3));
    session.send(KEY_ENTER);
    let opened = session.wait_for(
        "> Keep media playing while dictating",
        Duration::from_secs(3),
    );
    assert!(
        opened.contains("Pause playing media while dictating, then resume it"),
        "the Media pause screen should offer both choices:\n{opened}"
    );

    session.send(KEY_DOWN);
    session.send(KEY_ENTER);
    session.wait_for("Media pause setting saved.", Duration::from_secs(3));
    assert!(sandbox.config_contents().contains("pause_media = true"));

    session.send(KEY_ESC);
    let home = session.wait_for(
        "Media pause        pause while dictating",
        Duration::from_secs(3),
    );
    assert!(
        home.contains("> Media pause"),
        "selection should return to the Media pause row:\n{home}"
    );

    session.quit_and_wait();
}
