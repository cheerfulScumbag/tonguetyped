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
            .output()
            .unwrap()
    };

    assert!(!run(&["autostart", "status"]).status.success());

    assert!(run(&["autostart", "enable"]).status.success());
    assert_eq!(
        std::fs::read_to_string(&entry).unwrap(),
        include_str!("../data/tonguetyped.desktop")
    );
    let config = std::fs::read_to_string(config_home.join("tonguetyped/config.toml")).unwrap();
    assert!(toml::from_str::<Config>(&config).unwrap().startup.autostart);

    std::fs::write(&entry, "old packaged entry").unwrap();
    assert!(run(&["autostart", "enable"]).status.success());
    assert_eq!(
        std::fs::read_to_string(&entry).unwrap(),
        include_str!("../data/tonguetyped.desktop")
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
        std::fs::write(&entry, include_bytes!("../data/tonguetyped.desktop")).unwrap();

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
            .stdout(Stdio::null())
            .spawn()
            .unwrap();
        let mut disable = Command::new(binary)
            .args(["autostart", "disable"])
            .env("XDG_CONFIG_HOME", &config_home)
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
