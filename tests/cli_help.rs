//! Asserts that making the top-level subcommand optional (so bare
//! `tonguetyped` opens the setup console - see `tests/setup_console.rs`) left
//! `--help`, `--version`, and every existing subcommand's name/description
//! untouched. These spawn the real compiled binary; none of this greps
//! source.

use std::process::Command;

const ALL_COMMANDS_WITH_DESCRIPTIONS: &[(&str, &str)] = &[
    ("setup", "Configure TongueTyped interactively"),
    ("daemon", "Start the daemon process"),
    ("start", "Start a new recording"),
    ("stop", "Stop the current recording"),
    ("toggle", "Toggle recording on/off"),
    ("cancel", "Cancel current recording or processing"),
    ("status", "Get daemon status"),
    ("reload", "Reload daemon configuration"),
    ("last-result", "Return the most recent transcription"),
    (
        "transcribe",
        "Transcribe an audio file (WAV) and print the transcript",
    ),
    ("doctor", "Run system diagnostics"),
    (
        "shortcut-test",
        "Bind the desktop shortcut and wait for you to actually press it",
    ),
    ("autostart", "Manage desktop-session autostart"),
    (
        "model",
        "Manage the GGUF speech model catalog used for inference",
    ),
];

#[test]
fn help_lists_every_command_with_its_existing_description_plus_help_and_version() {
    let output = Command::new(env!("CARGO_BIN_EXE_tonguetyped"))
        .arg("--help")
        .output()
        .unwrap();
    assert!(output.status.success());
    let stdout = String::from_utf8(output.stdout).unwrap();

    assert!(stdout.contains("Usage: tonguetyped"), "{stdout}");
    assert!(stdout.contains("Linux dictation application"), "{stdout}");
    assert!(stdout.contains("Commands:"), "{stdout}");
    for (name, description) in ALL_COMMANDS_WITH_DESCRIPTIONS {
        assert!(
            stdout.contains(name) && stdout.contains(description),
            "missing {name:?} ({description:?}) in --help output:\n{stdout}"
        );
    }
    assert!(stdout.contains("-h, --help"), "{stdout}");
    assert!(stdout.contains("Print help"), "{stdout}");
    assert!(stdout.contains("-V, --version"), "{stdout}");
    assert!(stdout.contains("Print version"), "{stdout}");
}

#[test]
fn version_flag_still_prints_the_version() {
    let output = Command::new(env!("CARGO_BIN_EXE_tonguetyped"))
        .arg("--version")
        .output()
        .unwrap();
    assert!(output.status.success());
    let stdout = String::from_utf8(output.stdout).unwrap();
    assert!(stdout.starts_with("tonguetyped "), "{stdout}");
}

#[test]
fn every_subcommand_remains_directly_invocable_and_known_to_clap() {
    // Each of these must still parse as a recognized subcommand (not require
    // the no-longer-existing bare-invocation error) - `--help` on a
    // subcommand is a side-effect-free way to prove clap still recognizes
    // it without actually running the (daemon-dependent) action.
    for (name, _) in ALL_COMMANDS_WITH_DESCRIPTIONS {
        let output = Command::new(env!("CARGO_BIN_EXE_tonguetyped"))
            .args([*name, "--help"])
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "`tonguetyped {name} --help` failed: {}",
            String::from_utf8_lossy(&output.stderr)
        );
    }
}

#[test]
fn no_subcommand_without_a_terminal_runs_the_same_setup_as_the_setup_command() {
    // Bare invocation used to fail clap's required-subcommand check with exit
    // code 2 and "Usage: tonguetyped <COMMAND>" on stderr. It now opens the
    // setup console, and with no terminal attached it falls back to the same
    // line-based setup `tonguetyped setup` runs, which an empty stdin cancels
    // without writing anything.
    let root = std::env::temp_dir().join(format!(
        "tonguetyped-bare-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    let run = |args: &[&str]| {
        Command::new(env!("CARGO_BIN_EXE_tonguetyped"))
            .args(args)
            .env("XDG_CONFIG_HOME", &root)
            .env("NO_COLOR", "1")
            .stdin(std::process::Stdio::null())
            .output()
            .unwrap()
    };
    let bare = run(&[]);
    let setup = run(&["setup"]);
    let stderr = String::from_utf8_lossy(&bare.stderr);
    assert!(bare.status.success(), "stderr was: {stderr}");
    assert!(
        !stderr.contains("Usage: tonguetyped <COMMAND>"),
        "stderr was: {stderr}"
    );
    let stdout = String::from_utf8_lossy(&bare.stdout);
    assert!(stdout.contains("TongueTyped setup"), "stdout was: {stdout}");
    assert_eq!(stdout, String::from_utf8_lossy(&setup.stdout));
    assert!(!root.join("tonguetyped/config.toml").exists());
    let _ = std::fs::remove_dir_all(&root);
}
