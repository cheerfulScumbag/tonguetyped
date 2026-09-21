pub const PREFILL_SAMPLES: usize = 7200;
pub const HANGOVER_FRAMES: usize = 1650;

pub struct VadDetector {
    inner: vad_rs::Vad,
    prefill_buffer: VecDeque<f32>,
    hangover_counter: usize,
    speech_detected: bool,
    accepted: Vec<f32>,
}

impl VadDetector {
    pub fn new(model_path: &str, sample_rate: usize) -> anyhow::Result<Self> {
        let inner =
            vad_rs::Vad::new(model_path, sample_rate).map_err(|e| anyhow::anyhow!("{}", e))?;
        Ok(VadDetector {
            inner,
            prefill_buffer: VecDeque::with_capacity(PREFILL_SAMPLES),
            hangover_counter: 0,
            speech_detected: false,
            accepted: Vec::new(),
        })
    }

    pub fn reset(&mut self) {
        self.inner.reset();
        self.prefill_buffer.clear();
        self.hangover_counter = 0;
        self.speech_detected = false;
        self.accepted.clear();
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
        let is_speech = result.prob > 0.5;

        for &sample in &samples[..valid_samples] {
            if !self.speech_detected {
                self.prefill_buffer.push_back(sample);
                if self.prefill_buffer.len() > PREFILL_SAMPLES {
                    self.prefill_buffer.pop_front();
                }
            } else {
                self.accepted.push(sample);
            }
        }

        if is_speech && !self.speech_detected {
            self.speech_detected = true;
            self.accepted.extend(self.prefill_buffer.iter().copied());
            self.prefill_buffer.clear();
        }

        if is_speech {
            self.hangover_counter = HANGOVER_FRAMES;
        } else if self.speech_detected {
            if self.hangover_counter > 0 {
                self.hangover_counter = self.hangover_counter.saturating_sub(samples.len());
            }
        }

        if self.speech_detected && self.hangover_counter == 0 {
            self.speech_detected = false;
            return Ok(Some(std::mem::take(&mut self.accepted)));
        }

        Ok(None)
    }

    pub fn finish(&mut self) -> Option<Vec<f32>> {
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_vad_creation_fails_without_model() {
        let vad = VadDetector::new("nonexistent_model.onnx", 16000);
        assert!(vad.is_err());
    }
}
use std::collections::VecDeque;
