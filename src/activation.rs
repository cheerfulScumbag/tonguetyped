use crate::coordinator::Coordinator;
use anyhow::Context;
use ashpd::desktop::global_shortcuts::{GlobalShortcuts, NewShortcut};
use ashpd::desktop::Session;
use futures_util::StreamExt;
use std::path::Path;
use std::sync::Arc;
use std::time::Duration;

const SHORTCUT_ID: &str = "activation";
/// The human-readable action description the desktop shows in its shortcut
/// settings. Per the XDG GlobalShortcuts portal spec a `preferred_trigger`
/// is what the desktop stores as the action's "Default shortcut"; passing
/// one made KDE display an app-chosen default (`Meta+O`) beside the binding
/// the user actually set. TongueTyped therefore registers the shortcut with
/// this description and **no** preferred/default trigger, so the desktop
/// presents only the user's own binding.
const SHORTCUT_DESCRIPTION: &str = "Start or stop dictation";
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

/// Binds the "activation" shortcut on an already-created session with its
/// id and human description only - no `preferred_trigger`. The return value
/// is whatever trigger the desktop already has bound for the action (empty
/// when none), never an app-chosen key. Shared by `listen()`,
/// `test_shortcut_binding()`, and `reconfigure_shortcut()`, all three of
/// which need a bound shortcut before they can listen for presses or open
/// the native reconfigure dialog.
async fn bind_activation_shortcut<'a>(
    portal: &GlobalShortcuts<'a>,
    session: &Session<'a, GlobalShortcuts<'a>>,
) -> anyhow::Result<String> {
    let shortcut = NewShortcut::new(SHORTCUT_ID, SHORTCUT_DESCRIPTION);
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
/// checking that `bind_shortcuts` accepted the registration. Tests whatever
/// trigger the desktop currently has bound for the action; the user sets
/// that trigger through the desktop's own dialog (`reconfigure_shortcut`).
pub async fn test_shortcut_binding() -> Result<ShortcutTestOutcome, String> {
    test_shortcut_binding_inner()
        .await
        .map_err(|error| error.to_string())
}

async fn test_shortcut_binding_inner() -> anyhow::Result<ShortcutTestOutcome> {
    register_host_app().await?;
    let portal = GlobalShortcuts::new().await?;
    let session = portal.create_session().await?;
    bind_activation_shortcut(&portal, &session).await?;

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
/// (`GlobalShortcuts::configure_shortcuts`) so the user can set or change
/// the activation shortcut's trigger, then reports back whatever trigger is
/// really bound afterwards. This is the mechanism for setting a key now that
/// the registration ships no preferred/default trigger, and it works for the
/// first-ever bind exactly as it does for a rebind.
pub async fn reconfigure_shortcut() -> Result<ReconfigureOutcome, String> {
    reconfigure_shortcut_inner()
        .await
        .map_err(|error| error.to_string())
}

async fn reconfigure_shortcut_inner() -> anyhow::Result<ReconfigureOutcome> {
    register_host_app().await?;
    let portal = GlobalShortcuts::new().await?;
    let session = portal.create_session().await?;
    // `ConfigureShortcuts` requires a session that has already bound at
    // least one shortcut - this is that bind. It carries no preferred
    // trigger, so the dialog below is the only thing that sets the trigger.
    bind_activation_shortcut(&portal, &session).await?;

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

/// Rewrites the portal's trigger description into TongueTyped's own wording
/// for display: KDE calls the Super key `Meta`, so `Meta+O` becomes
/// `Super+O`. The stored value is only ever what the desktop reports is
/// bound - this is pure display normalization, never something registered
/// back to the desktop.
pub fn keybind_label(reported: &str) -> String {
    reported.replace("Meta", "Super")
}

/// The user-facing rendering of whatever the desktop reports is bound:
/// `keybind_label`, or a friendly placeholder when nothing is bound yet.
pub fn keybind_display(reported: &str) -> String {
    if reported.trim().is_empty() {
        "(none set yet)".to_string()
    } else {
        keybind_label(reported)
    }
}

pub async fn listen(
    coordinator: Arc<Coordinator>,
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
    let reported = match bind_activation_shortcut(&portal, &session).await {
        Ok(reported) => reported,
        Err(error) => {
            let message = error.to_string();
            let _ = ready.send(Err(message.clone()));
            anyhow::bail!(message);
        }
    };
    // The registration above is the app's only source of truth for what is
    // actually bound: persist whatever the desktop reports so `config` never
    // claims a key TongueTyped chose.
    coordinator.record_activation_binding(reported);

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
    fn reports_kde_wording_as_the_apps_own_super_modifier() {
        assert_eq!(keybind_label("Meta+O"), "Super+O");
        assert_eq!(keybind_label("Ctrl+Meta+Space"), "Ctrl+Super+Space");
        assert_eq!(keybind_label("Ctrl+Shift+Space"), "Ctrl+Shift+Space");
        assert_eq!(keybind_label(""), "");
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
