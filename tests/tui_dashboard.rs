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
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

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
}

impl Sandbox {
    fn new(tag: &str) -> Self {
        let root = sandbox_root(tag);
        let config_home = root.join("config");
        let data_home = root.join("data");
        let runtime_dir = root.join("runtime");
        std::fs::create_dir_all(config_home.join("tonguetyped")).unwrap();
        std::fs::create_dir_all(data_home.join("tonguetyped/models")).unwrap();
        std::fs::create_dir_all(&runtime_dir).unwrap();
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
        }
    }

    fn apply_env(&self, cmd: &mut CommandBuilder) {
        cmd.env("XDG_CONFIG_HOME", &self.config_home);
        cmd.env("XDG_DATA_HOME", &self.data_home);
        cmd.env("XDG_RUNTIME_DIR", &self.runtime_dir);
        cmd.env("NO_COLOR", "1");
        cmd.env("DBUS_SESSION_BUS_ADDRESS", "unix:path=/nonexistent");
    }
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

const ALL_COMMAND_NAMES: [&str; 13] = [
    "setup",
    "daemon",
    "start",
    "stop",
    "toggle",
    "cancel",
    "status",
    "reload",
    "last-result",
    "doctor",
    "shortcut-test",
    "autostart",
    "model",
];

#[test]
fn pty_bare_invocation_opens_the_dashboard_with_full_command_coverage_on_a_normal_terminal() {
    let sandbox = Sandbox::new("home");
    let session = Session::spawn(&sandbox, 100, 32);

    // The pre-change baseline (see .superdesign/replica_html_template and
    // this task's report) was a clap "missing subcommand" usage error on
    // stderr with exit code 2 - bare invocation must now instead render the
    // dashboard: the centered logo and every one of the 13 real commands on
    // one screen, no pagination.
    let screen = session.wait_for("Commands", Duration::from_secs(5));
    assert!(
        screen.contains("████████╗"),
        "a 100-column terminal should fit the full block-art logo:\n{screen}"
    );
    for name in ALL_COMMAND_NAMES {
        assert!(
            screen.contains(name),
            "home screen is missing command {name:?}; got:\n{screen}"
        );
    }
    assert!(
        screen.contains("Daemon:"),
        "missing status strip:\n{screen}"
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
    session.wait_for("Commands", Duration::from_secs(5));

    // "setup" (first item) is selected by default.
    let initial = session.visible_text();
    assert!(
        initial.contains("> setup"),
        "expected setup selected:\n{initial}"
    );

    session.send(KEY_DOWN);
    session.send(KEY_DOWN);
    let after_down = session.wait_for("> start", Duration::from_secs(3));
    assert!(
        !after_down.contains("> setup") && !after_down.contains("> daemon"),
        "only one row should carry the selection marker:\n{after_down}"
    );

    session.send(KEY_UP);
    session.wait_for("> daemon", Duration::from_secs(3));

    // Escape on the home screen must not quit or navigate anywhere.
    session.send(KEY_ESC);
    std::thread::sleep(Duration::from_millis(300));
    assert!(
        session.child.try_wait().unwrap().is_none(),
        "Esc must not quit the home screen"
    );
    let still_home = session.visible_text();
    assert!(
        still_home.contains("> daemon"),
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
    session.wait_for("Commands", Duration::from_secs(5));

    // Navigate to "status" (index 6: setup, daemon, start, stop, toggle,
    // cancel, status) and select it. No daemon is running in this sandbox,
    // so the dashboard must show a visible, actionable error - not hang,
    // not crash, not silently do nothing.
    for _ in 0..6 {
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
fn pty_model_screen_lists_the_catalog_and_activation_reports_confirmed_failure_for_a_fake_file() {
    // The sandbox's pre-seeded model file is an empty stub, not a real GGUF,
    // so activation's own confirmation step (re-running diagnostics) must
    // fail to load it - this exercises the full "download/verify, save,
    // reload, confirm, and report failure without claiming activation" flow
    // deterministically and offline, through the real TUI, rather than
    // requiring a multi-gigabyte network download in a test.
    let sandbox = Sandbox::new("model-activation");
    let mut session = Session::spawn(&sandbox, 100, 40);
    session.wait_for("Commands", Duration::from_secs(5));

    for _ in 0..12 {
        session.send(KEY_DOWN);
    }
    session.wait_for("> model", Duration::from_secs(3));
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
