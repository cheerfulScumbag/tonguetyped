use serde::Serialize;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

static NEXT_OPERATION_ID: AtomicU64 = AtomicU64::new(1);

pub trait Clock: Send + Sync {
    fn now(&self) -> Duration;
}

pub struct MonotonicClock {
    origin: Instant,
}

impl MonotonicClock {
    pub fn new() -> Self {
        Self {
            origin: Instant::now(),
        }
    }
}

impl Default for MonotonicClock {
    fn default() -> Self {
        Self::new()
    }
}

impl Clock for MonotonicClock {
    fn now(&self) -> Duration {
        self.origin.elapsed()
    }
}

#[derive(Debug, Clone, Copy)]
pub enum Phase {
    AudioFinalization,
    Vad,
    ModelLoad,
    Inference,
    Output,
    History,
}

#[derive(Debug, Clone, Serialize, PartialEq)]
pub struct LatencyRecord {
    pub operation_id: String,
    pub outcome: String,
    pub model_id: String,
    pub backend: String,
    pub device: String,
    pub cold_model_load: bool,
    pub stop_received_monotonic_ms: f64,
    pub audio_finalization_ms: f64,
    pub vad_ms: f64,
    pub model_load_ms: f64,
    pub inference_ms: f64,
    pub output_ms: f64,
    pub history_ms: f64,
    pub total_stop_to_idle_ms: f64,
}

pub trait LatencySink: Send + Sync {
    fn emit(&self, record: LatencyRecord);
}

pub struct TracingLatencySink;

impl LatencySink for TracingLatencySink {
    fn emit(&self, record: LatencyRecord) {
        tracing::info!(
            target: "tonguetyped::latency",
            operation_id = %record.operation_id,
            outcome = %record.outcome,
            model_id = %record.model_id,
            backend = %record.backend,
            device = %record.device,
            cold_model_load = record.cold_model_load,
            stop_received_monotonic_ms = record.stop_received_monotonic_ms,
            audio_finalization_ms = record.audio_finalization_ms,
            vad_ms = record.vad_ms,
            model_load_ms = record.model_load_ms,
            inference_ms = record.inference_ms,
            output_ms = record.output_ms,
            history_ms = record.history_ms,
            total_stop_to_idle_ms = record.total_stop_to_idle_ms,
            "dictation latency"
        );
    }
}

pub struct LatencyOperation {
    clock: Arc<dyn Clock>,
    operation_id: String,
    model_id: String,
    stop_received: Option<Duration>,
    audio_finalization_started: Option<Duration>,
    audio_finalization: Duration,
    vad: Duration,
    model_load: Duration,
    inference: Duration,
    output: Duration,
    history: Duration,
    cold_model_load: bool,
    terminal: bool,
}

impl LatencyOperation {
    pub fn new(model_id: String, clock: Arc<dyn Clock>) -> Self {
        let sequence = NEXT_OPERATION_ID.fetch_add(1, Ordering::Relaxed);
        Self {
            clock,
            operation_id: format!("{}-{sequence}", std::process::id()),
            model_id,
            stop_received: None,
            audio_finalization_started: None,
            audio_finalization: Duration::ZERO,
            vad: Duration::ZERO,
            model_load: Duration::ZERO,
            inference: Duration::ZERO,
            output: Duration::ZERO,
            history: Duration::ZERO,
            cold_model_load: false,
            terminal: false,
        }
    }

    pub fn mark_stop_received(&mut self) {
        self.mark_stop_received_at(self.clock.now());
    }

    pub fn mark_stop_received_at(&mut self, received: Duration) {
        self.stop_received = Some(
            self.stop_received
                .map_or(received, |current| current.min(received)),
        );
    }

    pub fn mark_audio_finalization_started(&mut self) {
        self.mark_audio_finalization_started_at(self.clock.now());
    }

    pub fn mark_audio_finalization_started_at(&mut self, started: Duration) {
        self.audio_finalization_started = Some(
            self.audio_finalization_started
                .map_or(started, |current| current.min(started)),
        );
    }

    pub fn phase_started(&self) -> Duration {
        self.clock.now()
    }

    pub fn finish_phase(&mut self, phase: Phase, started: Duration) {
        self.set_phase(phase, self.clock.now().saturating_sub(started));
    }

    pub fn set_phase(&mut self, phase: Phase, duration: Duration) {
        match phase {
            Phase::AudioFinalization => self.audio_finalization = duration,
            Phase::Vad => self.vad = duration,
            Phase::ModelLoad => self.model_load = duration,
            Phase::Inference => self.inference = duration,
            Phase::Output => self.output = duration,
            Phase::History => self.history = duration,
        }
    }

    pub fn set_cold_model_load(&mut self, cold: bool) {
        self.cold_model_load = cold;
    }

    pub fn elapsed_since_stop(&self) -> Duration {
        self.stop_received
            .map(|started| self.clock.now().saturating_sub(started))
            .unwrap_or_default()
    }

    pub fn elapsed_audio_finalization(&self) -> Duration {
        self.audio_finalization_started
            .map(|started| self.clock.now().saturating_sub(started))
            .unwrap_or_default()
    }

    pub fn finish(&mut self, outcome: &str) -> Option<LatencyRecord> {
        if self.terminal {
            return None;
        }
        self.mark_stop_received();
        self.terminal = true;
        let stop_received = self.stop_received.unwrap();
        let total = self.elapsed_since_stop();
        let backend = crate::inference::backend_info();
        Some(LatencyRecord {
            operation_id: self.operation_id.clone(),
            outcome: outcome.to_string(),
            model_id: self.model_id.clone(),
            backend: backend.backend,
            device: backend.device,
            cold_model_load: self.cold_model_load,
            stop_received_monotonic_ms: millis(stop_received),
            audio_finalization_ms: millis(self.audio_finalization),
            vad_ms: millis(self.vad),
            model_load_ms: millis(self.model_load),
            inference_ms: millis(self.inference),
            output_ms: millis(self.output),
            history_ms: millis(self.history),
            total_stop_to_idle_ms: millis(total),
        })
    }
}

fn millis(duration: Duration) -> f64 {
    duration.as_secs_f64() * 1_000.0
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Mutex;

    #[derive(Default)]
    struct FakeClock(Mutex<Duration>);

    impl FakeClock {
        fn advance(&self, duration: Duration) {
            *self.0.lock().unwrap() += duration;
        }
    }

    impl Clock for FakeClock {
        fn now(&self) -> Duration {
            *self.0.lock().unwrap()
        }
    }

    #[test]
    fn aggregates_phases_with_a_monotonic_fake_clock_once() {
        let clock = Arc::new(FakeClock::default());
        let mut operation = LatencyOperation::new("model-a".to_string(), clock.clone());
        operation.mark_stop_received();

        for (phase, milliseconds) in [
            (Phase::AudioFinalization, 2),
            (Phase::Vad, 3),
            (Phase::ModelLoad, 5),
            (Phase::Inference, 7),
            (Phase::Output, 11),
            (Phase::History, 13),
        ] {
            let started = operation.phase_started();
            clock.advance(Duration::from_millis(milliseconds));
            operation.finish_phase(phase, started);
        }
        operation.set_cold_model_load(true);

        let record = operation.finish("success").unwrap();
        assert_eq!(record.outcome, "success");
        assert_eq!(record.model_id, "model-a");
        assert!(record.cold_model_load);
        assert_eq!(record.audio_finalization_ms, 2.0);
        assert_eq!(record.vad_ms, 3.0);
        assert_eq!(record.model_load_ms, 5.0);
        assert_eq!(record.inference_ms, 7.0);
        assert_eq!(record.output_ms, 11.0);
        assert_eq!(record.history_ms, 13.0);
        assert_eq!(record.total_stop_to_idle_ms, 41.0);
        assert!(operation.finish("success").is_none());
    }
}
