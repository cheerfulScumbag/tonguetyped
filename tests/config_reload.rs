//! End-to-end coverage for the daemon's automatic config reload: editing
//! `config.toml` while the daemon runs applies the change without a manual
//! `reload`, and an invalid edit is rejected while the daemon keeps running on
//! its last good config.

use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

/// A config that exercises the live path without touching the network or a
/// desktop: no feedback surface, no VAD download, and hold activation.
fn config_with_activation(mode: &str) -> String {
    format!(
        "[activation]\nmode = \"{mode}\"\n\
         [audio]\nfeedback_sounds = false\n\
         [transcription]\nvad_enabled = false\n\
         [overlay]\nenabled = false\n"
    )
}

/// Places an empty stub at the default catalog model's resolved path, so
/// `prepare_dependencies` sees it as already installed and the reload never
/// triggers a real network download.
fn install_default_model_stub(data_home: &std::path::Path) {
    let entry = tonguetyped::catalog::find(tonguetyped::catalog::DEFAULT_MODEL_ID).unwrap();
    std::fs::write(
        data_home.join("tonguetyped/models").join(entry.filename),
        [],
    )
    .unwrap();
}

struct Daemon(Child);

impl Drop for Daemon {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

struct Env {
    config_home: std::path::PathBuf,
    data_home: std::path::PathBuf,
    runtime_dir: std::path::PathBuf,
}

impl Env {
    fn command(&self, binary: &str) -> Command {
        let mut command = Command::new(binary);
        command
            .env("XDG_CONFIG_HOME", &self.config_home)
            .env("XDG_DATA_HOME", &self.data_home)
            .env("XDG_RUNTIME_DIR", &self.runtime_dir)
            .env("DBUS_SESSION_BUS_ADDRESS", "unix:path=/nonexistent");
        command
    }

    fn status(&self, binary: &str) -> String {
        let output = self.command(binary).arg("status").output().unwrap();
        assert!(
            output.status.success(),
            "status failed: {}",
            String::from_utf8_lossy(&output.stderr)
        );
        String::from_utf8(output.stdout).unwrap()
    }

    fn config_path(&self) -> std::path::PathBuf {
        self.config_home.join("tonguetyped/config.toml")
    }
}

fn wait_for_socket(socket: &std::path::Path) {
    let deadline = Instant::now() + Duration::from_secs(5);
    while !socket.exists() {
        assert!(
            Instant::now() < deadline,
            "daemon did not open its control socket"
        );
        std::thread::sleep(Duration::from_millis(20));
    }
}

/// Polls `status` until it satisfies `predicate`, or panics at the deadline
/// showing the last thing it printed.
fn wait_for_status(
    env: &Env,
    binary: &str,
    what: &str,
    predicate: impl Fn(&str) -> bool,
) -> String {
    let deadline = Instant::now() + Duration::from_secs(20);
    loop {
        let status = env.status(binary);
        if predicate(&status) {
            return status;
        }
        assert!(
            Instant::now() < deadline,
            "waiting for {what}; last status:\n{status}"
        );
        std::thread::sleep(Duration::from_millis(50));
    }
}

#[test]
fn editing_the_config_reloads_live_and_an_invalid_edit_keeps_the_last_config() {
    let root = std::env::temp_dir().join(format!("tt-config-reload-{}", std::process::id()));
    let env = Env {
        config_home: root.join("config"),
        data_home: root.join("data"),
        runtime_dir: root.join("runtime"),
    };
    std::fs::create_dir_all(env.config_home.join("tonguetyped")).unwrap();
    std::fs::create_dir_all(env.data_home.join("tonguetyped/models")).unwrap();
    std::fs::create_dir_all(&env.runtime_dir).unwrap();
    std::fs::write(env.config_path(), config_with_activation("hold")).unwrap();
    install_default_model_stub(&env.data_home);

    let binary = env!("CARGO_BIN_EXE_tonguetyped");
    let child = env
        .command(binary)
        .arg("daemon")
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .unwrap();
    let _daemon = Daemon(child);

    let socket = env.runtime_dir.join("tonguetyped/control.sock");
    wait_for_socket(&socket);
    let initial = env.status(binary);
    assert!(
        initial.contains("activation mode:  hold"),
        "unexpected initial status:\n{initial}"
    );
    assert!(
        !initial.contains("config error:"),
        "no config error expected yet:\n{initial}"
    );

    // A valid edit is picked up live, with no `reload` and no restart.
    std::fs::write(env.config_path(), config_with_activation("toggle")).unwrap();
    let reloaded = wait_for_status(&env, binary, "the toggle mode to apply live", |status| {
        status.contains("activation mode:  toggle")
    });
    assert!(
        !reloaded.contains("config error:"),
        "a valid reload must not report a config error:\n{reloaded}"
    );

    // An invalid edit is rejected and the value it would have changed keeps
    // the last good config; the daemon stays up and answering.
    std::fs::write(env.config_path(), config_with_activation("bogus")).unwrap();
    let rejected = wait_for_status(&env, binary, "the invalid edit to be reported", |status| {
        status.contains("config error:")
    });
    assert!(
        rejected.contains("activation mode:  toggle"),
        "the previous config must stay in effect after a rejected edit:\n{rejected}"
    );

    std::fs::remove_dir_all(root).unwrap();
}
