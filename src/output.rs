use crate::config::{OutputConfig, OutputMethod, PasteKeys};
use anyhow::Context;
use std::process::Command;
use std::time::Duration;

/// How long `paste` mode waits between pressing the paste keys and pressing
/// Enter. Applications read the clipboard asynchronously after the paste
/// keys, so an Enter sent straight away can submit before the text arrives.
const PASTE_SETTLE: Duration = Duration::from_millis(200);

/// Where a transcript ended up. `Kept` means `none` delivered it nowhere;
/// every other variant puts it somewhere the user can reach outside
/// TongueTyped's own store. An `Err` from `output_text` means neither
/// insertion nor clipboard copy succeeded, though the text is still in the
/// in-memory `last-result` (and in history when saving is enabled).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Delivery {
    /// `none` mode: the transcript is not delivered anywhere.
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
    // Methods that type or paste need a resolved backend. `none` and
    // `clipboard` never touch one, so they skip the probe entirely.
    if !config.method.inserts_text() {
        return deliver(text, config, &SystemDesktop { plan: None });
    }
    let plan = resolve_typing(config);
    output_text_with_plan(text, config, &plan)
}

/// Delivers `text` through an already-resolved typing plan.
///
/// The daemon resolves once at load and keeps the plan up to date on reload
/// and display changes, so a dictation never re-runs the probes. An unresolved
/// plan (`resolved: None`, because every probe failed) fails into the normal
/// clipboard fallback with the precise reason, instead of retrying a backend
/// that is known not to work.
pub fn output_text_with_plan(
    text: &str,
    config: &OutputConfig,
    plan: &TypingPlan,
) -> anyhow::Result<Delivery> {
    deliver(text, config, &SystemDesktop { plan: Some(plan) })
}

/// How a pinned or automatic typing backend resolved on this session.
///
/// `configured` is what the config asked for, `resolved` is what will actually
/// be used (`None` when nothing probed usable), and `reasons` records every
/// failed probe with the session-specific cause. `display` is the display
/// identity the probes ran against, so the daemon can tell when the answer is
/// stale after a compositor or session change.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TypingPlan {
    pub configured: String,
    pub resolved: Option<String>,
    pub reasons: Vec<BackendProbeFailure>,
    pub display: DisplayIdentity,
}

/// One backend that failed its hidden probe, and why on this session.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BackendProbeFailure {
    pub backend: String,
    pub reason: String,
}

impl TypingPlan {
    /// A plan for a caller that does not type or has not probed yet (test
    /// runtimes, the status snapshot before load). It performs no probing and
    /// never claims a backend works, so a later `needs_reprobe` resolves it.
    pub fn unprobed(configured: &str) -> Self {
        Self {
            configured: configured.to_string(),
            resolved: None,
            reasons: Vec::new(),
            display: DisplayIdentity::current(),
        }
    }

    /// Whether this plan is stale for `config`: the pinned value changed, no
    /// backend resolved (a probe that ran before the compositor or portal was
    /// ready must not disable typing for the daemon's life), or the display
    /// identity changed under a running daemon.
    pub fn needs_reprobe(&self, config: &OutputConfig) -> bool {
        self.needs_reprobe_for(config, &DisplayIdentity::current())
    }

    /// Pure core of [`needs_reprobe`](Self::needs_reprobe), so the display
    /// identity rule is testable without mutating the process environment.
    fn needs_reprobe_for(&self, config: &OutputConfig, display: &DisplayIdentity) -> bool {
        self.configured != config.typing_backend
            || self.resolved.is_none()
            || &self.display != display
    }

    /// The probe reason for the configured backend, when it was probed.
    pub fn configured_reason(&self) -> Option<&str> {
        self.reasons
            .iter()
            .find(|failure| failure.backend == self.configured)
            .map(|failure| failure.reason.as_str())
    }

    /// A substitution or dead end worth showing the user, in one sentence.
    /// `None` when the configured choice works.
    pub fn warning(&self) -> Option<String> {
        match &self.resolved {
            Some(resolved) if self.configured != "auto" && resolved != &self.configured => {
                Some(format!(
                    "typing backend '{}' cannot work on this session ({}); using '{}' instead. \
                     Set output.typing_backend = \"auto\" to use {}.",
                    self.configured,
                    self.configured_reason()
                        .unwrap_or("the hidden probe failed"),
                    resolved,
                    resolved,
                ))
            }
            Some(_) => None,
            None => Some(self.failure_message()),
        }
    }

    /// Why delivery cannot type, naming the pinned backend, the session
    /// reason, and one remedy. Used as the delivery error that reaches the
    /// clipboard-fallback warning and the doctor test output.
    pub fn failure_message(&self) -> String {
        if self.configured == "auto" {
            format!(
                "no typing helper works on this session - {}",
                no_helper_remedy()
            )
        } else {
            format!(
                "typing backend '{}' cannot work on this session ({}), and no other typing \
                 helper works - {}",
                self.configured,
                self.configured_reason()
                    .unwrap_or("the hidden probe failed"),
                no_helper_remedy(),
            )
        }
    }
}

/// Which display the probes ran against. A compositor or session change
/// replaces the socket or the display name, which makes a cached plan stale.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct DisplayIdentity {
    key: Option<String>,
}

impl DisplayIdentity {
    pub fn current() -> Self {
        let key = if let Some(display) = std::env::var_os("WAYLAND_DISPLAY") {
            let display = display.to_string_lossy().into_owned();
            let path = wayland_socket_path(&display);
            let metadata = std::fs::metadata(&path).ok();
            #[cfg(unix)]
            {
                use std::os::unix::fs::MetadataExt;
                metadata.map(|metadata| {
                    format!(
                        "wayland:{display}:{}:{}:{}:{}",
                        metadata.dev(),
                        metadata.ino(),
                        metadata.mtime(),
                        metadata.mtime_nsec()
                    )
                })
            }
            #[cfg(not(unix))]
            {
                let _ = metadata;
                Some(format!("wayland:{display}"))
            }
        } else {
            std::env::var_os("DISPLAY").map(|display| format!("x11:{}", display.to_string_lossy()))
        };
        Self { key }
    }

    /// Whether the display the plan probed is still the current one. A plan
    /// probed with no display (`None`) stays current only while there is still
    /// no display, so a compositor appearing later triggers a re-probe.
    pub fn is_current(&self) -> bool {
        *self == Self::current()
    }
}

fn wayland_socket_path(display: &str) -> std::path::PathBuf {
    let path = std::path::Path::new(display);
    if path.is_absolute() {
        return path.to_path_buf();
    }
    std::env::var_os("XDG_RUNTIME_DIR")
        .map(std::path::PathBuf::from)
        .unwrap_or_default()
        .join(display)
}

/// The order `auto` tries backends in. `wtype` first because it is the direct
/// Wayland protocol, then the X11-only `enigo`, then the `/dev/uinput`
/// helper, and `libei` last because it needs a portal session.
pub const BACKEND_ORDER: [&str; 4] = ["wtype", "enigo", "dotool", "libei"];

/// Resolves the configured backend against this session's hidden probes.
pub fn resolve_typing(config: &OutputConfig) -> TypingPlan {
    resolve_typing_with(
        &config.typing_backend,
        DisplayIdentity::current(),
        &|backend| probe_backend(backend),
    )
}

/// Pure core of [`resolve_typing`]. `probe` is injected so the pinned and
/// automatic decisions can be tested without a Wayland, X11, uinput, or
/// portal session.
fn resolve_typing_with(
    configured: &str,
    display: DisplayIdentity,
    probe: &dyn Fn(&str) -> Result<(), String>,
) -> TypingPlan {
    let mut reasons = Vec::new();
    let mut resolved = None;

    // An explicit choice wins when it probes OK.
    if configured != "auto" {
        match probe(configured) {
            Ok(()) => resolved = Some(configured.to_string()),
            Err(reason) => reasons.push(BackendProbeFailure {
                backend: configured.to_string(),
                reason,
            }),
        }
    }

    if resolved.is_none() {
        for backend in BACKEND_ORDER {
            if backend == configured {
                // Already probed above and failed; do not run it twice.
                continue;
            }
            match probe(backend) {
                Ok(()) => {
                    resolved = Some(backend.to_string());
                    break;
                }
                Err(reason) => reasons.push(BackendProbeFailure {
                    backend: backend.to_string(),
                    reason,
                }),
            }
        }
    }

    TypingPlan {
        configured: configured.to_string(),
        resolved,
        reasons,
        display,
    }
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
    plan: Option<&'a TypingPlan>,
}

impl SystemDesktop<'_> {
    fn backend(&self) -> anyhow::Result<&str> {
        let plan = self
            .plan
            .ok_or_else(|| anyhow::anyhow!("no typing backend was resolved for this output"))?;
        plan.resolved
            .as_deref()
            .ok_or_else(|| anyhow::anyhow!(plan.failure_message()))
    }
}

impl Desktop for SystemDesktop<'_> {
    fn type_text(&self, text: &str) -> anyhow::Result<()> {
        type_text(text, self.backend()?)
    }

    fn press_paste(&self, keys: PasteKeys) -> anyhow::Result<()> {
        press_paste(keys, self.backend()?)
    }

    fn press_enter(&self) -> anyhow::Result<()> {
        press_enter(self.backend()?)
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
        // Native libei: the transcript is written to the EIS socket as key
        // events, never as a process argument. No helper is spawned at all.
        "libei" => crate::libei::type_text(text)?,
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
        "libei" => crate::libei::press_enter()?,
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
        "libei" => crate::libei::press_paste(keys)?,
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

/// Which typing helper is installed and can work here, in `auto` order.
///
/// Stateless on purpose: the daemon resolves once at load and keeps the plan
/// in memory, so a stale process-lifetime answer cannot outlive a config
/// reload, a helper install, or a compositor change. Callers that want a
/// cached answer hold a [`TypingPlan`].
pub fn probe_type_backend() -> String {
    BACKEND_ORDER
        .iter()
        .find(|backend| probe_backend(backend).is_ok())
        .map(|backend| backend.to_string())
        .unwrap_or_else(|| "none".to_string())
}

pub fn list_available_backends() -> Vec<String> {
    BACKEND_ORDER
        .iter()
        .filter(|backend| probe_backend(backend).is_ok())
        .map(|backend| backend.to_string())
        .collect()
}

pub fn has_any_type_backend() -> bool {
    BACKEND_ORDER
        .iter()
        .any(|backend| probe_backend(backend).is_ok())
}

pub fn type_backend_available(backend: &str) -> bool {
    if backend == "auto" {
        return has_any_type_backend();
    }
    probe_backend(backend).is_ok()
}

/// The hidden capability probe for one backend. It reports why a backend
/// cannot work, which is what makes the pinned-substitution and no-helper
/// warnings name the real cause instead of guessing from the desktop name.
///
/// None of these probes creates a window, requests focus, sends a key event,
/// opens a portal session, or creates a uinput device.
pub fn probe_backend(backend: &str) -> Result<(), String> {
    match backend {
        "wtype" => probe_wtype(),
        "dotool" => probe_dotool(),
        "enigo" => probe_enigo(),
        "libei" => probe_libei(),
        _ => Err(format!("unsupported typing backend: {backend}")),
    }
}

/// `wtype` needs the binary, a reachable Wayland display, the
/// `zwp_virtual_keyboard_manager_v1` global, and a keyboard-capable seat. The
/// registry roundtrip is authoritative when a display is reachable; with no
/// display, the helper's empty-input self-test keeps a headless sandbox
/// deterministic (it sends no key either).
fn probe_wtype() -> Result<(), String> {
    if !wtype_installed() {
        return Err("wtype is not installed".to_string());
    }
    match crate::wayland::virtual_keyboard() {
        Some(capability) if capability.usable() => Ok(()),
        Some(_) => Err(
            "the compositor does not advertise the Wayland virtual-keyboard protocol \
             with a keyboard-capable seat"
                .to_string(),
        ),
        None if helper_self_test("wtype") => Ok(()),
        None => Err("no Wayland display is reachable".to_string()),
    }
}

fn wtype_installed() -> bool {
    binary_on_path("wtype", std::env::var_os("PATH").as_deref())
}

/// The warning the configuration UIs and the doctor show when no typing
/// backend works here, naming the one action that can make typing work for
/// this compositor. Every branch checks whether the helper it names is
/// already installed, so the message never tells the user to install a binary
/// they already have.
pub fn typing_helper_warning() -> &'static str {
    warning_for(
        recommended_helper(),
        wtype_installed(),
        dotool_installed(),
        uinput_ready(),
    )
}

/// The remedy phrase without the leading sentence, shared by the warning and
/// the plan's failure message so both name the same next action.
fn no_helper_remedy() -> &'static str {
    remedy_for(
        recommended_helper(),
        wtype_installed(),
        dotool_installed(),
        uinput_ready(),
    )
}

/// The helper to name when nothing works. `wtype` is named only when the
/// compositor advertises the virtual-keyboard protocol (or, with no display to
/// probe, identifies as wlroots by name); then `libei` if its portal is
/// present; otherwise `dotool`, the helper that works through `/dev/uinput`
/// on KWin and most other compositors.
fn recommended_helper() -> &'static str {
    if session_is_wayland() && compositor_supports_virtual_keyboard() {
        "wtype"
    } else if crate::libei::available() {
        "libei"
    } else {
        "dotool"
    }
}

fn warning_for(
    helper: &str,
    wtype_installed: bool,
    dotool_installed: bool,
    uinput_ready: bool,
) -> &'static str {
    if helper == "wtype" {
        if !wtype_installed {
            "No typing helper found - install wtype, then restart TongueTyped."
        } else if dotool_installed && !uinput_ready {
            "wtype is installed but this compositor did not accept the virtual-keyboard \
             protocol, and dotool cannot open /dev/uinput - add your user to the 'input' \
             group or add a udev rule granting write access to /dev/uinput, then restart \
             TongueTyped."
        } else if dotool_installed {
            "wtype is installed but this compositor did not accept the virtual-keyboard \
             protocol, and dotool could not type here - check your compositor's \
             input-device support, or use the clipboard output method, then restart \
             TongueTyped."
        } else {
            "wtype is installed but this compositor did not accept the virtual-keyboard \
             protocol - install dotool (needs /dev/uinput access) or use the clipboard \
             output method, then restart TongueTyped."
        }
    } else if helper == "libei" {
        "The configured typing backend is unavailable - the native libei backend works on this session; set typing_backend = \"libei\" (or use the clipboard output method), then restart TongueTyped."
    } else if dotool_installed && !uinput_ready {
        "dotool is installed but cannot open /dev/uinput - add your user to the 'input' group or add a udev rule granting write access to /dev/uinput, then restart TongueTyped."
    } else if dotool_installed {
        "dotool is installed but could not type here - check your compositor's input-device support, or use the clipboard output method, then restart TongueTyped."
    } else {
        "No typing helper found - install dotool (needs /dev/uinput access), then restart TongueTyped."
    }
}

fn remedy_for(
    helper: &str,
    wtype_installed: bool,
    dotool_installed: bool,
    uinput_ready: bool,
) -> &'static str {
    if helper == "wtype" {
        if !wtype_installed {
            "install wtype, then restart TongueTyped"
        } else if dotool_installed && !uinput_ready {
            "dotool is installed but cannot open /dev/uinput - add your user to the 'input' \
             group or add a udev rule granting write access to /dev/uinput, then restart \
             TongueTyped"
        } else if dotool_installed {
            "dotool is installed but could not type here - check your compositor's \
             input-device support, or use the clipboard output method, then restart \
             TongueTyped"
        } else {
            "install dotool (it types through /dev/uinput) or use the clipboard output method, \
             then restart TongueTyped"
        }
    } else if helper == "libei" {
        "set typing_backend = \"libei\" (the native libei backend works on this session) or use \
         the clipboard output method, then restart TongueTyped"
    } else if dotool_installed && !uinput_ready {
        "dotool is installed but cannot open /dev/uinput - add your user to the 'input' group or \
         add a udev rule granting write access to /dev/uinput, then restart TongueTyped"
    } else if dotool_installed {
        "dotool is installed but could not type here - check your compositor's input-device \
         support, or use the clipboard output method, then restart TongueTyped"
    } else {
        "install dotool (needs /dev/uinput access), then restart TongueTyped"
    }
}

fn session_is_wayland() -> bool {
    std::env::var("XDG_SESSION_TYPE").is_ok_and(|session| session == "wayland")
}

/// Whether this compositor accepts `wtype`'s protocol.
///
/// The registry probe is authoritative when a Wayland display is reachable.
/// Without one, the desktop-name list is the fallback so a headless sandbox or
/// a daemon that starts before the compositor still gets a useful answer.
fn compositor_supports_virtual_keyboard() -> bool {
    match crate::wayland::virtual_keyboard() {
        Some(capability) => capability.usable(),
        None => {
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
    }
}

/// Pure core of the name fallback, kept separate so it is testable without
/// mutating the process-global environment.
fn desktop_names_are_wlroots(desktops: &[String]) -> bool {
    const WLROOTS: [&str; 7] = [
        "sway", "hyprland", "niri", "wlroots", "river", "wayfire", "labwc",
    ];
    desktops.iter().any(|desktop| {
        let desktop = desktop.to_ascii_lowercase();
        WLROOTS.iter().any(|name| desktop.contains(name))
    })
}

/// `enigo` is the X11 path. Connect to `$DISPLAY` and ask for the XTEST
/// extension version: no window, no event, no focus. `Enigo::new()` alone was
/// weaker, because it can connect to an X server without XTEST and only fail
/// on the first key.
fn probe_enigo() -> Result<(), String> {
    if session_is_wayland() {
        return Err("enigo is X11-only and this is a Wayland session".to_string());
    }
    use x11rb::protocol::xtest::ConnectionExt as _;
    let (connection, _screen) =
        x11rb::connect(None).map_err(|error| format!("no X11 display is reachable: {error}"))?;
    let cookie = connection
        .xtest_get_version(2, 2)
        .map_err(|error| format!("the XTEST extension is unavailable: {error}"))?;
    cookie
        .reply()
        .map_err(|error| format!("the XTEST version probe failed: {error}"))?;
    Ok(())
}

/// `dotool` needs the binary and permission to open `/dev/uinput`. The probe
/// opens the device and asks for the uinput ABI version with
/// `UI_GET_VERSION`; it never creates a device (`UI_DEV_CREATE`) and writes no
/// event, so it leaks no node and cannot disturb the input stack. A kernel
/// that does not answer `UI_GET_VERSION` still proves the open works.
fn probe_dotool() -> Result<(), String> {
    if !dotool_installed() {
        return Err("dotool is not installed".to_string());
    }
    if uinput_ready() {
        Ok(())
    } else {
        Err("/dev/uinput is not writable".to_string())
    }
}

/// `libei` needs the RemoteDesktop portal interface, checked over D-Bus only.
fn probe_libei() -> Result<(), String> {
    if !session_is_wayland() {
        return Err("libei needs a Wayland session".to_string());
    }
    if crate::libei::available() {
        Ok(())
    } else {
        Err("the RemoteDesktop portal is unavailable".to_string())
    }
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

/// Whether `/dev/uinput` can be opened by this user, the capability `dotool`
/// needs. See [`uinput_version_at`] for the probe and why it creates nothing.
fn uinput_ready() -> bool {
    uinput_version_at(std::path::Path::new("/dev/uinput")).is_some()
}

/// Opens `path` read-write and returns the uinput ABI version, or `Some(0)`
/// when the ioctl is unsupported. `None` means the device could not be opened,
/// which is the permission failure the warning text names.
fn uinput_version_at(path: &std::path::Path) -> Option<u32> {
    use std::os::fd::AsRawFd;

    let file = std::fs::OpenOptions::new()
        .read(true)
        .write(true)
        .open(path)
        .ok()?;
    // _IOR('U', 45, unsigned int): the uinput ABI version, no device created.
    const UI_GET_VERSION: libc::c_ulong = 0x8004_552D;
    let mut version: libc::c_uint = 0;
    // SAFETY: `file` owns a valid /dev/uinput descriptor for this call, and
    // the ioctl writes one `unsigned int` into `version`.
    let result = unsafe { libc::ioctl(file.as_raw_fd(), UI_GET_VERSION, &mut version) };
    Some(if result == 0 { version as u32 } else { 0 })
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
    fn test_probe_returns_string() {
        let backend = probe_type_backend();
        assert!(!backend.is_empty());
    }

    #[test]
    fn typing_helper_warning_names_a_concrete_remedy() {
        let warning = typing_helper_warning();
        assert!(
            warning.contains("install wtype")
                || warning.contains("install dotool")
                || warning.contains("typing_backend")
                || warning.contains("input"),
            "the warning must name a concrete remedy: {warning}"
        );
        assert!(
            warning.contains("restart TongueTyped"),
            "the warning must say a restart is needed: {warning}"
        );
    }

    #[test]
    fn libei_warning_points_at_libei_when_the_configured_backend_cannot_type() {
        // Reached when the configured typing backend cannot type but the
        // native libei backend is available on KDE/GNOME Wayland: the remedy
        // is to switch to libei, never to install a helper or to claim the
        // available libei is unavailable.
        let warning = warning_for("libei", true, true, true);
        assert!(
            warning.contains("libei") && warning.contains("typing_backend"),
            "the warning must name switching to libei: {warning}"
        );
        assert!(
            !warning.contains("install dotool") && !warning.contains("install wtype"),
            "a session where libei works must not be told to install another helper: {warning}"
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
        let dotool = warning_for("dotool", false, false, false);
        assert!(dotool.contains("install dotool"));
        assert!(
            dotool.contains("/dev/uinput"),
            "the dotool warning must state its device requirement: {dotool}"
        );
        let wtype = warning_for("wtype", false, false, false);
        assert!(wtype.contains("install wtype"));
        assert!(!wtype.contains("dotool"));
        assert!(!dotool.contains("wtype"));
    }

    #[test]
    fn dotool_warning_names_the_permission_remedy_when_installed() {
        // `dotool` present but `/dev/uinput` unreadable is the state the
        // packaged install creates when it grants no `input` group or udev
        // access: the warning must not tell the user to install it again.
        let warning = warning_for("dotool", true, true, false);
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
    fn uinput_probe_reports_openability_not_a_created_device() {
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
        assert!(
            uinput_version_at(&file).is_some(),
            "an openable path must pass even when the ioctl is unsupported"
        );
        assert!(
            uinput_version_at(&dir.join("missing")).is_none(),
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
    fn a_warning_never_tells_the_user_to_install_an_installed_helper() {
        // The live bug this task fixes: wtype and dotool were both installed
        // and working, yet the no-helper warning told the user to install one
        // of them. Every branch that names an installed helper must say what
        // is actually wrong instead.
        let wtype = warning_for("wtype", true, false, false);
        assert!(
            !wtype.contains("install wtype") && wtype.contains("wtype is installed"),
            "an installed wtype must not be reported missing: {wtype}"
        );
        let dotool = warning_for("dotool", true, true, false);
        assert!(
            !dotool.contains("install dotool") && dotool.contains("/dev/uinput"),
            "an installed dotool must not be reported missing: {dotool}"
        );
        let dotool_ready = warning_for("dotool", true, true, true);
        assert!(
            !dotool_ready.contains("install dotool"),
            "a usable dotool must not be reported missing: {dotool_ready}"
        );
        // The wtype branch can also be reached when wtype is installed but the
        // compositor rejects the protocol; it must check dotool's install and
        // device state before advising another install.
        let wtype_with_forbidden_dotool = warning_for("wtype", true, true, false);
        assert!(
            !wtype_with_forbidden_dotool.contains("install dotool")
                && wtype_with_forbidden_dotool.contains("/dev/uinput"),
            "an installed dotool must get the permission remedy: {wtype_with_forbidden_dotool}"
        );
        let wtype_with_usable_dotool = warning_for("wtype", true, true, true);
        assert!(
            !wtype_with_usable_dotool.contains("install dotool"),
            "a usable dotool must not be reported missing: {wtype_with_usable_dotool}"
        );
    }

    fn plan_with(configured: &str, working: &[&str]) -> TypingPlan {
        let display = DisplayIdentity {
            key: Some("test-display".to_string()),
        };
        resolve_typing_with(configured, display, &|backend| {
            if working.contains(&backend) {
                Ok(())
            } else {
                Err(format!("{backend} probe failed"))
            }
        })
    }

    #[test]
    fn auto_picks_the_first_working_backend_in_probe_order() {
        assert_eq!(
            plan_with("auto", &["wtype", "dotool"]).resolved.as_deref(),
            Some("wtype")
        );
        assert_eq!(
            plan_with("auto", &["dotool"]).resolved.as_deref(),
            Some("dotool")
        );
        assert_eq!(
            plan_with("auto", &["libei"]).resolved.as_deref(),
            Some("libei")
        );
        assert_eq!(plan_with("auto", &[]).resolved, None);
        let dead = plan_with("auto", &[]);
        assert!(
            dead.warning()
                .as_deref()
                .is_some_and(|warning| warning.contains("no typing helper works")),
            "{dead:?}"
        );
    }

    #[test]
    fn a_pinned_backend_that_works_stays_selected() {
        let plan = plan_with("libei", &["wtype", "libei"]);
        assert_eq!(plan.configured, "libei");
        assert_eq!(plan.resolved.as_deref(), Some("libei"));
        assert_eq!(plan.warning(), None);
        assert_eq!(plan.reasons, vec![]);
    }

    #[test]
    fn a_pinned_backend_that_cannot_work_is_replaced_and_reported() {
        // The live bug: libei pinned on Mango, while wtype works. The plan
        // must use wtype and say so instead of attempting libei every time.
        let plan = plan_with("libei", &["wtype"]);
        assert_eq!(plan.configured, "libei");
        assert_eq!(plan.resolved.as_deref(), Some("wtype"));
        let warning = plan.warning().unwrap();
        assert!(warning.contains("libei"), "{warning}");
        assert!(warning.contains("probe failed"), "{warning}");
        assert!(warning.contains("using 'wtype'"), "{warning}");
        assert!(plan
            .reasons
            .iter()
            .any(|failure| failure.backend == "libei"));
    }

    #[test]
    fn a_pinned_backend_with_no_fallback_names_the_dead_end() {
        let plan = plan_with("libei", &[]);
        assert_eq!(plan.resolved, None);
        let warning = plan.failure_message();
        assert!(warning.contains("libei"), "{warning}");
        assert!(
            warning.contains("no other typing helper works"),
            "{warning}"
        );
        assert_eq!(plan.reasons.len(), 4, "every backend was tried");
    }

    #[test]
    fn a_pinned_dotool_failure_names_the_uinput_requirement() {
        let plan = plan_with("dotool", &["wtype"]);
        assert_eq!(plan.resolved.as_deref(), Some("wtype"));
        let warning = plan.warning().unwrap();
        assert!(warning.contains("dotool"), "{warning}");
        assert!(warning.contains("probe failed"), "{warning}");
    }

    #[test]
    fn an_unresolved_plan_wants_a_reprobe_and_a_resolved_one_does_not() {
        let config = OutputConfig::default();
        let dead = plan_with("auto", &[]);
        assert!(dead.needs_reprobe(&config), "nothing working must retry");
        let working = resolve_typing_with("auto", DisplayIdentity::current(), &|_| Ok(()));
        assert!(
            !working.needs_reprobe(&config),
            "a resolved plan on the same display and config must not probe again"
        );
        let mut changed = config.clone();
        changed.typing_backend = "dotool".to_string();
        assert!(
            working.needs_reprobe(&changed),
            "a config change must invalidate the plan"
        );
    }

    #[test]
    fn a_changed_display_invalidates_a_resolved_plan() {
        let config = OutputConfig::default();
        let plan = TypingPlan {
            configured: "auto".to_string(),
            resolved: Some("wtype".to_string()),
            reasons: Vec::new(),
            display: DisplayIdentity {
                key: Some("wayland:wayland-0:1:2:3:4".to_string()),
            },
        };
        assert!(
            !plan.needs_reprobe_for(
                &config,
                &DisplayIdentity {
                    key: Some("wayland:wayland-0:1:2:3:4".to_string())
                }
            ),
            "the same display identity must stay valid"
        );
        assert!(
            plan.needs_reprobe_for(
                &config,
                &DisplayIdentity {
                    key: Some("wayland:wayland-1:9:8:7:6".to_string())
                }
            ),
            "a replaced compositor socket must trigger a re-probe"
        );
    }

    #[test]
    fn a_failure_record_keeps_the_display_it_probed() {
        let plan = plan_with("auto", &["wtype"]);
        assert_eq!(
            plan.display,
            DisplayIdentity {
                key: Some("test-display".to_string())
            }
        );
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
