//! Real-pseudo-terminal behavior tests for the overlay settings steps added
//! to the interactive setup console (`src/setup/console.rs`). These drive
//! the actual compiled binary through a PTY exactly as a human terminal
//! session would, mirroring `tests/tui_dashboard.rs`'s `Sandbox`/`Session`
//! pattern - no source-grepping, no calling internal functions directly.
//!
//! Named with a `pty_` prefix like `tests/tui_dashboard.rs` so `flake.nix`'s
//! `cargoTestFlags = ["--" "--skip" "pty_"]` continues to skip these in the
//! Nix sandboxed build (no usable pty/tty subsystem there); `nix develop -c
//! cargo test` and plain `cargo test` run them normally.

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
            "tt-setup-console-overlay-{tag}-{}-{}",
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
        // trick as tests/tui_dashboard.rs's Sandbox) so the setup console
        // never enters its Downloading step or touches the network.
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

    fn config_contents(&self) -> String {
        std::fs::read_to_string(self.config_home.join("tonguetyped/config.toml")).unwrap()
    }
}

impl Drop for Sandbox {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.root);
    }
}

/// Same `vt100::Parser`-backed screen reconstruction as `tests/tui_dashboard.rs::Session` -
/// ratatui only rewrites changed cells and jumps the cursor directly between them, so a
/// naive strip-and-concatenate read of the raw byte stream would merge unrelated rows.
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

    fn wait_for_exit(mut self, timeout: Duration) {
        let deadline = Instant::now() + timeout;
        loop {
            if let Ok(Some(_)) = self.child.try_wait() {
                return;
            }
            if Instant::now() >= deadline {
                let _ = self.child.kill();
                panic!("setup console did not exit in time");
            }
            std::thread::sleep(Duration::from_millis(50));
        }
    }
}

const KEY_UP: &[u8] = b"\x1b[A";
const KEY_DOWN: &[u8] = b"\x1b[B";
const KEY_ENTER: &[u8] = b"\r";
const KEY_TAB: &[u8] = b"\t";
const KEY_BACKSPACE: &[u8] = b"\x7f";

/// Advances through the setup console's early steps (Model, Inference
/// backend, Microphone, Activation, Shortcut, Output, Startup, History
/// retention, Transcript folder) with Enter, accepting every default, until
/// the Overlay step is reached.
fn advance_to_overlay_enabled(session: &mut Session) {
    session.wait_for("Speech model", Duration::from_secs(10));
    session.send(KEY_ENTER); // Model -> Inference backend
    session.wait_for("Inference backend", Duration::from_secs(5));
    advance_from_inference_backend_to_overlay_enabled(session);
}

fn advance_from_inference_backend_to_overlay_enabled(session: &mut Session) {
    session.send(KEY_ENTER); // Inference backend -> Microphone
    session.wait_for("Microphone", Duration::from_secs(5));
    session.send(KEY_ENTER); // Microphone -> Activation
    session.wait_for("Activation", Duration::from_secs(5));
    session.send(KEY_ENTER); // Activation -> Shortcut
    session.wait_for("Shortcut", Duration::from_secs(5));
    session.send(KEY_ENTER); // Shortcut -> Output
    session.wait_for("Transcript output", Duration::from_secs(5));
    session.send(KEY_ENTER); // Output -> Startup
    session.wait_for("Startup", Duration::from_secs(5));
    session.send(KEY_ENTER); // Startup -> History retention
    session.wait_for("History retention", Duration::from_secs(5));
    session.send(KEY_ENTER); // History retention -> Transcript folder
    session.wait_for("Transcript folder", Duration::from_secs(5));
    session.send(KEY_ENTER); // Transcript folder -> Overlay
    session.wait_for("Step", Duration::from_secs(5));
}

#[test]
fn pty_enabling_overlay_walks_through_position_style_and_streaming_and_persists_choices() {
    let sandbox = Sandbox::new("enable");
    let mut session = Session::spawn(&sandbox, 100, 32);

    advance_to_overlay_enabled(&mut session);
    let screen = session.wait_for("Overlay", Duration::from_secs(5));
    assert!(
        screen.contains("Disabled") && screen.contains("Enabled"),
        "overlay step should offer Disabled/Enabled choices:\n{screen}"
    );

    // Default selection is "Enabled" (OverlayConfig::enabled defaults true) -
    // confirm it directly into Position.
    session.send(KEY_ENTER);
    let position_screen = session.wait_for("Overlay position", Duration::from_secs(5));
    assert!(
        position_screen.contains("top-left") && position_screen.contains("bottom-right"),
        "position step should list the documented position values:\n{position_screen}"
    );

    // Default position is "top-right" (index 2); move down to "bottom-left"
    // (index 4) and confirm.
    for _ in 0..2 {
        session.send(KEY_DOWN);
    }
    session.send(KEY_ENTER);
    let style_screen = session.wait_for("Overlay style", Duration::from_secs(5));
    assert!(
        style_screen.contains("Badge")
            && style_screen.contains("Minimal")
            && style_screen.contains("Pill")
            && style_screen.contains("Blob"),
        "style step should list Badge/Minimal/Pill/Blob:\n{style_screen}"
    );

    // Move down to "Pill" (index 2) and confirm.
    session.send(KEY_DOWN);
    session.send(KEY_DOWN);
    session.send(KEY_ENTER);
    let streaming_screen = session.wait_for("Overlay streaming", Duration::from_secs(5));
    assert!(
        streaming_screen.contains("Simple pulse")
            && streaming_screen.contains("Streaming waveform"),
        "streaming step should list both indicator modes:\n{streaming_screen}"
    );

    // Select "Streaming waveform" (index 1) and confirm into Review.
    session.send(KEY_DOWN);
    session.send(KEY_ENTER);
    let confirm_screen = session.wait_for("Write this configuration?", Duration::from_secs(5));
    assert!(
        confirm_screen.contains("bottom-left")
            && confirm_screen.contains("pill")
            && confirm_screen.contains("streaming waveform"),
        "confirm screen should summarize the chosen overlay settings:\n{confirm_screen}"
    );

    session.send(KEY_ENTER); // save
    session.wait_for_exit(Duration::from_secs(5));

    let config = sandbox.config_contents();
    assert!(config.contains("enabled = true"), "{config}");
    assert!(config.contains("position = \"bottom-left\""), "{config}");
    assert!(config.contains("style = \"pill\""), "{config}");
    assert!(config.contains("streaming_indicator = true"), "{config}");
}

#[test]
fn pty_disabling_overlay_skips_the_position_style_and_streaming_steps() {
    let sandbox = Sandbox::new("disable");
    let mut session = Session::spawn(&sandbox, 100, 32);

    advance_to_overlay_enabled(&mut session);
    session.wait_for("Overlay", Duration::from_secs(5));

    // Default selection is "Enabled" (index 1); move up to "Disabled"
    // (index 0) and confirm - should skip straight to Confirm, never
    // showing Position/Style/Streaming.
    session.send(KEY_UP);
    session.send(KEY_ENTER);

    let confirm_screen = session.wait_for("Write this configuration?", Duration::from_secs(5));
    assert!(
        confirm_screen.contains("Overlay:     disabled"),
        "confirm screen should show the overlay as disabled:\n{confirm_screen}"
    );
    assert!(
        !confirm_screen.contains("Overlay position") && !confirm_screen.contains("Overlay style"),
        "disabling the overlay must skip the position/style/streaming sub-steps:\n{confirm_screen}"
    );

    session.send(KEY_ENTER); // save
    session.wait_for_exit(Duration::from_secs(5));

    let config = sandbox.config_contents();
    assert!(config.contains("enabled = false"), "{config}");
}

#[test]
fn pty_history_retention_and_transcript_folder_steps_persist_their_inputs() {
    let sandbox = Sandbox::new("retention");
    let mut session = Session::spawn(&sandbox, 100, 32);

    session.wait_for("Speech model", Duration::from_secs(10));
    session.send(KEY_ENTER); // Model -> Inference backend
    session.wait_for("Inference backend", Duration::from_secs(5));
    session.send(KEY_ENTER); // -> Microphone
    session.wait_for("Microphone", Duration::from_secs(5));
    session.send(KEY_ENTER); // -> Activation
    session.wait_for("Activation", Duration::from_secs(5));
    session.send(KEY_ENTER); // -> Shortcut
    session.wait_for("Shortcut", Duration::from_secs(5));
    session.send(KEY_ENTER); // -> Transcript output
    session.wait_for("Transcript output", Duration::from_secs(5));
    session.send(KEY_ENTER); // -> Startup
    session.wait_for("Startup", Duration::from_secs(5));
    session.send(KEY_ENTER); // -> History retention

    let retention = session.wait_for("History retention", Duration::from_secs(5));
    assert!(
        retention.contains("Maximum entries") && retention.contains("Maximum age (days)"),
        "retention step should show both limits:\n{retention}"
    );
    // Clear the prefilled "100" to 5, then Tab and clear "30" to 7.
    for _ in 0..3 {
        session.send(KEY_BACKSPACE);
    }
    session.send(b"5");
    session.send(KEY_TAB);
    for _ in 0..2 {
        session.send(KEY_BACKSPACE);
    }
    session.send(b"7");
    session.send(KEY_ENTER); // -> Transcript folder

    let folder = session.wait_for("Transcript folder", Duration::from_secs(5));
    assert!(folder.contains("Folder"), "{folder}");
    session.send(b"/tmp/tonguetyped-setup-folder");
    session.send(KEY_ENTER); // -> Overlay

    session.wait_for("Overlay", Duration::from_secs(5));
    session.send(KEY_UP); // Enabled -> Disabled (skips the sub-steps)
    session.send(KEY_ENTER);
    let confirm = session.wait_for("Write this configuration?", Duration::from_secs(5));
    assert!(
        confirm.contains("5 entries, 7 days") && confirm.contains("/tmp/tonguetyped-setup-folder"),
        "review should summarize retention and the folder:\n{confirm}"
    );

    session.send(KEY_ENTER); // save
    session.wait_for_exit(Duration::from_secs(5));

    let config = sandbox.config_contents();
    assert!(config.contains("max_entries = 5"), "{config}");
    assert!(config.contains("max_age_days = 7"), "{config}");
    assert!(
        config.contains("transcript_folder = \"/tmp/tonguetyped-setup-folder\""),
        "{config}"
    );
}

#[test]
fn pty_inference_backend_step_lists_usable_backends_and_persists_the_choice() {
    let sandbox = Sandbox::new("backend");
    let mut session = Session::spawn(&sandbox, 100, 40);

    session.wait_for("Speech model", Duration::from_secs(10));
    session.send(KEY_ENTER);
    let screen = session.wait_for("Inference backend", Duration::from_secs(5));
    assert!(
        screen.contains("> Auto (tries CUDA") && screen.contains("CPU"),
        "backend step should offer auto first and the always-available CPU:\n{screen}"
    );

    session.send(KEY_DOWN);
    session.wait_for("> CPU", Duration::from_secs(5));
    advance_from_inference_backend_to_overlay_enabled(&mut session);
    session.wait_for("Overlay", Duration::from_secs(5));
    session.send(KEY_UP); // Disabled -> skip straight to Review
    session.send(KEY_ENTER);
    let confirm_screen = session.wait_for("Write this configuration?", Duration::from_secs(5));
    assert!(
        confirm_screen.contains("Backend:     CPU"),
        "review should summarize the pinned backend:\n{confirm_screen}"
    );

    session.send(KEY_ENTER); // save
    session.wait_for_exit(Duration::from_secs(5));

    let config = sandbox.config_contents();
    assert!(config.contains("preferred_backend = \"cpu\""), "{config}");
}
