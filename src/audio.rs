use anyhow::Context;
use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};
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

pub struct AudioRecorder {
    stream: Option<cpal::Stream>,
    buffer: Arc<Mutex<Vec<f32>>>,
}

impl AudioRecorder {
    pub fn new(
        device_name: &str,
        target_rate: u32,
        _level_callback: Option<LevelCallback>,
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

        let mut supported_config = device
            .supported_input_configs()
            .context("failed to query supported configs")?;
        let config_range = supported_config
            .next()
            .context("no supported config")?;

        let channels = config_range.channels();
        let source_rate = config_range.max_sample_rate();

        let config = cpal::StreamConfig {
            channels,
            sample_rate: source_rate,
            buffer_size: cpal::BufferSize::Default,
        };

        let buffer: Arc<Mutex<Vec<f32>>> = Arc::new(Mutex::new(Vec::new()));
        let buf_clone = buffer.clone();

        let resampler = if source_rate.0 != target_rate {
            let rs = FftFixedIn::<f32>::new(
                source_rate.0 as usize,
                target_rate as usize,
                1024,
                1,
                1,
            )?;
            Some(Arc::new(Mutex::new(rs)))
        } else {
            None
        };

        let err_fn = move |err| {
            tracing::error!("audio stream error: {}", err);
        };

        let stream = device.build_input_stream(
            &config,
            move |data: &[f32], _: &cpal::InputCallbackInfo| {
                if let Some(ref rs) = resampler {
                    let mut rs_lock = rs.lock().unwrap();
                    let input: Vec<Vec<f32>> = vec![data.to_vec()];
                    if let Ok(output) = rs_lock.process(&input, None) {
                        let mut buf = buf_clone.lock().unwrap();
                        buf.extend_from_slice(&output[0]);
                    }
                } else {
                    let mut buf = buf_clone.lock().unwrap();
                    buf.extend_from_slice(data);
                }
            },
            err_fn,
            None,
        )?;

        Ok(AudioRecorder {
            stream: Some(stream),
            buffer,
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

    pub fn take_buffer(&self) -> Vec<f32> {
        let mut buf = self.buffer.lock().unwrap();
        std::mem::take(&mut *buf)
    }
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
        let samples: Vec<f32> = (0..100)
            .map(|i| (i as f32 / 100.0) * 2.0 - 1.0)
            .collect();
        let rms = compute_rms(&samples);
        assert!(rms > 0.0);
        assert!(rms <= 1.0);
    }
}