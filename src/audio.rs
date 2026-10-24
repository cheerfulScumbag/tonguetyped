use anyhow::Context;
use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};
use cpal::{FromSample, Sample, SizedSample};
use rubato::{FftFixedIn, Resampler};
use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

/// How long the microphone preview waits after the highlighted device
/// changes before opening the new stream: rapid navigation is not meaningful
/// audible feedback, only the settled choice is, and tearing streams down and
/// recreating them on every keystroke produced real device-open churn.
pub const MIC_PREVIEW_SETTLE_TIME: Duration = Duration::from_millis(250);

pub struct AudioDevice {
    pub id: String,
    pub name: String,
    pub is_default: bool,
    pub channels: u16,
    pub sample_rate: u32,
}

pub fn list_devices() -> anyhow::Result<Vec<AudioDevice>> {
    #[cfg(target_os = "linux")]
    let _alsa_errors = alsa::Output::local_error_handler().ok();
    let host = cpal::default_host();
    let default_device = host.default_input_device();

    let mut devices = Vec::new();
    if let Ok(device_list) = host.input_devices() {
        for device in device_list {
            let name = device
                .description()
                .map(|description| description.name().to_string())
                .unwrap_or_else(|_| "unknown".to_string());
            let id = device
                .id()
                .map(|id| id.to_string())
                .unwrap_or_else(|_| name.clone());
            let is_default = default_device.as_ref() == Some(&device);
            let config = device.default_input_config().ok();
            devices.push(AudioDevice {
                id,
                name,
                is_default,
                channels: config.as_ref().map(|c| c.channels()).unwrap_or(0),
                sample_rate: config.as_ref().map(|c| c.sample_rate()).unwrap_or(0),
            });
        }
    }
    Ok(devices)
}

pub type LevelCallback = Arc<dyn Fn(f32) + Send + Sync>;
pub type ErrorCallback = Arc<dyn Fn(String) + Send + Sync>;
/// Receives every resampled 16 kHz mono block as it is captured, in capture
/// order. The live-text path installs one to feed a streaming model's
/// [`transcribe_streaming`](crate::inference::InferenceEngine::transcribe_streaming)
/// while the user speaks; the level/error callbacks above stay untouched, and
/// the recorder still buffers the whole capture exactly as before.
pub type FrameCallback = Arc<dyn Fn(&[f32]) + Send + Sync>;

pub struct AudioRecorder {
    stream: Option<cpal::Stream>,
    buffer: Arc<Mutex<Vec<f32>>>,
    pending: Arc<Mutex<Vec<f32>>>,
    resampler: Option<Arc<Mutex<FftFixedIn<f32>>>>,
    source_frames: Arc<Mutex<usize>>,
    source_rate: u32,
    target_rate: u32,
    /// Set once, before `start`, by [`AudioRecorder::set_frame_callback`]. A
    /// `OnceLock` (not a `Mutex`) keeps the per-block capture path lock-free
    /// while still letting the callback be installed after `new` built the
    /// stream, so the common recorder construction sites need no new argument.
    frame_callback: Arc<std::sync::OnceLock<FrameCallback>>,
}

impl AudioRecorder {
    pub fn new(
        device_name: &str,
        target_rate: u32,
        level_callback: Option<LevelCallback>,
        error_callback: Option<ErrorCallback>,
    ) -> anyhow::Result<Self> {
        #[cfg(target_os = "linux")]
        let _alsa_errors = alsa::Output::local_error_handler().ok();
        let host = cpal::default_host();

        let device = if device_name == "default" {
            host.default_input_device()
                .context("no default input device")?
        } else {
            let by_id = device_name
                .parse::<cpal::DeviceId>()
                .ok()
                .and_then(|id| host.device_by_id(&id));
            let found = by_id.or_else(|| {
                host.input_devices().ok()?.find(|device| {
                    device
                        .description()
                        .is_ok_and(|description| description.name() == device_name)
                })
            });
            found.context(format!("input device '{}' not found", device_name))?
        };

        let supported_config = device
            .supported_input_configs()
            .context("failed to query supported configs")?;
        let config_range = supported_config
            .filter(|config| {
                matches!(
                    config.sample_format(),
                    cpal::SampleFormat::F32 | cpal::SampleFormat::I16 | cpal::SampleFormat::U16
                )
            })
            .max_by_key(|config| {
                input_config_rank(
                    config.sample_format(),
                    config.min_sample_rate(),
                    config.max_sample_rate(),
                    target_rate,
                )
            })
            .context("no supported input sample format")?;

        let channels = config_range.channels();
        let source_rate = target_rate.clamp(
            config_range.min_sample_rate(),
            config_range.max_sample_rate(),
        );
        let sample_format = config_range.sample_format();

        let config = cpal::StreamConfig {
            channels,
            sample_rate: source_rate,
            buffer_size: cpal::BufferSize::Default,
        };

        let buffer: Arc<Mutex<Vec<f32>>> = Arc::new(Mutex::new(Vec::new()));
        let resampler = if source_rate != target_rate {
            let rs =
                FftFixedIn::<f32>::new(source_rate as usize, target_rate as usize, 1024, 1, 1)?;
            Some(Arc::new(Mutex::new(rs)))
        } else {
            None
        };
        let pending = Arc::new(Mutex::new(Vec::new()));
        let source_frames = Arc::new(Mutex::new(0));
        let frame_callback = Arc::new(std::sync::OnceLock::new());
        let capture = CaptureState {
            channels: channels as usize,
            buffer: buffer.clone(),
            pending: pending.clone(),
            resampler: resampler.clone(),
            source_frames: source_frames.clone(),
            level_callback,
            frame_callback: frame_callback.clone(),
        };
        let stream = match sample_format {
            cpal::SampleFormat::F32 => {
                build_input_stream::<f32>(&device, config, capture, error_callback)?
            }
            cpal::SampleFormat::I16 => {
                build_input_stream::<i16>(&device, config, capture, error_callback)?
            }
            cpal::SampleFormat::U16 => {
                build_input_stream::<u16>(&device, config, capture, error_callback)?
            }
            format => anyhow::bail!("unsupported input sample format: {format}"),
        };

        Ok(AudioRecorder {
            stream: Some(stream),
            buffer,
            pending,
            resampler,
            source_frames,
            source_rate,
            target_rate,
            frame_callback,
        })
    }

    /// Install the callback every resampled 16 kHz mono block is delivered to.
    /// Called before [`start`](Self::start) by the live-text path; the first
    /// call wins, so a recorder is never re-pointed mid-capture.
    pub fn set_frame_callback(&self, callback: FrameCallback) {
        let _ = self.frame_callback.set(callback);
    }

    pub fn start(&mut self) -> anyhow::Result<()> {
        if let Some(ref stream) = self.stream {
            stream.play()?;
        }
        Ok(())
    }

    pub fn stop(&mut self) {
        if let Some(stream) = self.stream.take() {
            let _ = stream.pause();
            // cpal's ALSA backend can panic inside `Stream::drop` itself (not just
            // its background worker thread) when the worker already died from a
            // driver quirk on a given device - e.g. a bad `get_htstamp` reading on
            // some surround-channel ALSA aliases. Catch that here so one
            // misbehaving device can't take down the whole process.
            let _ = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| drop(stream)));
        }
    }

    pub fn take_buffer(&mut self) -> anyhow::Result<Vec<f32>> {
        if let Some(resampler) = &self.resampler {
            let mut resampler = resampler.lock().unwrap();
            let mut pending = self.pending.lock().unwrap();
            let mut buffer = self.buffer.lock().unwrap();
            finish_resampling(
                &mut resampler,
                &mut pending,
                &mut buffer,
                *self.source_frames.lock().unwrap(),
                self.source_rate,
                self.target_rate,
            )?;
        }
        let mut buf = self.buffer.lock().unwrap();
        Ok(std::mem::take(&mut *buf))
    }
}

fn input_config_rank(
    sample_format: cpal::SampleFormat,
    min_rate: u32,
    max_rate: u32,
    target_rate: u32,
) -> (bool, std::cmp::Reverse<u32>, u8) {
    let chosen_rate = target_rate.clamp(min_rate, max_rate);
    let format_rank = match sample_format {
        cpal::SampleFormat::F32 => 2,
        cpal::SampleFormat::I16 | cpal::SampleFormat::U16 => 1,
        _ => 0,
    };
    (
        chosen_rate >= target_rate,
        std::cmp::Reverse(chosen_rate.abs_diff(target_rate)),
        format_rank,
    )
}

fn finish_resampling(
    resampler: &mut FftFixedIn<f32>,
    pending: &mut Vec<f32>,
    buffer: &mut Vec<f32>,
    source_frames: usize,
    source_rate: u32,
    target_rate: u32,
) -> anyhow::Result<()> {
    if !pending.is_empty() {
        let input = vec![std::mem::take(pending)];
        let output = resampler.process_partial(Some(&input), None)?;
        buffer.extend_from_slice(&output[0]);
    }
    let expected =
        (source_frames as u64 * target_rate as u64).div_ceil(source_rate as u64) as usize;
    let delay = resampler.output_delay();
    while buffer.len() < delay + expected {
        let output = resampler.process_partial::<Vec<f32>>(None, None)?;
        if output[0].is_empty() {
            break;
        }
        buffer.extend_from_slice(&output[0]);
    }
    buffer.drain(..delay.min(buffer.len()));
    buffer.truncate(expected);
    Ok(())
}

/// Resample a single-channel buffer from `source_rate` to `target_rate`,
/// finalizing the resampler's trailing delay so the result is exactly the
/// resampled length (rounded up), the same way the live capture path does on
/// stop. Used by the file-transcription path (`crate::transcribe`), which
/// reads a whole file at once instead of streaming it.
pub fn resample_mono(
    samples: &[f32],
    source_rate: u32,
    target_rate: u32,
) -> anyhow::Result<Vec<f32>> {
    if source_rate == target_rate || samples.is_empty() {
        return Ok(samples.to_vec());
    }
    let mut resampler =
        FftFixedIn::<f32>::new(source_rate as usize, target_rate as usize, 1024, 1, 1)?;
    let chunk = resampler.input_frames_next();
    let mut output = Vec::new();
    let mut index = 0;
    while index + chunk <= samples.len() {
        let input = vec![samples[index..index + chunk].to_vec()];
        let processed = resampler.process(&input, None)?;
        output.extend_from_slice(&processed[0]);
        index += chunk;
    }
    let mut pending = samples[index..].to_vec();
    finish_resampling(
        &mut resampler,
        &mut pending,
        &mut output,
        samples.len(),
        source_rate,
        target_rate,
    )?;
    Ok(output)
}

struct CaptureState {
    channels: usize,
    buffer: Arc<Mutex<Vec<f32>>>,
    pending: Arc<Mutex<Vec<f32>>>,
    resampler: Option<Arc<Mutex<FftFixedIn<f32>>>>,
    source_frames: Arc<Mutex<usize>>,
    level_callback: Option<LevelCallback>,
    frame_callback: Arc<std::sync::OnceLock<FrameCallback>>,
}

fn build_input_stream<T>(
    device: &cpal::Device,
    config: cpal::StreamConfig,
    capture: CaptureState,
    error_callback: Option<ErrorCallback>,
) -> Result<cpal::Stream, cpal::Error>
where
    T: SizedSample,
    f32: FromSample<T>,
{
    device.build_input_stream(
        config,
        move |data: &[T], _| {
            let converted: Vec<f32> = data.iter().copied().map(f32::from_sample).collect();
            capture.process(&converted);
        },
        move |error| {
            if let Some(callback) = &error_callback {
                callback(error.to_string());
            } else {
                tracing::error!("audio stream error: {}", error);
            }
        },
        None,
    )
}

impl CaptureState {
    fn process(&self, data: &[f32]) {
        let mono = downmix_to_mono(data, self.channels);
        *self.source_frames.lock().unwrap() += mono.len();
        if let Some(callback) = &self.level_callback {
            callback(compute_rms(&mono));
        }
        if let Some(resampler) = &self.resampler {
            let mut resampler = resampler.lock().unwrap();
            let mut pending = self.pending.lock().unwrap();
            pending.extend_from_slice(&mono);
            let input_frames = resampler.input_frames_next();
            while pending.len() >= input_frames {
                let input: Vec<Vec<f32>> = vec![pending.drain(..input_frames).collect()];
                match resampler.process(&input, None) {
                    Ok(output) => {
                        // Publish the resampled block to a live consumer (the
                        // streaming model) before buffering it, so partials and
                        // the final buffer see the exact same 16 kHz samples.
                        if let Some(callback) = self.frame_callback.get() {
                            callback(&output[0]);
                        }
                        self.buffer.lock().unwrap().extend_from_slice(&output[0]);
                    }
                    Err(error) => {
                        tracing::error!("audio resampling failed: {}", error);
                        break;
                    }
                }
            }
        } else {
            if let Some(callback) = self.frame_callback.get() {
                callback(&mono);
            }
            self.buffer.lock().unwrap().extend_from_slice(&mono);
        }
    }
}

pub(crate) fn downmix_to_mono(samples: &[f32], channels: usize) -> Vec<f32> {
    if channels <= 1 {
        return samples.to_vec();
    }
    samples
        .chunks_exact(channels)
        .map(|frame| frame.iter().sum::<f32>() / channels as f32)
        .collect()
}

impl Drop for AudioRecorder {
    fn drop(&mut self) {
        self.stop();
    }
}

fn compute_rms(samples: &[f32]) -> f32 {
    if samples.is_empty() {
        return 0.0;
    }
    let sum_sq: f32 = samples.iter().map(|s| s * s).sum();
    let mean_sq = sum_sq / samples.len() as f32;
    mean_sq.sqrt()
}

/// The newest microphone RMS level, published from the audio-capture thread by
/// the recording path. The overlay actor reads it to make the recording
/// animation react to the live voice. A process-global is enough here because
/// the recorder and the overlay actor always live in the same process (the
/// daemon); a plain relaxed atomic keeps the capture callback free of locks.
static LIVE_LEVEL: AtomicU32 = AtomicU32::new(0);

/// Publishes the newest microphone RMS level for the overlay's live
/// reactivity. Called once per captured audio block, so it must stay cheap.
pub fn publish_level(level: f32) {
    LIVE_LEVEL.store(level.to_bits(), Ordering::Relaxed);
}

/// The newest published microphone RMS level (`0.0` when nothing has published
/// yet). The overlay samples this on its repaint cadence rather than reacting
/// to every audio block individually.
pub fn latest_level() -> f32 {
    f32::from_bits(LIVE_LEVEL.load(Ordering::Relaxed))
}

/// Maps a raw RMS level to the 0..1 fraction an input-level gauge shows,
/// on a -60 dB..0 dB scale, for the setup console's microphone gauge.
pub fn level_to_ratio(level: f32) -> f64 {
    if level <= 0.0 {
        return 0.0;
    }
    let db = 20.0 * level.log10();
    ((db + 60.0) / 60.0).clamp(0.0, 1.0) as f64
}

/// Owns the live microphone-preview stream for a caller-owned device list,
/// for the setup console's Microphone row: the settle delay, async error
/// routing, and stream restarts.
///
/// The caller drives it like this: `defer_restart()` on every highlight
/// change, `poll()` once per frame, `sync()` with the currently highlighted
/// device when `poll()` reports the settle delay elapsed (or when entering
/// the screen), and `stop()` when leaving it.
#[derive(Default)]
pub struct MicMonitor {
    level: Arc<Mutex<f32>>,
    stream_error: Arc<Mutex<Option<String>>>,
    recorder: Option<AudioRecorder>,
    active_index: Option<usize>,
    restart_at: Option<Instant>,
    error: Option<String>,
}

impl MicMonitor {
    pub fn new() -> Self {
        Self::default()
    }

    /// Index of the device whose preview stream is currently bound, if any.
    pub fn active_index(&self) -> Option<usize> {
        self.active_index
    }

    /// Whether a deferred restart is waiting out the settle delay.
    pub fn restart_pending(&self) -> bool {
        self.restart_at.is_some()
    }

    pub fn level(&self) -> f32 {
        self.level.lock().map(|guard| *guard).unwrap_or(0.0)
    }

    pub fn error(&self) -> Option<&str> {
        self.error.as_deref()
    }

    /// Stops the preview and schedules a restart after the settle delay.
    pub fn defer_restart(&mut self) {
        self.recorder = None;
        self.active_index = None;
        self.error = None;
        if let Ok(mut guard) = self.stream_error.lock() {
            *guard = None;
        }
        if let Ok(mut level) = self.level.lock() {
            *level = 0.0;
        }
        self.restart_at = Some(Instant::now() + MIC_PREVIEW_SETTLE_TIME);
    }

    /// Starts (or keeps) the preview on `index`/`device_id`, cancelling any
    /// deferred restart. A device whose stream cannot be opened surfaces the
    /// failure through `error()` instead of failing the whole UI.
    pub fn sync(&mut self, index: usize, device_id: &str) {
        self.restart_at = None;
        if self.active_index == Some(index) {
            return;
        }
        self.recorder = None;
        self.active_index = None;
        self.error = None;
        if let Ok(mut level) = self.level.lock() {
            *level = 0.0;
        }
        if let Ok(mut guard) = self.stream_error.lock() {
            *guard = None;
        }
        let level = self.level.clone();
        let callback: LevelCallback = Arc::new(move |value| {
            if let Ok(mut guard) = level.lock() {
                *guard = value;
            }
        });
        let stream_error = self.stream_error.clone();
        let error_callback: ErrorCallback = Arc::new(move |message| {
            if let Ok(mut guard) = stream_error.lock() {
                *guard = Some(message);
            }
        });
        match AudioRecorder::new(device_id, 16_000, Some(callback), Some(error_callback)) {
            Ok(mut recorder) => match recorder.start() {
                Ok(()) => self.recorder = Some(recorder),
                Err(err) => self.error = Some(err.to_string()),
            },
            Err(err) => self.error = Some(err.to_string()),
        }
        self.active_index = Some(index);
    }

    /// Drops the preview stream entirely (leaving the screen or step).
    pub fn stop(&mut self) {
        self.restart_at = None;
        self.recorder = None;
        self.active_index = None;
        self.error = None;
        if let Ok(mut guard) = self.stream_error.lock() {
            *guard = None;
        }
        if let Ok(mut level) = self.level.lock() {
            *level = 0.0;
        }
    }

    /// Per-frame maintenance: routes a background stream error into
    /// `error()`, and reports whether a deferred restart's settle delay has
    /// elapsed. The monitor cannot know the caller's device list, so on
    /// `true` the caller calls `sync` with the currently highlighted device.
    pub fn poll(&mut self) -> bool {
        let stream_error = self
            .stream_error
            .lock()
            .ok()
            .and_then(|mut guard| guard.take());
        if let Some(message) = stream_error {
            self.error = Some(message);
            self.recorder = None;
        }
        self.restart_at
            .is_some_and(|deadline| Instant::now() >= deadline)
    }
}

#[cfg(test)]
impl MicMonitor {
    /// Pretends a preview stream is already open on `index`, so tests can
    /// exercise navigation/error routing without a real audio device.
    pub(crate) fn simulate_active_preview(&mut self, index: usize) {
        self.active_index = Some(index);
    }

    /// Simulates what `AudioRecorder`'s error callback does from its
    /// background audio thread when a device disconnects mid-stream.
    pub(crate) fn inject_stream_error(&mut self, message: &str) {
        *self.stream_error.lock().unwrap() = Some(message.to_string());
    }

    pub(crate) fn has_pending_stream_error(&self) -> bool {
        self.stream_error.lock().unwrap().is_some()
    }

    pub(crate) fn has_open_stream(&self) -> bool {
        self.recorder.is_some()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_rms_silence() {
        let samples = vec![0.0f32; 100];
        let rms = compute_rms(&samples);
        assert_eq!(rms, 0.0);
    }

    #[test]
    fn silence_maps_to_empty_gauge() {
        assert_eq!(level_to_ratio(0.0), 0.0);
    }

    #[test]
    fn full_scale_maps_to_full_gauge() {
        assert_eq!(level_to_ratio(1.0), 1.0);
    }

    #[test]
    fn quiet_signal_is_between_bounds() {
        let ratio = level_to_ratio(0.01);
        assert!(ratio > 0.0 && ratio < 1.0);
    }

    #[test]
    fn test_rms_signal() {
        let samples: Vec<f32> = (0..100).map(|i| (i as f32 / 100.0) * 2.0 - 1.0).collect();
        let rms = compute_rms(&samples);
        assert!(rms > 0.0);
        assert!(rms <= 1.0);
    }

    #[test]
    fn downmixes_interleaved_channels() {
        assert_eq!(downmix_to_mono(&[1.0, -1.0, 0.5, 0.5], 2), [0.0, 0.5]);
    }

    #[test]
    fn process_publishes_each_block_to_the_frame_callback() {
        // The live-text path installs a frame callback before `start`; every
        // captured block must reach it (and still be buffered) in capture order,
        // so the streaming model sees the same 16 kHz audio the batch path
        // would.
        let buffer = Arc::new(Mutex::new(Vec::new()));
        let frame_callback = Arc::new(std::sync::OnceLock::new());
        let captured: Arc<Mutex<Vec<Vec<f32>>>> = Arc::new(Mutex::new(Vec::new()));
        let sink = captured.clone();
        let callback: FrameCallback = Arc::new(move |samples: &[f32]| {
            sink.lock().unwrap().push(samples.to_vec());
        });
        frame_callback.set(callback).ok();
        let state = CaptureState {
            channels: 1,
            buffer: buffer.clone(),
            pending: Arc::new(Mutex::new(Vec::new())),
            resampler: None,
            source_frames: Arc::new(Mutex::new(0)),
            level_callback: None,
            frame_callback,
        };

        state.process(&[0.1, 0.2, 0.3]);
        state.process(&[0.4]);

        assert_eq!(
            *captured.lock().unwrap(),
            vec![vec![0.1, 0.2, 0.3], vec![0.4]]
        );
        assert_eq!(*buffer.lock().unwrap(), vec![0.1, 0.2, 0.3, 0.4]);
    }

    #[test]
    fn process_without_a_frame_callback_only_buffers() {
        let buffer = Arc::new(Mutex::new(Vec::new()));
        let state = CaptureState {
            channels: 1,
            buffer: buffer.clone(),
            pending: Arc::new(Mutex::new(Vec::new())),
            resampler: None,
            source_frames: Arc::new(Mutex::new(0)),
            level_callback: None,
            frame_callback: Arc::new(std::sync::OnceLock::new()),
        };
        state.process(&[0.5, -0.5]);
        assert_eq!(*buffer.lock().unwrap(), vec![0.5, -0.5]);
    }

    #[test]
    fn finalizes_short_resampled_recording() {
        let mut resampler = FftFixedIn::<f32>::new(48_000, 16_000, 1024, 1, 1).unwrap();
        let mut pending = vec![1.0; 480];
        let mut output = Vec::new();

        finish_resampling(
            &mut resampler,
            &mut pending,
            &mut output,
            480,
            48_000,
            16_000,
        )
        .unwrap();

        assert_eq!(output.len(), 160);
        assert!(output.iter().any(|sample| sample.abs() > f32::EPSILON));
    }

    #[test]
    fn prefers_oversampled_input_to_closer_undersampled_input() {
        let undersampled = input_config_rank(cpal::SampleFormat::F32, 8_000, 8_000, 16_000);
        let oversampled = input_config_rank(cpal::SampleFormat::I16, 48_000, 48_000, 16_000);

        assert!(oversampled > undersampled);
    }

    #[test]
    fn resamples_a_whole_buffer_to_the_target_rate() {
        let input: Vec<f32> = (0..4_800).map(|i| (i as f32 * 0.05).sin()).collect();
        let output = resample_mono(&input, 48_000, 16_000).unwrap();
        assert_eq!(output.len(), 1_600);
        assert!(output.iter().any(|sample| sample.abs() > f32::EPSILON));
    }

    #[test]
    fn a_matching_rate_is_returned_untouched() {
        let input = vec![0.25_f32; 320];
        assert_eq!(resample_mono(&input, 16_000, 16_000).unwrap(), input);
    }

    // cpal 0.15.3's ALSA backend can panic its background worker thread on some
    // ALSA capture aliases (e.g. surround channel layouts the hardware doesn't
    // really support for input); once that worker has died, `Stream::drop`'s own
    // self-pipe wakeup then panics too, on whichever thread drops the stream.
    // Skips if this machine doesn't expose the triggering device name.
    #[test]
    fn stop_does_not_propagate_worker_thread_panic() {
        let device_name = "surround40:CARD=Generic,DEV=0";
        let devices = list_devices().unwrap();
        if !devices.iter().any(|d| d.id == device_name) {
            return;
        }
        let mut recorder = match AudioRecorder::new(device_name, 16_000, None, None) {
            Ok(recorder) => recorder,
            Err(_) => return,
        };
        if recorder.start().is_err() {
            return;
        }
        std::thread::sleep(std::time::Duration::from_millis(500));
        recorder.stop();
    }
}
