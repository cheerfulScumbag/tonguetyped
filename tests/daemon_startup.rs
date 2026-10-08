use std::io::Read;
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

/// Places an empty stub at the default catalog model's resolved path, so
/// `prepare_dependencies`/`doctor` see it as already installed and these
/// tests never trigger a real network download.
fn install_default_model_stub(data_home: &std::path::Path) {
    let entry = tonguetyped::catalog::find(tonguetyped::catalog::DEFAULT_MODEL_ID).unwrap();
    std::fs::write(
        data_home.join("tonguetyped/models").join(entry.filename),
        [],
    )
    .unwrap();
}

/// Polls for the control socket to appear, the same way every daemon-startup
/// test here already waited inline before this helper existed.
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

struct Daemon(Child);

impl Drop for Daemon {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

#[test]
fn ipc_starts_when_shortcut_portal_is_unavailable() {
    let root = std::env::temp_dir().join(format!("tt-daemon-{}", std::process::id()));
    let config_home = root.join("config");
    let data_home = root.join("data");
    let runtime_dir = root.join("runtime");
    std::fs::create_dir_all(config_home.join("tonguetyped")).unwrap();
    std::fs::create_dir_all(data_home.join("tonguetyped/models")).unwrap();
    std::fs::create_dir_all(&runtime_dir).unwrap();
    std::fs::write(
        config_home.join("tonguetyped/config.toml"),
        "[audio]\nfeedback_sounds = false\n[transcription]\nvad_enabled = false\n",
    )
    .unwrap();
    install_default_model_stub(&data_home);

    let binary = env!("CARGO_BIN_EXE_tonguetyped");
    let child = Command::new(binary)
        .arg("daemon")
        .env("XDG_CONFIG_HOME", &config_home)
        .env("XDG_DATA_HOME", &data_home)
        .env("XDG_RUNTIME_DIR", &runtime_dir)
        .env("DBUS_SESSION_BUS_ADDRESS", "unix:path=/nonexistent")
        .stdout(Stdio::null())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    let mut daemon = Daemon(child);
    let socket = runtime_dir.join("tonguetyped/control.sock");
    let deadline = Instant::now() + Duration::from_secs(5);
    while !socket.exists() && Instant::now() < deadline {
        if daemon.0.try_wait().unwrap().is_some() {
            let mut stderr = String::new();
            daemon
                .0
                .stderr
                .take()
                .unwrap()
                .read_to_string(&mut stderr)
                .unwrap();
            panic!("daemon exited before opening its control socket: {stderr}");
        }
        std::thread::sleep(Duration::from_millis(20));
    }
    assert!(socket.exists(), "daemon did not open its control socket");

    let second = Command::new(binary)
        .arg("daemon")
        .env("XDG_CONFIG_HOME", &config_home)
        .env("XDG_DATA_HOME", &data_home)
        .env("XDG_RUNTIME_DIR", &runtime_dir)
        .env("DBUS_SESSION_BUS_ADDRESS", "unix:path=/nonexistent")
        .output()
        .unwrap();
    assert!(!second.status.success());
    assert!(String::from_utf8(second.stderr)
        .unwrap()
        .contains("daemon is already running"));

    let deadline = Instant::now() + Duration::from_secs(5);
    loop {
        let status = Command::new(binary)
            .arg("status")
            .env("XDG_CONFIG_HOME", &config_home)
            .env("XDG_DATA_HOME", &data_home)
            .env("XDG_RUNTIME_DIR", &runtime_dir)
            .output()
            .unwrap();
        assert!(status.status.success());
        if String::from_utf8(status.stdout)
            .unwrap()
            .contains("activation listener failed")
        {
            break;
        }
        assert!(
            Instant::now() < deadline,
            "shortcut failure was not reported"
        );
        std::thread::sleep(Duration::from_millis(20));
    }

    let doctor = Command::new(binary)
        .arg("doctor")
        .env("XDG_CONFIG_HOME", &config_home)
        .env("XDG_DATA_HOME", &data_home)
        .env("XDG_RUNTIME_DIR", &runtime_dir)
        .env("DBUS_SESSION_BUS_ADDRESS", "unix:path=/nonexistent")
        .output()
        .unwrap();
    assert!(doctor.status.success());
    let stdout = String::from_utf8(doctor.stdout).unwrap();
    assert!(stdout.contains("shortcut error:  activation listener failed:"));
    // Same binary for daemon and doctor, so the daemon's reported build matches.
    assert!(
        stdout.contains("matches this binary") || stdout.contains("cannot confirm"),
        "doctor should compare against the running daemon's build:\n{stdout}"
    );
    assert!(stdout.contains("backends:       available: "));

    drop(daemon);
    std::fs::remove_dir_all(root).unwrap();
}

#[test]
fn doctor_without_daemon_defers_shortcut_binding_test() {
    let root = std::env::temp_dir().join(format!("tt-doctor-{}", std::process::id()));
    let config_home = root.join("config");
    let data_home = root.join("data");
    let runtime_dir = root.join("runtime");
    std::fs::create_dir_all(config_home.join("tonguetyped")).unwrap();
    std::fs::create_dir_all(data_home.join("tonguetyped/models")).unwrap();
    std::fs::create_dir_all(&runtime_dir).unwrap();
    std::fs::write(
        config_home.join("tonguetyped/config.toml"),
        "[audio]\nfeedback_sounds = false\n[transcription]\nvad_enabled = false\n",
    )
    .unwrap();

    let output = Command::new(env!("CARGO_BIN_EXE_tonguetyped"))
        .arg("doctor")
        .env("XDG_CONFIG_HOME", &config_home)
        .env("XDG_DATA_HOME", &data_home)
        .env("XDG_RUNTIME_DIR", &runtime_dir)
        .env("DBUS_SESSION_BUS_ADDRESS", "unix:path=/nonexistent")
        .output()
        .unwrap();
    assert!(output.status.success());
    let stdout = String::from_utf8(output.stdout).unwrap();
    assert!(stdout.contains("shortcut portal: not tested"));
    assert!(!stdout.contains("shortcut portal: unavailable"));

    let shortcut_test = Command::new(env!("CARGO_BIN_EXE_tonguetyped"))
        .arg("shortcut-test")
        .env("XDG_CONFIG_HOME", &config_home)
        .env("XDG_DATA_HOME", &data_home)
        .env("XDG_RUNTIME_DIR", &runtime_dir)
        .env("DBUS_SESSION_BUS_ADDRESS", "unix:path=/nonexistent")
        .output()
        .unwrap();
    assert!(!shortcut_test.status.success());
    assert!(String::from_utf8(shortcut_test.stderr)
        .unwrap()
        .contains("shortcut binding test failed"));

    std::fs::remove_dir_all(root).unwrap();
}

#[test]
fn doctor_distinguishes_invalid_model_from_missing_model() {
    let root = std::env::temp_dir().join(format!("tt-invalid-model-{}", std::process::id()));
    let config_home = root.join("config");
    let data_home = root.join("data");
    let runtime_dir = root.join("runtime");
    std::fs::create_dir_all(config_home.join("tonguetyped")).unwrap();
    std::fs::create_dir_all(data_home.join("tonguetyped/models")).unwrap();
    std::fs::create_dir_all(&runtime_dir).unwrap();
    std::fs::write(
        config_home.join("tonguetyped/config.toml"),
        "[audio]\nfeedback_sounds = false\n[transcription]\nvad_enabled = false\n",
    )
    .unwrap();
    install_default_model_stub(&data_home);

    let output = Command::new(env!("CARGO_BIN_EXE_tonguetyped"))
        .arg("doctor")
        .env("XDG_CONFIG_HOME", &config_home)
        .env("XDG_DATA_HOME", &data_home)
        .env("XDG_RUNTIME_DIR", &runtime_dir)
        .env("DBUS_SESSION_BUS_ADDRESS", "unix:path=/nonexistent")
        .output()
        .unwrap();
    let stdout = String::from_utf8(output.stdout).unwrap();
    assert!(output.status.success());
    assert!(stdout.contains("model:          invalid"));
    assert!(stdout.contains("model error:"));
    assert!(stdout.contains(&format!(
        "model id:       {}",
        tonguetyped::catalog::DEFAULT_MODEL_ID
    )));
    // On a plain build this is always transcribe.cpp/cpu. A gpu-vulkan/
    // gpu-cuda/gpu-rocm/gpu-metal build instead reports whatever backend the
    // host's hardware actually supports (that's the point of doctor
    // reporting the real backend), so only the CPU-only build asserts the
    // specific CPU backend/device text.
    #[cfg(not(any(
        feature = "gpu-vulkan",
        feature = "gpu-cuda",
        feature = "gpu-rocm",
        feature = "gpu-metal"
    )))]
    {
        assert!(stdout.contains("backend:        transcribe.cpp/cpu"));
        assert!(stdout.contains("device:         CPU"));
    }
    #[cfg(any(
        feature = "gpu-vulkan",
        feature = "gpu-cuda",
        feature = "gpu-rocm",
        feature = "gpu-metal"
    ))]
    {
        assert!(stdout.contains("backend:        "));
        assert!(stdout.contains("device:         "));
    }

    std::fs::remove_dir_all(root).unwrap();
}

#[test]
fn doctor_reports_a_missing_model_with_no_error_and_no_file() {
    let root = std::env::temp_dir().join(format!("tt-missing-model-{}", std::process::id()));
    let config_home = root.join("config");
    let data_home = root.join("data");
    let runtime_dir = root.join("runtime");
    std::fs::create_dir_all(config_home.join("tonguetyped")).unwrap();
    std::fs::create_dir_all(data_home.join("tonguetyped/models")).unwrap();
    std::fs::create_dir_all(&runtime_dir).unwrap();
    std::fs::write(
        config_home.join("tonguetyped/config.toml"),
        "[audio]\nfeedback_sounds = false\n[transcription]\nvad_enabled = false\n",
    )
    .unwrap();
    // No model stub written at all - the catalog file genuinely does not exist.

    let output = Command::new(env!("CARGO_BIN_EXE_tonguetyped"))
        .arg("doctor")
        .env("XDG_CONFIG_HOME", &config_home)
        .env("XDG_DATA_HOME", &data_home)
        .env("XDG_RUNTIME_DIR", &runtime_dir)
        .env("DBUS_SESSION_BUS_ADDRESS", "unix:path=/nonexistent")
        .output()
        .unwrap();
    let stdout = String::from_utf8(output.stdout).unwrap();
    assert!(output.status.success());
    assert!(stdout.contains("model:          not found"));
    assert!(!stdout.contains("model error:"));
    assert!(stdout.contains(&format!(
        "model id:       {}",
        tonguetyped::catalog::DEFAULT_MODEL_ID
    )));

    std::fs::remove_dir_all(root).unwrap();
}

#[test]
fn doctor_reports_the_actually_configured_model_id_not_the_default() {
    let root = std::env::temp_dir().join(format!("tt-configured-model-{}", std::process::id()));
    let config_home = root.join("config");
    let data_home = root.join("data");
    let runtime_dir = root.join("runtime");
    std::fs::create_dir_all(config_home.join("tonguetyped")).unwrap();
    std::fs::create_dir_all(data_home.join("tonguetyped/models")).unwrap();
    std::fs::create_dir_all(&runtime_dir).unwrap();
    let non_default_id = "whisper-tiny-q5_k_m";
    assert_ne!(non_default_id, tonguetyped::catalog::DEFAULT_MODEL_ID);
    std::fs::write(
        config_home.join("tonguetyped/config.toml"),
        format!(
            "[audio]\nfeedback_sounds = false\n[transcription]\nvad_enabled = false\n[model]\nactive_model = \"{non_default_id}\"\n"
        ),
    )
    .unwrap();
    let entry = tonguetyped::catalog::find(non_default_id).unwrap();
    std::fs::write(
        data_home.join("tonguetyped/models").join(entry.filename),
        [],
    )
    .unwrap();

    let output = Command::new(env!("CARGO_BIN_EXE_tonguetyped"))
        .arg("doctor")
        .env("XDG_CONFIG_HOME", &config_home)
        .env("XDG_DATA_HOME", &data_home)
        .env("XDG_RUNTIME_DIR", &runtime_dir)
        .env("DBUS_SESSION_BUS_ADDRESS", "unix:path=/nonexistent")
        .output()
        .unwrap();
    let stdout = String::from_utf8(output.stdout).unwrap();
    assert!(output.status.success());
    assert!(stdout.contains(&format!("model id:       {non_default_id}")));
    assert!(stdout.contains(entry.filename));

    std::fs::remove_dir_all(root).unwrap();
}

#[test]
fn daemon_stop_gracefully_shuts_down_and_removes_socket() {
    let root = std::env::temp_dir().join(format!("tt-daemon-stop-{}", std::process::id()));
    let config_home = root.join("config");
    let data_home = root.join("data");
    let runtime_dir = root.join("runtime");
    std::fs::create_dir_all(config_home.join("tonguetyped")).unwrap();
    std::fs::create_dir_all(data_home.join("tonguetyped/models")).unwrap();
    std::fs::create_dir_all(&runtime_dir).unwrap();
    std::fs::write(
        config_home.join("tonguetyped/config.toml"),
        "[audio]\nfeedback_sounds = false\n[transcription]\nvad_enabled = false\n",
    )
    .unwrap();
    install_default_model_stub(&data_home);

    let binary = env!("CARGO_BIN_EXE_tonguetyped");
    let child = Command::new(binary)
        .arg("daemon")
        .env("XDG_CONFIG_HOME", &config_home)
        .env("XDG_DATA_HOME", &data_home)
        .env("XDG_RUNTIME_DIR", &runtime_dir)
        .env("DBUS_SESSION_BUS_ADDRESS", "unix:path=/nonexistent")
        .stdout(Stdio::null())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    let mut daemon = Daemon(child);
    let socket = runtime_dir.join("tonguetyped/control.sock");
    wait_for_socket(&socket);

    let stop = Command::new(binary)
        .args(["daemon", "stop"])
        .env("XDG_CONFIG_HOME", &config_home)
        .env("XDG_DATA_HOME", &data_home)
        .env("XDG_RUNTIME_DIR", &runtime_dir)
        .output()
        .unwrap();
    assert!(
        stop.status.success(),
        "daemon stop failed: {}",
        String::from_utf8_lossy(&stop.stderr)
    );
    assert!(
        !socket.exists(),
        "control socket was not removed after stop"
    );

    let deadline = Instant::now() + Duration::from_secs(5);
    loop {
        if let Some(status) = daemon.0.try_wait().unwrap() {
            assert!(status.success(), "daemon did not exit cleanly: {status}");
            break;
        }
        assert!(
            Instant::now() < deadline,
            "daemon process did not exit after `daemon stop`"
        );
        std::thread::sleep(Duration::from_millis(20));
    }

    std::fs::remove_dir_all(root).unwrap();
}

#[test]
fn daemon_stop_without_a_running_daemon_fails_clearly_and_does_not_hang() {
    let root = std::env::temp_dir().join(format!("tt-daemon-stop-missing-{}", std::process::id()));
    let runtime_dir = root.join("runtime");
    std::fs::create_dir_all(&runtime_dir).unwrap();

    let output = Command::new(env!("CARGO_BIN_EXE_tonguetyped"))
        .args(["daemon", "stop"])
        .env("XDG_RUNTIME_DIR", &runtime_dir)
        .output()
        .unwrap();
    assert!(!output.status.success());
    assert!(String::from_utf8(output.stderr)
        .unwrap()
        .contains("daemon is not running"));

    std::fs::remove_dir_all(root).unwrap();
}

#[test]
fn daemon_restart_stops_the_old_process_and_starts_a_new_one() {
    let root = std::env::temp_dir().join(format!("tt-daemon-restart-{}", std::process::id()));
    let config_home = root.join("config");
    let data_home = root.join("data");
    let runtime_dir = root.join("runtime");
    std::fs::create_dir_all(config_home.join("tonguetyped")).unwrap();
    std::fs::create_dir_all(data_home.join("tonguetyped/models")).unwrap();
    std::fs::create_dir_all(&runtime_dir).unwrap();
    std::fs::write(
        config_home.join("tonguetyped/config.toml"),
        "[audio]\nfeedback_sounds = false\n[transcription]\nvad_enabled = false\n",
    )
    .unwrap();
    install_default_model_stub(&data_home);

    let binary = env!("CARGO_BIN_EXE_tonguetyped");
    let child = Command::new(binary)
        .arg("daemon")
        .env("XDG_CONFIG_HOME", &config_home)
        .env("XDG_DATA_HOME", &data_home)
        .env("XDG_RUNTIME_DIR", &runtime_dir)
        .env("DBUS_SESSION_BUS_ADDRESS", "unix:path=/nonexistent")
        .stdout(Stdio::null())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    let mut first = Daemon(child);
    let socket = runtime_dir.join("tonguetyped/control.sock");
    wait_for_socket(&socket);

    let restart = Command::new(binary)
        .args(["daemon", "restart"])
        .env("XDG_CONFIG_HOME", &config_home)
        .env("XDG_DATA_HOME", &data_home)
        .env("XDG_RUNTIME_DIR", &runtime_dir)
        .env("DBUS_SESSION_BUS_ADDRESS", "unix:path=/nonexistent")
        .output()
        .unwrap();
    assert!(
        restart.status.success(),
        "daemon restart failed: {}",
        String::from_utf8_lossy(&restart.stderr)
    );

    // The original process must have actually exited, not merely dropped its
    // socket momentarily between the old process removing it and the new one
    // rebinding.
    let deadline = Instant::now() + Duration::from_secs(5);
    loop {
        if let Some(status) = first.0.try_wait().unwrap() {
            assert!(
                status.success(),
                "old daemon did not exit cleanly: {status}"
            );
            break;
        }
        assert!(
            Instant::now() < deadline,
            "old daemon process did not exit after `daemon restart`"
        );
        std::thread::sleep(Duration::from_millis(20));
    }

    // `restart` launches the replacement detached and returns before it
    // necessarily finishes (re)binding, so confirm the new daemon is up by
    // waiting for the socket and then successfully talking to it.
    wait_for_socket(&socket);
    let status = Command::new(binary)
        .arg("status")
        .env("XDG_CONFIG_HOME", &config_home)
        .env("XDG_DATA_HOME", &data_home)
        .env("XDG_RUNTIME_DIR", &runtime_dir)
        .output()
        .unwrap();
    assert!(status.status.success());

    // The replacement is a detached grandchild, not `first`'s child, so it
    // must be stopped explicitly rather than relying on any Drop guard here.
    let stop = Command::new(binary)
        .args(["daemon", "stop"])
        .env("XDG_CONFIG_HOME", &config_home)
        .env("XDG_DATA_HOME", &data_home)
        .env("XDG_RUNTIME_DIR", &runtime_dir)
        .output()
        .unwrap();
    assert!(stop.status.success());

    std::fs::remove_dir_all(root).unwrap();
}
