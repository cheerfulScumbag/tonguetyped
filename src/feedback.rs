use crate::config::{Config, OverlayBackend};
use std::collections::HashMap;
use std::process::{Command, Stdio};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FeedbackEvent {
    Recording,
    Processing,
    Success,
    Cancelled,
    Error,
}

pub trait Feedback: Send + Sync {
    fn send(&self, event: FeedbackEvent, config: &Config);
}

pub struct DesktopFeedback;

impl Feedback for DesktopFeedback {
    fn send(&self, event: FeedbackEvent, config: &Config) {
        let overlay = config.overlay.clone();
        let audio = config.audio.clone();
        if !overlay.enabled && !audio.feedback_sounds {
            return;
        }

        std::thread::spawn(move || {
            if overlay.enabled {
                show_visual(event, &overlay.backend);
            }
            if audio.feedback_sounds {
                play_sound(event, audio.feedback_volume, &audio.feedback_device);
            }
        });
    }
}

pub struct NoFeedback;

impl Feedback for NoFeedback {
    fn send(&self, _event: FeedbackEvent, _config: &Config) {}
}

fn show_visual(event: FeedbackEvent, backend: &OverlayBackend) {
    let use_plasma = match backend {
        OverlayBackend::Auto => std::env::var("XDG_CURRENT_DESKTOP")
            .unwrap_or_default()
            .to_ascii_lowercase()
            .contains("kde"),
        OverlayBackend::Plasma => true,
        OverlayBackend::Notification => false,
    };

    if use_plasma && run_plasma_osd(event) {
        return;
    }
    if !run_notification(event) {
        tracing::warn!("visual feedback unavailable: no desktop notification service");
    }
}

fn run_plasma_osd(event: FeedbackEvent) -> bool {
    let (icon, _, message, _) = event_style(event);
    let Ok(runtime) = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
    else {
        return false;
    };
    runtime.block_on(async {
        let Ok(connection) = ashpd::zbus::Connection::session().await else {
            return false;
        };
        connection
            .call_method(
                Some("org.kde.plasmashell"),
                "/org/kde/osdService",
                Some("org.kde.osdService"),
                "showText",
                &(icon, message),
            )
            .await
            .is_ok()
    })
}

fn run_notification(event: FeedbackEvent) -> bool {
    let (icon, _, message, urgency) = event_style(event);
    let Ok(runtime) = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
    else {
        return false;
    };
    runtime.block_on(async {
        let Ok(connection) = ashpd::zbus::Connection::session().await else {
            return false;
        };
        let mut hints = HashMap::new();
        hints.insert("transient", ashpd::zvariant::Value::from(true));
        hints.insert(
            "urgency",
            ashpd::zvariant::Value::from(urgency_byte(urgency)),
        );
        connection
            .call_method(
                Some("org.freedesktop.Notifications"),
                "/org/freedesktop/Notifications",
                Some("org.freedesktop.Notifications"),
                "Notify",
                &(
                    "TongueTyped",
                    0u32,
                    icon,
                    "TongueTyped",
                    message,
                    Vec::<&str>::new(),
                    hints,
                    1_400i32,
                ),
            )
            .await
            .is_ok()
    })
}

fn urgency_byte(urgency: &str) -> u8 {
    match urgency {
        "low" => 0,
        "critical" => 2,
        _ => 1,
    }
}

fn play_sound(event: FeedbackEvent, volume: f64, device: &str) {
    let (_, sound, _, _) = event_style(event);
    let decibels = if volume == 0.0 {
        -100.0
    } else {
        20.0 * volume.log10()
    };
    let mut command = command("canberra-gtk-play");
    command.args([
        &format!("--id={sound}"),
        &format!("--volume={decibels:.1}"),
        "--description=TongueTyped feedback",
    ]);
    if device != "default" {
        command.env("PULSE_SINK", device);
    }
    if !command.status().is_ok_and(|status| status.success()) {
        tracing::warn!("sound feedback unavailable: install canberra-gtk-play");
    }
}

fn event_style(event: FeedbackEvent) -> (&'static str, &'static str, &'static str, &'static str) {
    match event {
        FeedbackEvent::Recording => (
            "audio-input-microphone-symbolic",
            "audio-volume-change",
            "Listening",
            "normal",
        ),
        FeedbackEvent::Processing => (
            "view-refresh-symbolic",
            "button-pressed",
            "Transcribing",
            "low",
        ),
        FeedbackEvent::Success => ("emblem-ok-symbolic", "complete", "Ready", "low"),
        FeedbackEvent::Cancelled => (
            "process-stop-symbolic",
            "dialog-warning",
            "Cancelled",
            "low",
        ),
        FeedbackEvent::Error => (
            "dialog-error-symbolic",
            "dialog-error",
            "Transcription failed",
            "critical",
        ),
    }
}

fn command(program: &str) -> Command {
    let mut command = Command::new(program);
    command
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null());
    command
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_state_has_distinct_restrained_copy_and_sound() {
        let events = [
            FeedbackEvent::Recording,
            FeedbackEvent::Processing,
            FeedbackEvent::Success,
            FeedbackEvent::Cancelled,
            FeedbackEvent::Error,
        ];
        let styles: Vec<_> = events.into_iter().map(event_style).collect();

        for (index, style) in styles.iter().enumerate() {
            assert!(styles[..index]
                .iter()
                .all(|other| other.1 != style.1 && other.2 != style.2));
        }
        assert_eq!(event_style(FeedbackEvent::Error).3, "critical");
    }
}
