//! Manual validation tool for the layer-shell overlay (src/overlay.rs).
//!
//! Cycles through every semantic state with pauses in between so a human (or
//! a screenshot script) can confirm placement and appearance on a given
//! compositor, without needing a microphone or a downloaded Whisper model.
//! Run with `cargo run --example overlay_preview -- top-left`.
//! Pass a state name (recording/transcribing/success/cancelled/error/
//! streaming) as a second argument to hold on just that one state for 30s,
//! e.g. for screenshotting: `cargo run --example overlay_preview -- top-right
//! recording`. "streaming" holds the Recording phase with
//! `overlay.streaming_indicator` enabled, to preview the live-capture
//! waveform look instead of the default pulse. Pass a style name
//! (badge/minimal/pill/blob/border) as a third argument to preview one of the
//! other `overlay.style` looks, e.g. `cargo run --example overlay_preview --
//! top-right recording blob`. The `border` style ignores `position`: it is a
//! full-screen frame that hugs every screen edge.

use std::thread::sleep;
use std::time::Duration;

use tonguetyped::config::OverlayConfig;
use tonguetyped::feedback::FeedbackEvent;
use tonguetyped::overlay::OverlayHandle;

fn main() {
    tracing_subscriber::fmt().with_env_filter("debug").init();
    let position = std::env::args()
        .nth(1)
        .unwrap_or_else(|| "top-right".to_string());
    let hold = std::env::args().nth(2);
    let style = std::env::args()
        .nth(3)
        .unwrap_or_else(|| "badge".to_string());
    let mut config = OverlayConfig {
        enabled: true,
        position,
        monitor: "active".to_string(),
        streaming_indicator: false,
        style,
    };

    if !tonguetyped::overlay::probe_available() {
        eprintln!("zwlr_layer_shell_v1 is not available on this compositor; nothing to preview.");
        std::process::exit(1);
    }

    let handle = OverlayHandle::new();

    if let Some(name) = hold {
        let event = match name.as_str() {
            "recording" => FeedbackEvent::Recording,
            "streaming" => {
                config.streaming_indicator = true;
                FeedbackEvent::Recording
            }
            "transcribing" => FeedbackEvent::Processing,
            "success" => FeedbackEvent::Success,
            "cancelled" => FeedbackEvent::Cancelled,
            "error" => FeedbackEvent::Error,
            other => {
                eprintln!("unknown state {other}");
                std::process::exit(1);
            }
        };
        handle.try_send(event, &config);
        sleep(Duration::from_secs(30));
        return;
    }

    let sequence = [
        (
            "recording",
            FeedbackEvent::Recording,
            Duration::from_secs(2),
        ),
        (
            "transcribing",
            FeedbackEvent::Processing,
            Duration::from_secs(2),
        ),
        ("success", FeedbackEvent::Success, Duration::from_secs(2)),
        (
            "recording (again)",
            FeedbackEvent::Recording,
            Duration::from_secs(2),
        ),
        (
            "cancelled",
            FeedbackEvent::Cancelled,
            Duration::from_secs(2),
        ),
        (
            "recording (again)",
            FeedbackEvent::Recording,
            Duration::from_secs(2),
        ),
        ("error", FeedbackEvent::Error, Duration::from_secs(3)),
    ];

    for (label, event, pause) in sequence {
        println!("{label}");
        if !handle.try_send(event, &config) {
            eprintln!("overlay unavailable after startup; exiting");
            std::process::exit(1);
        }
        sleep(pause);
    }

    println!("done; overlay should now have auto-hidden");
    sleep(Duration::from_secs(2));
}
