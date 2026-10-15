use crate::config::Config;
use crate::overlay::OverlayHandle;
use std::collections::HashMap;
use std::process::{Command, Stdio};
use std::sync::Arc;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FeedbackEvent {
    Recording,
    Processing,
    Success,
    /// The dictation finished but produced no text: the microphone captured
    /// only silence (muted, unplugged, or pointed at a zero-sample source), so
    /// VAD rejected everything. Distinct from `Success` so a silent no-op
    /// still shows the user why nothing was typed.
    NoSpeech,
    /// `clipboard` mode finished: the transcript is on the clipboard, ready
    /// for the user to paste.
    Copied,
    /// Typing or pasting failed, so the transcript was left on the clipboard
    /// instead. Distinct from `Copied` so the user learns typing is broken.
    ClipboardFallback,
    /// The transcript was inserted, but the trailing Enter (`auto_submit`)
    /// failed, so it may not have been submitted. Distinct from
    /// `ClipboardFallback`: the text is already in the focused application, so
    /// nothing was copied and the user must not paste it again.
    SubmitFailed,
    /// The transcript was delivered, but the model's decode limit cut it short
    /// before end-of-stream, so words are missing. Distinct from `Success` so
    /// the user learns the dictation is partial rather than complete.
    Truncated,
    Cancelled,
    Error,
}

pub trait Feedback: Send + Sync {
    fn send(&self, event: FeedbackEvent, config: &Config);

    /// Show a live-text snapshot (a streaming model's partial transcript) in
    /// the overlay. This is preview-only and never a state transition: it is
    /// painted by the `live-text` overlay presentation, never typed, persisted,
    /// or mirrored to the notification fallback chain (partial words rewriting
    /// in a desktop OSD would be worse than showing nothing). The default is a
    /// no-op, so runtimes and test fakes that do not show live text compile
    /// unchanged.
    fn send_live_text(&self, _text: crate::overlay::LiveText, _config: &Config) {}
}

#[derive(Default)]
pub struct DesktopFeedback {
    overlay: Arc<OverlayHandle>,
}

impl DesktopFeedback {
    pub fn new() -> Self {
        Self {
            overlay: Arc::new(OverlayHandle::new()),
        }
    }
}

impl Feedback for DesktopFeedback {
    fn send(&self, event: FeedbackEvent, config: &Config) {
        let overlay_config = config.overlay.clone();
        let audio = config.audio.clone();
        if !overlay_config.enabled && !audio.feedback_sounds {
            return;
        }

        // The overlay's first probe blocks briefly (Wayland connect + one
        // global-list round trip); every call after that is a non-blocking
        // channel send. Keep this off the coordinator's state-machine thread
        // the same way the legacy D-Bus calls below already were.
        let overlay = self.overlay.clone();
        std::thread::spawn(move || {
            if overlay_config.enabled && !overlay.try_send(event, &overlay_config) {
                show_visual(event);
            }
            if audio.feedback_sounds {
                play_sound(event, audio.feedback_volume, &audio.feedback_device);
            }
        });
    }

    fn send_live_text(&self, text: crate::overlay::LiveText, config: &Config) {
        let overlay_config = config.overlay.clone();
        if !overlay_config.enabled {
            return;
        }
        let overlay = self.overlay.clone();
        // Once the actor is up the send is a non-blocking channel write, so
        // send inline: partials arrive at the rate the model's text changes,
        // and a fresh OS thread per snapshot buys nothing on this path. Only
        // the first call, before the one-time probe has resolved, stays
        // off-thread like `send`. A partial the compositor cannot show is
        // dropped, never routed to the desktop notification fallback.
        if overlay.is_started() {
            overlay.try_send_live_text(text, &overlay_config);
        } else {
            std::thread::spawn(move || {
                overlay.try_send_live_text(text, &overlay_config);
            });
        }
    }
}

pub struct NoFeedback;

impl Feedback for NoFeedback {
    fn send(&self, _event: FeedbackEvent, _config: &Config) {}
}

fn show_visual(event: FeedbackEvent) {
    let use_plasma = std::env::var("XDG_CURRENT_DESKTOP")
        .unwrap_or_default()
        .to_ascii_lowercase()
        .contains("kde");

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
            "dialog-information",
            "Transcribing",
            "low",
        ),
        FeedbackEvent::Success => ("emblem-ok-symbolic", "complete", "Ready", "low"),
        FeedbackEvent::NoSpeech => (
            "audio-input-microphone-muted-symbolic",
            "bell",
            "No speech detected",
            "normal",
        ),
        FeedbackEvent::Copied => (
            "edit-paste-symbolic",
            "message",
            "Copied to clipboard",
            "low",
        ),
        FeedbackEvent::ClipboardFallback => (
            "dialog-warning-symbolic",
            "window-attention",
            "Typing failed, copied to clipboard",
            "normal",
        ),
        FeedbackEvent::SubmitFailed => (
            "dialog-warning-symbolic",
            "alarm-clock",
            "Text entered, but Enter failed",
            "normal",
        ),
        FeedbackEvent::Truncated => (
            "edit-cut-symbolic",
            "dialog-question",
            "Transcript cut short",
            "normal",
        ),
        FeedbackEvent::Cancelled => (
            "process-stop-symbolic",
            "dialog-warning",
            "Cancelled",
            "low",
        ),
        FeedbackEvent::Error => (
            "dialog-error-symbolic",
            "dialog-error",
            "Operation failed",
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
            FeedbackEvent::NoSpeech,
            FeedbackEvent::Copied,
            FeedbackEvent::ClipboardFallback,
            FeedbackEvent::SubmitFailed,
            FeedbackEvent::Truncated,
            FeedbackEvent::Cancelled,
            FeedbackEvent::Error,
        ];
        let styles: Vec<_> = events.into_iter().map(event_style).collect();

        for (index, style) in styles.iter().enumerate() {
            assert!(styles[..index]
                .iter()
                .all(|other| other.1 != style.1 && other.2 != style.2));
        }
        assert_eq!(event_style(FeedbackEvent::Error).2, "Operation failed");
        assert_eq!(event_style(FeedbackEvent::Error).3, "critical");
    }
}
