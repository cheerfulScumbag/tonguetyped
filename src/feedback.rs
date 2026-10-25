use crate::config::Config;
use crate::overlay::OverlayHandle;
use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};
use cpal::{FromSample, SizedSample, I24, U24};
use std::collections::HashMap;
use std::fmt;
use std::io::Cursor;
use std::sync::{mpsc, Arc};
use std::time::Duration;

/// The embedded feedback sounds. Each one is a short, quiet mono WAV baked
/// into the binary, so playback needs no theme, no `$PATH` entry, and no
/// helper program. `assets/sounds/` holds the sources; the daemon and the
/// setup console both play these exact bytes.
const RECORDING_WAV: &[u8] = include_bytes!("../assets/sounds/recording.wav");
const PROCESSING_WAV: &[u8] = include_bytes!("../assets/sounds/processing.wav");
const SUCCESS_WAV: &[u8] = include_bytes!("../assets/sounds/success.wav");
const NO_SPEECH_WAV: &[u8] = include_bytes!("../assets/sounds/no_speech.wav");
const COPIED_WAV: &[u8] = include_bytes!("../assets/sounds/copied.wav");
const CLIPBOARD_FALLBACK_WAV: &[u8] = include_bytes!("../assets/sounds/clipboard_fallback.wav");
const SUBMIT_FAILED_WAV: &[u8] = include_bytes!("../assets/sounds/submit_failed.wav");
const TRUNCATED_WAV: &[u8] = include_bytes!("../assets/sounds/truncated.wav");
const CANCELLED_WAV: &[u8] = include_bytes!("../assets/sounds/cancelled.wav");
const ERROR_WAV: &[u8] = include_bytes!("../assets/sounds/error.wav");
const CONFIG_ERROR_WAV: &[u8] = include_bytes!("../assets/sounds/config_error.wav");

/// How long a stream may take beyond the sound's own length before playback
/// is treated as stuck. Device open is synchronous in cpal, so this only
/// guards the callback actually rendering the buffered samples.
const PLAYBACK_TIMEOUT_GRACE: Duration = Duration::from_secs(2);
/// Let the device drain its last buffer before the stream is dropped;
/// otherwise the final few milliseconds can be cut off.
const PLAYBACK_DRAIN: Duration = Duration::from_millis(30);

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
    /// A config-file edit was rejected, so the daemon kept its last good
    /// config. Distinct from `Error` so the user is told the edit was rejected
    /// and pointed at `tonguetyped status` for the detail instead of a generic
    /// failure.
    ConfigError,
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

    /// Shows `event` off-thread, falling back to the desktop notification path
    /// when the overlay is unavailable.
    fn send_command(&self, event: FeedbackEvent, config: &Config) {
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
                if let Err(error) =
                    play_feedback_sound(event, audio.feedback_volume, &audio.feedback_device)
                {
                    match &error {
                        SoundError::NoOutputDevice { .. } => {
                            tracing::warn!("sound feedback unavailable: {error}")
                        }
                        SoundError::Playback(_) => {
                            tracing::warn!("sound feedback failed: {error}")
                        }
                    }
                }
            }
        });
    }
}

impl Feedback for DesktopFeedback {
    fn send(&self, event: FeedbackEvent, config: &Config) {
        self.send_command(event, config);
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
    let (icon, message, _) = event_style(event);
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
    let (icon, message, urgency) = event_style(event);
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

/// Why a feedback sound could not be played. Kept distinct so the daemon
/// journal and the setup console can tell "this machine has no output device"
/// from "the device exists but playback failed", and both carry the underlying
/// detail instead of a generic hint.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SoundError {
    /// No usable output device: the default device is absent, or the
    /// configured one no longer exists (or only supports input).
    NoOutputDevice { requested: String },
    /// The device was found, but decoding, configuring, or rendering the
    /// sound failed. The string is the underlying error text.
    Playback(String),
}

impl fmt::Display for SoundError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::NoOutputDevice { requested } if requested == "default" => {
                write!(formatter, "no audio output device")
            }
            Self::NoOutputDevice { requested } => {
                write!(formatter, "no audio output device '{requested}'")
            }
            Self::Playback(detail) => write!(formatter, "audio playback failed: {detail}"),
        }
    }
}

impl std::error::Error for SoundError {}

/// Plays `event`'s embedded sound in-process through cpal. Returns the real
/// failure, so callers that can show it (the setup console) do not have to
/// guess from a journal line.
pub fn play_feedback_sound(
    event: FeedbackEvent,
    volume: f64,
    device: &str,
) -> Result<(), SoundError> {
    let gain = volume_gain(volume);
    if gain == 0.0 {
        return Ok(());
    }
    let (_, wav) = sound_for(event);
    let (samples, sample_rate) = decode_wav(wav)?;
    #[cfg(target_os = "linux")]
    let _alsa_errors = alsa::Output::local_error_handler().ok();
    let host = cpal::default_host();
    let output = find_output_device(&host, device)?;
    play_on_device(&output, &samples, sample_rate, gain)
}

/// `audio.feedback_volume` is the same 0..1 scale the old dB conversion used:
/// `20 * log10(volume)` dB is exactly a linear gain of `volume`.
fn volume_gain(volume: f64) -> f32 {
    if !volume.is_finite() {
        return 0.0;
    }
    volume.clamp(0.0, 1.0) as f32
}

/// The WAV embedded for each event. `name` is the asset's file stem, used by
/// tests to prove every event selects its own sound.
fn sound_for(event: FeedbackEvent) -> (&'static str, &'static [u8]) {
    match event {
        FeedbackEvent::Recording => ("recording", RECORDING_WAV),
        FeedbackEvent::Processing => ("processing", PROCESSING_WAV),
        FeedbackEvent::Success => ("success", SUCCESS_WAV),
        FeedbackEvent::NoSpeech => ("no_speech", NO_SPEECH_WAV),
        FeedbackEvent::Copied => ("copied", COPIED_WAV),
        FeedbackEvent::ClipboardFallback => ("clipboard_fallback", CLIPBOARD_FALLBACK_WAV),
        FeedbackEvent::SubmitFailed => ("submit_failed", SUBMIT_FAILED_WAV),
        FeedbackEvent::Truncated => ("truncated", TRUNCATED_WAV),
        FeedbackEvent::Cancelled => ("cancelled", CANCELLED_WAV),
        FeedbackEvent::Error => ("error", ERROR_WAV),
        FeedbackEvent::ConfigError => ("config_error", CONFIG_ERROR_WAV),
    }
}

fn decode_wav(wav: &[u8]) -> Result<(Vec<f32>, u32), SoundError> {
    let mut reader = hound::WavReader::new(Cursor::new(wav)).map_err(|error| {
        SoundError::Playback(format!("embedded feedback sound is not valid WAV: {error}"))
    })?;
    let spec = reader.spec();
    if spec.channels == 0 {
        return Err(SoundError::Playback(
            "embedded feedback sound has no channels".into(),
        ));
    }
    if spec.bits_per_sample == 0 || spec.bits_per_sample > 32 {
        return Err(SoundError::Playback(format!(
            "unsupported embedded feedback sample depth: {} bits",
            spec.bits_per_sample
        )));
    }
    let read_error = |error: hound::Error| {
        SoundError::Playback(format!("could not read embedded sound: {error}"))
    };
    let interleaved: Vec<f32> = match spec.sample_format {
        hound::SampleFormat::Float => reader
            .samples::<f32>()
            .collect::<Result<Vec<_>, _>>()
            .map_err(read_error)?,
        hound::SampleFormat::Int => {
            let scale = 1.0 / (1u64 << (spec.bits_per_sample - 1)) as f32;
            reader
                .samples::<i32>()
                .map(|sample| sample.map(|value| value as f32 * scale))
                .collect::<Result<Vec<_>, _>>()
                .map_err(read_error)?
        }
    };
    let channels = spec.channels as usize;
    let samples: Vec<f32> = if channels == 1 {
        interleaved
    } else {
        interleaved
            .chunks(channels)
            .map(|frame| frame.iter().sum::<f32>() / channels as f32)
            .collect()
    };
    if samples.is_empty() {
        return Err(SoundError::Playback(
            "embedded feedback sound has no samples".into(),
        ));
    }
    Ok((samples, spec.sample_rate))
}

/// Resolves `audio.feedback_device` to a real output device. `"default"`
/// follows the system default; any other value is matched by cpal's stable
/// device id first, then by the device's display name, so a saved name keeps
/// working when the id is unavailable.
fn find_output_device(host: &cpal::Host, wanted: &str) -> Result<cpal::Device, SoundError> {
    let missing = || SoundError::NoOutputDevice {
        requested: wanted.to_string(),
    };
    if wanted == "default" {
        return host.default_output_device().ok_or_else(missing);
    }
    let by_id = wanted
        .parse::<cpal::DeviceId>()
        .ok()
        .and_then(|id| host.device_by_id(&id));
    by_id
        .or_else(|| {
            host.output_devices().ok()?.find(|device| {
                device
                    .description()
                    .is_ok_and(|description| description.name() == wanted)
                    || device.id().is_ok_and(|id| id.to_string() == wanted)
            })
        })
        .filter(|device| device.supports_output())
        .ok_or_else(missing)
}

fn play_on_device(
    device: &cpal::Device,
    samples: &[f32],
    sample_rate: u32,
    gain: f32,
) -> Result<(), SoundError> {
    let supported = device.default_output_config().map_err(|error| {
        SoundError::Playback(format!("could not query the output configuration: {error}"))
    })?;
    let sample_format = supported.sample_format();
    let config = supported.config();
    let samples =
        crate::audio::resample_mono(samples, sample_rate, config.sample_rate).map_err(|error| {
            SoundError::Playback(format!("could not resample the feedback sound: {error}"))
        })?;
    let samples = Arc::new(samples);
    let duration = Duration::from_secs_f64(samples.len() as f64 / config.sample_rate as f64);
    let (finished, finished_rx) = mpsc::channel();

    let stream = match sample_format {
        cpal::SampleFormat::F32 => {
            build_output_stream::<f32>(device, &config, samples, gain, finished)
        }
        cpal::SampleFormat::I16 => {
            build_output_stream::<i16>(device, &config, samples, gain, finished)
        }
        cpal::SampleFormat::U16 => {
            build_output_stream::<u16>(device, &config, samples, gain, finished)
        }
        cpal::SampleFormat::I8 => {
            build_output_stream::<i8>(device, &config, samples, gain, finished)
        }
        cpal::SampleFormat::U8 => {
            build_output_stream::<u8>(device, &config, samples, gain, finished)
        }
        cpal::SampleFormat::I24 => {
            build_output_stream::<I24>(device, &config, samples, gain, finished)
        }
        cpal::SampleFormat::U24 => {
            build_output_stream::<U24>(device, &config, samples, gain, finished)
        }
        cpal::SampleFormat::I32 => {
            build_output_stream::<i32>(device, &config, samples, gain, finished)
        }
        cpal::SampleFormat::U32 => {
            build_output_stream::<u32>(device, &config, samples, gain, finished)
        }
        cpal::SampleFormat::I64 => {
            build_output_stream::<i64>(device, &config, samples, gain, finished)
        }
        cpal::SampleFormat::U64 => {
            build_output_stream::<u64>(device, &config, samples, gain, finished)
        }
        cpal::SampleFormat::F64 => {
            build_output_stream::<f64>(device, &config, samples, gain, finished)
        }
        other => {
            return Err(SoundError::Playback(format!(
                "unsupported output sample format: {other}"
            )))
        }
    }
    .map_err(|error| SoundError::Playback(format!("could not open an output stream: {error}")))?;

    stream.play().map_err(|error| {
        SoundError::Playback(format!("could not start feedback playback: {error}"))
    })?;

    // Wait for the callback to render the last frame rather than sleeping a
    // fixed length: a resample or a slow device start would otherwise cut the
    // sound or hold the thread longer than needed.
    let outcome = finished_rx.recv_timeout(duration + PLAYBACK_TIMEOUT_GRACE);
    std::thread::sleep(PLAYBACK_DRAIN);
    let _ = stream.pause();
    // cpal's ALSA backend can panic inside `Stream::drop` when the worker
    // already died from a driver quirk, the same hazard `audio.rs` guards.
    let _ = std::panic::catch_unwind(std::panic::AssertUnwindSafe(move || drop(stream)));

    match outcome {
        Ok(Ok(())) => Ok(()),
        Ok(Err(error)) => Err(SoundError::Playback(error)),
        Err(mpsc::RecvTimeoutError::Timeout) => Err(SoundError::Playback(format!(
            "the output stream did not finish within {} ms",
            (duration + PLAYBACK_TIMEOUT_GRACE).as_millis()
        ))),
        Err(mpsc::RecvTimeoutError::Disconnected) => Err(SoundError::Playback(
            "the output stream stopped before the sound finished".to_string(),
        )),
    }
}

fn build_output_stream<T>(
    device: &cpal::Device,
    config: &cpal::StreamConfig,
    samples: Arc<Vec<f32>>,
    gain: f32,
    finished: mpsc::Sender<Result<(), String>>,
) -> Result<cpal::Stream, cpal::Error>
where
    T: SizedSample + FromSample<f32>,
{
    let channels = config.channels as usize;
    let end = samples.len();
    let mut position = 0usize;
    let mut finished_data = Some(finished.clone());
    device.build_output_stream(
        *config,
        move |output: &mut [T], _| {
            for frame in output.chunks_mut(channels) {
                let sample = if position < end {
                    (samples[position] * gain).clamp(-1.0, 1.0)
                } else {
                    0.0
                };
                position += 1;
                let value = T::from_sample(sample);
                for channel in frame.iter_mut() {
                    *channel = value;
                }
            }
            if position >= end {
                if let Some(sender) = finished_data.take() {
                    let _ = sender.send(Ok(()));
                }
            }
        },
        move |error| {
            let _ = finished.send(Err(error.to_string()));
        },
        None,
    )
}

fn event_style(event: FeedbackEvent) -> (&'static str, &'static str, &'static str) {
    match event {
        FeedbackEvent::Recording => ("audio-input-microphone-symbolic", "Listening", "normal"),
        FeedbackEvent::Processing => ("view-refresh-symbolic", "Transcribing", "low"),
        FeedbackEvent::Success => ("emblem-ok-symbolic", "Ready", "low"),
        FeedbackEvent::NoSpeech => (
            "audio-input-microphone-muted-symbolic",
            "No speech detected",
            "normal",
        ),
        FeedbackEvent::Copied => ("edit-paste-symbolic", "Copied to clipboard", "low"),
        FeedbackEvent::ClipboardFallback => (
            "dialog-warning-symbolic",
            "Typing failed, copied to clipboard",
            "normal",
        ),
        FeedbackEvent::SubmitFailed => (
            "dialog-warning-symbolic",
            "Text entered, but Enter failed",
            "normal",
        ),
        FeedbackEvent::Truncated => ("edit-cut-symbolic", "Transcript cut short", "normal"),
        FeedbackEvent::Cancelled => ("process-stop-symbolic", "Cancelled", "low"),
        FeedbackEvent::Error => ("dialog-error-symbolic", "Operation failed", "critical"),
        FeedbackEvent::ConfigError => (
            "dialog-warning-symbolic",
            "Config edit rejected - run tonguetyped status",
            "normal",
        ),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const EVENTS: [FeedbackEvent; 11] = [
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
        FeedbackEvent::ConfigError,
    ];

    #[test]
    fn every_state_has_distinct_restrained_copy() {
        let styles: Vec<_> = EVENTS.into_iter().map(event_style).collect();

        for (index, style) in styles.iter().enumerate() {
            assert!(
                styles[..index].iter().all(|other| other.1 != style.1),
                "duplicate message: {}",
                style.1
            );
        }
        assert_eq!(event_style(FeedbackEvent::Error).1, "Operation failed");
        assert_eq!(event_style(FeedbackEvent::Error).2, "critical");

        let config_error = event_style(FeedbackEvent::ConfigError).1;
        assert_ne!(config_error, "Operation failed");
        assert!(config_error.contains("tonguetyped status"));
    }

    #[test]
    fn every_event_selects_its_own_embedded_wav() {
        let mut names = std::collections::HashSet::new();
        let sounds: Vec<_> = EVENTS
            .into_iter()
            .map(|event| {
                let (name, wav) = sound_for(event);
                assert!(names.insert(name), "duplicate sound asset: {name}");
                assert!(!wav.is_empty(), "{name} is empty");
                (name, wav)
            })
            .collect();

        for (index, (name, wav)) in sounds.iter().enumerate() {
            assert!(
                sounds[..index]
                    .iter()
                    .all(|(_, other)| other.as_ptr() != wav.as_ptr()),
                "{name} reuses another event's asset"
            );
        }
    }

    #[test]
    fn every_embedded_asset_parses_as_a_playable_wav() {
        for event in EVENTS {
            let (name, wav) = sound_for(event);
            assert!(!wav.is_empty(), "{name} is empty");

            let reader = hound::WavReader::new(Cursor::new(wav))
                .unwrap_or_else(|error| panic!("{name} is not valid WAV: {error}"));
            let spec = reader.spec();
            assert_eq!(spec.channels, 1, "{name} is not mono");
            assert!(spec.sample_rate >= 8_000, "{name} sample rate");
            assert!(reader.duration() > 0, "{name} has no frames");

            let (samples, sample_rate) =
                decode_wav(wav).unwrap_or_else(|error| panic!("{name}: {error}"));
            assert!(!samples.is_empty(), "{name} decodes to nothing");
            assert!(
                samples
                    .iter()
                    .all(|sample| sample.is_finite() && sample.abs() <= 1.0),
                "{name} leaves the -1..1 range"
            );
            let duration = samples.len() as f64 / sample_rate as f64;
            assert!(
                (0.03..0.5).contains(&duration),
                "{name} is not a short, restrained sound: {duration}s"
            );
        }
    }

    #[test]
    fn feedback_volume_maps_to_linear_gain() {
        assert_eq!(volume_gain(0.0), 0.0);
        assert_eq!(volume_gain(0.5), 0.5);
        assert_eq!(volume_gain(1.0), 1.0);
        assert_eq!(volume_gain(-1.0), 0.0, "below the config range clamps");
        assert_eq!(volume_gain(2.0), 1.0, "above the config range clamps");
        assert_eq!(volume_gain(f64::NAN), 0.0, "a non-finite volume is silent");

        // 0.7 is -3.1 dB, exactly the attenuation the old external player
        // produced, so the 0..1 config scale sounds the same as before.
        let decibels = 20.0 * 0.7f64.log10();
        assert!((decibels + 3.1).abs() < 0.05);
        let from_decibels = 10f64.powf(decibels / 20.0);
        assert!((volume_gain(0.7) as f64 - from_decibels).abs() < 1e-6);
    }

    #[test]
    fn missing_output_device_is_reported_separately() {
        #[cfg(target_os = "linux")]
        let _alsa_errors = alsa::Output::local_error_handler().ok();
        let host = cpal::default_host();
        let wanted = "tonguetyped-test-missing-output-device";
        let error = find_output_device(&host, wanted).expect_err("no device can carry this name");
        assert!(matches!(error, SoundError::NoOutputDevice { .. }));
        assert_eq!(
            error.to_string(),
            format!("no audio output device '{wanted}'")
        );

        let default = SoundError::NoOutputDevice {
            requested: "default".into(),
        };
        assert_eq!(default.to_string(), "no audio output device");
    }

    #[test]
    fn playback_error_keeps_the_underlying_text() {
        let error = SoundError::Playback("device busy".into());
        assert_eq!(error.to_string(), "audio playback failed: device busy");
    }
}
