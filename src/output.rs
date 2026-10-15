use crate::config::{OutputConfig, OutputMethod, PasteKeys};
use anyhow::Context;
use std::process::Command;
use std::sync::OnceLock;
use std::time::Duration;

/// How long `paste` mode waits between pressing the paste keys and pressing
/// Enter. Applications read the clipboard asynchronously after the paste
/// keys, so an Enter sent straight away can submit before the text arrives.
const PASTE_SETTLE: Duration = Duration::from_millis(200);

/// Where a transcript ended up. `Kept` holds the transcript in TongueTyped
/// only; every other variant puts it somewhere the user can reach outside
/// TongueTyped's own history. An `Err` from `output_text` means neither
/// insertion nor clipboard copy succeeded, though the text is still in the
/// history and `last-result`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Delivery {
    /// `none` mode: the transcript stays in TongueTyped only.
    Kept,
    /// Typed or pasted into the focused application.
    Inserted,
    /// `clipboard` mode: copied, as configured.
    Copied,
    /// Typing or pasting failed with this error, and the transcript was left
    /// on the clipboard instead so it is not lost.
    CopiedAfterFailure(String),
    /// The transcript was inserted, but the trailing Enter (`auto_submit`)
    /// failed with this error. The text is already in the focused application,
    /// so it is never copied to the clipboard.
    SubmitFailed(String),
}

pub fn output_text(text: &str, config: &OutputConfig) -> anyhow::Result<Delivery> {
    deliver(
        text,
        config,
        &SystemDesktop {
            backend: &config.typing_backend,
        },
    )
}

/// The three desktop actions output needs, split out so the mode and
/// fallback decisions in `deliver` are testable without a session. Insertion
/// and submission are separate steps: a failed `press_enter` means the text
/// already reached the application, so it must not trigger the clipboard
/// fallback the way a failed `type_text` or `press_paste` does.
trait Desktop {
    fn type_text(&self, text: &str) -> anyhow::Result<()>;
    fn press_paste(&self, keys: PasteKeys) -> anyhow::Result<()>;
    fn press_enter(&self) -> anyhow::Result<()>;
    fn copy(&self, text: &str) -> anyhow::Result<()>;
}

fn deliver(text: &str, config: &OutputConfig, desktop: &impl Desktop) -> anyhow::Result<Delivery> {
    match config.method {
        OutputMethod::None => {
            tracing::debug!(
                "output method 'none': skipping output of {} chars",
                text.len()
            );
            Ok(Delivery::Kept)
        }
        OutputMethod::Type => match desktop.type_text(text) {
            Ok(()) => {
                if let Some(failure) = press_enter_if_requested(config.auto_submit, desktop) {
                    return Ok(failure);
                }
                Ok(Delivery::Inserted)
            }
            Err(typing) => match desktop.copy(text) {
                Ok(()) => Ok(Delivery::CopiedAfterFailure(format!("{typing:#}"))),
                Err(copy) => {
                    Err(typing.context(format!("the clipboard fallback also failed: {copy:#}")))
                }
            },
        },
        OutputMethod::Clipboard => {
            desktop.copy(text)?;
            Ok(Delivery::Copied)
        }
        // The copy comes first, so a failed key press still leaves the
        // transcript on the clipboard with nothing more to do.
        OutputMethod::Paste => {
            desktop.copy(text)?;
            match desktop.press_paste(config.paste_keys) {
                Ok(()) => {
                    if config.auto_submit {
                        // Applications read the clipboard asynchronously after
                        // the paste keys, so an Enter sent straight away can
                        // submit before the text arrives.
                        std::thread::sleep(PASTE_SETTLE);
                    }
                    if let Some(failure) = press_enter_if_requested(config.auto_submit, desktop) {
                        return Ok(failure);
                    }
                    Ok(Delivery::Inserted)
                }
                Err(paste) => Ok(Delivery::CopiedAfterFailure(format!("{paste:#}"))),
            }
        }
    }
}

/// Presses Enter for `auto_submit`, reporting a failure as `SubmitFailed`: the
/// text is already inserted, so it is never copied to the clipboard.
fn press_enter_if_requested(auto_submit: bool, desktop: &impl Desktop) -> Option<Delivery> {
    if !auto_submit {
        return None;
    }
    match desktop.press_enter() {
        Ok(()) => None,
        Err(enter) => Some(Delivery::SubmitFailed(format!("{enter:#}"))),
    }
}

struct SystemDesktop<'a> {
    backend: &'a str,
}

impl SystemDesktop<'_> {
    fn resolved_backend(&self) -> anyhow::Result<String> {
        let backend = if self.backend == "auto" {
            cached_auto_backend()
        } else {
            self.backend.to_string()
        };
        if backend == "none" {
            anyhow::bail!("no typing helper is available: {}", typing_helper_warning());
        }
        Ok(backend)
    }
}

impl Desktop for SystemDesktop<'_> {
    fn type_text(&self, text: &str) -> anyhow::Result<()> {
        type_text(text, &self.resolved_backend()?)
    }

    fn press_paste(&self, keys: PasteKeys) -> anyhow::Result<()> {
        press_paste(keys, &self.resolved_backend()?)
    }

    fn press_enter(&self) -> anyhow::Result<()> {
        press_enter(&self.resolved_backend()?)
    }

    fn copy(&self, text: &str) -> anyhow::Result<()> {
        copy_to_clipboard(text)
    }
}

fn type_text(text: &str, backend: &str) -> anyhow::Result<()> {
    match backend {
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
        }
        "enigo" => {
            use enigo::{Enigo, Keyboard, Settings};
            let mut enigo = Enigo::new(&Settings::default())?;
            enigo.text(text)?;
        }
        "dotool" => {
            let mut child = Command::new("dotool")
                .stdin(std::process::Stdio::piped())
                .spawn()
                .context("failed to spawn dotool")?;
            if let Some(mut stdin) = child.stdin.take() {
                write_dotool_commands(&mut stdin, text)?;
            }
            require_success("dotool", child.wait()?)?;
        }
        "eitype" => {
            require_success(
                "eitype",
                Command::new("eitype")
                    .args(eitype_type_args(text, compositor_is_kde()))
                    .status()
                    .context("failed to spawn eitype")?,
            )?;
        }
        _ => {
            anyhow::bail!("unsupported typing backend: {}", backend);
        }
    }

    Ok(())
}

/// Presses Enter to submit through the typing helper.
fn press_enter(backend: &str) -> anyhow::Result<()> {
    match backend {
        "wtype" => {
            require_success(
                "wtype",
                Command::new("wtype")
                    .args(["-k", "Return"])
                    .status()
                    .context("failed to spawn wtype")?,
            )?;
        }
        "enigo" => {
            use enigo::{Direction, Enigo, Key, Keyboard, Settings};
            let mut enigo = Enigo::new(&Settings::default())?;
            enigo.key(Key::Return, Direction::Click)?;
        }
        "dotool" => {
            use std::io::Write;
            let mut child = Command::new("dotool")
                .stdin(std::process::Stdio::piped())
                .spawn()
                .context("failed to spawn dotool")?;
            if let Some(mut stdin) = child.stdin.take() {
                writeln!(stdin, "key enter")?;
            }
            require_success("dotool", child.wait()?)?;
        }
        "eitype" => {
            require_success(
                "eitype",
                Command::new("eitype")
                    .args(["-k", "return"])
                    .status()
                    .context("failed to spawn eitype")?,
            )?;
        }
        _ => {
            anyhow::bail!("unsupported typing backend: {}", backend);
        }
    }

    Ok(())
}

/// Presses `keys` through the typing helper. Enter is a separate step
/// (`press_enter`), so a delivered paste is distinguishable from a failed one.
fn press_paste(keys: PasteKeys, backend: &str) -> anyhow::Result<()> {
    match backend {
        "wtype" => {
            let status = Command::new("wtype")
                .args(wtype_paste_args(keys))
                .status()
                .context("failed to spawn wtype")?;
            require_success("wtype", status)?;
        }
        "enigo" => {
            use enigo::{Direction, Enigo, Key, Keyboard, Settings};
            let mut enigo = Enigo::new(&Settings::default())?;
            let (modifier, key) = match keys {
                PasteKeys::CtrlV => (Key::Control, Key::Unicode('v')),
                PasteKeys::ShiftInsert => (Key::Shift, Key::Insert),
            };
            enigo.key(modifier, Direction::Press)?;
            let pressed = enigo.key(key, Direction::Click);
            enigo.key(modifier, Direction::Release)?;
            pressed?;
        }
        "dotool" => {
            use std::io::Write;
            let mut child = Command::new("dotool")
                .stdin(std::process::Stdio::piped())
                .spawn()
                .context("failed to spawn dotool")?;
            if let Some(mut stdin) = child.stdin.take() {
                writeln!(stdin, "key {}", dotool_paste_keys(keys))?;
                stdin.flush()?;
            }
            require_success("dotool", child.wait()?)?;
        }
        "eitype" => {
            let status = Command::new("eitype")
                .args(eitype_paste_args(keys))
                .status()
                .context("failed to spawn eitype")?;
            require_success("eitype", status)?;
        }
        _ => {
            anyhow::bail!("unsupported typing backend: {}", backend);
        }
    }
    Ok(())
}

fn wtype_paste_args(keys: PasteKeys) -> Vec<String> {
    let (modifier, key) = match keys {
        PasteKeys::CtrlV => ("ctrl", "v"),
        PasteKeys::ShiftInsert => ("shift", "Insert"),
    };
    ["-M", modifier, "-k", key, "-m", modifier]
        .into_iter()
        .map(str::to_string)
        .collect()
}

/// KWin can acknowledge a zero-delay `eitype` process even when it has dropped
/// the tail of a fast key burst, so typing on KDE gets the smallest positive
/// inter-key delay that keeps the whole transcript. One millisecond is
/// Voxtype's source-backed value for the same compositor behavior
/// (`V:src/output/eitype.rs:16-34`).
const EITYPE_KDE_TYPE_DELAY_MS: u32 = 1;

/// The `eitype` arguments that type `text`: `eitype -- <text>`, with a forced
/// small inter-key delay on KDE. The `--` end-of-options separator keeps a
/// transcript that begins with `-` (for example `--help`) from being parsed as
/// an option, which would print help, exit 0, and type nothing. `eitype` maps
/// `\n` in the text to Return, so newlines need no separate key press. Kept
/// separate from the command so the KDE pacing is testable without mutating the
/// process-global environment.
fn eitype_type_args(text: &str, kde: bool) -> Vec<String> {
    let mut args = Vec::new();
    if kde {
        args.push("-d".to_string());
        args.push(EITYPE_KDE_TYPE_DELAY_MS.to_string());
    }
    args.push("--".to_string());
    args.push(text.to_string());
    args
}

/// The `eitype` arguments that press one paste combination. Held modifiers are
/// released at the end of `eitype`'s action list, so no explicit release key
/// (`wtype`'s trailing `-m`) is needed and no modifier is left stuck.
fn eitype_paste_args(keys: PasteKeys) -> [&'static str; 4] {
    match keys {
        PasteKeys::CtrlV => ["-M", "ctrl", "-k", "v"],
        PasteKeys::ShiftInsert => ["-M", "shift", "-k", "Insert"],
    }
}

fn dotool_paste_keys(keys: PasteKeys) -> &'static str {
    match keys {
        PasteKeys::CtrlV => "ctrl+v",
        PasteKeys::ShiftInsert => "shift+insert",
    }
}

/// Copies `text` to the clipboard with the session's clipboard helper. Both
/// helpers fork a background process that keeps serving the selection after
/// this call returns, so stdout and stderr are discarded: a pipe held open by
/// that process would make `wait` block until the next copy.
fn copy_to_clipboard(text: &str) -> anyhow::Result<()> {
    use std::io::Write;
    let (program, args) = clipboard_command(clipboard_uses_wayland());
    let mut child = Command::new(program)
        .args(args)
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .spawn()
        .with_context(|| format!("failed to spawn {program}"))?;
    if let Some(mut stdin) = child.stdin.take() {
        stdin.write_all(text.as_bytes())?;
    }
    require_success(program, child.wait()?)
}

/// The clipboard helper and its arguments: `wl-copy` (from `wl-clipboard`)
/// on Wayland, `xclip` on X11.
fn clipboard_command(wayland: bool) -> (&'static str, &'static [&'static str]) {
    if wayland {
        ("wl-copy", &[])
    } else {
        ("xclip", &["-selection", "clipboard"])
    }
}

/// A daemon started by a systemd user unit can see `WAYLAND_DISPLAY` without
/// `XDG_SESSION_TYPE`, and `wl-copy` only needs the former.
fn clipboard_uses_wayland() -> bool {
    session_is_wayland() || std::env::var_os("WAYLAND_DISPLAY").is_some()
}

/// The clipboard helper this session copies with.
pub fn clipboard_helper() -> &'static str {
    clipboard_command(clipboard_uses_wayland()).0
}

/// Whether the clipboard helper is installed. This is a PATH lookup, never a
/// trial copy, so probing cannot overwrite what the user has copied.
pub fn clipboard_available() -> bool {
    binary_on_path(clipboard_helper(), std::env::var_os("PATH").as_deref())
}

/// The warning shown when `clipboard_available` is false, naming the package
/// to install for this session. The helper runs fresh on every copy, so no
/// restart is needed once it is installed.
pub fn clipboard_helper_warning() -> &'static str {
    clipboard_warning_for(clipboard_uses_wayland())
}

fn clipboard_warning_for(wayland: bool) -> &'static str {
    if wayland {
        "No clipboard helper found - install wl-clipboard (it provides wl-copy)."
    } else {
        "No clipboard helper found - install xclip."
    }
}

/// Why `method` cannot work here, as the warning that names the remedy, or
/// `None` when it can. `type` needs a typing helper; `clipboard` needs the
/// clipboard helper; `paste` needs both, because it copies first and then
/// presses the paste keys through the typing helper.
pub fn method_unavailable_reason(
    method: OutputMethod,
    has_typing_helper: bool,
    has_clipboard: bool,
) -> Option<&'static str> {
    match method {
        OutputMethod::None => None,
        OutputMethod::Type => (!has_typing_helper).then(typing_helper_warning),
        OutputMethod::Clipboard => (!has_clipboard).then(clipboard_helper_warning),
        OutputMethod::Paste => {
            if !has_clipboard {
                Some(clipboard_helper_warning())
            } else if !has_typing_helper {
                Some(typing_helper_warning())
            } else {
                None
            }
        }
    }
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
    if eitype_installed() {
        return "eitype".to_string();
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
    if eitype_installed() {
        backends.push("eitype".to_string());
    }
    backends
}

pub fn has_any_type_backend() -> bool {
    helper_self_test("wtype") || enigo_available() || dotool_available() || eitype_installed()
}

pub fn type_backend_available(backend: &str) -> bool {
    match backend {
        "auto" => has_any_type_backend(),
        "wtype" => helper_self_test("wtype"),
        "dotool" => dotool_available(),
        "enigo" => enigo_available(),
        "eitype" => eitype_installed(),
        _ => false,
    }
}

/// The warning the configuration UIs and the daemon startup log show when no
/// typing helper is usable, naming the one action that makes a helper work for
/// this compositor. `enigo` needs no install - it is built in - so the missing
/// piece is always one of the external helpers: `wtype`, which types through
/// the Wayland virtual-keyboard protocol that only wlroots-based compositors
/// implement; `dotool`, which types through `/dev/uinput` and so works on KDE's
/// KWin and other compositors too; or `eitype`, which types through the
/// RemoteDesktop portal's libei path and needs no `/dev/uinput` access, making
/// it the helper that works on KDE and GNOME Wayland without it. When `dotool`
/// is already installed but `/dev/uinput` is not writable, the warning names
/// the permission remedy instead of telling the user to install a binary they
/// already have.
/// The restart note is real, not boilerplate: `cached_auto_backend`, this
/// warning, and the daemon process all memoize the probe, so a helper
/// installed while TongueTyped is running is not picked up until it restarts.
/// The warning is memoized here because the setup console's render path calls
/// it on every frame while the Output step is open.
pub fn typing_helper_warning() -> &'static str {
    static WARNING: OnceLock<&'static str> = OnceLock::new();
    WARNING.get_or_init(|| {
        warning_for(
            recommended_helper(),
            dotool_installed(),
            device_is_writable(std::path::Path::new("/dev/uinput")),
        )
    })
}

/// The external helper to name for this session. `wtype` only on a Wayland
/// session whose compositor is wlroots-based (sway, Hyprland, niri, ...),
/// which implements the `zwp_virtual_keyboard_manager_v1` protocol `wtype`
/// needs. On any other Wayland session - KDE's KWin, GNOME's Mutter, and the
/// rest - `wtype` cannot work and `dotool` needs `/dev/uinput`, so `eitype`
/// (libei through the RemoteDesktop portal) is the helper that works without
/// it. On X11, `enigo` is built in and covers typing, so `dotool` remains the
/// named fallback.
fn recommended_helper() -> &'static str {
    if session_is_wayland() {
        if compositor_is_wlroots() {
            "wtype"
        } else {
            "eitype"
        }
    } else {
        "dotool"
    }
}

fn warning_for(helper: &str, dotool_installed: bool, uinput_writable: bool) -> &'static str {
    if helper == "wtype" {
        "No typing helper found - install wtype, then restart TongueTyped."
    } else if helper == "eitype" {
        if dotool_installed && !uinput_writable {
            "dotool is installed but cannot open /dev/uinput - install eitype (it types through the RemoteDesktop portal and needs no /dev/uinput), or add your user to the 'input' group or add a udev rule granting write access to /dev/uinput, then restart TongueTyped."
        } else {
            "No typing helper found - install eitype (it types through the RemoteDesktop portal and needs no /dev/uinput access), then restart TongueTyped."
        }
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
/// takes the `eitype` branch.
fn compositor_is_wlroots() -> bool {
    desktop_names_are_wlroots(&session_desktops())
}

/// Whether this session's desktop identifies KDE Plasma, whose KWin can drop
/// the tail of a fast `eitype` key burst and so needs the forced small
/// inter-key delay in `eitype_type_args`.
fn compositor_is_kde() -> bool {
    desktop_names_are_kde(&session_desktops())
}

/// The desktop-identification environment values, in preference order.
fn session_desktops() -> Vec<String> {
    [
        "XDG_CURRENT_DESKTOP",
        "XDG_SESSION_DESKTOP",
        "DESKTOP_SESSION",
    ]
    .iter()
    .filter_map(|key| std::env::var(key).ok())
    .collect()
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

/// Pure core of `compositor_is_kde`. `XDG_CURRENT_DESKTOP` is colon-separated
/// and KDE is one component (`KDE`, or `KDE:foo`), so match a whole component
/// case-insensitively rather than a substring that could match an unrelated
/// name.
fn desktop_names_are_kde(desktops: &[String]) -> bool {
    desktops
        .iter()
        .flat_map(|desktop| desktop.split(':'))
        .any(|component| component.eq_ignore_ascii_case("kde"))
}

fn enigo_available() -> bool {
    if session_is_wayland() {
        return false;
    }
    use enigo::{Enigo, Settings};
    Enigo::new(&Settings::default()).is_ok()
}

/// Whether the `dotool` binary is installed, independent of whether the
/// current user can open `/dev/uinput`. Running `dotool` to find out cannot
/// distinguish the two: it opens `/dev/uinput` at startup and exits non-zero
/// when that open fails, so its self-test reports an installed-but-forbidden
/// `dotool` as if the binary were missing. The PATH lookup is what makes the
/// permission remedy in `warning_for` reachable.
fn dotool_installed() -> bool {
    binary_on_path("dotool", std::env::var_os("PATH").as_deref())
}

/// Whether the `eitype` binary is installed. This is a PATH lookup, never a
/// live call: any `eitype` invocation opens a RemoteDesktop portal session, and
/// on the first run that pops the authorization dialog, so probing by running
/// it would surprise the user from `doctor` or `setup` before they ever type.
/// Voxtype probes the same way for the same reason
/// (`V:src/output/eitype.rs:246-256`). Unlike `dotool`, `eitype` has no
/// separate device check: its permission grant is stored as a portal restore
/// token by the helper itself, and whether the portal will allow it cannot be
/// known without the dialog this probe exists to avoid.
fn eitype_installed() -> bool {
    binary_on_path("eitype", std::env::var_os("PATH").as_deref())
}

/// Whether an executable named `name` appears in `path`, the `PATH`
/// environment value. Kept separate from the environment so the lookup can be
/// tested against a constructed directory list.
fn binary_on_path(name: &str, path: Option<&std::ffi::OsStr>) -> bool {
    let Some(path) = path else {
        return false;
    };
    std::env::split_paths(path).any(|dir| is_executable_file(&dir.join(name)))
}

fn is_executable_file(path: &std::path::Path) -> bool {
    let Ok(metadata) = std::fs::metadata(path) else {
        return false;
    };
    if !metadata.is_file() {
        return false;
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        if metadata.permissions().mode() & 0o111 == 0 {
            return false;
        }
    }
    true
}

/// Whether `dotool` is present *and* can actually inject. It types through
/// `/dev/uinput`, so a binary that is installed but whose user cannot open
/// that device (no `input` group membership or matching udev rule) would
/// silently type nothing - a plain binary-exists check is not enough. The
/// device check is also what separates "installed but forbidden" from
/// "missing", which is why presence above is a PATH lookup rather than a
/// `dotool` self-test. Opening the device for writing is side-effect-free: a
/// virtual device is only created by a later `UI_DEV_CREATE` ioctl, so this
/// probe leaks no node.
fn dotool_available() -> bool {
    dotool_installed() && device_is_writable(std::path::Path::new("/dev/uinput"))
}

fn device_is_writable(path: &std::path::Path) -> bool {
    std::fs::OpenOptions::new().write(true).open(path).is_ok()
}

fn write_dotool_commands(writer: &mut impl std::io::Write, text: &str) -> std::io::Result<()> {
    for (index, line) in text.split('\n').enumerate() {
        if index > 0 {
            writeln!(writer, "key enter")?;
        }
        writeln!(writer, "type {line}")?;
    }
    Ok(())
}

fn require_success(backend: &str, status: std::process::ExitStatus) -> anyhow::Result<()> {
    if !status.success() {
        anyhow::bail!("{backend} failed with {status}");
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
        let result = output_text("test", &OutputConfig::default());
        assert_eq!(result.unwrap(), Delivery::Kept);
    }

    /// Records every desktop action and fails the ones it is told to.
    #[derive(Default)]
    struct FakeDesktop {
        fail_typing: bool,
        fail_paste: bool,
        fail_enter: bool,
        fail_copy: bool,
        actions: std::cell::RefCell<Vec<String>>,
    }

    impl Desktop for FakeDesktop {
        fn type_text(&self, text: &str) -> anyhow::Result<()> {
            self.actions.borrow_mut().push(format!("type {text:?}"));
            if self.fail_typing {
                anyhow::bail!("dotool failed with exit status: 1");
            }
            Ok(())
        }

        fn press_paste(&self, keys: PasteKeys) -> anyhow::Result<()> {
            self.actions.borrow_mut().push(format!("paste {keys}"));
            if self.fail_paste {
                anyhow::bail!("no typing helper is available");
            }
            Ok(())
        }

        fn press_enter(&self) -> anyhow::Result<()> {
            self.actions.borrow_mut().push("enter".to_string());
            if self.fail_enter {
                anyhow::bail!("wtype failed with exit status: 1");
            }
            Ok(())
        }

        fn copy(&self, text: &str) -> anyhow::Result<()> {
            self.actions.borrow_mut().push(format!("copy {text:?}"));
            if self.fail_copy {
                anyhow::bail!("failed to spawn wl-copy");
            }
            Ok(())
        }
    }

    fn output_config(method: OutputMethod) -> OutputConfig {
        OutputConfig {
            method,
            ..OutputConfig::default()
        }
    }

    #[test]
    fn each_method_performs_only_its_own_actions() {
        let cases = [
            (OutputMethod::None, Delivery::Kept, vec![]),
            (OutputMethod::Type, Delivery::Inserted, vec!["type \"hi\""]),
            (
                OutputMethod::Clipboard,
                Delivery::Copied,
                vec!["copy \"hi\""],
            ),
            (
                OutputMethod::Paste,
                Delivery::Inserted,
                vec!["copy \"hi\"", "paste Ctrl+V"],
            ),
        ];
        for (method, delivery, actions) in cases {
            let desktop = FakeDesktop::default();
            assert_eq!(
                deliver("hi", &output_config(method), &desktop).unwrap(),
                delivery,
                "{method}"
            );
            assert_eq!(*desktop.actions.borrow(), actions, "{method}");
        }
    }

    #[test]
    fn paste_and_enter_passes_the_configured_keys_and_submit() {
        let desktop = FakeDesktop::default();
        let config = OutputConfig {
            method: OutputMethod::Paste,
            auto_submit: true,
            paste_keys: PasteKeys::ShiftInsert,
            ..OutputConfig::default()
        };
        assert_eq!(
            deliver("hi", &config, &desktop).unwrap(),
            Delivery::Inserted
        );
        assert_eq!(
            *desktop.actions.borrow(),
            vec!["copy \"hi\"", "paste Shift+Insert", "enter"]
        );
    }

    #[test]
    fn failed_typing_falls_back_to_the_clipboard() {
        let desktop = FakeDesktop {
            fail_typing: true,
            ..FakeDesktop::default()
        };
        let config = OutputConfig {
            method: OutputMethod::Type,
            auto_submit: true,
            ..OutputConfig::default()
        };
        let delivery = deliver("hi", &config, &desktop).unwrap();
        assert_eq!(
            delivery,
            Delivery::CopiedAfterFailure("dotool failed with exit status: 1".to_string())
        );
        assert_eq!(
            *desktop.actions.borrow(),
            vec!["type \"hi\"", "copy \"hi\""],
            "the fallback copies the same text and presses nothing"
        );
    }

    #[test]
    fn a_failed_enter_after_typing_never_touches_the_clipboard() {
        let desktop = FakeDesktop {
            fail_enter: true,
            ..FakeDesktop::default()
        };
        let config = OutputConfig {
            method: OutputMethod::Type,
            auto_submit: true,
            ..OutputConfig::default()
        };
        let delivery = deliver("hi", &config, &desktop).unwrap();
        assert_eq!(
            delivery,
            Delivery::SubmitFailed("wtype failed with exit status: 1".to_string())
        );
        assert_eq!(
            *desktop.actions.borrow(),
            vec!["type \"hi\"", "enter"],
            "the text is already typed, so the clipboard is left untouched"
        );
    }

    #[test]
    fn a_failed_enter_after_pasting_is_not_a_paste_failure() {
        let desktop = FakeDesktop {
            fail_enter: true,
            ..FakeDesktop::default()
        };
        let config = OutputConfig {
            method: OutputMethod::Paste,
            auto_submit: true,
            ..OutputConfig::default()
        };
        let delivery = deliver("hi", &config, &desktop).unwrap();
        assert_eq!(
            delivery,
            Delivery::SubmitFailed("wtype failed with exit status: 1".to_string())
        );
        assert_eq!(
            *desktop.actions.borrow(),
            vec!["copy \"hi\"", "paste Ctrl+V", "enter"],
            "the paste was delivered, so only the submit failed"
        );
    }

    #[test]
    fn failed_paste_keys_leave_the_already_copied_text() {
        let desktop = FakeDesktop {
            fail_paste: true,
            ..FakeDesktop::default()
        };
        let delivery = deliver("hi", &output_config(OutputMethod::Paste), &desktop).unwrap();
        assert!(matches!(delivery, Delivery::CopiedAfterFailure(_)));
        assert_eq!(
            *desktop.actions.borrow(),
            vec!["copy \"hi\"", "paste Ctrl+V"],
            "the text is on the clipboard already, so nothing is copied twice"
        );
    }

    #[test]
    fn typing_and_clipboard_both_failing_is_an_error_naming_both() {
        let desktop = FakeDesktop {
            fail_typing: true,
            fail_copy: true,
            ..FakeDesktop::default()
        };
        let error = deliver("hi", &output_config(OutputMethod::Type), &desktop).unwrap_err();
        let message = format!("{error:#}");
        assert!(message.contains("dotool failed"), "{message}");
        assert!(message.contains("wl-copy"), "{message}");
    }

    #[test]
    fn clipboard_and_paste_fail_when_the_copy_fails() {
        for method in [OutputMethod::Clipboard, OutputMethod::Paste] {
            let desktop = FakeDesktop {
                fail_copy: true,
                ..FakeDesktop::default()
            };
            assert!(deliver("hi", &output_config(method), &desktop).is_err());
            assert_eq!(
                desktop.actions.borrow().len(),
                1,
                "{method}: no paste after a failed copy"
            );
        }
    }

    #[test]
    fn unavailable_methods_name_the_missing_helper() {
        assert_eq!(
            method_unavailable_reason(OutputMethod::None, false, false),
            None
        );
        assert_eq!(
            method_unavailable_reason(OutputMethod::Type, true, false),
            None
        );
        assert_eq!(
            method_unavailable_reason(OutputMethod::Clipboard, false, true),
            None
        );
        assert_eq!(
            method_unavailable_reason(OutputMethod::Paste, true, true),
            None
        );
        assert_eq!(
            method_unavailable_reason(OutputMethod::Type, false, true),
            Some(typing_helper_warning())
        );
        assert_eq!(
            method_unavailable_reason(OutputMethod::Clipboard, true, false),
            Some(clipboard_helper_warning())
        );
        // Paste needs both: the clipboard to hold the text, the typing
        // helper to press the keys.
        assert_eq!(
            method_unavailable_reason(OutputMethod::Paste, true, false),
            Some(clipboard_helper_warning())
        );
        assert_eq!(
            method_unavailable_reason(OutputMethod::Paste, false, true),
            Some(typing_helper_warning())
        );
    }

    #[test]
    fn clipboard_helper_matches_the_session() {
        assert_eq!(clipboard_command(true).0, "wl-copy");
        assert_eq!(
            clipboard_command(false),
            ("xclip", &["-selection", "clipboard"][..])
        );
        assert!(clipboard_warning_for(true).contains("wl-clipboard"));
        assert!(clipboard_warning_for(false).contains("xclip"));
    }

    #[test]
    fn paste_keys_reach_each_helper_in_its_own_syntax() {
        assert_eq!(
            wtype_paste_args(PasteKeys::CtrlV),
            ["-M", "ctrl", "-k", "v", "-m", "ctrl"]
        );
        assert_eq!(
            wtype_paste_args(PasteKeys::ShiftInsert),
            ["-M", "shift", "-k", "Insert", "-m", "shift"]
        );
        assert_eq!(dotool_paste_keys(PasteKeys::CtrlV), "ctrl+v");
        assert_eq!(dotool_paste_keys(PasteKeys::ShiftInsert), "shift+insert");
    }

    #[test]
    fn eitype_paste_args_press_one_combination_with_no_stuck_modifier() {
        // `eitype` releases held modifiers at the end of the action list, so
        // the arguments carry `-M` with no matching explicit release.
        assert_eq!(
            eitype_paste_args(PasteKeys::CtrlV),
            ["-M", "ctrl", "-k", "v"]
        );
        assert_eq!(
            eitype_paste_args(PasteKeys::ShiftInsert),
            ["-M", "shift", "-k", "Insert"]
        );
    }

    #[test]
    fn eitype_type_args_add_the_kde_delay_only_on_kde() {
        assert_eq!(
            eitype_type_args("hi", false),
            vec!["--".to_string(), "hi".to_string()]
        );
        assert_eq!(
            eitype_type_args("hi", true),
            vec![
                "-d".to_string(),
                EITYPE_KDE_TYPE_DELAY_MS.to_string(),
                "--".to_string(),
                "hi".to_string()
            ]
        );
        // A newline is passed through in the one text argument; eitype maps it
        // to Return internally, so no separate key press is needed.
        assert_eq!(
            eitype_type_args("a\nb", false),
            vec!["--".to_string(), "a\nb".to_string()]
        );
    }

    #[test]
    fn eitype_type_args_place_text_after_the_end_of_options_separator() {
        // A transcript that begins with a hyphen (for example `--help`) must
        // reach eitype as the positional text, not be parsed as an option:
        // `eitype --help` would print help, exit 0, and type nothing.
        assert_eq!(
            eitype_type_args("--help", false),
            vec!["--".to_string(), "--help".to_string()]
        );
        assert_eq!(
            eitype_type_args("--help", true),
            vec![
                "-d".to_string(),
                EITYPE_KDE_TYPE_DELAY_MS.to_string(),
                "--".to_string(),
                "--help".to_string()
            ]
        );
    }

    #[test]
    fn kde_desktop_names_match_a_whole_component() {
        for (desktop, kde) in [
            ("KDE", true),
            ("kde", true),
            ("KDE:GNOME", true),
            ("ubuntu:GNOME", false),
            ("GNOME", false),
            ("sway:GNOME", false),
            ("X-Cinnamon", false),
        ] {
            assert_eq!(
                desktop_names_are_kde(&[desktop.to_string()]),
                kde,
                "{desktop}"
            );
        }
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
                || warning.contains("install eitype")
                || warning.contains("input"),
            "the warning must name a concrete remedy: {warning}"
        );
        assert!(
            warning.contains("restart TongueTyped"),
            "the warning must say a restart is needed: {warning}"
        );
    }

    #[test]
    fn desktop_names_are_wlroots_classifies_wlroots_compositors() {
        // `desktop_names_are_wlroots` is true only for wlroots-based
        // compositors; KDE, GNOME, and every other desktop are not wlroots.
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
    fn eitype_warning_points_at_the_portal_helper_not_only_uinput() {
        // KDE/GNOME Wayland with neither dotool nor eitype: the warning must
        // name eitype, the helper that works without /dev/uinput access.
        let missing = warning_for("eitype", false, false);
        assert!(
            missing.contains("install eitype"),
            "the eitype warning must name eitype to install: {missing}"
        );
        assert!(
            missing.contains("/dev/uinput"),
            "the eitype warning must say no /dev/uinput access is needed: {missing}"
        );
        assert!(
            missing.contains("RemoteDesktop"),
            "the eitype warning must say how it types: {missing}"
        );
        // dotool installed but forbidden: keep the permission remedy, but also
        // offer eitype so a KDE/GNOME user has a path that needs no uinput.
        let forbidden = warning_for("eitype", true, false);
        assert!(forbidden.contains("install eitype"), "{forbidden}");
        assert!(
            forbidden.contains("input") && forbidden.contains("/dev/uinput"),
            "the eitype warning must still name the dotool permission remedy: {forbidden}"
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
    fn binary_on_path_finds_an_executable_independently_of_uinput() {
        // The exact production case: an installed `dotool` must read as
        // present even when it cannot open /dev/uinput (the state the
        // permission remedy exists for). Running `dotool` cannot tell that
        // apart from "not installed", so presence is a PATH lookup.
        let dir = std::env::temp_dir().join(format!(
            "tt-dotool-on-path-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        let dotool = dir.join("dotool");
        std::fs::write(&dotool, "#!/bin/sh\nexit 1\n").unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&dotool, std::fs::Permissions::from_mode(0o755)).unwrap();
        }
        let path = std::env::join_paths([dir.as_path()]).unwrap();
        assert!(
            binary_on_path("dotool", Some(path.as_os_str())),
            "an executable on PATH must be found without running it"
        );
        assert!(
            !binary_on_path("dotool", Some(std::ffi::OsStr::new("/nonexistent"))),
            "a name absent from every PATH directory is not installed"
        );
        assert!(
            !binary_on_path("wtype", Some(path.as_os_str())),
            "only the requested binary counts"
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
        write_dotool_commands(&mut commands, "notes\nkey ctrl+a").unwrap();
        assert_eq!(
            String::from_utf8(commands).unwrap(),
            "type notes\nkey enter\ntype key ctrl+a\n"
        );
    }
}
