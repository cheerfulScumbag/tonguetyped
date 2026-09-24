use std::collections::VecDeque;

pub const PREFILL_SAMPLES: usize = 7200;
pub const HANGOVER_MILLISECONDS: usize = 1650;
// vad-rs classifies probabilities below 0.35 as silence.
const SILENCE_PROBABILITY_THRESHOLD: f32 = 0.35;

pub struct VadDetector {
    inner: vad_rs::Vad,
    gate: SpeechGate,
}

struct SpeechGate {
    prefill_buffer: VecDeque<f32>,
    hangover_counter: usize,
    speech_detected: bool,
    accepted: Vec<f32>,
    hangover_samples: usize,
}

impl VadDetector {
    pub fn new(model_path: &str, sample_rate: usize) -> anyhow::Result<Self> {
        let inner =
            vad_rs::Vad::new(model_path, sample_rate).map_err(|e| anyhow::anyhow!("{}", e))?;
        Ok(VadDetector {
            inner,
            gate: SpeechGate::new(sample_rate),
        })
    }

    pub fn reset(&mut self) {
        self.inner.reset();
        self.gate.reset();
    }

    pub fn process_window(
        &mut self,
        samples: &[f32],
        valid_samples: usize,
    ) -> anyhow::Result<Option<Vec<f32>>> {
        let result = self
            .inner
            .compute(samples)
            .map_err(|e| anyhow::anyhow!("{}", e))?;
        Ok(self
            .gate
            .process_window(samples, valid_samples, result.prob))
    }

    pub fn finish(&mut self) -> Option<Vec<f32>> {
        self.gate.finish()
    }
}

impl SpeechGate {
    fn new(sample_rate: usize) -> Self {
        Self {
            prefill_buffer: VecDeque::with_capacity(PREFILL_SAMPLES),
            hangover_counter: 0,
            speech_detected: false,
            accepted: Vec::new(),
            hangover_samples: hangover_samples(sample_rate),
        }
    }

    fn reset(&mut self) {
        self.prefill_buffer.clear();
        self.hangover_counter = 0;
        self.speech_detected = false;
        self.accepted.clear();
    }

    fn process_window(
        &mut self,
        samples: &[f32],
        valid_samples: usize,
        probability: f32,
    ) -> Option<Vec<f32>> {
        let is_speech = probability >= SILENCE_PROBABILITY_THRESHOLD;

        if !self.speech_detected {
            for &sample in &samples[..valid_samples] {
                self.prefill_buffer.push_back(sample);
                if self.prefill_buffer.len() > PREFILL_SAMPLES {
                    self.prefill_buffer.pop_front();
                }
            }
            if is_speech {
                self.speech_detected = true;
                self.accepted.extend(self.prefill_buffer.drain(..));
                self.hangover_counter = self.hangover_samples;
            }
        } else if is_speech {
            self.accepted.extend_from_slice(&samples[..valid_samples]);
            self.hangover_counter = self.hangover_samples;
        } else {
            let retained = retain_hangover_samples(&mut self.hangover_counter, valid_samples);
            self.accepted.extend_from_slice(&samples[..retained]);
        }

        if self.speech_detected && self.hangover_counter == 0 {
            self.speech_detected = false;
            return Some(std::mem::take(&mut self.accepted));
        }

        None
    }

    fn finish(&mut self) -> Option<Vec<f32>> {
        self.speech_detected = false;
        self.hangover_counter = 0;
        self.prefill_buffer.clear();
        if self.accepted.is_empty() {
            None
        } else {
            Some(std::mem::take(&mut self.accepted))
        }
    }
}

fn hangover_samples(sample_rate: usize) -> usize {
    sample_rate * HANGOVER_MILLISECONDS / 1000
}

fn retain_hangover_samples(remaining: &mut usize, available: usize) -> usize {
    let retained = (*remaining).min(available);
    *remaining -= retained;
    retained
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_vad_creation_fails_without_model() {
        let vad = VadDetector::new("nonexistent_model.onnx", 16000);
        assert!(vad.is_err());
    }

    #[test]
    fn hangover_duration_is_converted_to_samples() {
        assert_eq!(hangover_samples(16_000), 26_400);
    }

    #[test]
    fn final_hangover_window_is_partially_retained() {
        let mut remaining = hangover_samples(16_000);
        let mut retained = 0;
        while remaining > 0 {
            retained += retain_hangover_samples(&mut remaining, 512);
        }
        assert_eq!(retained, 26_400);
    }

    #[test]
    fn short_low_confidence_speech_is_retained_from_eight_second_recording() {
        let mut gate = SpeechGate::new(16_000);
        let mut retained = Vec::new();
        let speech_window = 125;
        let input_samples = 250 * 512;

        for window in 0..250 {
            let samples = if window == speech_window {
                [0.25; 512]
            } else {
                [0.0; 512]
            };
            let probability = if window == speech_window { 0.4 } else { 0.0 };
            if let Some(segment) = gate.process_window(&samples, samples.len(), probability) {
                retained.extend(segment);
            }
        }
        if let Some(segment) = gate.finish() {
            retained.extend(segment);
        }

        assert_eq!(retained.len(), PREFILL_SAMPLES + 26_400);
        assert!(retained.len() < input_samples);
        assert!(retained.contains(&0.25));
    }

    #[test]
    fn eight_seconds_of_silence_are_rejected() {
        let mut gate = SpeechGate::new(16_000);

        for _ in 0..250 {
            assert_eq!(gate.process_window(&[0.0; 512], 512, 0.0), None);
        }

        assert_eq!(gate.finish(), None);
    }
}
