//! Real-pseudo-terminal behavior tests for the setup console's Ctrl+R
//! "reconfigure via the system dialog" shortcut path (`src/setup/console.rs`).
//! Same `Sandbox`/`Session`/`vt100::Parser` pattern as
//! `tests/setup_console.rs` - drives the actual compiled binary through a
//! PTY, never the console's internal functions directly.
//!
//! Most of these point `DBUS_SESSION_BUS_ADDRESS` at a nonexistent socket, the
//! same isolation `tests/daemon_startup.rs`'s shortcut-test coverage already
//! relies on: `activation::reconfigure_shortcut` then fails immediately
//! (no real portal to call), which lets the test observe the error-reporting UI
//! live without ever touching a real desktop's global-shortcut registrations.
//! `pty_repeated_ctrl_r_reconfigure_never_hangs_on_the_cached_portal_connection`
//! instead points at a private, empty bus, because only a *reachable* bus makes
//! `ashpd` cache a portal connection - the precondition for the reopen hang it
//! guards against.
//!
//! Named with a `pty_` prefix like `tests/tui_dashboard.rs` so
//! `flake.nix`'s `cargoTestFlags = ["--" "--skip" "pty_"]` continues to
//! skip these in the Nix sandboxed build (no usable pty/tty subsystem
//! there); `nix develop -c cargo test` and plain `cargo test` run them
//! normally.

use portable_pty::{native_pty_system, Child, CommandBuilder, MasterPty, PtySize};
use std::io::{BufRead, Read, Write};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

/// Each test spawns a full setup console (ratatui + audio probing); running
/// them concurrently on a loaded machine makes the fixed startup timeouts flake,
/// so they take turns.
static PTY_LOCK: Mutex<()> = Mutex::new(());

fn serialize() -> std::sync::MutexGuard<'static, ()> {
    PTY_LOCK
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
}

/// A private, empty D-Bus session bus (no portals on it). Pointing the setup
/// console at a *live* bus - unlike the nonexistent socket the other tests
/// here use - is what reproduces the reconfigure-reopen hang: `ashpd` caches
/// its process-global portal connection on the first Ctrl+R, so a second one
/// reuses that connection (and hangs when its socket reader died with the
/// first attempt's runtime). The bus is started from a minimal config with no
/// service directories, so D-Bus never auto-activates the host's real
/// xdg-desktop-portal and the test cannot touch a real desktop's shortcuts.
struct PrivateBus {
    _config_dir: std::path::PathBuf,
    child: std::process::Child,
    address: String,
}

impl PrivateBus {
    fn start() -> Self {
        let config_dir = std::env::temp_dir().join(format!(
            "tt-private-bus-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir_all(&config_dir).unwrap();
        let config_path = config_dir.join("session.conf");
        std::fs::write(
            &config_path,
            "<busconfig><type>session</type><listen>unix:tmpdir=/tmp</listen>\
             <policy context=\"default\"><allow send_destination=\"*\"/>\
             <allow eavesdrop=\"true\"/><allow own=\"*\"/></policy></busconfig>",
        )
        .unwrap();
        let mut child = std::process::Command::new("dbus-daemon")
            .arg(format!("--config-file={}", config_path.display()))
            .args(["--nofork", "--print-address=1"])
            .stdout(std::process::Stdio::piped())
            .spawn()
            .expect("spawn dbus-daemon");
        let stdout = child.stdout.take().expect("dbus-daemon stdout");
        let mut address = String::new();
        std::io::BufReader::new(stdout)
            .read_line(&mut address)
            .expect("read dbus-daemon address");
        let address = address.trim().to_string();
        assert!(!address.is_empty(), "dbus-daemon printed no address");
        Self {
            _config_dir: config_dir,
            child,
            address,
        }
    }
}

impl Drop for PrivateBus {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
        let _ = std::fs::remove_dir_all(&self._config_dir);
    }
}

struct Sandbox {
    root: std::path::PathBuf,
    config_home: std::path::PathBuf,
    data_home: std::path::PathBuf,
    runtime_dir: std::path::PathBuf,
    dbus_address: String,
}

impl Sandbox {
    fn new(tag: &str) -> Self {
        Self::with_bus(tag, "unix:path=/nonexistent")
    }

    fn with_bus(tag: &str, dbus_address: &str) -> Self {
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
            dbus_address: dbus_address.to_string(),
        }
    }

    fn apply_env(&self, cmd: &mut CommandBuilder) {
        cmd.env("XDG_CONFIG_HOME", &self.config_home);
        cmd.env("XDG_DATA_HOME", &self.data_home);
        cmd.env("XDG_RUNTIME_DIR", &self.runtime_dir);
        cmd.env("NO_COLOR", "1");
        cmd.env("DBUS_SESSION_BUS_ADDRESS", &self.dbus_address);
        // The console owns the alternate screen via ratatui's cursor-positioned
        // redraws, but `zbus`/`tracing` warnings go to stderr, which shares
        // this pty and would corrupt the parsed screen; cap logging at error so
        // the screen the test reads is exactly what the console drew.
        cmd.env("RUST_LOG", "error");
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
        self.try_wait_for(needle, timeout).unwrap_or_else(|| {
            panic!(
                "timed out waiting for {needle:?}; current screen:\n{}",
                self.visible_text()
            )
        })
    }

    fn try_wait_for(&self, needle: &str, timeout: Duration) -> Option<String> {
        let deadline = Instant::now() + timeout;
        loop {
            let snapshot = self.visible_text();
            if snapshot.contains(needle) {
                return Some(snapshot);
            }
            if Instant::now() >= deadline {
                return None;
            }
            std::thread::sleep(Duration::from_millis(50));
        }
    }
}

impl Session {
    fn wait_for_exit(&mut self, timeout: Duration) {
        let deadline = Instant::now() + timeout;
        loop {
            if let Ok(Some(_)) = self.child.try_wait() {
                return;
            }
            if Instant::now() >= deadline {
                let _ = self.child.kill();
                panic!(
                    "setup console did not exit in time; screen:\n{}",
                    self.visible_text()
                );
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

const KEY_ESC: &[u8] = b"\x1b";
const KEY_CTRL_R: &[u8] = b"\x12";

/// Waits for the console's first frame, then lets raw mode settle so the
/// first keypress is not lost to the pty's line discipline.
fn wait_until_ready(session: &Session) {
    session.wait_for("Shortcut", Duration::from_secs(15));
    std::thread::sleep(Duration::from_millis(300));
}

#[test]
fn pty_ctrl_r_surfaces_a_failed_reconfigure_and_keeps_setup_usable() {
    let _serial = serialize();
    let sandbox = Sandbox::new("reconfigure");
    let mut session = Session::spawn(&sandbox, 100, 32);
    wait_until_ready(&session);
    let screen = session.visible_text();
    assert!(
        !screen.contains("Super+O"),
        "setup must not pre-fill or imply an app-chosen default:\n{screen}"
    );
    assert!(
        screen.contains("Shortcut  (none set yet)"),
        "with nothing bound the row should say so:\n{screen}"
    );

    // Trigger the real `activation::reconfigure_shortcut` code path. With no
    // portal reachable on the (nonexistent) bus, `GlobalShortcuts::new()`
    // fails immediately inside the background thread, so this resolves in
    // well under a second rather than waiting anywhere near the real
    // 120s dialog timeout.
    session.send(KEY_CTRL_R);
    let failed = session.wait_for("Error:", Duration::from_secs(10));
    assert!(
        failed.contains("Shortcut  (none set yet)"),
        "a failed reconfigure must not fabricate a bound trigger:\n{failed}"
    );

    // Setup stays usable: tabs still switch and Esc at the root leaves.
    session.send(b"2");
    session.wait_for("Speech models", Duration::from_secs(5));
    session.send(KEY_ESC);
    session.wait_for_exit(Duration::from_secs(5));
}

#[test]
fn pty_repeated_ctrl_r_reconfigure_never_hangs_on_the_cached_portal_connection() {
    // A live-but-empty bus: the first Ctrl+R reaches it, so `ashpd` caches its
    // process-global portal connection, then errors on the missing portal. The
    // second Ctrl+R must settle the same way instead of blocking forever on a
    // connection whose socket reader died with the first attempt's runtime -
    // the "stuck in the shortcut menu" reopen dead end.
    let _serial = serialize();
    let bus = PrivateBus::start();
    let sandbox = Sandbox::with_bus("repeat-reconfigure", &bus.address);
    let mut session = Session::spawn(&sandbox, 100, 32);
    wait_until_ready(&session);

    session.send(KEY_CTRL_R);
    session.wait_for("Error:", Duration::from_secs(15));

    // The empty-bus error is instant, so if the second attempt has not settled
    // after a few seconds it is stuck on "A shortcut window is open", not
    // slow.
    session.send(KEY_CTRL_R);
    std::thread::sleep(Duration::from_secs(4));
    let second = session.visible_text();
    assert!(
        !second.contains("A shortcut window is open"),
        "a repeat Ctrl+R must not hang on the cached portal connection:\n{second}"
    );
    assert!(
        second.contains("Error:"),
        "a repeat Ctrl+R must report the portal failure:\n{second}"
    );
}
