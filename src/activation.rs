use crate::coordinator::Coordinator;
use anyhow::Context;
use ashpd::desktop::global_shortcuts::{GlobalShortcuts, NewShortcut};
use ashpd::desktop::Session;
use futures_util::StreamExt;
use std::path::Path;
use std::sync::Arc;
use std::time::Duration;

const SHORTCUT_ID: &str = "activation";
// A press is a single keystroke - the user either does it within a few
// seconds or the configured combo doesn't reach this app at all (wrong
// combo, grabbed by something else, desktop shortcut portal unavailable).
const SHORTCUT_PRESS_TIMEOUT: Duration = Duration::from_secs(15);
// `ConfigureShortcuts` opens the desktop's own native dialog and hands
// control to the user for as long as it takes them to press the new combo -
// generous on purpose, unlike the single-keystroke timeout above.
const SHORTCUT_RECONFIGURE_TIMEOUT: Duration = Duration::from_secs(120);
const INSTALLED_APPLICATION_ID: &str = "io.github.cheerfulScumbag.tonguetyped";
const DEVELOPMENT_APPLICATION_ID: &str = "io.github.cheerfulScumbag.tonguetyped.Devel";
const DEVELOPMENT_DESKTOP_ENTRY: &str = concat!(
    include_str!("../data/tonguetyped.desktop"),
    "NoDisplay=true\n"
);

fn application_id() -> anyhow::Result<&'static str> {
    if !cfg!(debug_assertions) {
        return Ok(INSTALLED_APPLICATION_ID);
    }

    let data_dir = directories::BaseDirs::new()
        .context("could not determine the user data directory")?
        .data_dir()
        .to_owned();
    install_development_desktop_entry(&data_dir)?;
    Ok(DEVELOPMENT_APPLICATION_ID)
}

fn install_development_desktop_entry(data_dir: &Path) -> anyhow::Result<()> {
    let applications_dir = data_dir.join("applications");
    std::fs::create_dir_all(&applications_dir).with_context(|| {
        format!(
            "could not create desktop application directory {}",
            applications_dir.display()
        )
    })?;
    let path = applications_dir.join(format!("{DEVELOPMENT_APPLICATION_ID}.desktop"));
    std::fs::write(&path, DEVELOPMENT_DESKTOP_ENTRY).with_context(|| {
        format!(
            "could not write development desktop entry {}",
            path.display()
        )
    })
}

async fn register_host_app() -> anyhow::Result<()> {
    ashpd::register_host_app(application_id()?.try_into()?)
        .await
        .context(
            "desktop application identity is unavailable; sign out and back in after installing TongueTyped",
        )?;
    Ok(())
}

/// Binds the "activation" shortcut on an already-created session, using
/// `keybind` as the portal's `preferred_trigger` hint. Per the XDG
/// GlobalShortcuts portal spec, that hint is only honored the very first
/// time this app ever binds this shortcut id - every later call keeps
/// whatever trigger the desktop already has on file for it, regardless of
/// what's passed here. Shared by `listen()`, `test_shortcut_binding()`, and
/// `reconfigure_shortcut()`, all three of which need a bound shortcut before
/// they can listen for presses or open the native reconfigure dialog.
async fn bind_activation_shortcut<'a>(
    portal: &GlobalShortcuts<'a>,
    session: &Session<'a, GlobalShortcuts<'a>>,
    keybind: &str,
) -> anyhow::Result<String> {
    let trigger = portal_trigger(keybind)?;
    let shortcut = NewShortcut::new(SHORTCUT_ID, "Start or stop dictation")
        .preferred_trigger(Some(trigger.as_str()));
    let response = portal
        .bind_shortcuts(session, &[shortcut], None)
        .await?
        .response()?;
    response
        .shortcuts()
        .iter()
        .find(|shortcut| shortcut.id() == SHORTCUT_ID)
        .map(|shortcut| shortcut.trigger_description().to_string())
        .ok_or_else(|| anyhow::anyhow!("global shortcuts portal did not bind activation key"))
}

pub enum ShortcutTestOutcome {
    /// The configured shortcut was actually pressed and observed via the
    /// portal's `Activated` signal - not just accepted by `BindShortcuts`.
    Pressed,
    TimedOut,
}

/// Binds the shortcut and then actually listens for it to be pressed (via
/// the same `receive_activated()` stream `listen()` uses), instead of only
/// checking that `bind_shortcuts` accepted the registration. The old
/// behavior reported "shortcut binding available" for any syntactically
/// valid keybind without ever confirming a press reached the app.
pub async fn test_shortcut_binding(keybind: &str) -> Result<ShortcutTestOutcome, String> {
    test_shortcut_binding_inner(keybind)
        .await
        .map_err(|error| error.to_string())
}

async fn test_shortcut_binding_inner(keybind: &str) -> anyhow::Result<ShortcutTestOutcome> {
    register_host_app().await?;
    let portal = GlobalShortcuts::new().await?;
    let session = portal.create_session().await?;
    bind_activation_shortcut(&portal, &session, keybind).await?;

    let mut activated = portal.receive_activated().await?;
    let wait_for_press = async {
        loop {
            let Some(event) = activated.next().await else {
                anyhow::bail!("global shortcuts event stream closed");
            };
            if event.shortcut_id() == SHORTCUT_ID {
                return Ok(());
            }
        }
    };
    match tokio::time::timeout(SHORTCUT_PRESS_TIMEOUT, wait_for_press).await {
        Ok(result) => result.map(|()| ShortcutTestOutcome::Pressed),
        Err(_) => Ok(ShortcutTestOutcome::TimedOut),
    }
}

pub struct ReconfigureOutcome {
    /// The portal's human-readable description of whatever trigger is now
    /// actually bound - not the string the user typed, which the desktop is
    /// free to ignore once the shortcut has been bound once (see
    /// `bind_activation_shortcut`'s doc comment).
    pub trigger_description: String,
}

/// Opens the desktop's own native "press your new shortcut" dialog
/// (`GlobalShortcuts::configure_shortcuts`) so the user can actually change
/// an already-bound shortcut's trigger, then reports back whatever trigger
/// is really bound afterwards. `preferred_trigger` (what `bind_shortcuts`
/// alone relies on) cannot do this once a shortcut has ever been bound
/// before - see the diagnosis in `bind_activation_shortcut`'s doc comment.
pub async fn reconfigure_shortcut(keybind: &str) -> Result<ReconfigureOutcome, String> {
    reconfigure_shortcut_inner(keybind)
        .await
        .map_err(|error| error.to_string())
}

async fn reconfigure_shortcut_inner(keybind: &str) -> anyhow::Result<ReconfigureOutcome> {
    register_host_app().await?;
    let portal = GlobalShortcuts::new().await?;
    let session = portal.create_session().await?;
    // `ConfigureShortcuts` requires a session that has already bound at
    // least one shortcut - this is that bind. Its `preferred_trigger` only
    // matters if this is the very first time "activation" has ever been
    // bound for this app; otherwise it's a no-op and the dialog below is
    // what actually changes the trigger.
    bind_activation_shortcut(&portal, &session, keybind).await?;

    // Subscribe before opening the dialog, not after: the dialog can close
    // (and emit ShortcutsChanged) at any point once ConfigureShortcuts is
    // called, and that call itself returns immediately without waiting for
    // the dialog - subscribing afterwards could miss the signal entirely.
    let mut changed = portal.receive_shortcuts_changed().await?;
    portal.configure_shortcuts(&session, None, None).await?;

    let wait_for_change = async {
        loop {
            let Some(event) = changed.next().await else {
                anyhow::bail!("global shortcuts event stream closed");
            };
            if let Some(shortcut) = event.shortcuts().iter().find(|s| s.id() == SHORTCUT_ID) {
                return Ok(shortcut.trigger_description().to_string());
            }
        }
    };
    let trigger_description = tokio::time::timeout(SHORTCUT_RECONFIGURE_TIMEOUT, wait_for_change)
        .await
        .map_err(|_| anyhow::anyhow!("timed out waiting for the shortcut dialog"))??;
    Ok(ReconfigureOutcome {
        trigger_description,
    })
}

pub fn portal_trigger(keybind: &str) -> anyhow::Result<String> {
    let mut parts: Vec<&str> = keybind.split('+').map(str::trim).collect();
    let key = parts
        .pop()
        .filter(|key| !key.is_empty())
        .ok_or_else(|| anyhow::anyhow!("activation.keybind must contain a key"))?;
    if parts.iter().any(|part| part.is_empty()) {
        anyhow::bail!("activation.keybind has an empty modifier");
    }

    let mut modifiers = Vec::new();
    for modifier in parts {
        let modifier = match modifier.to_ascii_lowercase().as_str() {
            "super" => "LOGO",
            "ctrl" => "CTRL",
            "alt" => "ALT",
            "shift" => "SHIFT",
            _ => anyhow::bail!("unsupported activation modifier: {modifier}"),
        };
        if modifiers.contains(&modifier) {
            anyhow::bail!("duplicate activation modifier: {modifier}");
        }
        modifiers.push(modifier);
    }
    let key = key.to_ascii_lowercase();
    if modifiers.is_empty() {
        return Ok(key);
    }
    Ok(format!("{}+{key}", modifiers.join("+")))
}

pub async fn listen(
    coordinator: Arc<Coordinator>,
    keybind: String,
    ready: tokio::sync::oneshot::Sender<Result<(), String>>,
) -> anyhow::Result<()> {
    if let Err(error) = register_host_app().await {
        let message = format!("failed to register desktop application identity: {error}");
        let _ = ready.send(Err(message.clone()));
        anyhow::bail!(message);
    }
    let portal = match GlobalShortcuts::new().await {
        Ok(portal) => portal,
        Err(error) => {
            let message = format!("global shortcuts portal unavailable: {error}");
            let _ = ready.send(Err(message.clone()));
            anyhow::bail!(message);
        }
    };
    let session = portal.create_session().await?;
    if let Err(error) = bind_activation_shortcut(&portal, &session, &keybind).await {
        let message = error.to_string();
        let _ = ready.send(Err(message.clone()));
        anyhow::bail!(message);
    }

    let mut activated = portal.receive_activated().await?;
    let mut deactivated = portal.receive_deactivated().await?;
    let _ = ready.send(Ok(()));
    let mut event_order = EventOrder::default();
    loop {
        let event = tokio::select! {
            event = activated.next() => event.map(|event| (event.timestamp(), event.shortcut_id() == SHORTCUT_ID, true)),
            event = deactivated.next() => event.map(|event| (event.timestamp(), event.shortcut_id() == SHORTCUT_ID, false)),
        };
        let Some(event) = event else {
            anyhow::bail!("global shortcuts event stream closed");
        };
        let (timestamp, is_activation, pressed) = event;
        if is_activation {
            for pressed in event_order.push(timestamp, pressed) {
                if let Err(error) = coordinator.handle_activation(pressed).await {
                    tracing::error!("activation command failed: {error}");
                }
            }
        }
    }
}

#[derive(Default)]
struct EventOrder {
    press_timestamp: Option<std::time::Duration>,
    pending_release: Option<std::time::Duration>,
}

impl EventOrder {
    fn push(&mut self, timestamp: std::time::Duration, pressed: bool) -> Vec<bool> {
        if pressed {
            self.press_timestamp = Some(timestamp);
            let mut events = vec![true];
            if self
                .pending_release
                .take()
                .is_some_and(|release| timestamp <= release)
            {
                self.press_timestamp = None;
                events.push(false);
            }
            events
        } else if let Some(press_timestamp) = self.press_timestamp {
            if timestamp < press_timestamp {
                Vec::new()
            } else {
                self.press_timestamp = None;
                vec![false]
            }
        } else {
            self.pending_release = Some(timestamp);
            Vec::new()
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn installs_discoverable_development_application_identity() {
        let data_dir = Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("target")
            .join(format!("activation-test-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&data_dir);

        install_development_desktop_entry(&data_dir).unwrap();

        let entry = data_dir
            .join("applications")
            .join(format!("{DEVELOPMENT_APPLICATION_ID}.desktop"));
        assert_eq!(
            std::fs::read_to_string(entry).unwrap(),
            DEVELOPMENT_DESKTOP_ENTRY
        );
        std::fs::remove_dir_all(data_dir).unwrap();
    }

    #[test]
    fn normalizes_configured_keybind_for_portal() {
        assert_eq!(portal_trigger("Super+O").unwrap(), "LOGO+o");
        assert_eq!(
            portal_trigger("Ctrl+Shift+Space").unwrap(),
            "CTRL+SHIFT+space"
        );
        assert!(portal_trigger("Super+Super+O").is_err());
        assert!(portal_trigger("Hyper+O").is_err());
        assert!(portal_trigger("Meta+O").is_err());
        assert!(portal_trigger("Control+O").is_err());
    }

    #[test]
    fn accepts_bare_key_without_modifier() {
        assert_eq!(portal_trigger("F13").unwrap(), "f13");
        assert_eq!(portal_trigger("Alt_R").unwrap(), "alt_r");
        assert_eq!(portal_trigger(" F13 ").unwrap(), "f13");
        assert!(portal_trigger("").is_err());
        assert!(portal_trigger("+").is_err());
        assert!(portal_trigger("+F13").is_err());
        assert!(portal_trigger("Ctrl+").is_err());
        assert!(portal_trigger("Ctrl++F13").is_err());
    }

    #[test]
    fn orders_release_that_arrives_before_earlier_press() {
        let mut order = EventOrder::default();
        assert!(order
            .push(std::time::Duration::from_millis(20), false)
            .is_empty());
        assert_eq!(
            order.push(std::time::Duration::from_millis(10), true),
            [true, false]
        );
    }

    #[test]
    fn ignores_late_delivery_of_release_before_active_press() {
        let mut order = EventOrder::default();
        assert_eq!(
            order.push(std::time::Duration::from_millis(20), true),
            [true]
        );
        assert!(order
            .push(std::time::Duration::from_millis(10), false)
            .is_empty());
        assert_eq!(
            order.push(std::time::Duration::from_millis(30), false),
            [false]
        );
    }
}
