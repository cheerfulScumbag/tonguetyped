//! Manual validation tool for the layer-shell overlay (src/overlay.rs).
//!
//! Cycles through every semantic state with pauses in between so a human (or
//! a screenshot script) can confirm placement and appearance on a given
//! compositor, without needing a microphone or a downloaded Whisper model.
//! Run with `cargo run --example overlay_preview -- top-left`.
//! Pass a state name (recording/transcribing/success/no-speech/copied/
//! clipboard-fallback/submit-failed/truncated/cancelled/error/waveform) as a
//! second argument to hold on just that one state for 30s,
//! e.g. for screenshotting: `cargo run --example overlay_preview -- top-right
//! recording`. "waveform" holds the Recording phase with
//! `overlay.live_waveform` enabled, to preview the live microphone waveform
//! look instead of the default pulsing dot. Pass a style name
//! (badge/minimal/pill/blob/border/half-circle/long-pill/live-text/
//! live-text-minimal/live-text-box) as a third
//! argument to preview one of the other `overlay.style` looks, e.g. `cargo run
//! --example overlay_preview -- top-right recording blob`. The `border` style
//! ignores `position`: it is a full-screen frame that hugs every screen edge.
//! The
//! `half-circle` style also ignores `position`: it is a semicircle resting on
//! the top edge, always at top-centre. The overlay shows on every connected
//! output by default; pass `overlay.monitor` as a fourth argument
//! (`all`/`active` for every screen, or a monitor name like `DP-1` to pin one)
//! to preview the scoping, e.g. `cargo run --example overlay_preview --
//! top-right recording badge DP-1`.
//!
//! Recording states need a live microphone to react to; with no recorder
//! running this example publishes a synthesized voice-like level (soft bursts
//! separated by quiet gaps) so the waveform/pulse reactivity is visible in the
//! preview.

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
    let monitor = std::env::args().nth(4).unwrap_or_else(|| "all".to_string());
    let mut config = OverlayConfig {
        enabled: true,
        position,
        monitor,
        live_waveform: false,
        style,
    };

    if !tonguetyped::overlay::probe_available() {
        eprintln!("zwlr_layer_shell_v1 is not available on this compositor; nothing to preview.");
        std::process::exit(1);
    }

    // The overlay actor reads the process-global live microphone level
    // (`audio::latest_level`), which no recorder publishes here. Feed it a
    // synthetic voice envelope on a background thread so the recording
    // animations visibly react during the preview.
    std::thread::spawn(|| {
        use std::f32::consts::TAU;
        let start = std::time::Instant::now();
        loop {
            let seconds = start.elapsed().as_secs_f32();
            // Speech bursts (~1.6s of a 2.6s cycle) shaped by a few syllable
            // pulses, with real quiet gaps between them.
            let burst = (seconds % 2.6 < 1.6) as u8 as f32;
            let syllable = 0.55 + 0.45 * (seconds * TAU * 2.7).sin();
            let level = burst * syllable * (0.05 + 0.15 * (seconds * TAU * 0.7).cos().abs());
            tonguetyped::audio::publish_level(level);
            sleep(Duration::from_millis(16));
        }
    });

    let handle = OverlayHandle::new();

    if let Some(name) = hold {
        let event = match name.as_str() {
            "recording" => FeedbackEvent::Recording,
            "waveform" => {
                config.live_waveform = true;
                FeedbackEvent::Recording
            }
            "transcribing" => FeedbackEvent::Processing,
            "success" => FeedbackEvent::Success,
            "no-speech" => FeedbackEvent::NoSpeech,
            "copied" => FeedbackEvent::Copied,
            "clipboard-fallback" => FeedbackEvent::ClipboardFallback,
            "submit-failed" => FeedbackEvent::SubmitFailed,
            "truncated" => FeedbackEvent::Truncated,
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
        ("copied", FeedbackEvent::Copied, Duration::from_secs(2)),
        (
            "clipboard fallback",
            FeedbackEvent::ClipboardFallback,
            Duration::from_secs(3),
        ),
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
