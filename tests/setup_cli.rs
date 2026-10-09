use std::io::Write;
use std::process::{Command, Stdio};

fn root(name: &str) -> std::path::PathBuf {
    std::env::temp_dir().join(format!(
        "tonguetyped-setup-{name}-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ))
}

fn run_setup(
    config_home: &std::path::Path,
    answers: &str,
    extra_path: Option<&std::path::Path>,
) -> std::process::Output {
    let mut command = Command::new(env!("CARGO_BIN_EXE_tonguetyped"));
    command
        .arg("setup")
        .env("XDG_CONFIG_HOME", config_home)
        .env("NO_COLOR", "1")
        // Pin the session so `enigo` (X11-only) can never count as a typing
        // helper on a developer's desktop, and replace PATH entirely when a
        // bin directory is given so helper detection only sees its stubs.
        .env("XDG_SESSION_TYPE", "wayland")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    if let Some(extra_path) = extra_path {
        command.env("PATH", extra_path);
    }
    let mut child = command.spawn().unwrap();
    child
        .stdin
        .take()
        .unwrap()
        .write_all(answers.as_bytes())
        .unwrap();
    child.wait_with_output().unwrap()
}

#[test]
fn setup_writes_a_complete_configuration_without_color() {
    let root = root("success");
    let bin = root.join("bin");
    std::fs::create_dir_all(&bin).unwrap();
    let wtype = bin.join("wtype");
    std::fs::write(&wtype, "#!/bin/sh\nexit 0\n").unwrap();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&wtype, std::fs::Permissions::from_mode(0o755)).unwrap();
    }
    let output = run_setup(&root, "\n\n2\nCtrl+Shift+Space\n2\n2\n2\n\n", Some(&bin));
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let stdout = String::from_utf8(output.stdout).unwrap();
    assert!(stdout.contains("Configuration saved."));
    assert!(!stdout.contains('\u{1b}'));

    let content = std::fs::read_to_string(root.join("tonguetyped/config.toml")).unwrap();
    let config: tonguetyped::config::Config = toml::from_str(&content).unwrap();
    config.validate().unwrap();
    assert_eq!(
        config.activation.mode,
        tonguetyped::config::ActivationMode::Toggle
    );
    assert_eq!(config.activation.keybind, "Ctrl+Shift+Space");
    assert_eq!(
        config.output.method,
        tonguetyped::config::OutputMethod::Type
    );
    assert_eq!(config.output.typing_backend, "wtype");
    assert!(config.startup.autostart);
    assert_eq!(
        std::fs::read_to_string(root.join("autostart/tonguetyped.desktop")).unwrap(),
        include_str!("../data/tonguetyped.desktop")
    );

    let output = run_setup(&root, "\n\n\n\n\n\n1\n\n", Some(&bin));
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let content = std::fs::read_to_string(root.join("tonguetyped/config.toml")).unwrap();
    let config: tonguetyped::config::Config = toml::from_str(&content).unwrap();
    assert!(!config.startup.autostart);
    assert!(!root.join("autostart/tonguetyped.desktop").exists());
    std::fs::remove_dir_all(root).unwrap();
}

#[test]
fn setup_refuses_typing_without_a_helper_and_says_what_to_install() {
    let root = root("no-helper");
    let bin = root.join("bin");
    std::fs::create_dir_all(&bin).unwrap();

    // Answer 2 at "Transcript output" (type) while the sandbox PATH has no
    // helper: the setup must refuse it with the install warning, re-prompt,
    // then accept 1 (keep in TongueTyped) and finish normally.
    let output = run_setup(&root, "\n\n\n\n2\n1\n\n\n", Some(&bin));
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let stdout = String::from_utf8(output.stdout).unwrap();
    let stderr = String::from_utf8(output.stderr).unwrap();
    assert!(
        stdout.contains("Keep transcripts in TongueTyped")
            && stdout.contains("Type into the focused application"),
        "both transcript output choices must be listed even with no helper:\n{stdout}"
    );
    assert!(
        stdout.contains("install wtype"),
        "the warning must say what to install, before the choices:\n{stdout}"
    );
    assert!(
        stderr.contains("install wtype"),
        "the refused choice must repeat what to install:\n{stderr}"
    );

    let content = std::fs::read_to_string(root.join("tonguetyped/config.toml")).unwrap();
    let config: tonguetyped::config::Config = toml::from_str(&content).unwrap();
    assert_eq!(
        config.output.method,
        tonguetyped::config::OutputMethod::None,
        "a refused type choice must not be saved"
    );
    std::fs::remove_dir_all(root).unwrap();
}

#[test]
fn cancellation_preserves_the_existing_file_byte_for_byte() {
    let root = root("cancel");
    let directory = root.join("tonguetyped");
    std::fs::create_dir_all(&directory).unwrap();
    let original = "# keep this comment\n[activation]\nmode = \"toggle\"\n";
    std::fs::write(directory.join("config.toml"), original).unwrap();

    let output = run_setup(&root, "\nq\n", None);
    assert!(output.status.success());
    assert!(String::from_utf8(output.stdout)
        .unwrap()
        .contains("No changes were made"));
    assert_eq!(
        std::fs::read_to_string(directory.join("config.toml")).unwrap(),
        original
    );
    std::fs::remove_dir_all(root).unwrap();
}

#[test]
fn invalid_answers_are_rejected_and_eof_does_not_write() {
    let root = root("invalid");
    let output = run_setup(&root, "99\n", None);
    assert!(output.status.success());
    assert!(String::from_utf8(output.stderr)
        .unwrap()
        .contains("Invalid choice"));
    assert!(!root.join("tonguetyped/config.toml").exists());
    std::fs::remove_dir_all(root).unwrap();
}

#[test]
fn successful_write_leaves_only_the_complete_destination() {
    let root = root("atomic");
    let directory = root.join("tonguetyped");
    std::fs::create_dir_all(&directory).unwrap();
    std::fs::write(
        directory.join("config.toml"),
        "[activation]\nmode = \"hold\"\n",
    )
    .unwrap();

    let output = run_setup(&root, "\n\n\n\n\n\n\n", None);
    assert!(output.status.success());
    let entries: Vec<_> = std::fs::read_dir(&directory)
        .unwrap()
        .map(|entry| entry.unwrap().file_name())
        .collect();
    assert_eq!(entries, [std::ffi::OsString::from("config.toml")]);
    let content = std::fs::read_to_string(directory.join("config.toml")).unwrap();
    let config: tonguetyped::config::Config = toml::from_str(&content).unwrap();
    config.validate().unwrap();
    std::fs::remove_dir_all(root).unwrap();
}
