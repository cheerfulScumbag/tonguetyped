//! Native typing through Emulated Input (libei) and the XDG RemoteDesktop
//! portal.
//!
//! `wtype` types through the wlroots-only virtual-keyboard protocol, `enigo` is
//! disabled on Wayland, and `dotool` needs write access to `/dev/uinput`. On
//! KDE Plasma and GNOME Wayland without that access, the one input path that
//! works is the compositor's EIS socket reached through the
//! `org.freedesktop.portal.RemoteDesktop` portal, and that is what this module
//! drives.
//!
//! Two properties this module exists to hold:
//!
//! * **The transcript never touches a process argument list.** There is no
//!   helper process: the text is encoded as EI key events and written to the
//!   EIS socket the daemon already owns. Nothing about the transcript is ever
//!   visible in `ps` or `/proc/<pid>/cmdline`.
//! * **One session for the daemon's lifetime.** The portal session and its EIS
//!   file descriptor are opened lazily on the first dictation that needs them
//!   and then held, so a dictation costs one flush, not a portal handshake -
//!   and KDE's "Remote Control" indicator does not flash on every dictation.
//!   A session that dies is dropped and reconnected on the next dictation, and
//!   that dictation falls back to the clipboard rather than losing the text
//!   ([`run`]).
//!
//! The port and a real session cannot be exercised headlessly, so the tests
//! here cover the pure mapping and probe logic and the fallback path, and the
//! live check is manual: on KDE Plasma 6.1+ or GNOME 45+ with no `/dev/uinput`
//! access, dictate once to grant the portal (the dialog names TongueTyped), then
//! dictate again and confirm the text is typed with no dialog and no companion
//! process (`ps -ef | grep -v grep | grep -i ei` shows nothing per dictation).

use crate::config::PasteKeys;
use anyhow::Context;
use ashpd::desktop::remote_desktop::{DeviceType, RemoteDesktop};
use ashpd::desktop::PersistMode;
use reis::ei;
use reis::handshake::ei_handshake_blocking;
use reis::PendingRequestResult;
use std::collections::HashMap;
use std::io::Read;
use std::os::unix::net::UnixStream;
use std::sync::{Mutex, MutexGuard, OnceLock};
use std::time::{Duration, Instant};
use xkbcommon::xkb;

/// The name the EIS connection introduces itself with. The portal dialog names
/// the *application* (via `register_host_app`), not this string, but the
/// compositor logs the EIS client under it and it is what `libei` clients
/// conventionally pass to the handshake.
const EIS_APP_NAME: &str = "tonguetyped";

/// The inter-key delay forced on KDE. KWin can acknowledge a zero-delay burst
/// and still drop its tail, so typing there gets the smallest positive delay
/// that keeps the whole transcript. One millisecond is the value Voxtype uses
/// for the same compositor behavior (`V:src/output/eitype.rs:16-34`).
const KDE_KEY_DELAY: Duration = Duration::from_millis(1);

/// How long startup waits for the compositor to advertise a resumed keyboard
/// device after the portal hands over the EIS socket. A real compositor does
/// this within milliseconds; the bound exists so a broken portal backend fails
/// into the clipboard fallback instead of hanging the dictation.
const KEYBOARD_WAIT: Duration = Duration::from_secs(10);

/// The cached portal restore token, so only the first run on a machine prompts.
/// The portal names this a restore token; it is not a secret and it lives
/// beside the other per-user state under the data directory.
const RESTORE_TOKEN_FILE: &str = "libei-restore-token";

/// Whether this session can type through the portal: a Wayland session whose
/// portal frontend exports `org.freedesktop.portal.RemoteDesktop`.
///
/// The probe is a plain D-Bus interface check, not a portal session: it never
/// calls `CreateSession`, `SelectDevices`, `Start`, or `ConnectToEIS`, so it
/// cannot raise the consent dialog. That keeps it safe to run at daemon load
/// and in the configuration UIs. It is capability-based, not
/// compositor-name-based: a session whose portal does not implement
/// RemoteDesktop (wlroots portals do not) answers false without opening
/// anything. A KDE 6.0 or GNOME 44 session still answers true here and may
/// fail `ConnectToEIS` on first use, which [`run`] turns into the normal
/// clipboard fallback.
pub fn available() -> bool {
    wayland_session() && portal_has_remote_desktop()
}

/// Reads only the portal's D-Bus interface list. Runs on the shared portal
/// runtime from its own thread so a blocking join never nests inside a tokio
/// runtime, the same shape [`portal_stream`] uses. Bounded so a wedged bus
/// cannot delay daemon startup.
fn portal_has_remote_desktop() -> bool {
    std::thread::spawn(|| {
        crate::activation::portal_runtime().block_on(async {
            let probe = async {
                let connection = zbus::Connection::session().await.ok()?;
                let has_owner: bool = connection
                    .call_method(
                        Some("org.freedesktop.DBus"),
                        "/org/freedesktop/DBus",
                        Some("org.freedesktop.DBus"),
                        "NameHasOwner",
                        &("org.freedesktop.portal.Desktop",),
                    )
                    .await
                    .ok()?
                    .body()
                    .deserialize()
                    .ok()?;
                if !has_owner {
                    return None;
                }
                let xml: String = connection
                    .call_method(
                        Some("org.freedesktop.portal.Desktop"),
                        "/org/freedesktop/portal/desktop",
                        Some("org.freedesktop.DBus.Introspectable"),
                        "Introspect",
                        &(),
                    )
                    .await
                    .ok()?
                    .body()
                    .deserialize()
                    .ok()?;
                Some(xml_has_remote_desktop(&xml))
            };
            tokio::time::timeout(std::time::Duration::from_millis(750), probe)
                .await
                .ok()
                .flatten()
                .unwrap_or(false)
        })
    })
    .join()
    .ok()
    .unwrap_or(false)
}

/// Pure core of the introspection check, kept separate for a headless test.
fn xml_has_remote_desktop(xml: &str) -> bool {
    xml.contains("interface name=\"org.freedesktop.portal.RemoteDesktop\"")
}

/// Whether this session is Wayland. A daemon started by a systemd user unit can
/// see `WAYLAND_DISPLAY` without `XDG_SESSION_TYPE`, so either signal counts,
/// matching the clipboard helper's own check.
fn wayland_session() -> bool {
    std::env::var("XDG_SESSION_TYPE").is_ok_and(|session| session == "wayland")
        || std::env::var_os("WAYLAND_DISPLAY").is_some()
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

/// Whether this session is KDE Plasma, whose KWin needs [`KDE_KEY_DELAY`].
fn kde_session() -> bool {
    session_desktops()
        .iter()
        .flat_map(|desktop| desktop.split(':'))
        .any(|component| component.trim().eq_ignore_ascii_case("KDE"))
}

/// Typing goes through one process-global session, created on first use and
/// kept until the process exits or it dies. The `Mutex` is the only thing that
/// ever touches the session, so a dropped session is replaced under the same
/// lock and two dictations can never interleave keystrokes.
static SESSION: OnceLock<Mutex<Option<Session>>> = OnceLock::new();

fn session_slot() -> &'static Mutex<Option<Session>> {
    SESSION.get_or_init(|| Mutex::new(None))
}

fn lock_session(slot: &'static Mutex<Option<Session>>) -> MutexGuard<'static, Option<Session>> {
    // A poisoned lock means another thread panicked while holding the session;
    // the session itself is still usable, so recover rather than propagate.
    slot.lock().unwrap_or_else(|poisoned| poisoned.into_inner())
}

/// Runs `op` against the held session, connecting on first use. A connection or
/// send failure marks the session dead and drops it so the next dictation
/// reconnects; the failed dictation's error reaches the caller, which is
/// `output.rs`'s clipboard fallback.
fn run(op: impl FnOnce(&mut Session) -> anyhow::Result<()>) -> anyhow::Result<()> {
    let slot = session_slot();
    let mut guard = lock_session(slot);
    if guard.is_none() {
        *guard = Some(Session::connect()?);
    }
    let session = guard.as_mut().expect("session is present after connect");
    let result = session.pump().and_then(|()| op(session));
    if session.dead {
        *guard = None;
    }
    result
}

/// Types `text` into the focused application through libei.
pub fn type_text(text: &str) -> anyhow::Result<()> {
    run(|session| session.type_text(text))
}

/// Presses Enter through libei.
pub fn press_enter() -> anyhow::Result<()> {
    run(|session| session.press_named(session.enter_key))
}

/// Presses the paste key combination through libei.
pub fn press_paste(keys: PasteKeys) -> anyhow::Result<()> {
    run(|session| session.press_paste(keys))
}

/// The live EIS session: the portal's EIS stream, the keyboard device and its
/// translated keymap, and the serial/sequence counters the protocol needs.
struct Session {
    context: ei::Context,
    keyboard: Option<ei::Keyboard>,
    device: Option<ei::Device>,
    /// The keyboard capability bit the seat advertised, bound on `seat.done`.
    keyboard_capability: u64,
    /// Whether the keyboard device is resumed and may accept key events.
    resumed: bool,
    /// The last serial the EIS implementation reported; every request carries it.
    last_serial: u32,
    /// The `start_emulating` transaction counter.
    sequence: u32,
    /// Monotonic session origin for EI frame timestamps.
    started: Instant,
    /// The raw keymap the compositor advertised, read from the keymap fd. Held
    /// as bytes rather than a compiled `xkb::Keymap` so the session stays `Send`
    /// without an `unsafe` assertion; it is compiled once into `chars`.
    keymap_bytes: Option<Vec<u8>>,
    /// The active layout group (from a keyboard modifiers event), used to build
    /// the character lookup.
    layout: u32,
    /// The inter-key delay this session types with.
    delay: Duration,
    /// The connection is unusable and must be dropped before the next call.
    dead: bool,
    /// Character -> (evdev keycode, modifier keycodes to hold). Built once from
    /// the keymap, for the four-level layout scheme.
    chars: HashMap<char, (u32, Vec<u32>)>,
    shift_key: Option<u32>,
    control_key: Option<u32>,
    altgr_key: Option<u32>,
    enter_key: Option<u32>,
    insert_key: Option<u32>,
}

impl Session {
    /// Opens the portal session and the EIS connection, then waits for a
    /// resumed keyboard device and builds the character lookup.
    fn connect() -> anyhow::Result<Self> {
        let token = load_restore_token();
        let (stream, new_token) = portal_stream(token)?;
        if let Some(token) = new_token {
            save_restore_token(&token);
        }
        stream
            .set_nonblocking(true)
            .context("could not make the EIS socket non-blocking")?;
        let context = ei::Context::new(stream).context("could not create the EIS connection")?;
        let handshake =
            ei_handshake_blocking(&context, EIS_APP_NAME, ei::handshake::ContextType::Sender)
                .context("the EIS handshake failed")?;
        let mut session = Session {
            context,
            keyboard: None,
            device: None,
            keyboard_capability: 0,
            resumed: false,
            last_serial: handshake.serial,
            sequence: 1,
            started: Instant::now(),
            keymap_bytes: None,
            layout: 0,
            delay: if kde_session() {
                KDE_KEY_DELAY
            } else {
                Duration::ZERO
            },
            dead: false,
            chars: HashMap::new(),
            shift_key: None,
            control_key: None,
            altgr_key: None,
            enter_key: None,
            insert_key: None,
        };
        session.flush()?;
        session.await_keyboard()?;
        session.build_lookup()?;
        session.start_emulating()?;
        Ok(session)
    }

    /// Blocks (briefly) until the compositor has advertised a resumed keyboard
    /// device, pumping requests the EIS implementation expects in between.
    fn await_keyboard(&mut self) -> anyhow::Result<()> {
        let deadline = Instant::now() + KEYBOARD_WAIT;
        loop {
            self.pump()?;
            if self.keyboard.is_some() && self.device.is_some() && self.resumed {
                return Ok(());
            }
            if Instant::now() >= deadline {
                anyhow::bail!("timed out waiting for the compositor's libei keyboard device");
            }
            std::thread::sleep(Duration::from_millis(5));
        }
    }

    /// Reads and handles every pending event, then flushes. Flushing here is
    /// required, not cosmetic: `seat.bind` (when the seat announces its
    /// capabilities) and the pong that answers a ping are both buffered
    /// requests, and the compositor only creates the keyboard device - or keeps
    /// the connection alive - once they reach it.
    fn pump(&mut self) -> anyhow::Result<()> {
        match self.context.read() {
            Ok(_) => {}
            Err(error) if error.kind() == std::io::ErrorKind::UnexpectedEof => {
                self.dead = true;
                anyhow::bail!("the compositor closed the libei connection");
            }
            Err(error) => {
                self.dead = true;
                return Err(error).context("could not read from the libei connection");
            }
        }
        while let Some(request) = self.context.pending_event() {
            match request {
                PendingRequestResult::Request(event) => self.handle_event(event)?,
                PendingRequestResult::ParseError(error) => {
                    self.dead = true;
                    anyhow::bail!("could not parse a libei event: {error}");
                }
                // A request raced a freshly destroyed object; the protocol is
                // asynchronous, so this is informational, not fatal.
                PendingRequestResult::InvalidObject(_) => {}
            }
        }
        self.flush()
    }

    /// Applies one EI event to the session state.
    fn handle_event(&mut self, event: ei::Event) -> anyhow::Result<()> {
        match event {
            ei::Event::Connection(
                _,
                ei::connection::Event::Disconnected {
                    last_serial,
                    reason,
                    explanation,
                },
            ) => {
                self.dead = true;
                self.last_serial = last_serial;
                anyhow::bail!(
                    "the compositor disconnected the libei session: {reason:?} {explanation}"
                );
            }
            ei::Event::Connection(_, ei::connection::Event::Ping { ping }) => {
                ping.done(0);
            }
            ei::Event::Seat(_, ei::seat::Event::Capability { mask, interface }) => {
                if interface == "ei_keyboard" {
                    self.keyboard_capability |= mask;
                }
            }
            ei::Event::Seat(seat, ei::seat::Event::Done) => {
                if self.keyboard_capability != 0 {
                    seat.bind(self.keyboard_capability);
                }
            }
            ei::Event::Device(device, ei::device::Event::Interface { object }) => {
                if object.interface() == "ei_keyboard" {
                    if let Some(keyboard) = object.downcast::<ei::Keyboard>() {
                        self.keyboard = Some(keyboard);
                        self.device = Some(device);
                    }
                }
            }
            ei::Event::Device(device, ei::device::Event::Resumed { serial }) => {
                self.last_serial = serial;
                if self.device.as_ref() == Some(&device) {
                    self.resumed = true;
                }
            }
            ei::Event::Device(device, ei::device::Event::Paused { serial }) => {
                self.last_serial = serial;
                if self.device.as_ref() == Some(&device) {
                    self.resumed = false;
                }
            }
            ei::Event::Keyboard(
                _,
                ei::keyboard::Event::Keymap {
                    keymap_type,
                    size,
                    keymap,
                },
            ) => {
                if keymap_type == ei::keyboard::KeymapType::Xkb {
                    self.keymap_bytes = Some(read_keymap(keymap, size)?);
                }
            }
            ei::Event::Keyboard(_, ei::keyboard::Event::Modifiers { group, .. }) => {
                self.layout = group;
            }
            _ => {}
        }
        Ok(())
    }

    fn start_emulating(&mut self) -> anyhow::Result<()> {
        let device = self
            .device
            .as_ref()
            .ok_or_else(|| anyhow::anyhow!("no libei keyboard device to emulate"))?;
        device.start_emulating(self.last_serial, self.sequence);
        self.sequence = self.sequence.wrapping_add(1);
        self.flush()
    }

    /// Compiles the compositor's keymap (or a system default when none was
    /// advertised) into the character lookup this session types from.
    fn build_lookup(&mut self) -> anyhow::Result<()> {
        let context = xkb::Context::new(xkb::CONTEXT_NO_FLAGS);
        let keymap = self
            .keymap_bytes
            .as_ref()
            .and_then(|bytes| {
                // The compositor's keymap buffer is NUL-terminated and `size`
                // includes the terminator; `new_from_string` panics on an
                // interior NUL, so trim the terminator first.
                let text = String::from_utf8_lossy(bytes);
                let text = text.trim_end_matches('\0');
                xkb::Keymap::new_from_string(
                    &context,
                    text.to_string(),
                    xkb::KEYMAP_FORMAT_TEXT_V1,
                    xkb::KEYMAP_COMPILE_NO_FLAGS,
                )
            })
            .or_else(|| {
                xkb::Keymap::new_from_names(
                    &context,
                    "",
                    "",
                    "",
                    "",
                    None,
                    xkb::KEYMAP_COMPILE_NO_FLAGS,
                )
            })
            .ok_or_else(|| anyhow::anyhow!("could not load a keymap for libei typing"))?;
        self.shift_key = modifier_keycode(&keymap, "Shift");
        self.control_key = modifier_keycode(&keymap, "Control");
        self.altgr_key = modifier_keycode(&keymap, "Mod5");
        self.enter_key = keycode_for_keysym(&keymap, xkb::Keysym::Return, self.layout);
        self.insert_key = keycode_for_keysym(&keymap, xkb::Keysym::Insert, self.layout);
        self.chars = build_char_map(&keymap, self.layout, self.shift_key, self.altgr_key);
        Ok(())
    }

    fn type_text(&mut self, text: &str) -> anyhow::Result<()> {
        for ch in text.chars() {
            self.tap_char(ch)?;
        }
        Ok(())
    }

    fn tap_char(&mut self, ch: char) -> anyhow::Result<()> {
        // Enter is delivered as a newline in dictation; treat CR the same way.
        let ch = if ch == '\r' { '\n' } else { ch };
        let (keycode, mods) = self.chars.get(&ch).cloned().ok_or_else(|| {
            anyhow::anyhow!("the active keyboard layout cannot type {ch:?} through libei")
        })?;
        self.tap(keycode, &mods)
    }

    fn press_named(&mut self, keycode: Option<u32>) -> anyhow::Result<()> {
        let keycode = keycode.ok_or_else(|| {
            anyhow::anyhow!("the active keyboard layout has no key for this action")
        })?;
        self.tap(keycode, &[])
    }

    fn press_paste(&mut self, keys: PasteKeys) -> anyhow::Result<()> {
        let (modifier, keycode) = match keys {
            PasteKeys::CtrlV => (self.control_key, self.chars.get(&'v').map(|(key, _)| *key)),
            PasteKeys::ShiftInsert => (self.shift_key, self.insert_key),
        };
        let modifier = modifier
            .ok_or_else(|| anyhow::anyhow!("the active keyboard layout has no paste modifier"))?;
        let keycode = keycode
            .ok_or_else(|| anyhow::anyhow!("the active keyboard layout has no paste key"))?;
        self.tap(keycode, &[modifier])
    }

    /// Sends one press+release of `keycode` with `mods` held. Every key event
    /// gets its own frame: within a single frame the compositor treats a press
    /// and release of the same key as a logical no-op, so press and release must
    /// not share one. The held modifiers are released with the key so no
    /// modifier is ever left stuck.
    fn tap(&mut self, keycode: u32, mods: &[u32]) -> anyhow::Result<()> {
        for &modifier in mods {
            self.send_key(modifier, ei::keyboard::KeyState::Press)?;
        }
        self.send_key(keycode, ei::keyboard::KeyState::Press)?;
        self.pace();
        self.send_key(keycode, ei::keyboard::KeyState::Released)?;
        for &modifier in mods.iter().rev() {
            self.send_key(modifier, ei::keyboard::KeyState::Released)?;
        }
        self.pace();
        Ok(())
    }

    /// Sends one key request in its own frame.
    fn send_key(&mut self, keycode: u32, state: ei::keyboard::KeyState) -> anyhow::Result<()> {
        let keyboard = self
            .keyboard
            .clone()
            .ok_or_else(|| anyhow::anyhow!("no libei keyboard device"))?;
        let device = self
            .device
            .clone()
            .ok_or_else(|| anyhow::anyhow!("no libei keyboard device"))?;
        if !self.resumed {
            anyhow::bail!("the libei keyboard device is not resumed");
        }
        keyboard.key(keycode, state);
        device.frame(self.last_serial, self.started.elapsed().as_micros() as u64);
        self.flush()
    }

    /// The forced inter-key delay (KDE only): KWin can drop the tail of a fast
    /// key burst, so each key event is spaced by [`KDE_KEY_DELAY`].
    fn pace(&self) {
        if !self.delay.is_zero() {
            std::thread::sleep(self.delay);
        }
    }

    /// Flushes buffered requests, retrying while the socket buffer is full so a
    /// long transcript is not truncated by a transient `EAGAIN`. A non-blocking
    /// flush that cannot make progress is fatal: the session is marked dead and
    /// the caller falls back to the clipboard.
    fn flush(&mut self) -> anyhow::Result<()> {
        for attempt in 0..40u32 {
            match self.context.flush() {
                Ok(()) => return Ok(()),
                Err(error) => {
                    let error: std::io::Error = error.into();
                    if error.kind() == std::io::ErrorKind::WouldBlock {
                        std::thread::sleep(Duration::from_millis(1 << attempt.min(5)));
                        continue;
                    }
                    self.dead = true;
                    return Err(error).context("could not write to the libei connection");
                }
            }
        }
        self.dead = true;
        anyhow::bail!("the libei socket buffer stayed full");
    }
}

/// Opens the RemoteDesktop portal session and returns the EIS socket plus any
/// new restore token. Runs on the shared portal runtime so the cached `zbus`
/// connection's reader task keeps running, on its own thread so blocking here
/// can never nest inside a tokio runtime.
fn portal_stream(restore_token: Option<String>) -> anyhow::Result<(UnixStream, Option<String>)> {
    std::thread::spawn(move || {
        crate::activation::portal_runtime().block_on(async move {
            crate::activation::register_host_app()
                .await
                .context("could not register the desktop application identity")?;
            let proxy = RemoteDesktop::new()
                .await
                .context("the RemoteDesktop portal is unavailable")?;
            let session = proxy
                .create_session()
                .await
                .context("could not create a portal session")?;
            proxy
                .select_devices(
                    &session,
                    DeviceType::Keyboard.into(),
                    restore_token.as_deref(),
                    PersistMode::ExplicitlyRevoked,
                )
                .await
                .context("could not request keyboard emulation")?;
            let response = proxy
                .start(&session, None)
                .await
                .context("could not start the portal session")?
                .response()
                .context("the portal refused the libei session")?;
            let new_token = response.restore_token().map(str::to_string);
            let fd = proxy.connect_to_eis(&session).await.context(
                "the compositor does not support EIS through the portal (needs KDE Plasma 6.1+ or GNOME 45+)",
            )?;
            Ok((UnixStream::from(fd), new_token))
        })
    })
    .join()
    .map_err(|_| anyhow::anyhow!("the libei connection thread panicked"))?
}

fn restore_token_path() -> Option<std::path::PathBuf> {
    directories::BaseDirs::new()
        .map(|dirs| dirs.data_dir().join("tonguetyped").join(RESTORE_TOKEN_FILE))
}

fn load_restore_token() -> Option<String> {
    let path = restore_token_path()?;
    let token = std::fs::read_to_string(path).ok()?;
    let token = token.trim().to_string();
    (!token.is_empty()).then_some(token)
}

/// Persists the portal's restore token so later runs skip the dialog. Best
/// effort: a failure only means the dialog appears again.
fn save_restore_token(token: &str) {
    let Some(path) = restore_token_path() else {
        return;
    };
    if let Some(parent) = path.parent() {
        if std::fs::create_dir_all(parent).is_err() {
            return;
        }
    }
    let _ = std::fs::write(path, token);
}

/// Reads exactly `size` bytes of the keymap the compositor handed over.
fn read_keymap(fd: std::os::fd::OwnedFd, size: u32) -> anyhow::Result<Vec<u8>> {
    let file = std::fs::File::from(fd);
    let mut bytes = Vec::with_capacity(size as usize);
    file.take(size as u64)
        .read_to_end(&mut bytes)
        .context("could not read the keymap from the compositor")?;
    Ok(bytes)
}

/// The evdev keycode a key that produces the named real modifier maps to in
/// this keymap, or `None` when the modifier is unbindable.
///
/// Walks the keycode range and simulates a press through `xkb::State`, so it
/// honors the keymap's modmap (including remaps like `lv3:ralt_alt`). Two
/// heuristics pick the right key when several produce the same modifier: keep
/// the highest keycode per level-0 keysym name (xkb's evdev keycodes file
/// aliases low-numbered phantom keycodes beside the real physical keys), then
/// the lowest among the distinct physical keys (the Left variant).
fn modifier_keycode(keymap: &xkb::Keymap, name: &str) -> Option<u32> {
    let index = keymap.mod_get_index(name);
    if index == xkb::MOD_INVALID {
        return None;
    }
    let bit = 1u32 << index;
    let min = keymap.min_keycode().raw();
    let max = keymap.max_keycode().raw();
    let mut by_keysym: HashMap<String, u32> = HashMap::new();
    for raw in min..=max {
        let keycode = xkb::Keycode::new(raw);
        let mut state = xkb::State::new(keymap);
        state.update_key(keycode, xkb::KeyDirection::Down);
        if state.serialize_mods(xkb::STATE_MODS_DEPRESSED) & bit == 0 {
            continue;
        }
        let keysym = keymap
            .key_get_syms_by_level(keycode, 0, 0)
            .first()
            .map(|sym| xkb::keysym_get_name(*sym))
            .unwrap_or_default();
        let evdev = raw - 8;
        by_keysym
            .entry(keysym)
            .and_modify(|existing| *existing = (*existing).max(evdev))
            .or_insert(evdev);
    }
    by_keysym.values().copied().min()
}

/// The evdev keycode that produces `keysym` at `layout`, searching every key
/// and level. Used for the named keys (Enter, Insert) the character map does
/// not carry.
fn keycode_for_keysym(keymap: &xkb::Keymap, keysym: xkb::Keysym, layout: u32) -> Option<u32> {
    let min = keymap.min_keycode().raw();
    let max = keymap.max_keycode().raw();
    for raw in min..=max {
        let keycode = xkb::Keycode::new(raw);
        let level_layout = if layout < keymap.num_layouts_for_key(keycode) {
            layout
        } else {
            0
        };
        let levels = keymap.num_levels_for_key(keycode, level_layout);
        for level in 0..levels {
            if keymap
                .key_get_syms_by_level(keycode, level_layout, level)
                .contains(&keysym)
            {
                return Some(raw - 8);
            }
        }
    }
    None
}

/// Builds `char -> (evdev keycode, modifiers)` for the standard four-level
/// layout scheme: level 0 unmodified, level 1 Shift, level 2 AltGr (Mod5), and
/// level 3 Shift+AltGr. Keys that do not define the active layout fall back to
/// layout 0 (layout-independent keys such as space and Enter).
fn build_char_map(
    keymap: &xkb::Keymap,
    layout: u32,
    shift: Option<u32>,
    altgr: Option<u32>,
) -> HashMap<char, (u32, Vec<u32>)> {
    let mut map = HashMap::new();
    let min = keymap.min_keycode().raw();
    let max = keymap.max_keycode().raw();
    for raw in min..=max {
        let keycode = xkb::Keycode::new(raw);
        let level_layout = if layout < keymap.num_layouts_for_key(keycode) {
            layout
        } else {
            0
        };
        let levels = keymap.num_levels_for_key(keycode, level_layout);
        for level in 0..levels.min(4) {
            let Some(mods) = level_modifiers(level, shift, altgr) else {
                continue;
            };
            for keysym in keymap.key_get_syms_by_level(keycode, level_layout, level) {
                if let Some(ch) = keysym_to_char(*keysym) {
                    map.entry(ch).or_insert((raw - 8, mods.clone()));
                }
            }
        }
    }
    map
}

/// The modifier keycodes that select `level` in the four-level scheme, or
/// `None` when the level needs a modifier this keymap does not bind.
fn level_modifiers(level: u32, shift: Option<u32>, altgr: Option<u32>) -> Option<Vec<u32>> {
    match level {
        0 => Some(Vec::new()),
        1 => shift.map(|key| vec![key]),
        2 => altgr.map(|key| vec![key]),
        3 => match (shift, altgr) {
            (Some(shift), Some(altgr)) => Some(vec![shift, altgr]),
            _ => None,
        },
        _ => None,
    }
}

/// The character a keysym types, with Return normalized to a newline so
/// dictation's line breaks type through the same lookup as every other
/// character. `None` for keysyms that type nothing (NoSymbol, dead keys).
fn keysym_to_char(keysym: xkb::Keysym) -> Option<char> {
    if keysym == xkb::Keysym::Return {
        return Some('\n');
    }
    let codepoint = xkb::keysym_to_utf32(keysym);
    if codepoint == 0 {
        return None;
    }
    char::from_u32(codepoint)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn introspection_requires_the_remote_desktop_interface() {
        let with = r#"<node>
            <interface name="org.freedesktop.portal.ScreenCast"/>
            <interface name="org.freedesktop.portal.RemoteDesktop"/>
        </node>"#;
        let without = r#"<node>
            <interface name="org.freedesktop.portal.ScreenCast"/>
        </node>"#;
        assert!(xml_has_remote_desktop(with));
        assert!(!xml_has_remote_desktop(without));
        assert!(!xml_has_remote_desktop(""));
        // A substring in an unrelated name must not count.
        assert!(!xml_has_remote_desktop(
            r#"<interface name="org.freedesktop.portal.RemoteDesktopNot"/>"#
        ));
    }

    #[test]
    fn return_types_a_newline_and_other_keysyms_map_to_their_character() {
        assert_eq!(keysym_to_char(xkb::Keysym::Return), Some('\n'));
        assert_eq!(keysym_to_char(xkb::Keysym::new(b'a' as u32)), Some('a'));
        assert_eq!(keysym_to_char(xkb::Keysym::new(b' ' as u32)), Some(' '));
        assert_eq!(keysym_to_char(xkb::Keysym::new(0xe9)), Some('é'));
        assert_eq!(keysym_to_char(xkb::Keysym::NoSymbol), None);
    }

    #[test]
    fn four_level_scheme_maps_levels_to_the_modifiers_it_has() {
        assert_eq!(level_modifiers(0, Some(1), Some(2)), Some(vec![]));
        assert_eq!(level_modifiers(1, Some(1), Some(2)), Some(vec![1]));
        assert_eq!(level_modifiers(2, Some(1), Some(2)), Some(vec![2]));
        assert_eq!(level_modifiers(3, Some(1), Some(2)), Some(vec![1, 2]));
        // A keymap without AltGr cannot reach levels 2 and 3.
        assert_eq!(level_modifiers(2, Some(1), None), None);
        assert_eq!(level_modifiers(3, Some(1), None), None);
        assert_eq!(level_modifiers(4, Some(1), Some(2)), None);
    }

    /// When the system keymap is available, the built lookup maps letters and
    /// their shifted forms to the same key with and without Shift. Skipped
    /// without xkb data files, which the real desktop always has.
    #[test]
    fn character_lookup_maps_letters_and_shifted_forms() {
        let context = xkb::Context::new(xkb::CONTEXT_NO_FLAGS);
        let Some(keymap) = xkb::Keymap::new_from_names(
            &context,
            "",
            "",
            "us",
            "",
            None,
            xkb::KEYMAP_COMPILE_NO_FLAGS,
        ) else {
            return;
        };
        let shift = modifier_keycode(&keymap, "Shift");
        let map = build_char_map(&keymap, 0, shift, modifier_keycode(&keymap, "Mod5"));
        // The `us` layout puts 'a' on evdev keycode 30 unshifted and 'A' on the
        // same key with Shift held.
        assert_eq!(map.get(&'a'), Some(&(30, Vec::new())));
        assert_eq!(map.get(&'A'), shift.map(|shift| (30, vec![shift])).as_ref());
        // Shift modifiers are only ever the ones this keymap binds.
        assert!(shift.is_some(), "the us keymap must bind Shift");
    }
}
