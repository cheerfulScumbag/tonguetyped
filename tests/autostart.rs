use std::process::{Command, Stdio};

use tonguetyped::config::Config;

#[test]
fn autostart_lifecycle_updates_config_and_desktop_entry() {
    let root = std::env::temp_dir().join(format!(
        "tt-autostart-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    let config_home = root.join("config");
    let entry = config_home.join("autostart/tonguetyped.desktop");
    let binary = env!("CARGO_BIN_EXE_tonguetyped");
    let run = |arguments: &[&str]| {
        Command::new(binary)
            .args(arguments)
            .env("XDG_CONFIG_HOME", &config_home)
            // Keep the host's own NixOS module unit out of the result.
            .env("TONGUETYPED_SYSTEMD_USER_DIR", root.join("systemd-user"))
            .output()
            .unwrap()
    };

    assert!(!run(&["autostart", "status"]).status.success());

    assert!(run(&["autostart", "enable"]).status.success());
    assert_eq!(
        std::fs::read_to_string(&entry).unwrap(),
        include_str!("../data/tonguetyped-daemon.desktop")
    );
    let config = std::fs::read_to_string(config_home.join("tonguetyped/config.toml")).unwrap();
    assert!(toml::from_str::<Config>(&config).unwrap().startup.autostart);

    std::fs::write(&entry, "old packaged entry").unwrap();
    assert!(run(&["autostart", "enable"]).status.success());
    assert_eq!(
        std::fs::read_to_string(&entry).unwrap(),
        include_str!("../data/tonguetyped-daemon.desktop")
    );

    assert!(run(&["autostart", "disable"]).status.success());
    assert!(run(&["autostart", "disable"]).status.success());
    assert!(!entry.exists());
    let config = std::fs::read_to_string(config_home.join("tonguetyped/config.toml")).unwrap();
    assert!(!toml::from_str::<Config>(&config).unwrap().startup.autostart);

    assert!(run(&["autostart", "enable"]).status.success());
    assert!(entry.exists());

    std::fs::remove_dir_all(root).unwrap();
}

#[test]
fn enable_refuses_when_a_systemd_unit_owns_autostart() {
    let root = std::env::temp_dir().join(format!(
        "tt-autostart-systemd-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    let config_home = root.join("config");
    let entry = config_home.join("autostart/tonguetyped.desktop");
    let unit = config_home.join("systemd/user/tonguetyped.service");
    std::fs::create_dir_all(unit.parent().unwrap()).unwrap();
    std::fs::write(&unit, "[Service]\n").unwrap();

    let output = Command::new(env!("CARGO_BIN_EXE_tonguetyped"))
        .args(["autostart", "enable"])
        .env("XDG_CONFIG_HOME", &config_home)
        .env("TONGUETYPED_SYSTEMD_USER_DIR", root.join("systemd-user"))
        .output()
        .unwrap();

    assert!(
        !output.status.success(),
        "enable must fail while a systemd unit owns autostart"
    );
    let stderr = String::from_utf8(output.stderr).unwrap();
    let unit_display = unit.display().to_string();
    assert!(
        stderr.contains("owned by the systemd unit") && stderr.contains(&unit_display),
        "the refusal must name the unit that owns autostart:\n{stderr}"
    );
    assert!(
        !entry.exists(),
        "a refused enable must not write the XDG entry"
    );
    std::fs::remove_dir_all(root).unwrap();
}

#[test]
fn disable_removes_entry_without_rewriting_invalid_config() {
    let invalid_configs: &[&[u8]] = &[
        b"[startup\nautostart = true\n",
        b"[startup]\nautostart = true\n\n[transcription]\nmax_recording_seconds = 0\n",
    ];
    let binary = env!("CARGO_BIN_EXE_tonguetyped");

    for (index, invalid_config) in invalid_configs.iter().enumerate() {
        let root = std::env::temp_dir().join(format!(
            "tt-autostart-invalid-{index}-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        let config_home = root.join("config");
        let config_path = config_home.join("tonguetyped/config.toml");
        let entry = config_home.join("autostart/tonguetyped.desktop");
        std::fs::create_dir_all(config_path.parent().unwrap()).unwrap();
        std::fs::create_dir_all(entry.parent().unwrap()).unwrap();
        std::fs::write(&config_path, invalid_config).unwrap();
        std::fs::write(&entry, include_bytes!("../data/tonguetyped-daemon.desktop")).unwrap();

        let output = Command::new(binary)
            .args(["autostart", "disable"])
            .env("XDG_CONFIG_HOME", &config_home)
            .output()
            .unwrap();

        assert!(output.status.success());
        assert!(!entry.exists());
        assert_eq!(std::fs::read(&config_path).unwrap(), *invalid_config);
        std::fs::remove_dir_all(root).unwrap();
    }
}

#[test]
fn concurrent_commands_leave_config_and_entry_consistent() {
    let root = std::env::temp_dir().join(format!(
        "tt-autostart-concurrent-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    let config_home = root.join("config");
    let entry = config_home.join("autostart/tonguetyped.desktop");
    let binary = env!("CARGO_BIN_EXE_tonguetyped");

    for _ in 0..20 {
        let mut enable = Command::new(binary)
            .args(["autostart", "enable"])
            .env("XDG_CONFIG_HOME", &config_home)
            .env("TONGUETYPED_SYSTEMD_USER_DIR", root.join("systemd-user"))
            .stdout(Stdio::null())
            .spawn()
            .unwrap();
        let mut disable = Command::new(binary)
            .args(["autostart", "disable"])
            .env("XDG_CONFIG_HOME", &config_home)
            .env("TONGUETYPED_SYSTEMD_USER_DIR", root.join("systemd-user"))
            .stdout(Stdio::null())
            .spawn()
            .unwrap();
        assert!(enable.wait().unwrap().success());
        assert!(disable.wait().unwrap().success());

        let content = std::fs::read_to_string(config_home.join("tonguetyped/config.toml")).unwrap();
        let config = toml::from_str::<Config>(&content).unwrap();
        assert_eq!(config.startup.autostart, entry.exists());
    }

    std::fs::remove_dir_all(root).unwrap();
}
