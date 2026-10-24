//! Real-pseudo-terminal behavior tests for the tabbed setup console
//! (`src/setup/console.rs`). These drive the compiled binary through a PTY
//! exactly as a terminal session would, mirroring `tests/tui_dashboard.rs`'s
//! `Sandbox`/`Session` pattern - no source-grepping, no calling internal
//! functions directly.
//!
//! Named with a `pty_` prefix like `tests/tui_dashboard.rs` so `flake.nix`'s
//! `cargoTestFlags = ["--" "--skip" "pty_"]` continues to skip these in the
//! Nix sandboxed build (no usable pty/tty subsystem there); `nix develop -c
//! cargo test` and plain `cargo test` run them normally.

use portable_pty::{native_pty_system, Child, CommandBuilder, MasterPty, PtySize};
use std::io::{Read, Write};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

/// Each test spawns a full setup console (ratatui + audio probing); running
/// them concurrently on a loaded machine makes the fixed timeouts flake, so
/// they take turns.
static PTY_LOCK: Mutex<()> = Mutex::new(());

fn serialize() -> std::sync::MutexGuard<'static, ()> {
    PTY_LOCK
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
}

struct Sandbox {
    root: std::path::PathBuf,
    config_home: std::path::PathBuf,
    data_home: std::path::PathBuf,
    runtime_dir: std::path::PathBuf,
}

impl Sandbox {
    fn new(tag: &str) -> Self {
        let root = std::env::temp_dir().join(format!(
            "tt-setup-console-{tag}-{}-{}",
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
        // Pre-seed the default catalog model as "already installed" (same
        // trick as tests/tui_dashboard.rs's Sandbox) so selecting it never
        // starts a download or touches the network.
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

    /// Seeds `config.toml`, so the console opens on known values.
    fn with_config(tag: &str, config: &str) -> Self {
        let sandbox = Self::new(tag);
        std::fs::write(sandbox.config_path(), config).unwrap();
        sandbox
    }

    fn apply_env(&self, cmd: &mut CommandBuilder) {
        cmd.env("XDG_CONFIG_HOME", &self.config_home);
        cmd.env("XDG_DATA_HOME", &self.data_home);
        // No daemon socket lives here, so the console sees a stopped daemon.
        cmd.env("XDG_RUNTIME_DIR", &self.runtime_dir);
        cmd.env("NO_COLOR", "1");
        cmd.env("DBUS_SESSION_BUS_ADDRESS", "unix:path=/nonexistent");
        // No compositor either: the overlay test must report that, not hang.
        cmd.env_remove("WAYLAND_DISPLAY");
        cmd.env("RUST_LOG", "error");
    }

    fn config_path(&self) -> std::path::PathBuf {
        self.config_home.join("tonguetyped/config.toml")
    }

    fn config_contents(&self) -> String {
        std::fs::read_to_string(self.config_path()).unwrap()
    }
}

impl Drop for Sandbox {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.root);
    }
}

/// Same `vt100::Parser`-backed screen reconstruction as
/// `tests/tui_dashboard.rs::Session` - ratatui only rewrites changed cells and
/// jumps the cursor directly between them, so a naive strip-and-concatenate
/// read of the raw byte stream would merge unrelated rows.
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
        let session = Session {
            master: pair.master,
            writer,
            parser,
            child,
        };
        session.wait_for("VOX // ", Duration::from_secs(15));
        // Let raw mode settle so the first keypress is not eaten by the
        // pty's line discipline.
        std::thread::sleep(Duration::from_millis(300));
        session
    }

    fn send(&mut self, bytes: &[u8]) {
        self.writer.write_all(bytes).unwrap();
        self.writer.flush().unwrap();
        // One key per frame: the console reads a key per poll tick.
        std::thread::sleep(Duration::from_millis(30));
    }

    fn type_text(&mut self, text: &str) {
        for c in text.chars() {
            self.send(c.to_string().as_bytes());
        }
    }

    fn visible_text(&self) -> String {
        self.parser.lock().unwrap().screen().contents()
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

    fn wait_for(&self, needle: &str, timeout: Duration) -> String {
        self.try_wait_for(needle, timeout).unwrap_or_else(|| {
            panic!(
                "timed out waiting for {needle:?}; current screen:\n{}",
                self.visible_text()
            )
        })
    }

    /// Presses `j` until the focused row (the one carrying `▸ `) shows
    /// `label`, for rows whose position depends on the host (how many
    /// microphones it has).
    fn focus_row(&mut self, label: &str) {
        for _ in 0..30 {
            if self
                .visible_text()
                .lines()
                .any(|line| line.contains(&format!("\u{25b8} {label}")))
            {
                return;
            }
            self.send(b"j");
            std::thread::sleep(Duration::from_millis(50));
        }
        panic!(
            "never focused {label:?}; current screen:\n{}",
            self.visible_text()
        );
    }

    fn wait_for_exit(&mut self, timeout: Duration) {
        let deadline = Instant::now() + timeout;
        loop {
            if let Ok(Some(_)) = self.child.try_wait() {
                // Let the reader thread drain the final output.
                std::thread::sleep(Duration::from_millis(200));
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

const KEY_ENTER: &[u8] = b"\r";
const KEY_ESC: &[u8] = b"\x1b";
const KEY_BACKSPACE: &[u8] = b"\x7f";
const KEY_CTRL_C: &[u8] = b"\x03";
const KEY_CTRL_S: &[u8] = b"\x13";

#[test]
fn pty_every_tab_opens_with_number_keys_at_80x24_and_60x20() {
    let _serial = serialize();
    for (cols, rows) in [(80, 24), (60, 20)] {
        let sandbox = Sandbox::new("tabs");
        let mut session = Session::spawn(&sandbox, cols, rows);
        let audio = session.wait_for("Microphone", Duration::from_secs(5));
        assert!(audio.contains("[1] Audio"), "{audio}");
        assert!(
            audio.contains("STOPPED"),
            "the badge shows the real daemon state:\n{audio}"
        );
        assert!(
            !audio.contains("████████╗"),
            "no block-letter banner:\n{audio}"
        );
        for (key, marker) in [
            (b"2", "Speech models"),
            (b"3", "Delivery"),
            (b"4", "Desktop overlay"),
            (b"1", "Microphone"),
        ] {
            session.send(key);
            let screen = session.wait_for(marker, Duration::from_secs(5));
            // A row that overflowed would wrap and push its right border off
            // the line, so every framed row must still end in one.
            for line in screen.lines() {
                let line = line.trim_end();
                if let Some(first) = line.chars().next().filter(|c| "╭│╰".contains(*c)) {
                    let last = line.chars().last().unwrap();
                    let closing = match first {
                        '╭' => "╮",
                        '╰' => "╯",
                        _ => "│▲▼",
                    };
                    assert!(
                        closing.contains(last) || "╮│╯".contains(last),
                        "row lost its right border at {cols}x{rows}: {line:?}\n{screen}"
                    );
                }
            }
            assert!(screen.contains("save & quit"), "footer keys:\n{screen}");
        }
        session.send(b"]");
        session.wait_for("Speech models", Duration::from_secs(5));
        session.send(b"[");
        session.wait_for("Microphone", Duration::from_secs(5));
    }
}

#[test]
fn pty_overlay_steppers_persist_when_quitting_with_q() {
    let _serial = serialize();
    let sandbox = Sandbox::with_config(
        "overlay",
        "[overlay]\nenabled = true\nposition = \"bottom\"\nstyle = \"badge\"\n",
    );
    let mut session = Session::spawn(&sandbox, 100, 32);
    session.send(b"4");
    session.wait_for("\u{25b8} Enabled", Duration::from_secs(5));
    session.send(b"j"); // Position
    session.send(b"l"); // bottom -> the next position
    session.send(b"j"); // Style
    session.send(b"l"); // badge -> the next style
    let screen = session.visible_text();
    session.send(b"q");
    session.wait_for_exit(Duration::from_secs(5));
    let config = sandbox.config_contents();
    assert!(
        !config.contains("position = \"bottom\""),
        "{config}\n{screen}"
    );
    assert!(!config.contains("style = \"badge\""), "{config}\n{screen}");
    assert!(
        session.visible_text().contains("Configuration saved."),
        "{}",
        session.visible_text()
    );
}

#[test]
fn pty_history_fields_persist_their_inputs() {
    let _serial = serialize();
    let sandbox = Sandbox::with_config("history", "[output]\nmethod = \"clipboard\"\n");
    let mut session = Session::spawn(&sandbox, 100, 32);
    session.send(b"3");
    session.focus_row("[ ] Save transcript history");
    session.send(b" ");
    session.focus_row("Max entries");
    session.send(KEY_ENTER);
    for _ in 0..3 {
        session.send(KEY_BACKSPACE);
    }
    session.send(b"5");
    session.send(KEY_ENTER);
    session.focus_row("Max age (days)");
    session.send(KEY_ENTER);
    for _ in 0..2 {
        session.send(KEY_BACKSPACE);
    }
    session.send(b"7");
    session.send(KEY_ENTER);
    session.focus_row("Export folder");
    session.send(KEY_ENTER);
    session.type_text("/tmp/tonguetyped-setup-folder");
    session.send(KEY_ENTER);
    session.wait_for("/tmp/tonguetyped-setup-folder", Duration::from_secs(5));
    session.send(KEY_CTRL_S);
    session.wait_for("Saved.", Duration::from_secs(5));

    let config = sandbox.config_contents();
    assert!(config.contains("enabled = true"), "{config}");
    assert!(config.contains("max_entries = 5"), "{config}");
    assert!(config.contains("max_age_days = 7"), "{config}");
    assert!(
        config.contains("transcript_folder = \"/tmp/tonguetyped-setup-folder\""),
        "{config}"
    );
}

#[test]
fn pty_text_filters_persist_replacements_and_passes() {
    let _serial = serialize();
    let sandbox = Sandbox::with_config("text", "[output]\nmethod = \"clipboard\"\n");
    let mut session = Session::spawn(&sandbox, 100, 32);
    session.send(b"3");
    session.focus_row("[ ] Spoken punctuation");
    session.send(b" ");
    session.focus_row("[ ] Remove filler words");
    session.send(b" ");
    session.focus_row("Replacements");
    session.send(KEY_ENTER);
    session.type_text("vox type => Voxtype");
    session.send(KEY_ENTER);
    session.wait_for("vox type => Voxtype", Duration::from_secs(5));
    session.send(b"q");
    session.wait_for_exit(Duration::from_secs(5));

    let config = sandbox.config_contents();
    assert!(config.contains("\"vox type\" = \"Voxtype\""), "{config}");
    assert!(config.contains("spoken_punctuation = true"), "{config}");
    assert!(config.contains("remove_fillers = true"), "{config}");
}

#[test]
fn pty_translation_and_the_decoding_drawer_persist_their_inputs() {
    let _serial = serialize();
    let sandbox = Sandbox::new("decoding");
    let mut session = Session::spawn(&sandbox, 100, 32);
    session.send(b"2");
    session.wait_for("Speech models", Duration::from_secs(5));
    session.send(b"l"); // table -> backend
    session.focus_row("[ ] Translate to English");
    session.send(b" ");
    session.focus_row("Decoding");
    session.send(KEY_ENTER);
    let drawer = session.wait_for("Whisper decoding", Duration::from_secs(5));
    assert!(
        drawer.contains("model default (0.6)") && drawer.contains("Whisper models only"),
        "the drawer shows every knob and its model default:\n{drawer}"
    );
    session.send(KEY_ENTER);
    session.type_text("TongueTyped");
    session.send(KEY_ENTER);
    for _ in 0..5 {
        session.send(b"j");
    }
    session.send(KEY_ENTER);
    session.send(b"2");
    session.send(KEY_ENTER);
    session.wait_for(
        "No-speech limit must be between 0 and 1",
        Duration::from_secs(5),
    );
    session.send(KEY_BACKSPACE);
    session.type_text("inf");
    session.send(KEY_ENTER);
    session.send(KEY_ESC); // closes the drawer, not setup
    session.wait_for("\u{25b8} Decoding", Duration::from_secs(5));
    session.send(KEY_ESC); // at the root: save and leave
    session.wait_for_exit(Duration::from_secs(5));

    let config = sandbox.config_contents();
    assert!(config.contains("translate = true"), "{config}");
    assert!(
        config.contains("initial_prompt = \"TongueTyped\""),
        "{config}"
    );
    assert!(config.contains("no_speech_threshold = inf"), "{config}");
    assert!(!config.contains("temperature"), "{config}");
}

#[test]
fn pty_the_backend_selector_persists_the_choice() {
    let _serial = serialize();
    let sandbox = Sandbox::new("backend");
    let mut session = Session::spawn(&sandbox, 100, 32);
    session.send(b"2");
    session.wait_for("Speech models", Duration::from_secs(5));
    session.send(b"l"); // table -> backend
    let screen = session.wait_for("\u{25b8} \u{25cf} auto", Duration::from_secs(5));
    assert!(screen.contains("cpu"), "{screen}");
    session.send(b"l"); // auto -> cpu, which every build has
    session.wait_for("\u{25cf} cpu", Duration::from_secs(5));
    session.send(b"q");
    session.wait_for_exit(Duration::from_secs(5));
    let config = sandbox.config_contents();
    assert!(config.contains("preferred_backend = \"cpu\""), "{config}");
}

#[test]
fn pty_ctrl_c_discards_unsaved_changes() {
    let _serial = serialize();
    let original = "[overlay]\nenabled = true\nposition = \"bottom\"\n";
    let sandbox = Sandbox::with_config("discard", original);
    let mut session = Session::spawn(&sandbox, 100, 32);
    session.send(b"4");
    session.wait_for("\u{25b8} Enabled", Duration::from_secs(5));
    session.send(b" ");
    session.wait_for("unsaved changes", Duration::from_secs(5));
    session.send(KEY_CTRL_C);
    session.wait_for_exit(Duration::from_secs(5));
    assert_eq!(sandbox.config_contents(), original, "Ctrl+C writes nothing");
    assert!(
        session
            .visible_text()
            .contains("Unsaved changes were discarded."),
        "{}",
        session.visible_text()
    );
}

#[test]
fn pty_the_overlay_test_reports_a_desktop_without_layer_shell() {
    let _serial = serialize();
    let sandbox = Sandbox::new("overlay-test");
    let mut session = Session::spawn(&sandbox, 100, 32);
    session.send(b"4");
    session.focus_row("[Enter / Space] Test Overlay on Desktop");
    session.send(KEY_ENTER);
    // With no Wayland display the probe fails within its two-second budget
    // and the console says so instead of pretending to flash.
    session.wait_for("no layer-shell overlay", Duration::from_secs(10));
    session.send(b"q");
    session.wait_for_exit(Duration::from_secs(5));
}

#[test]
fn pty_dictation_with_no_daemon_says_how_to_start_one() {
    let _serial = serialize();
    let sandbox = Sandbox::new("dictation");
    let mut session = Session::spawn(&sandbox, 100, 32);
    session.wait_for("[ DAEMON STOPPED ]", Duration::from_secs(5));
    session.focus_row("[ Start Dictation ]");
    session.send(KEY_ENTER);
    session.wait_for("The daemon is not running", Duration::from_secs(5));
    session.send(KEY_CTRL_C);
    session.wait_for_exit(Duration::from_secs(5));
}

#[test]
fn pty_quitting_untouched_settings_writes_nothing() {
    let _serial = serialize();
    let original = "[output]\nmethod = \"clipboard\"\n";
    let sandbox = Sandbox::with_config("untouched", original);
    let mut session = Session::spawn(&sandbox, 100, 32);
    session.send(b"q");
    session.wait_for_exit(Duration::from_secs(5));
    assert_eq!(sandbox.config_contents(), original);
    assert!(
        session.visible_text().contains("No changes were made."),
        "{}",
        session.visible_text()
    );
}
