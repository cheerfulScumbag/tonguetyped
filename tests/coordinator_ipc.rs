use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{mpsc, Arc, Condvar, Mutex};
use std::time::Duration;
use tonguetyped::config::Config;
use tonguetyped::coordinator::{Coordinator, CoordinatorRuntime, RecordingSignal};
use tonguetyped::daemon::dispatch;
use tonguetyped::ipc::{Request, Response};

#[derive(Default)]
struct TestRuntime {
    recordings: AtomicUsize,
    microphone_error: AtomicBool,
    timed_out: AtomicBool,
    block_transcription: AtomicBool,
    transcription_started: AtomicBool,
    transcription_gate: (Mutex<bool>, Condvar),
    outputs: Mutex<Vec<String>>,
}

impl TestRuntime {
    fn release_transcription(&self) {
        *self.transcription_gate.0.lock().unwrap() = true;
        self.transcription_gate.1.notify_all();
    }
}

impl CoordinatorRuntime for TestRuntime {
    fn record(
        &self,
        _microphone: &str,
        max_duration: Duration,
        signal_rx: mpsc::Receiver<RecordingSignal>,
    ) -> anyhow::Result<Vec<f32>> {
        self.recordings.fetch_add(1, Ordering::SeqCst);
        if self.microphone_error.load(Ordering::SeqCst) {
            anyhow::bail!("simulated microphone disconnect");
        }
        match signal_rx.recv_timeout(max_duration) {
            Ok(RecordingSignal::Stop) => Ok(vec![0.1; 512]),
            Ok(RecordingSignal::Cancel) => anyhow::bail!("cancelled"),
            Ok(RecordingSignal::MicrophoneError(message)) => anyhow::bail!(message),
            Err(mpsc::RecvTimeoutError::Timeout) => {
                self.timed_out.store(true, Ordering::SeqCst);
                Ok(vec![0.1; 512])
            }
            Err(mpsc::RecvTimeoutError::Disconnected) => Ok(Vec::new()),
        }
    }

    fn transcribe(&self, _samples: &[f32], _config: &Config) -> anyhow::Result<String> {
        self.transcription_started.store(true, Ordering::SeqCst);
        if self.block_transcription.load(Ordering::SeqCst) {
            let mut released = self.transcription_gate.0.lock().unwrap();
            while !*released {
                released = self.transcription_gate.1.wait(released).unwrap();
            }
        }
        Ok("test transcript".to_string())
    }

    fn output(&self, text: &str, _config: &Config) -> anyhow::Result<()> {
        self.outputs.lock().unwrap().push(text.to_string());
        Ok(())
    }
}

fn coordinator(runtime: Arc<TestRuntime>, max_seconds: u64) -> Arc<Coordinator> {
    let mut config = Config::default();
    config.history.enabled = false;
    config.transcription.max_recording_seconds = max_seconds;
    Arc::new(Coordinator::with_runtime(config, runtime))
}

async fn wait_for_state(coordinator: &Arc<Coordinator>, expected: &str) {
    for _ in 0..100 {
        if let Response::Status { state, .. } = dispatch(coordinator, Request::Status).await {
            if state == expected {
                return;
            }
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    panic!("coordinator did not reach {expected}");
}

async fn wait_for_flag(flag: &AtomicBool) {
    for _ in 0..100 {
        if flag.load(Ordering::SeqCst) {
            return;
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    panic!("expected operation did not start");
}

#[tokio::test]
async fn toggle_and_hold_commands_complete_recordings() {
    let runtime = Arc::new(TestRuntime::default());
    let coordinator = coordinator(runtime.clone(), 2);

    assert!(matches!(dispatch(&coordinator, Request::Toggle).await, Response::RecordingStarted));
    tokio::time::sleep(Duration::from_millis(35)).await;
    assert!(matches!(dispatch(&coordinator, Request::Toggle).await, Response::RecordingStopped));
    wait_for_state(&coordinator, "idle").await;
    assert!(matches!(dispatch(&coordinator, Request::Toggle).await, Response::Error { .. }));

    tokio::time::sleep(Duration::from_millis(35)).await;
    assert!(matches!(dispatch(&coordinator, Request::HoldPress).await, Response::RecordingStarted));
    assert!(matches!(dispatch(&coordinator, Request::HoldRelease).await, Response::RecordingStopped));
    assert!(matches!(dispatch(&coordinator, Request::HoldPress).await, Response::Error { .. }));
    assert!(matches!(dispatch(&coordinator, Request::Status).await, Response::Status { recording: true, .. }));
    tokio::time::sleep(Duration::from_millis(35)).await;
    assert!(matches!(dispatch(&coordinator, Request::HoldRelease).await, Response::RecordingStopped));
    wait_for_state(&coordinator, "idle").await;
    assert_eq!(runtime.recordings.load(Ordering::SeqCst), 2);
}

#[tokio::test]
async fn cancel_during_processing_suppresses_stale_output() {
    let runtime = Arc::new(TestRuntime::default());
    runtime.block_transcription.store(true, Ordering::SeqCst);
    let coordinator = coordinator(runtime.clone(), 2);

    dispatch(&coordinator, Request::Start).await;
    tokio::time::sleep(Duration::from_millis(35)).await;
    dispatch(&coordinator, Request::Stop).await;
    wait_for_state(&coordinator, "processing").await;
    wait_for_flag(&runtime.transcription_started).await;
    assert!(matches!(dispatch(&coordinator, Request::Cancel).await, Response::Cancelled));
    runtime.release_transcription();
    tokio::time::sleep(Duration::from_millis(30)).await;

    assert!(runtime.outputs.lock().unwrap().is_empty());
    assert!(matches!(dispatch(&coordinator, Request::Status).await, Response::Status { state, .. } if state == "idle"));
}

#[tokio::test]
async fn microphone_errors_restore_idle_state() {
    let runtime = Arc::new(TestRuntime::default());
    runtime.microphone_error.store(true, Ordering::SeqCst);
    let coordinator = coordinator(runtime, 2);

    assert!(matches!(dispatch(&coordinator, Request::Start).await, Response::RecordingStarted));
    wait_for_state(&coordinator, "idle").await;
}

#[tokio::test]
async fn maximum_duration_stops_recording() {
    let runtime = Arc::new(TestRuntime::default());
    let coordinator = coordinator(runtime.clone(), 1);

    dispatch(&coordinator, Request::Start).await;
    wait_for_state(&coordinator, "idle").await;
    assert!(runtime.timed_out.load(Ordering::SeqCst));
}
