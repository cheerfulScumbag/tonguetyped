//! End-to-end output through the real binary: `tonguetyped doctor --test-type`
//! sends a test transcript through the configured output method, with stub
//! `dotool` and `wl-copy` helpers on a sandboxed PATH that record what they
//! receive.

use std::path::{Path, PathBuf};
use std::process::Command;

struct Sandbox {
    root: PathBuf,
}

impl Sandbox {
    /// A config home holding `output` as the `[output]` table, and a bin
    /// directory with a `wl-copy` stub that saves its input to `clipboard`
    /// and a `dotool` stub that saves its commands to `dotool.log`, then
    /// exits with `dotool_status`.
    fn new(name: &str, output: &str, dotool_status: u8) -> Self {
        let root = std::env::temp_dir().join(format!(
            "tonguetyped-output-{name}-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        let config = root.join("config/tonguetyped");
        std::fs::create_dir_all(&config).unwrap();
        std::fs::write(config.join("config.toml"), format!("[output]\n{output}")).unwrap();
        let bin = root.join("bin");
        std::fs::create_dir_all(&bin).unwrap();
        // Shell builtins only: PATH holds nothing but these stubs.
        let record = |file: &Path| {
            format!(
                "while IFS= read -r line || [ -n \"$line\" ]; do printf '%s\\n' \"$line\" >> '{}'; done\n",
                file.display()
            )
        };
        write_script(
            &bin.join("wl-copy"),
            &format!("#!/bin/sh\n{}", record(&root.join("clipboard"))),
        );
        write_script(
            &bin.join("dotool"),
            &format!(
                "#!/bin/sh\n{}echo 'dotool: failed to open /dev/uinput' >&2\nexit {dotool_status}\n",
                record(&root.join("dotool.log"))
            ),
        );
        Self { root }
    }

    fn run(&self) -> std::process::Output {
        Command::new(env!("CARGO_BIN_EXE_tonguetyped"))
            .args(["doctor", "--test-type"])
            .env("XDG_CONFIG_HOME", self.root.join("config"))
            .env("XDG_SESSION_TYPE", "wayland")
            .env("PATH", self.root.join("bin"))
            .output()
            .unwrap()
    }

    fn read(&self, name: &str) -> Option<String> {
        std::fs::read_to_string(self.root.join(name)).ok()
    }
}

impl Drop for Sandbox {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.root);
    }
}

fn write_script(path: &Path, content: &str) {
    std::fs::write(path, content).unwrap();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o755)).unwrap();
    }
}

#[test]
fn a_typing_failure_falls_back_to_the_clipboard() {
    // The KDE-from-.deb case: dotool cannot open /dev/uinput, so typing
    // fails. The transcript must still land on the clipboard, and Enter must
    // not be pressed into whatever has focus.
    let sandbox = Sandbox::new(
        "type-fails",
        "method = \"type\"\ntyping_backend = \"dotool\"\nauto_submit = true\n",
        1,
    );
    let output = sandbox.run();
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        output.status.success(),
        "a fallback is a success: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(
        stdout.contains("Typing failed")
            && stdout.contains("copied the test text to the clipboard"),
        "{stdout}"
    );
    assert_eq!(
        sandbox.read("clipboard").as_deref(),
        Some("TongueTyped typing test\n")
    );
}

#[test]
fn clipboard_mode_copies_without_typing() {
    let sandbox = Sandbox::new(
        "clipboard",
        "method = \"clipboard\"\ntyping_backend = \"dotool\"\n",
        0,
    );
    let output = sandbox.run();
    assert!(output.status.success());
    assert!(String::from_utf8_lossy(&output.stdout).contains("Copied the test text"));
    assert_eq!(
        sandbox.read("clipboard").as_deref(),
        Some("TongueTyped typing test\n")
    );
    assert_eq!(
        sandbox.read("dotool.log"),
        None,
        "clipboard mode never types"
    );
}

#[test]
fn paste_and_enter_copies_then_presses_the_paste_keys_and_enter() {
    let sandbox = Sandbox::new(
        "paste",
        "method = \"paste\"\ntyping_backend = \"dotool\"\nauto_submit = true\npaste_keys = \"shift-insert\"\n",
        0,
    );
    let output = sandbox.run();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(String::from_utf8_lossy(&output.stdout).contains("Pasted the test text"));
    assert_eq!(
        sandbox.read("clipboard").as_deref(),
        Some("TongueTyped typing test\n")
    );
    assert_eq!(
        sandbox.read("dotool.log").as_deref(),
        Some("key shift+insert\nkey enter\n")
    );
}

#[test]
fn failed_paste_keys_still_leave_the_text_on_the_clipboard() {
    let sandbox = Sandbox::new(
        "paste-fails",
        "method = \"paste\"\ntyping_backend = \"dotool\"\n",
        1,
    );
    let output = sandbox.run();
    assert!(output.status.success());
    assert!(String::from_utf8_lossy(&output.stdout).contains("Typing failed"));
    assert_eq!(
        sandbox.read("clipboard").as_deref(),
        Some("TongueTyped typing test\n")
    );
}
