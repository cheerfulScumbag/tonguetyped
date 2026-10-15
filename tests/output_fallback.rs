//! End-to-end output through the real binary: `tonguetyped doctor --test-type`
//! sends a test transcript through the configured output method, with stub
//! `dotool`, `eitype`, and `wl-copy` helpers on a sandboxed PATH that record
//! what they receive.

use std::path::{Path, PathBuf};
use std::process::Command;

struct Sandbox {
    root: PathBuf,
}

impl Sandbox {
    /// A config home holding `output` as the `[output]` table, and a bin
    /// directory with a `wl-copy` stub that saves its input to `clipboard`
    /// and a `dotool` stub that saves its commands to `dotool.log`, then
    /// exits with `dotool_status`. No `eitype` stub is installed.
    fn new(name: &str, output: &str, dotool_status: u8) -> Self {
        Self::build(
            name,
            output,
            Some(|log: &Path| {
                format!(
                    "#!/bin/sh\n{}echo 'dotool: failed to open /dev/uinput' >&2\nexit {dotool_status}\n",
                    record_into(log)
                )
            }),
            None,
        )
    }

    /// Like `new`, but the `dotool` stub exits non-zero only for the
    /// invocation carrying the trailing Enter, so a successful type is
    /// followed by a failed submit.
    fn new_failing_submit(name: &str, output: &str) -> Self {
        Self::build(
            name,
            output,
            Some(|log: &Path| {
                format!(
                    "#!/bin/sh\nstatus=0\nwhile IFS= read -r line || [ -n \"$line\" ]; do printf '%s\\n' \"$line\" >> '{}'; case \"$line\" in 'key enter') status=1;; esac; done\necho 'dotool: failed to open /dev/uinput' >&2\nexit $status\n",
                    log.display()
                )
            }),
            None,
        )
    }

    /// A bin directory with an `eitype` stub but no `dotool`, so the `auto`
    /// probe has exactly one backend it can find. The stub records its argv,
    /// space-joined, one line per invocation to `eitype.log`, then exits with
    /// `eitype_status`.
    fn new_eitype(name: &str, output: &str, eitype_status: u8) -> Self {
        Self::build(
            name,
            output,
            None::<fn(&Path) -> String>,
            Some(eitype_status),
        )
    }

    fn build(
        name: &str,
        output: &str,
        dotool_script: Option<impl FnOnce(&Path) -> String>,
        eitype_status: Option<u8>,
    ) -> Self {
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
        if let Some(dotool_script) = dotool_script {
            write_script(
                &bin.join("dotool"),
                &dotool_script(&root.join("dotool.log")),
            );
        }
        if let Some(status) = eitype_status {
            write_script(
                &bin.join("eitype"),
                &format!(
                    "#!/bin/sh\nprintf '%s\\n' \"$*\" >> '{}'\nexit {status}\n",
                    root.join("eitype.log").display()
                ),
            );
        }
        Self { root }
    }

    fn run(&self) -> std::process::Output {
        self.run_with_desktop(None)
    }

    /// Like `run`, but with `XDG_CURRENT_DESKTOP` forced, so a
    /// compositor-dependent branch (the KDE `eitype` delay) is deterministic
    /// instead of depending on the test host's own session.
    fn run_as(&self, desktop: &str) -> std::process::Output {
        self.run_with_desktop(Some(desktop))
    }

    fn run_with_desktop(&self, desktop: Option<&str>) -> std::process::Output {
        let mut command = Command::new(env!("CARGO_BIN_EXE_tonguetyped"));
        command
            .args(["doctor", "--test-type"])
            .env("XDG_CONFIG_HOME", self.root.join("config"))
            .env("XDG_SESSION_TYPE", "wayland")
            .env("PATH", self.root.join("bin"))
            .env_remove("XDG_CURRENT_DESKTOP")
            .env_remove("XDG_SESSION_DESKTOP")
            .env_remove("DESKTOP_SESSION");
        if let Some(desktop) = desktop {
            command.env("XDG_CURRENT_DESKTOP", desktop);
        }
        command.output().unwrap()
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

#[test]
fn the_auto_probe_finds_eitype_when_dotool_cannot_work() {
    // KDE/GNOME Wayland without /dev/uinput access: dotool and wtype are
    // unavailable, so the auto probe must fall through to the installed
    // eitype instead of reporting no helper.
    let sandbox = Sandbox::new_eitype(
        "auto-eitype",
        "method = \"type\"\ntyping_backend = \"auto\"\n",
        0,
    );
    let output = sandbox.run();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(String::from_utf8_lossy(&output.stdout).contains("Typed the test text"));
    assert_eq!(
        sandbox.read("eitype.log").as_deref(),
        Some("TongueTyped typing test\n")
    );
}

#[test]
fn a_failing_eitype_falls_back_to_the_clipboard() {
    // eitype present but its portal call fails (denied, or no EIS support):
    // the non-zero exit must flow through the existing fallback, leaving the
    // transcript on the clipboard and pressing no Enter.
    let sandbox = Sandbox::new_eitype(
        "eitype-fails",
        "method = \"type\"\ntyping_backend = \"eitype\"\nauto_submit = true\n",
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
        sandbox.read("eitype.log").as_deref(),
        Some("TongueTyped typing test\n"),
        "eitype was attempted before the fallback"
    );
    assert_eq!(
        sandbox.read("clipboard").as_deref(),
        Some("TongueTyped typing test\n")
    );
}

#[test]
fn eitype_typing_on_kde_forces_a_small_inter_key_delay() {
    // KWin can drop the tail of a fast key burst, so typing on KDE must pass
    // eitype's smallest positive inter-key delay.
    let sandbox = Sandbox::new_eitype(
        "kde-eitype",
        "method = \"type\"\ntyping_backend = \"eitype\"\n",
        0,
    );
    let output = sandbox.run_as("KDE");
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert_eq!(
        sandbox.read("eitype.log").as_deref(),
        Some("-d 1 TongueTyped typing test\n")
    );
}

#[test]
fn eitype_paste_and_enter_use_the_portal_key_syntax() {
    let sandbox = Sandbox::new_eitype(
        "eitype-paste",
        "method = \"paste\"\ntyping_backend = \"eitype\"\nauto_submit = true\npaste_keys = \"shift-insert\"\n",
        0,
    );
    let output = sandbox.run();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(String::from_utf8_lossy(&output.stdout).contains("Pasted the test text"));
    // `eitype` is invoked once for the paste keys and once for the trailing
    // Enter, each as its own argv, with no explicit modifier release.
    assert_eq!(
        sandbox.read("eitype.log").as_deref(),
        Some("-M shift -k Insert\n-k return\n")
    );
    assert_eq!(
        sandbox.read("clipboard").as_deref(),
        Some("TongueTyped typing test\n")
    );
}
