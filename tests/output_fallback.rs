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
        Self::build(name, output, |log| {
            format!(
                "#!/bin/sh\n{}echo 'dotool: failed to open /dev/uinput' >&2\nexit {dotool_status}\n",
                record_into(log)
            )
        })
    }

    /// Like `new`, but the `dotool` stub exits non-zero only for the
    /// invocation carrying the trailing Enter, so a successful type is
    /// followed by a failed submit.
    fn new_failing_submit(name: &str, output: &str) -> Self {
        Self::build(name, output, |log| {
            format!(
                "#!/bin/sh\nstatus=0\nwhile IFS= read -r line || [ -n \"$line\" ]; do printf '%s\\n' \"$line\" >> '{}'; case \"$line\" in 'key enter') status=1;; esac; done\necho 'dotool: failed to open /dev/uinput' >&2\nexit $status\n",
                log.display()
            )
        })
    }

    fn build(name: &str, output: &str, dotool_script: impl FnOnce(&Path) -> String) -> Self {
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
        write_script(
            &bin.join("wl-copy"),
            &format!("#!/bin/sh\n{}", record_into(&root.join("clipboard"))),
        );
        write_script(
            &bin.join("dotool"),
            &dotool_script(&root.join("dotool.log")),
        );
        Self { root }
    }

    fn run(&self) -> std::process::Output {
        Command::new(env!("CARGO_BIN_EXE_tonguetyped"))
            .args(["doctor", "--test-type"])
            .env("XDG_CONFIG_HOME", self.root.join("config"))
            .env("XDG_DATA_HOME", self.root.join("data"))
            .env("XDG_SESSION_TYPE", "wayland")
            // A sandboxed PATH plus a D-Bus address that points nowhere: no
            // test may reach the operator's real session bus or compositor, so
            // the libei backend can only fail here, never pop a portal dialog.
            .env("PATH", self.root.join("bin"))
            .env(
                "DBUS_SESSION_BUS_ADDRESS",
                "unix:path=/nonexistent-tonguetyped-test",
            )
            .env_remove("WAYLAND_DISPLAY")
            .env_remove("DISPLAY")
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

/// A shell fragment that appends every stdin line to `file`.
fn record_into(file: &Path) -> String {
    format!(
        "while IFS= read -r line || [ -n \"$line\" ]; do printf '%s\\n' \"$line\" >> '{}'; done\n",
        file.display()
    )
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
fn native_libei_typing_without_a_portal_falls_back_to_the_clipboard() {
    // The KDE/GNOME Wayland case with no working portal session: the native
    // libei backend cannot connect, so the transcript must land on the
    // clipboard and the failure must be reported, not crash the process. The
    // sandbox's D-Bus address points nowhere, so this exercises the graceful
    // degradation without ever touching a real portal.
    let sandbox = Sandbox::new(
        "libei-fallback",
        "method = \"type\"\ntyping_backend = \"libei\"\nauto_submit = true\n",
        0,
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
    assert_eq!(
        sandbox.read("dotool.log"),
        None,
        "the libei backend must not fall back to dotool"
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

#[test]
fn a_failed_submit_after_typing_leaves_the_clipboard_untouched() {
    // The text is already in the focused application when the trailing Enter
    // fails; copying it again would clobber the user's clipboard and invite a
    // duplicate paste, so the failure is reported without a fallback.
    let sandbox = Sandbox::new_failing_submit(
        "submit-fails",
        "method = \"type\"\ntyping_backend = \"dotool\"\nauto_submit = true\n",
    );
    let output = sandbox.run();
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(stdout.contains("pressing Enter failed"), "{stdout}");
    assert_eq!(
        sandbox.read("clipboard"),
        None,
        "the text was typed, so the fallback must not copy"
    );
    assert_eq!(
        sandbox.read("dotool.log").as_deref(),
        Some("type TongueTyped typing test\nkey enter\n")
    );
}
