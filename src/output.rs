use crate::config::OutputMethod;
use anyhow::Context;
use std::process::Command;
use std::sync::OnceLock;

pub fn output_text(
    text: &str,
    method: &OutputMethod,
    backend: &str,
    auto_submit: bool,
) -> anyhow::Result<()> {
    match method {
        OutputMethod::None => {
            tracing::debug!(
                "output method 'none': skipping output of {} chars",
                text.len()
            );
        }
        OutputMethod::Type => {
            type_text(text, backend, auto_submit)?;
        }
    }
    Ok(())
}

fn type_text(text: &str, backend: &str, auto_submit: bool) -> anyhow::Result<()> {
    let backend = if backend == "auto" {
        cached_auto_backend()
    } else {
        backend.to_string()
    };

    match backend.as_str() {
        "wtype" => {
            let mut child = Command::new("wtype")
                .args(["-"])
                .stdin(std::process::Stdio::piped())
                .spawn()
                .context("failed to spawn wtype")?;
            if let Some(mut stdin) = child.stdin.take() {
                use std::io::Write;
                stdin.write_all(text.as_bytes())?;
            }
            require_success("wtype", child.wait()?)?;
            if auto_submit {
                require_success(
                    "wtype",
                    Command::new("wtype").args(["-k", "Return"]).status()?,
                )?;
            }
        }
        "enigo" => {
            use enigo::{Enigo, Keyboard, Settings};
            let mut enigo = Enigo::new(&Settings::default())?;
            enigo.text(text)?;
            if auto_submit {
                enigo.key(enigo::Key::Return, enigo::Direction::Click)?;
            }
        }
        "dotool" => {
            let mut child = Command::new("dotool")
                .stdin(std::process::Stdio::piped())
                .spawn()
                .context("failed to spawn dotool")?;
            if let Some(mut stdin) = child.stdin.take() {
                write_dotool_commands(&mut stdin, text, auto_submit)?;
            }
            require_success("dotool", child.wait()?)?;
        }
        _ => {
            anyhow::bail!("unsupported typing backend: {}", backend);
        }
    }

    Ok(())
}

/// Memoized `probe_type_backend()`: which typing helper is actually installed
/// cannot change over a daemon process's lifetime, so re-running its
/// subprocess self-tests (observed costing over a second in the real
/// stop-to-idle latency log, since "auto" means probing wtype, then enigo,
/// then dotool on every single dictation) on every "auto"-backend output is
/// pure waste - same probe-then-cache shape as `inference::backend_info`'s
/// GPU device probe and `overlay::OverlayHandle`'s layer-shell probe.
fn cached_auto_backend() -> String {
    static BACKEND: OnceLock<String> = OnceLock::new();
    BACKEND.get_or_init(probe_type_backend).clone()
}

pub fn probe_type_backend() -> String {
    if helper_self_test("wtype") {
        return "wtype".to_string();
    }
    if enigo_available() {
        return "enigo".to_string();
    }
    if dotool_available() {
        return "dotool".to_string();
    }
    "none".to_string()
}

pub fn list_available_backends() -> Vec<String> {
    let mut backends = Vec::new();
    if helper_self_test("wtype") {
        backends.push("wtype".to_string());
    }
    if enigo_available() {
        backends.push("enigo".to_string());
    }
    if dotool_available() {
        backends.push("dotool".to_string());
    }
    backends
}

pub fn has_any_type_backend() -> bool {
    helper_self_test("wtype") || enigo_available() || dotool_available()
}

pub fn type_backend_available(backend: &str) -> bool {
    match backend {
        "auto" => has_any_type_backend(),
        "wtype" => helper_self_test("wtype"),
        "dotool" => dotool_available(),
        "enigo" => enigo_available(),
        _ => false,
    }
}

/// The warning the configuration UIs and the daemon startup log show when no
/// typing helper is usable, naming the one action that makes a helper work for
/// this compositor. `enigo` needs no install - it is built in - so the missing
/// piece is always one of the two external helpers: `wtype`, which types
/// through the Wayland virtual-keyboard protocol that only wlroots-based
/// compositors implement, or `dotool`, which types through `/dev/uinput` and
/// so works on KDE's KWin and other compositors too. When `dotool` is already
/// installed but `/dev/uinput` is not writable, the warning names the
/// permission remedy instead of telling the user to install a binary they
/// already have.
/// The restart note is real, not boilerplate: `cached_auto_backend` and the
/// daemon process both memoize the probe, so a helper installed while
/// TongueTyped is running is not picked up until it restarts.
pub fn typing_helper_warning() -> &'static str {
    let dotool_installed = helper_self_test("dotool");
    let uinput_writable = device_is_writable(std::path::Path::new("/dev/uinput"));
    warning_for(recommended_helper(), dotool_installed, uinput_writable)
}

/// The external helper to name for this session: `wtype` only on a Wayland
/// session whose compositor is wlroots-based (sway, Hyprland, niri, ...),
/// which implements the `zwp_virtual_keyboard_manager_v1` protocol `wtype`
/// needs. Everywhere else - KDE's KWin, other Wayland compositors, and X11 -
/// `dotool` is the helper that can work.
fn recommended_helper() -> &'static str {
    if session_is_wayland() && compositor_is_wlroots() {
        "wtype"
    } else {
        "dotool"
    }
}

fn warning_for(helper: &str, dotool_installed: bool, uinput_writable: bool) -> &'static str {
    if helper == "wtype" {
        "No typing helper found - install wtype, then restart TongueTyped."
    } else if dotool_installed && !uinput_writable {
        "dotool is installed but cannot open /dev/uinput - add your user to the 'input' group or add a udev rule granting write access to /dev/uinput, then restart TongueTyped."
    } else {
        "No typing helper found - install dotool (needs /dev/uinput access), then restart TongueTyped."
    }
}

fn session_is_wayland() -> bool {
    std::env::var("XDG_SESSION_TYPE").is_ok_and(|session| session == "wayland")
}

/// Whether this session's desktop identifies a wlroots-based compositor, the
/// only family that implements the `zwp_virtual_keyboard_manager_v1` protocol
/// `wtype` types through. KWin advertises `KDE`, not a wlroots name, so it
/// takes the `dotool` branch.
fn compositor_is_wlroots() -> bool {
    let desktops: Vec<String> = [
        "XDG_CURRENT_DESKTOP",
        "XDG_SESSION_DESKTOP",
        "DESKTOP_SESSION",
    ]
    .iter()
    .filter_map(|key| std::env::var(key).ok())
    .collect();
    desktop_names_are_wlroots(&desktops)
}

/// Pure core of `compositor_is_wlroots`, kept separate so it is testable
/// without mutating the process-global environment.
fn desktop_names_are_wlroots(desktops: &[String]) -> bool {
    const WLROOTS: [&str; 7] = [
        "sway", "hyprland", "niri", "wlroots", "river", "wayfire", "labwc",
    ];
    desktops.iter().any(|desktop| {
        let desktop = desktop.to_ascii_lowercase();
        WLROOTS.iter().any(|name| desktop.contains(name))
    })
}

fn enigo_available() -> bool {
    if session_is_wayland() {
        return false;
    }
    use enigo::{Enigo, Settings};
    Enigo::new(&Settings::default()).is_ok()
}

/// Whether `dotool` is present *and* can actually inject. It types through
/// `/dev/uinput`, so a binary that is installed but whose user cannot open
/// that device (no `input` group membership or matching udev rule) would
/// silently type nothing - a plain binary-exists check is not enough. Opening
/// the device for writing is side-effect-free: a virtual device is only
/// created by a later `UI_DEV_CREATE` ioctl, so this probe leaks no node.
fn dotool_available() -> bool {
    helper_self_test("dotool") && device_is_writable(std::path::Path::new("/dev/uinput"))
}

fn device_is_writable(path: &std::path::Path) -> bool {
    std::fs::OpenOptions::new().write(true).open(path).is_ok()
}

fn write_dotool_commands(
    writer: &mut impl std::io::Write,
    text: &str,
    auto_submit: bool,
) -> std::io::Result<()> {
    for (index, line) in text.split('\n').enumerate() {
        if index > 0 {
            writeln!(writer, "key enter")?;
        }
        writeln!(writer, "type {line}")?;
    }
    if auto_submit {
        writeln!(writer, "key enter")?;
    }
    Ok(())
}

fn require_success(backend: &str, status: std::process::ExitStatus) -> anyhow::Result<()> {
    if !status.success() {
        anyhow::bail!("{} exited with status {}", backend, status);
    }
    Ok(())
}

fn helper_self_test(command: &str) -> bool {
    let mut command = Command::new(command);
    if command.get_program() == "wtype" {
        command.arg("-");
    }
    command
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status()
        .is_ok_and(|status| status.success())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_none_output() {
        let result = output_text("test", &OutputMethod::None, "auto", false);
        assert!(result.is_ok());
    }

    #[test]
    fn test_probe_returns_string() {
        let backend = probe_type_backend();
        assert!(!backend.is_empty());
    }

    #[test]
    fn typing_helper_warning_names_a_helper_to_install() {
        let warning = typing_helper_warning();
        assert!(
            warning.contains("install wtype")
                || warning.contains("install dotool")
                || warning.contains("input"),
            "the warning must name a concrete remedy: {warning}"
        );
        assert!(
            warning.contains("restart TongueTyped"),
            "the warning must say a restart is needed: {warning}"
        );
    }

    #[test]
    fn wlroots_compositors_recommend_wtype_and_others_dotool() {
        // Only wlroots-based compositors implement the virtual-keyboard
        // protocol wtype needs; KDE's KWin and everything else must get
        // dotool instead.
        for (desktop, wlroots) in [
            ("sway", true),
            ("Hyprland", true),
            ("niri", true),
            ("wlroots", true),
            ("KDE", false),
            ("GNOME", false),
            ("ubuntu:GNOME", false),
        ] {
            let names = vec![desktop.to_string()];
            assert_eq!(desktop_names_are_wlroots(&names), wlroots, "{desktop}");
        }
    }

    #[test]
    fn dotool_warning_states_the_uinput_requirement() {
        let dotool = warning_for("dotool", false, false);
        assert!(dotool.contains("install dotool"));
        assert!(
            dotool.contains("/dev/uinput"),
            "the dotool warning must state its device requirement: {dotool}"
        );
        let wtype = warning_for("wtype", false, false);
        assert!(wtype.contains("install wtype"));
        assert!(!wtype.contains("dotool"));
        assert!(!dotool.contains("wtype"));
    }

    #[test]
    fn dotool_warning_names_the_permission_remedy_when_installed() {
        // `dotool` present but `/dev/uinput` unreadable is the state the
        // packaged install creates when it grants no `input` group or udev
        // access: the warning must not tell the user to install it again.
        let warning = warning_for("dotool", true, false);
        assert!(
            !warning.contains("install dotool"),
            "an installed dotool must not be reported missing: {warning}"
        );
        assert!(
            warning.contains("/dev/uinput") && warning.contains("input"),
            "the permission warning must name /dev/uinput and the input group: {warning}"
        );
        assert!(
            warning.contains("udev") || warning.contains("rule"),
            "the permission warning must name the udev-rule alternative: {warning}"
        );
    }

    #[test]
    fn device_is_writable_reflects_real_write_access() {
        let dir = std::env::temp_dir().join(format!(
            "tt-uinput-probe-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        let file = dir.join("uinput");
        std::fs::write(&file, b"").unwrap();
        assert!(device_is_writable(&file), "a writable file must pass");
        assert!(
            !device_is_writable(&dir.join("missing")),
            "a missing device must fail the dotool probe"
        );
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn auto_backend_choice_is_memoized_across_calls() {
        let first = cached_auto_backend();
        let second = cached_auto_backend();
        assert_eq!(first, second);
        assert_eq!(first, probe_type_backend());
    }

    #[test]
    fn dotool_encodes_newlines_as_keys() {
        let mut commands = Vec::new();
        write_dotool_commands(&mut commands, "notes\nkey ctrl+a", true).unwrap();
        assert_eq!(
            String::from_utf8(commands).unwrap(),
            "type notes\nkey enter\ntype key ctrl+a\nkey enter\n"
        );
    }
}
