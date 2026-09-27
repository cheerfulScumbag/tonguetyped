use crate::coordinator::Coordinator;
use anyhow::Context;
use ashpd::desktop::global_shortcuts::{GlobalShortcuts, NewShortcut};
use futures_util::StreamExt;
use std::path::Path;
use std::sync::Arc;

const SHORTCUT_ID: &str = "activation";
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

pub async fn test_shortcut_binding(keybind: &str) -> Option<String> {
    async {
        register_host_app().await?;
        let portal = GlobalShortcuts::new().await?;
        let session = portal.create_session().await?;
        let trigger = portal_trigger(keybind)?;
        let shortcut = NewShortcut::new(SHORTCUT_ID, "Start or stop dictation")
            .preferred_trigger(Some(trigger.as_str()));
        let response = portal
            .bind_shortcuts(&session, &[shortcut], None)
            .await?
            .response()?;
        if !response
            .shortcuts()
            .iter()
            .any(|shortcut| shortcut.id() == SHORTCUT_ID)
        {
            anyhow::bail!("global shortcuts portal did not bind activation key");
        }
        Ok::<(), anyhow::Error>(())
    }
    .await
    .err()
    .map(|error| error.to_string())
}

pub fn portal_trigger(keybind: &str) -> anyhow::Result<String> {
    let mut parts: Vec<&str> = keybind.split('+').map(str::trim).collect();
    let key = parts
        .pop()
        .filter(|key| !key.is_empty())
        .ok_or_else(|| anyhow::anyhow!("activation.keybind must contain modifiers and a key"))?;
    if parts.is_empty() || parts.iter().any(|part| part.is_empty()) {
        anyhow::bail!("activation.keybind must contain modifiers and a key");
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
    Ok(format!(
        "{}+{}",
        modifiers.join("+"),
        key.to_ascii_lowercase()
    ))
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
    let trigger = portal_trigger(&keybind)?;
    let shortcut = NewShortcut::new(SHORTCUT_ID, "Start or stop dictation")
        .preferred_trigger(Some(trigger.as_str()));
    let response = portal
        .bind_shortcuts(&session, &[shortcut], None)
        .await?
        .response()?;
    if !response
        .shortcuts()
        .iter()
        .any(|shortcut| shortcut.id() == SHORTCUT_ID)
    {
        let message = "global shortcuts portal did not bind activation key".to_string();
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
