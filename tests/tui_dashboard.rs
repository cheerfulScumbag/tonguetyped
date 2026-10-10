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
}

impl Sandbox {
    fn new(tag: &str) -> Self {
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
        cmd.env("XDG_CURRENT_DESKTOP", "KDE");
        cmd.env("XDG_SESSION_DESKTOP", "KDE");
        cmd.env("DESKTOP_SESSION", "KDE");
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
        let parser = Arc::new(Mutex::new(vt100::Parser::new(rows, cols, 0)));
        let parser_for_thread = parser.clone();
        std::thread::spawn(move || {
            let mut chunk = [0u8; 4096];
            loop {
                match reader.read(&mut chunk) {
                    Ok(0) | Err(_) => break,
                    Ok(n) => parser_for_thread.lock().unwrap().process(&chunk[..n]),
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
const KEY_ENTER: &[u8] = b"\r";
const KEY_ESC: &[u8] = b"\x1b";
const KEY_TAB: &[u8] = b"\t";
const KEY_BACKSPACE: &[u8] = b"\x7f";
const KEY_PAGE_DOWN: &[u8] = b"\x1b[6~";

const SETTING_LABELS: [&str; 10] = [
    "Model",
    "Microphone",
    "Activation",
    "Shortcut",
    "Transcript output",
    "Typing backend",
    "Transcript folder",
    "History retention",
    "Startup",
    "Overlay",
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

/// Home index of the first command row (after the ten Settings rows), for
/// tests that need to arrow down to a specific command.
const FIRST_COMMAND_ROW: usize = SETTING_LABELS.len();

#[test]
fn pty_bare_invocation_opens_the_dashboard_with_settings_and_every_command_on_a_normal_terminal() {
    let sandbox = Sandbox::new("home");
    // Tall enough for the ten Settings rows plus every Commands row with no
    // scrolling of either panel.
    let session = Session::spawn(&sandbox, 100, 34);

    // The pre-change baseline (see .superdesign/replica_html_template and the
    // dashboard task's report) listed 11 CLI commands with model/autostart
    // among them. The signed-off settings-menu redesign turns the home screen
    // into two stacked panels with one selection cursor: ten Settings rows
    // with current values (model and autostart included) above the remaining
    // Commands. All of it must fit one normal terminal, no pagination.
    let screen = session.wait_for("Settings", Duration::from_secs(5));
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
    for name in COMMAND_NAMES {
        assert!(
            screen.contains(name),
            "home screen is missing command {name:?}; got:\n{screen}"
        );
    }
    assert!(
        !screen.contains("start          Start a new recording"),
        "start/stop stay hidden from the Commands panel:\n{screen}"
    );
    // The Settings panel shows real current values, not placeholders.
    assert!(
        screen.contains("Model              whisper-small-q5_k_m"),
        "the Model row should show the active catalog model:\n{screen}"
    );
    assert!(
        screen.contains("Overlay            enabled, top-right, badge, simple pulse"),
        "the Overlay row should summarize the real config:\n{screen}"
    );
    assert!(
        screen.contains("Daemon:"),
        "missing status strip:\n{screen}"
    );

    session.quit_and_wait();
}

#[test]
fn pty_home_selection_crosses_from_the_settings_panel_into_the_commands_panel() {
    let sandbox = Sandbox::new("cross-panel-nav");
    let mut session = Session::spawn(&sandbox, 100, 40);
    session.wait_for("Settings", Duration::from_secs(5));

    // "Model" (first settings row) is selected by default.
    let initial = session.visible_text();
    assert!(
        initial.contains("> Model"),
        "expected Model selected:\n{initial}"
    );

    // Walking down past all ten settings rows lands on "setup", the first
    // command row - one cursor spans both panels.
    for _ in 0..FIRST_COMMAND_ROW {
        session.send(KEY_DOWN);
    }
    let commands_selected = session.wait_for("> setup", Duration::from_secs(3));
    assert!(
        !commands_selected.contains("> Model"),
        "only one row should carry the selection marker:\n{commands_selected}"
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
fn pty_arrow_keys_move_the_home_selection_marker_and_escape_is_a_no_op_on_the_home_screen() {
    let sandbox = Sandbox::new("nav");
    let mut session = Session::spawn(&sandbox, 100, 32);
    session.wait_for("Settings", Duration::from_secs(5));

    // "Model" (first settings row) is selected by default.
    let initial = session.visible_text();
    assert!(
        initial.contains("> Model"),
        "expected Model selected:\n{initial}"
    );

    session.send(KEY_DOWN);
    session.send(KEY_DOWN);
    let after_down = session.wait_for("> Activation", Duration::from_secs(3));
    assert!(
        !after_down.contains("> Model") && !after_down.contains("> Microphone"),
        "only one row should carry the selection marker:\n{after_down}"
    );

    session.send(KEY_UP);
    session.wait_for("> Microphone", Duration::from_secs(3));

    // Escape on the home screen must not quit or navigate anywhere.
    session.send(KEY_ESC);
    std::thread::sleep(Duration::from_millis(300));
    assert!(
        session.child.try_wait().unwrap().is_none(),
        "Esc must not quit the home screen"
    );
    let still_home = session.visible_text();
    assert!(
        still_home.contains("> Microphone"),
        "Esc must be a no-op on home:\n{still_home}"
    );

    session.quit_and_wait();
}

#[test]
fn pty_logo_recenters_for_wide_and_narrow_terminals_without_clipping() {
    let sandbox = Sandbox::new("resize");
    let session = Session::spawn(&sandbox, 140, 32);
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
fn pty_selecting_an_ipc_action_without_a_running_daemon_shows_an_actionable_error() {
    let sandbox = Sandbox::new("action-feedback");
    let mut session = Session::spawn(&sandbox, 100, 32);
    session.wait_for("Settings", Duration::from_secs(5));

    // Navigate to "status" (the fifth Commands row, after the Settings rows:
    // setup, daemon, toggle, cancel, status) and select it. No daemon
    // is running in this sandbox, so the dashboard must show a visible,
    // actionable error - not hang, not crash, not silently do nothing.
    for _ in 0..FIRST_COMMAND_ROW + 4 {
        session.send(KEY_DOWN);
    }
    session.send(KEY_ENTER);

    let result = session.wait_for("daemon is not running", Duration::from_secs(5));
    assert!(
        result.contains("Status"),
        "expected the Status action's title:\n{result}"
    );

    // Esc/Enter return to Home from the result screen (the selection stays
    // on "status", the item that was just activated).
    session.send(KEY_ESC);
    let back_home = session.wait_for("> status", Duration::from_secs(3));
    assert!(
        !back_home.contains("daemon is not running"),
        "returning to Home should clear the prior result screen:\n{back_home}"
    );

    session.quit_and_wait();
}

#[test]
fn pty_daemon_home_item_opens_a_start_stop_restart_screen_instead_of_starting_immediately() {
    let sandbox = Sandbox::new("daemon-screen");
    let mut session = Session::spawn(&sandbox, 100, 32);
    session.wait_for("Settings", Duration::from_secs(5));

    // "daemon" is the second Commands row, right after "setup". Selecting it
    // must open a sub-screen offering Start/Stop/Restart, not immediately
    // launch the daemon the way the old single-action binding did.
    for _ in 0..FIRST_COMMAND_ROW + 1 {
        session.send(KEY_DOWN);
    }
    session.wait_for("> daemon", Duration::from_secs(3));
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
    session.wait_for("> daemon", Duration::from_secs(3));

    // Reopen the daemon screen (selection resets to "Start"), move down once
    // to select "Stop", and run it. No daemon is running in this sandbox, so
    // this must surface the same actionable error the CLI's `daemon stop`
    // would, not hang or silently do nothing.
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
    session.wait_for("> daemon", Duration::from_secs(3));

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
    session.wait_for("Settings", Duration::from_secs(5));

    // "Model" is the first Settings row and is already selected.
    session.wait_for("> Model", Duration::from_secs(3));
    session.send(KEY_ENTER);

    let catalog_screen = session.wait_for("Model catalog", Duration::from_secs(5));
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
    session.wait_for("Settings", Duration::from_secs(5));

    // Activation is the third Settings row (Model, Microphone, Activation).
    session.send(KEY_DOWN);
    session.send(KEY_DOWN);
    session.wait_for("> Activation", Duration::from_secs(3));
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
    session.wait_for("Settings", Duration::from_secs(3));
    session.send(KEY_ENTER);
    session.wait_for(
        "> Press once to start and again to stop",
        Duration::from_secs(3),
    );
    assert!(sandbox.config_contents().contains("mode = \"toggle\""));

    session.quit_and_wait();
}

#[test]
fn pty_overlay_setting_cycles_position_and_persists_it() {
    let sandbox = Sandbox::new("overlay-setting");
    let mut session = Session::spawn(&sandbox, 100, 40);
    session.wait_for("Settings", Duration::from_secs(5));

    // Overlay is the tenth and last Settings row.
    for _ in 0..SETTING_LABELS.len() - 1 {
        session.send(KEY_DOWN);
    }
    session.wait_for("> Overlay", Duration::from_secs(3));
    session.send(KEY_ENTER);

    let opened = session.wait_for("Position    top-right", Duration::from_secs(3));
    assert!(
        opened.contains("Style       Badge") && opened.contains("Streaming   Simple pulse"),
        "the Overlay screen should show all current values:\n{opened}"
    );
    assert!(
        opened.contains("> Enabled"),
        "the current enable state should be selected:\n{opened}"
    );

    // Selection starts on "Enabled"; one Down selects Position, Enter cycles
    // it to the next configured value.
    session.send(KEY_DOWN);
    session.send(KEY_ENTER);
    let cycled = session.wait_for("Position    center", Duration::from_secs(3));
    assert!(
        cycled.contains("Overlay saved."),
        "cycling a value should report the save:\n{cycled}"
    );
    assert!(sandbox.config_contents().contains("position = \"center\""));

    // Home's summary row reflects the new value.
    session.send(KEY_ESC);
    let home = session.wait_for(
        "enabled, center, badge, simple pulse",
        Duration::from_secs(3),
    );
    assert!(
        home.contains("> Overlay"),
        "selection should return to the Overlay row:\n{home}"
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

    let home = session.wait_for("Overlay            disabled", Duration::from_secs(5));
    assert!(
        home.contains("Overlay            disabled"),
        "the summary row should reflect the disabled overlay:\n{home}"
    );
    for _ in 0..SETTING_LABELS.len() - 1 {
        session.send(KEY_DOWN);
    }
    session.wait_for("> Overlay", Duration::from_secs(3));
    session.send(KEY_ENTER);

    // Disabled state: only the two enable rows, no configuration sub-rows.
    let disabled = session.wait_for("> Disabled", Duration::from_secs(3));
    assert!(
        !disabled.contains("Position") && !disabled.contains("Streaming"),
        "a disabled overlay must not show position/style/streaming:\n{disabled}"
    );

    // Move to Enabled and apply: the three configuration rows appear.
    session.send(KEY_DOWN);
    session.send(KEY_ENTER);
    let enabled = session.wait_for("Position", Duration::from_secs(3));
    assert!(
        enabled.contains("Overlay saved.") && enabled.contains("> Enabled"),
        "enabling should save and keep the row selected:\n{enabled}"
    );
    assert!(sandbox.config_contents().contains("enabled = true"));

    session.quit_and_wait();
}

#[test]
fn pty_transcript_folder_setting_persists_an_absolute_path() {
    let sandbox = Sandbox::new("folder-setting");
    let mut session = Session::spawn(&sandbox, 100, 40);
    session.wait_for("Settings", Duration::from_secs(5));

    // Transcript folder is the seventh Settings row.
    for _ in 0..6 {
        session.send(KEY_DOWN);
    }
    session.wait_for("> Transcript folder", Duration::from_secs(3));
    session.send(KEY_ENTER);

    let opened = session.wait_for("Folder", Duration::from_secs(3));
    assert!(
        opened.contains("Leave empty to disable"),
        "the folder screen should explain that empty disables it:\n{opened}"
    );

    // A short absolute path, so the home row it later renders is never clipped.
    let target = "/tmp/tonguetyped-tt-folder";
    session.send(target.as_bytes());
    session.wait_for(target, Duration::from_secs(3));
    session.send(KEY_ENTER);
    let saved = session.wait_for("Transcript folder saved.", Duration::from_secs(3));
    assert!(saved.contains(target), "{saved}");

    let config = sandbox.config_contents();
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
fn pty_history_retention_setting_persists_both_limits() {
    let sandbox = Sandbox::new("retention-setting");
    let mut session = Session::spawn(&sandbox, 100, 40);
    session.wait_for("Settings", Duration::from_secs(5));

    // History retention is the eighth Settings row.
    for _ in 0..7 {
        session.send(KEY_DOWN);
    }
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
    session.wait_for("Settings", Duration::from_secs(5));

    // Shortcut is the fourth Settings row.
    for _ in 0..3 {
        session.send(KEY_DOWN);
    }
    session.wait_for("> Shortcut", Duration::from_secs(3));
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
    session.wait_for("Settings", Duration::from_secs(3));
    session.quit_and_wait();
}

#[test]
fn pty_microphone_screen_lists_devices_with_a_level_panel() {
    let sandbox = Sandbox::new("microphone-setting");
    let mut session = Session::spawn(&sandbox, 100, 40);
    session.wait_for("Settings", Duration::from_secs(5));

    // Microphone is the second Settings row.
    session.send(KEY_DOWN);
    session.wait_for("> Microphone", Duration::from_secs(3));
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
    session.wait_for("Settings", Duration::from_secs(5));

    // Transcript output is the fifth Settings row.
    for _ in 0..4 {
        session.send(KEY_DOWN);
    }
    session.wait_for("> Transcript output", Duration::from_secs(3));
    session.send(KEY_ENTER);
    let screen = session.wait_for("Type into the focused application", Duration::from_secs(3));
    assert!(
        screen.contains("Keep transcripts in TongueTyped"),
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
    session.wait_for("Settings", Duration::from_secs(3));
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
fn pty_transcript_output_explains_a_missing_helper_and_refuses_typing() {
    // The sandbox PATH has no typing helper and the session is pinned to
    // Wayland + KDE, so this is the exact "no helper installed" state the
    // warning exists for, and it must name dotool (wtype cannot work on KWin).
    let sandbox = Sandbox::new("output-no-helper");
    let mut session = Session::spawn(&sandbox, 100, 40);
    session.wait_for("Settings", Duration::from_secs(5));

    // Transcript output is the fifth Settings row.
    for _ in 0..4 {
        session.send(KEY_DOWN);
    }
    session.wait_for("> Transcript output", Duration::from_secs(3));
    session.send(KEY_ENTER);

    // The type choice stays listed and the warning names what to install.
    let screen = session.wait_for("Type into the focused application", Duration::from_secs(3));
    assert!(
        screen.contains("Keep transcripts in TongueTyped"),
        "the type choice must not be dropped silently:\n{screen}"
    );
    assert!(
        screen.contains("install dotool"),
        "the screen must say what to install:\n{screen}"
    );

    // Choosing it is refused with an explanation, nothing is saved, and the
    // install warning stays visible above the refusal.
    session.send(KEY_DOWN);
    session.wait_for(
        "> Type into the focused application",
        Duration::from_secs(3),
    );
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
    session.wait_for("Settings", Duration::from_secs(3));
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
    session.wait_for("Settings", Duration::from_secs(5));

    // Startup is the ninth Settings row.
    for _ in 0..8 {
        session.send(KEY_DOWN);
    }
    session.wait_for("> Startup", Duration::from_secs(3));
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
fn pty_model_screen_saves_a_pinned_inference_backend_and_reports_it() {
    // Same stub-model trick as the activation test above: pinning the
    // always-available CPU backend must save the setting, then report that
    // the stub still can't be loaded on it rather than claiming success.
    let sandbox = Sandbox::new("backend-selection");
    let mut session = Session::spawn(&sandbox, 100, 40);
    session.wait_for("Settings", Duration::from_secs(5));

    // "Model" is the first Settings row and is already selected.
    session.wait_for("> Model", Duration::from_secs(3));
    session.send(KEY_ENTER);

    let screen = session.wait_for("Inference backend", Duration::from_secs(5));
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
/// The tag must stay short: this is the only PTY test that starts a daemon, and
/// a Unix-domain socket path must fit under SUN_LEN (~108 bytes), which the
/// longer human-readable tags the other sandboxes use would exceed under Nix's
/// already-long TMPDIR.
fn open_last_result(tag: &str, transcript: &str) -> (Sandbox, Daemon, Session) {
    let sandbox = Sandbox::new(tag);
    sandbox
        .write_config("[audio]\nfeedback_sounds = false\n[transcription]\nvad_enabled = false\n");
    sandbox.seed_history(transcript);
    let daemon = start_daemon(&sandbox);

    let mut session = Session::spawn(&sandbox, 100, 32);
    session.wait_for("Settings", Duration::from_secs(5));
    // "last-result" is the seventh Commands row.
    for _ in 0..FIRST_COMMAND_ROW + 6 {
        session.send(KEY_DOWN);
    }
    session.wait_for("> last-result", Duration::from_secs(3));
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
