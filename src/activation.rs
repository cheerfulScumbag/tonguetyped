use crate::coordinator::Coordinator;
use ashpd::desktop::global_shortcuts::{GlobalShortcuts, NewShortcut};
use futures_util::{FutureExt, StreamExt};
use std::sync::Arc;

const SHORTCUT_ID: &str = "activation";

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
            "super" | "meta" | "logo" => "LOGO",
            "ctrl" | "control" => "CTRL",
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
    loop {
        let mut events = Vec::new();
        let event = tokio::select! {
            event = activated.next() => event.map(|event| (event.timestamp(), event.shortcut_id() == SHORTCUT_ID, true)),
            event = deactivated.next() => event.map(|event| (event.timestamp(), event.shortcut_id() == SHORTCUT_ID, false)),
        };
        let Some(event) = event else {
            anyhow::bail!("global shortcuts event stream closed");
        };
        events.push(event);
        while let Some(Some(event)) = activated.next().now_or_never() {
            events.push((event.timestamp(), event.shortcut_id() == SHORTCUT_ID, true));
        }
        while let Some(Some(event)) = deactivated.next().now_or_never() {
            events.push((event.timestamp(), event.shortcut_id() == SHORTCUT_ID, false));
        }
        events.sort_by_key(|event| (event.0, !event.2));
        for (_, is_activation, pressed) in events {
            if is_activation {
                if let Err(error) = coordinator.handle_activation(pressed).await {
                    tracing::error!("activation command failed: {error}");
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn normalizes_configured_keybind_for_portal() {
        assert_eq!(portal_trigger("Super+O").unwrap(), "LOGO+o");
        assert_eq!(
            portal_trigger("Ctrl+Shift+Space").unwrap(),
            "CTRL+SHIFT+space"
        );
        assert!(portal_trigger("Super+Super+O").is_err());
        assert!(portal_trigger("Hyper+O").is_err());
    }
}
