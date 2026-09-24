use std::io::Read;
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

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
    std::fs::write(data_home.join("tonguetyped/models/ggml-small-q5_1.bin"), []).unwrap();

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
    assert!(String::from_utf8(doctor.stdout)
        .unwrap()
        .contains("shortcut error:  activation listener failed:"));

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
    std::fs::create_dir_all(config_home.join("tonguetyped")).unwrap();
    std::fs::create_dir_all(data_home.join("tonguetyped/models")).unwrap();
    std::fs::write(
        config_home.join("tonguetyped/config.toml"),
        "[audio]\nfeedback_sounds = false\n[transcription]\nvad_enabled = false\n",
    )
    .unwrap();
    std::fs::write(data_home.join("tonguetyped/models/ggml-small-q5_1.bin"), []).unwrap();

    let output = Command::new(env!("CARGO_BIN_EXE_tonguetyped"))
        .arg("doctor")
        .env("XDG_CONFIG_HOME", &config_home)
        .env("XDG_DATA_HOME", &data_home)
        .env("DBUS_SESSION_BUS_ADDRESS", "unix:path=/nonexistent")
        .output()
        .unwrap();
    let stdout = String::from_utf8(output.stdout).unwrap();
    assert!(output.status.success());
    assert!(stdout.contains("model:          invalid"));
    assert!(stdout.contains("model error:"));

    std::fs::remove_dir_all(root).unwrap();
}
