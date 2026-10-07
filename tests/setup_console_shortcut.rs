//! Real-pseudo-terminal behavior tests for the Shortcut step's Ctrl+R
//! "reconfigure via the system dialog" addition (`src/setup/console.rs`).
//! Same `Sandbox`/`Session`/`vt100::Parser` pattern as
//! `tests/setup_console_overlay.rs` - drives the actual compiled binary
//! through a PTY, never the console's internal functions directly.
//!
//! `DBUS_SESSION_BUS_ADDRESS` is pointed at a nonexistent socket, the same
//! isolation `tests/daemon_startup.rs`'s shortcut-test coverage already
//! relies on: `activation::reconfigure_shortcut` then fails immediately
//! (no real portal to call), which is exactly what lets this test observe
//! the new error-reporting UI live without ever touching a real desktop's
//! global-shortcut registrations.
//!
//! Named with a `pty_` prefix like `tests/tui_dashboard.rs` so
//! `flake.nix`'s `cargoTestFlags = ["--" "--skip" "pty_"]` continues to
//! skip these in the Nix sandboxed build (no usable pty/tty subsystem
//! there); `nix develop -c cargo test` and plain `cargo test` run them
//! normally.

use portable_pty::{native_pty_system, Child, CommandBuilder, MasterPty, PtySize};
use std::io::{Read, Write};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

struct Sandbox {
    root: std::path::PathBuf,
    config_home: std::path::PathBuf,
    data_home: std::path::PathBuf,
    runtime_dir: std::path::PathBuf,
}

impl Sandbox {
    fn new(tag: &str) -> Self {
        let root = std::env::temp_dir().join(format!(
            "tt-setup-console-shortcut-{tag}-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
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

struct Session {
    #[allow(dead_code)]
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
        cmd.arg("setup");
        sandbox.apply_env(&mut cmd);
        let child = pair.slave.spawn_command(cmd).expect("spawn setup console");
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
}

impl Drop for Session {
    fn drop(&mut self) {
        let _ = self.child.kill();
    }
}

const KEY_ENTER: &[u8] = b"\r";
const KEY_ESC: &[u8] = b"\x1b";
const KEY_CTRL_R: &[u8] = b"\x12";

#[test]
fn pty_shortcut_step_hints_at_ctrl_r_and_surfaces_a_failed_reconfigure() {
    let sandbox = Sandbox::new("reconfigure");
    let mut session = Session::spawn(&sandbox, 100, 32);

    session.wait_for("Speech model", Duration::from_secs(10));
    session.send(KEY_ENTER); // Model -> Microphone
    session.wait_for("Microphone", Duration::from_secs(5));
    session.send(KEY_ENTER); // Microphone -> Activation
    session.wait_for("Activation", Duration::from_secs(5));
    session.send(KEY_ENTER); // Activation -> Shortcut

    let shortcut_screen = session.wait_for("Shortcut", Duration::from_secs(5));
    assert!(
        shortcut_screen.contains("Ctrl+R set via system dialog"),
        "Shortcut step hint should advertise the new Ctrl+R reconfigure path:\n{shortcut_screen}"
    );
    assert!(
        shortcut_screen.contains("Super+O"),
        "Shortcut step should start pre-filled with the configured default keybind:\n{shortcut_screen}"
    );
    assert!(
        !shortcut_screen.contains("Currently bound:"),
        "an untested keybind must not claim a real bound trigger yet:\n{shortcut_screen}"
    );

    // Trigger the real `activation::reconfigure_shortcut` code path. With no
    // portal reachable on the (nonexistent) bus, `GlobalShortcuts::new()`
    // fails immediately inside the background thread, so this resolves in
    // well under a second rather than waiting anywhere near the real
    // 120s dialog timeout.
    session.send(KEY_CTRL_R);
    let failed_screen = session.wait_for("Reconfigure failed:", Duration::from_secs(10));
    assert!(
        !failed_screen.contains("Currently bound:"),
        "a failed reconfigure must not fabricate a bound trigger:\n{failed_screen}"
    );

    // The console must still be fully usable afterwards: Enter now advances
    // past Shortcut instead of staying stuck behind the cleared handle.
    session.send(KEY_ENTER);
    session.wait_for("Transcript output", Duration::from_secs(5));

    // Esc back twice (Output -> Shortcut) and confirm the failure message
    // doesn't linger into a plain re-visit of the step.
    session.send(KEY_ESC);
    let revisited_screen = session.wait_for("Shortcut", Duration::from_secs(5));
    assert!(
        revisited_screen.contains("Reconfigure failed:"),
        "feedback from the last reconfigure attempt should still be visible on re-entry:\n{revisited_screen}"
    );
}
