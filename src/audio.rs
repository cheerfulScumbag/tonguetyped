use anyhow::Context;
use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};
use cpal::{FromSample, Sample, SizedSample};
use rubato::{FftFixedIn, Resampler};
use std::sync::{Arc, Mutex};

pub struct AudioDevice {
    pub name: String,
    pub is_default: bool,
    pub channels: u16,
    pub sample_rate: u32,
}

pub fn list_devices() -> anyhow::Result<Vec<AudioDevice>> {
    let host = cpal::default_host();
    let default_device = host.default_input_device();
    let default_name = default_device.as_ref().and_then(|d| d.name().ok());

    let mut devices = Vec::new();
    if let Ok(device_list) = host.input_devices() {
        for device in device_list {
            let name = device.name().unwrap_or_else(|_| "unknown".to_string());
            let is_default = Some(&name) == default_name.as_ref();
            let config = device.default_input_config().ok();
            devices.push(AudioDevice {
                name,
                is_default,
                channels: config.as_ref().map(|c| c.channels()).unwrap_or(0),
                sample_rate: config.as_ref().map(|c| c.sample_rate().0).unwrap_or(0),
            });
        }
    }
    Ok(devices)
}

pub fn check_audio_available() -> bool {
    let host = cpal::default_host();
    host.default_input_device().is_some()
}

pub type LevelCallback = Arc<dyn Fn(f32) + Send + Sync>;
pub type ErrorCallback = Arc<dyn Fn(String) + Send + Sync>;

pub struct AudioRecorder {
    stream: Option<cpal::Stream>,
    buffer: Arc<Mutex<Vec<f32>>>,
    pending: Arc<Mutex<Vec<f32>>>,
    resampler: Option<Arc<Mutex<FftFixedIn<f32>>>>,
    source_frames: Arc<Mutex<usize>>,
    source_rate: u32,
    target_rate: u32,
}

impl AudioRecorder {
    pub fn new(
        device_name: &str,
        target_rate: u32,
        level_callback: Option<LevelCallback>,
        error_callback: Option<ErrorCallback>,
    ) -> anyhow::Result<Self> {
        let host = cpal::default_host();

        let device = if device_name == "default" || device_name.is_empty() {
            host.default_input_device()
                .context("no default input device")?
        } else {
            let mut found = None;
            if let Ok(devices) = host.input_devices() {
                for d in devices {
                    if d.name().map(|n| n == device_name).unwrap_or(false) {
                        found = Some(d);
                        break;
                    }
                }
            }
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
                let format_rank = match config.sample_format() {
                    cpal::SampleFormat::F32 => 2,
                    cpal::SampleFormat::I16 | cpal::SampleFormat::U16 => 1,
                    _ => 0,
                };
                let min = config.min_sample_rate().0;
                let max = config.max_sample_rate().0;
                let chosen = target_rate.clamp(min, max);
                let distance = chosen.abs_diff(target_rate);
                (std::cmp::Reverse(distance), chosen, format_rank)
            })
            .context("no supported input sample format")?;

        let channels = config_range.channels();
        let source_rate = cpal::SampleRate(target_rate.clamp(
            config_range.min_sample_rate().0,
            config_range.max_sample_rate().0,
        ));
        let sample_format = config_range.sample_format();

        let config = cpal::StreamConfig {
            channels,
            sample_rate: source_rate,
            buffer_size: cpal::BufferSize::Default,
        };

        let buffer: Arc<Mutex<Vec<f32>>> = Arc::new(Mutex::new(Vec::new()));
        let resampler = if source_rate.0 != target_rate {
            let rs =
                FftFixedIn::<f32>::new(source_rate.0 as usize, target_rate as usize, 1024, 1, 1)?;
            Some(Arc::new(Mutex::new(rs)))
        } else {
            None
        };
        let pending = Arc::new(Mutex::new(Vec::new()));
        let source_frames = Arc::new(Mutex::new(0));
        let capture = CaptureState {
            channels: channels as usize,
            buffer: buffer.clone(),
            pending: pending.clone(),
            resampler: resampler.clone(),
            source_frames: source_frames.clone(),
            level_callback,
        };
        let stream = match sample_format {
            cpal::SampleFormat::F32 => {
                build_input_stream::<f32>(&device, &config, capture, error_callback)?
            }
            cpal::SampleFormat::I16 => {
                build_input_stream::<i16>(&device, &config, capture, error_callback)?
            }
            cpal::SampleFormat::U16 => {
                build_input_stream::<u16>(&device, &config, capture, error_callback)?
            }
            format => anyhow::bail!("unsupported input sample format: {format}"),
        };

        Ok(AudioRecorder {
            stream: Some(stream),
            buffer,
            pending,
            resampler,
            source_frames,
            source_rate: source_rate.0,
            target_rate,
        })
    }

    pub fn start(&mut self) -> anyhow::Result<()> {
        if let Some(ref stream) = self.stream {
            stream.play()?;
        }
        Ok(())
    }

    pub fn stop(&mut self) {
        if let Some(ref stream) = self.stream {
            let _ = stream.pause();
        }
    }

    pub fn buffer_len(&self) -> usize {
        self.buffer.lock().unwrap().len()
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

struct CaptureState {
    channels: usize,
    buffer: Arc<Mutex<Vec<f32>>>,
    pending: Arc<Mutex<Vec<f32>>>,
    resampler: Option<Arc<Mutex<FftFixedIn<f32>>>>,
    source_frames: Arc<Mutex<usize>>,
    level_callback: Option<LevelCallback>,
}

fn build_input_stream<T>(
    device: &cpal::Device,
    config: &cpal::StreamConfig,
    capture: CaptureState,
    error_callback: Option<ErrorCallback>,
) -> Result<cpal::Stream, cpal::BuildStreamError>
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
            tracing::error!("audio stream error: {}", error);
            if let Some(callback) = &error_callback {
                callback(error.to_string());
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
                    Ok(output) => self.buffer.lock().unwrap().extend_from_slice(&output[0]),
                    Err(error) => {
                        tracing::error!("audio resampling failed: {}", error);
                        break;
                    }
                }
            }
        } else {
            self.buffer.lock().unwrap().extend_from_slice(&mono);
        }
    }
}

fn downmix_to_mono(samples: &[f32], channels: usize) -> Vec<f32> {
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
}
