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
    block_startup: AtomicBool,
    startup_started: AtomicBool,
    block_transcription: AtomicBool,
    transcription_started: AtomicBool,
    empty_transcript: AtomicBool,
    transcription_gate: (Mutex<bool>, Condvar),
    startup_gate: (Mutex<bool>, Condvar),
    microphone_error_gate: (Mutex<bool>, Condvar),
    outputs: Mutex<Vec<String>>,
}

impl TestRuntime {
    fn release_transcription(&self) {
        *self.transcription_gate.0.lock().unwrap() = true;
        self.transcription_gate.1.notify_all();
    }

    fn release_startup(&self) {
        *self.startup_gate.0.lock().unwrap() = true;
        self.startup_gate.1.notify_all();
    }

    fn release_microphone_error(&self) {
        *self.microphone_error_gate.0.lock().unwrap() = true;
        self.microphone_error_gate.1.notify_all();
    }
}

impl CoordinatorRuntime for TestRuntime {
    fn record(
        &self,
        _microphone: &str,
        max_duration: Duration,
        signal_rx: mpsc::Receiver<RecordingSignal>,
        started: tokio::sync::oneshot::Sender<Result<(), String>>,
    ) -> anyhow::Result<Vec<f32>> {
        self.recordings.fetch_add(1, Ordering::SeqCst);
        self.startup_started.store(true, Ordering::SeqCst);
        if self.block_startup.load(Ordering::SeqCst) {
            let mut released = self.startup_gate.0.lock().unwrap();
            while !*released {
                released = self.startup_gate.1.wait(released).unwrap();
            }
        }
        let _ = started.send(Ok(()));
        if self.microphone_error.load(Ordering::SeqCst) {
            let mut failed = self.microphone_error_gate.0.lock().unwrap();
            while !*failed {
                failed = self.microphone_error_gate.1.wait(failed).unwrap();
            }
            anyhow::bail!("simulated microphone disconnect");
        }
        match signal_rx.recv_timeout(max_duration) {
            Ok(RecordingSignal::Stop) => Ok(vec![0.1; 512]),
            Ok(RecordingSignal::Cancel) => anyhow::bail!("cancelled"),
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
        if self.empty_transcript.load(Ordering::SeqCst) {
            Ok("   ".to_string())
        } else {
            Ok("test transcript".to_string())
        }
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
    Arc::new(Coordinator::with_runtime(config, runtime).unwrap())
}

async fn wait_for_state(coordinator: &Arc<Coordinator>, expected: &str) {
    for _ in 0..300 {
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

async fn wait_for_worker_completion(runtime: &Arc<TestRuntime>, owner_count: usize) {
    for _ in 0..100 {
        if Arc::strong_count(runtime) == owner_count {
            return;
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    panic!("recording worker did not finish");
}

#[tokio::test]
async fn toggle_and_hold_commands_complete_recordings() {
    let runtime = Arc::new(TestRuntime::default());
    let coordinator = coordinator(runtime.clone(), 2);

    assert!(matches!(
        dispatch(&coordinator, Request::Toggle).await,
        Response::RecordingStarted
    ));
    tokio::time::sleep(Duration::from_millis(35)).await;
    assert!(matches!(
        dispatch(&coordinator, Request::Toggle).await,
        Response::RecordingStopped
    ));
    wait_for_state(&coordinator, "idle").await;
    assert!(matches!(
        dispatch(&coordinator, Request::Toggle).await,
        Response::Error { .. }
    ));

    tokio::time::sleep(Duration::from_millis(35)).await;
    assert!(matches!(
        coordinator.handle_activation(true).await.unwrap(),
        tonguetyped::coordinator::CoordinatorResponse::RecordingStarted
    ));
    assert!(matches!(
        coordinator.handle_activation(false).await.unwrap(),
        tonguetyped::coordinator::CoordinatorResponse::RecordingStopped
    ));
    assert!(matches!(
        coordinator.handle_activation(true).await.unwrap(),
        tonguetyped::coordinator::CoordinatorResponse::Ignored(_)
    ));
    assert!(matches!(
        dispatch(&coordinator, Request::Status).await,
        Response::Status { ref state, .. } if state == "recording"
    ));
    tokio::time::sleep(Duration::from_millis(35)).await;
    assert!(matches!(
        coordinator.handle_activation(false).await.unwrap(),
        tonguetyped::coordinator::CoordinatorResponse::RecordingStopped
    ));
    wait_for_state(&coordinator, "idle").await;
    assert_eq!(runtime.recordings.load(Ordering::SeqCst), 2);
}

#[tokio::test]
async fn cancel_during_processing_suppresses_stale_output() {
    let runtime = Arc::new(TestRuntime::default());
    runtime.block_transcription.store(true, Ordering::SeqCst);
    let coordinator = coordinator(runtime.clone(), 2);
    let owner_count = Arc::strong_count(&runtime);

    dispatch(&coordinator, Request::Start).await;
    tokio::time::sleep(Duration::from_millis(35)).await;
    dispatch(&coordinator, Request::Stop).await;
    wait_for_state(&coordinator, "processing").await;
    wait_for_flag(&runtime.transcription_started).await;
    assert!(matches!(
        dispatch(&coordinator, Request::Cancel).await,
        Response::Cancelled
    ));
    runtime.release_transcription();
    wait_for_worker_completion(&runtime, owner_count).await;

    assert!(runtime.outputs.lock().unwrap().is_empty());
    assert!(
        matches!(dispatch(&coordinator, Request::Status).await, Response::Status { state, .. } if state == "idle")
    );
}

#[tokio::test]
async fn cancel_during_startup_does_not_report_recording_started() {
    let runtime = Arc::new(TestRuntime::default());
    runtime.block_startup.store(true, Ordering::SeqCst);
    let coordinator = coordinator(runtime.clone(), 2);
    let start_coordinator = coordinator.clone();
    let start = tokio::spawn(async move { dispatch(&start_coordinator, Request::Start).await });

    wait_for_flag(&runtime.startup_started).await;
    assert!(matches!(
        dispatch(&coordinator, Request::Cancel).await,
        Response::Cancelled
    ));
    runtime.release_startup();

    assert!(matches!(start.await.unwrap(), Response::Error { .. }));
}

#[tokio::test]
async fn microphone_errors_restore_idle_state() {
    let runtime = Arc::new(TestRuntime::default());
    runtime.microphone_error.store(true, Ordering::SeqCst);
    let coordinator = coordinator(runtime.clone(), 2);

    assert!(matches!(
        dispatch(&coordinator, Request::Start).await,
        Response::RecordingStarted
    ));
    runtime.release_microphone_error();
    wait_for_state(&coordinator, "idle").await;
    assert!(matches!(
        dispatch(&coordinator, Request::Status).await,
        Response::Status { error: Some(error), .. } if error.contains("simulated microphone disconnect")
    ));
}

#[tokio::test]
async fn maximum_duration_stops_recording() {
    let runtime = Arc::new(TestRuntime::default());
    let coordinator = coordinator(runtime.clone(), 1);

    coordinator.handle_activation(true).await.unwrap();
    wait_for_state(&coordinator, "idle").await;
    assert!(runtime.timed_out.load(Ordering::SeqCst));

    assert!(matches!(
        coordinator.handle_activation(false).await.unwrap(),
        tonguetyped::coordinator::CoordinatorResponse::Ignored(_)
    ));
    assert!(matches!(
        coordinator.handle_activation(true).await.unwrap(),
        tonguetyped::coordinator::CoordinatorResponse::Ignored(_)
    ));
    assert_eq!(runtime.recordings.load(Ordering::SeqCst), 1);

    coordinator.handle_activation(false).await.unwrap();
    tokio::time::sleep(Duration::from_millis(60)).await;
}

#[tokio::test]
async fn hold_release_uses_mode_from_active_press() {
    let runtime = Arc::new(TestRuntime::default());
    let coordinator = coordinator(runtime, 2);

    assert!(matches!(
        coordinator.handle_activation(true).await.unwrap(),
        tonguetyped::coordinator::CoordinatorResponse::RecordingStarted
    ));
    let mut config = Config::default();
    config.history.enabled = false;
    config.activation.mode = tonguetyped::config::ActivationMode::Toggle;
    coordinator.reload_config(config).unwrap();
    assert!(matches!(
        coordinator.handle_activation(false).await.unwrap(),
        tonguetyped::coordinator::CoordinatorResponse::RecordingStopped
    ));
    wait_for_state(&coordinator, "idle").await;
}

#[tokio::test]
async fn toggle_release_uses_mode_from_active_press() {
    let runtime = Arc::new(TestRuntime::default());
    let mut config = Config::default();
    config.history.enabled = false;
    config.activation.mode = tonguetyped::config::ActivationMode::Toggle;
    let coordinator = Arc::new(Coordinator::with_runtime(config, runtime).unwrap());

    assert!(matches!(
        coordinator.handle_activation(true).await.unwrap(),
        tonguetyped::coordinator::CoordinatorResponse::RecordingStarted
    ));
    let mut config = Config::default();
    config.history.enabled = false;
    coordinator.reload_config(config).unwrap();
    assert!(matches!(
        coordinator.handle_activation(false).await.unwrap(),
        tonguetyped::coordinator::CoordinatorResponse::Ok
    ));
    assert!(matches!(
        dispatch(&coordinator, Request::Status).await,
        Response::Status { ref state, .. } if state == "recording"
    ));
    dispatch(&coordinator, Request::Cancel).await;
}

#[tokio::test]
async fn hold_mode_survives_reload_and_autorepeat_until_final_release() {
    let runtime = Arc::new(TestRuntime::default());
    let coordinator = coordinator(runtime, 2);

    assert!(matches!(
        coordinator.handle_activation(true).await.unwrap(),
        tonguetyped::coordinator::CoordinatorResponse::RecordingStarted
    ));
    let mut config = Config::default();
    config.history.enabled = false;
    config.activation.mode = tonguetyped::config::ActivationMode::Toggle;
    coordinator.reload_config(config).unwrap();

    for _ in 0..2 {
        assert!(matches!(
            coordinator.handle_activation(false).await.unwrap(),
            tonguetyped::coordinator::CoordinatorResponse::RecordingStopped
        ));
        assert!(matches!(
            coordinator.handle_activation(true).await.unwrap(),
            tonguetyped::coordinator::CoordinatorResponse::Ignored(_)
        ));
    }
    tokio::time::sleep(Duration::from_millis(60)).await;
    assert!(matches!(
        dispatch(&coordinator, Request::Status).await,
        Response::Status { ref state, .. } if state == "recording"
    ));

    assert!(matches!(
        coordinator.handle_activation(false).await.unwrap(),
        tonguetyped::coordinator::CoordinatorResponse::RecordingStopped
    ));
    wait_for_state(&coordinator, "idle").await;
}

#[tokio::test]
async fn start_and_toggle_report_busy_during_processing_before_debounce() {
    let runtime = Arc::new(TestRuntime::default());
    runtime.block_transcription.store(true, Ordering::SeqCst);
    let coordinator = coordinator(runtime.clone(), 2);

    assert!(matches!(
        dispatch(&coordinator, Request::Start).await,
        Response::RecordingStarted
    ));
    tokio::time::sleep(Duration::from_millis(35)).await;
    assert!(matches!(
        dispatch(&coordinator, Request::Stop).await,
        Response::RecordingStopped
    ));
    assert!(matches!(
        dispatch(&coordinator, Request::Start).await,
        Response::Busy
    ));
    assert!(matches!(
        dispatch(&coordinator, Request::Toggle).await,
        Response::Busy
    ));

    runtime.release_transcription();
    wait_for_state(&coordinator, "idle").await;
}

#[tokio::test]
async fn ignored_hold_press_absorbs_autorepeat_until_final_release() {
    let runtime = Arc::new(TestRuntime::default());
    let coordinator = coordinator(runtime.clone(), 2);

    assert!(matches!(
        coordinator.handle_activation(true).await.unwrap(),
        tonguetyped::coordinator::CoordinatorResponse::RecordingStarted
    ));
    assert!(matches!(
        dispatch(&coordinator, Request::Cancel).await,
        Response::Cancelled
    ));
    assert!(matches!(
        coordinator.handle_activation(true).await.unwrap(),
        tonguetyped::coordinator::CoordinatorResponse::Ignored(_)
    ));
    tokio::time::sleep(Duration::from_millis(35)).await;
    assert!(matches!(
        coordinator.handle_activation(false).await.unwrap(),
        tonguetyped::coordinator::CoordinatorResponse::Ignored(_)
    ));
    assert!(matches!(
        coordinator.handle_activation(true).await.unwrap(),
        tonguetyped::coordinator::CoordinatorResponse::Ignored(_)
    ));
    assert_eq!(runtime.recordings.load(Ordering::SeqCst), 1);

    assert!(matches!(
        coordinator.handle_activation(false).await.unwrap(),
        tonguetyped::coordinator::CoordinatorResponse::Ignored(_)
    ));
    tokio::time::sleep(Duration::from_millis(60)).await;

    let mut config = Config::default();
    config.history.enabled = false;
    config.activation.mode = tonguetyped::config::ActivationMode::Toggle;
    coordinator.reload_config(config).unwrap();
    assert!(matches!(
        coordinator.handle_activation(true).await.unwrap(),
        tonguetyped::coordinator::CoordinatorResponse::RecordingStarted
    ));
    assert!(matches!(
        coordinator.handle_activation(false).await.unwrap(),
        tonguetyped::coordinator::CoordinatorResponse::Ok
    ));
    assert!(matches!(
        dispatch(&coordinator, Request::Status).await,
        Response::Status { ref state, .. } if state == "recording"
    ));
    dispatch(&coordinator, Request::Cancel).await;
}

#[tokio::test]
async fn hold_release_during_processing_absorbs_autorepeat_before_reloaded_mode() {
    let runtime = Arc::new(TestRuntime::default());
    runtime.block_transcription.store(true, Ordering::SeqCst);
    let coordinator = coordinator(runtime.clone(), 1);

    assert!(matches!(
        coordinator.handle_activation(true).await.unwrap(),
        tonguetyped::coordinator::CoordinatorResponse::RecordingStarted
    ));
    wait_for_state(&coordinator, "processing").await;
    assert!(matches!(
        coordinator.handle_activation(false).await.unwrap(),
        tonguetyped::coordinator::CoordinatorResponse::Busy
    ));

    let mut config = Config::default();
    config.history.enabled = false;
    config.activation.mode = tonguetyped::config::ActivationMode::Toggle;
    coordinator.reload_config(config).unwrap();
    assert!(matches!(
        coordinator.handle_activation(true).await.unwrap(),
        tonguetyped::coordinator::CoordinatorResponse::Ignored(_)
    ));
    assert_eq!(runtime.recordings.load(Ordering::SeqCst), 1);

    runtime.release_transcription();
    wait_for_state(&coordinator, "idle").await;
    assert!(matches!(
        coordinator.handle_activation(false).await.unwrap(),
        tonguetyped::coordinator::CoordinatorResponse::Ignored(_)
    ));
    tokio::time::sleep(Duration::from_millis(60)).await;

    assert!(matches!(
        coordinator.handle_activation(true).await.unwrap(),
        tonguetyped::coordinator::CoordinatorResponse::RecordingStarted
    ));
    assert!(matches!(
        coordinator.handle_activation(false).await.unwrap(),
        tonguetyped::coordinator::CoordinatorResponse::Ok
    ));
    assert!(matches!(
        dispatch(&coordinator, Request::Status).await,
        Response::Status { ref state, .. } if state == "recording"
    ));
    dispatch(&coordinator, Request::Cancel).await;
}

#[tokio::test]
async fn toggle_mode_absorbs_autorepeat_until_final_release() {
    let runtime = Arc::new(TestRuntime::default());
    let mut config = Config::default();
    config.history.enabled = false;
    config.activation.mode = tonguetyped::config::ActivationMode::Toggle;
    let coordinator = Arc::new(Coordinator::with_runtime(config, runtime.clone()).unwrap());

    assert!(matches!(
        coordinator.handle_activation(true).await.unwrap(),
        tonguetyped::coordinator::CoordinatorResponse::RecordingStarted
    ));
    tokio::time::sleep(Duration::from_millis(35)).await;
    assert!(matches!(
        coordinator.handle_activation(false).await.unwrap(),
        tonguetyped::coordinator::CoordinatorResponse::Ok
    ));
    assert!(matches!(
        coordinator.handle_activation(true).await.unwrap(),
        tonguetyped::coordinator::CoordinatorResponse::Ignored(_)
    ));
    assert!(matches!(
        dispatch(&coordinator, Request::Status).await,
        Response::Status { ref state, .. } if state == "recording"
    ));
    assert_eq!(runtime.recordings.load(Ordering::SeqCst), 1);

    assert!(matches!(
        coordinator.handle_activation(false).await.unwrap(),
        tonguetyped::coordinator::CoordinatorResponse::Ok
    ));
    tokio::time::sleep(Duration::from_millis(60)).await;
    assert!(matches!(
        coordinator.handle_activation(true).await.unwrap(),
        tonguetyped::coordinator::CoordinatorResponse::RecordingStopped
    ));
    wait_for_state(&coordinator, "idle").await;
}

#[tokio::test]
async fn ipc_start_finalizes_pending_release_before_mode_reload() {
    let runtime = Arc::new(TestRuntime::default());
    let coordinator = coordinator(runtime, 1);

    assert!(matches!(
        coordinator.handle_activation(true).await.unwrap(),
        tonguetyped::coordinator::CoordinatorResponse::RecordingStarted
    ));
    wait_for_state(&coordinator, "idle").await;
    assert!(matches!(
        coordinator.handle_activation(false).await.unwrap(),
        tonguetyped::coordinator::CoordinatorResponse::Ignored(_)
    ));
    assert!(matches!(
        dispatch(&coordinator, Request::Start).await,
        Response::RecordingStarted
    ));

    let mut config = Config::default();
    config.history.enabled = false;
    config.activation.mode = tonguetyped::config::ActivationMode::Toggle;
    coordinator.reload_config(config).unwrap();
    tokio::time::sleep(Duration::from_millis(35)).await;

    assert!(matches!(
        coordinator.handle_activation(true).await.unwrap(),
        tonguetyped::coordinator::CoordinatorResponse::RecordingStopped
    ));
    wait_for_state(&coordinator, "idle").await;
}

#[tokio::test]
async fn ipc_start_absorbs_held_activation_autorepeat_until_final_release() {
    for mode in [
        tonguetyped::config::ActivationMode::Hold,
        tonguetyped::config::ActivationMode::Toggle,
    ] {
        let runtime = Arc::new(TestRuntime::default());
        runtime.block_transcription.store(true, Ordering::SeqCst);
        let mut config = Config::default();
        config.history.enabled = false;
        config.activation.mode = mode;
        config.transcription.max_recording_seconds = 1;
        let coordinator = Arc::new(Coordinator::with_runtime(config, runtime.clone()).unwrap());

        assert!(matches!(
            coordinator.handle_activation(true).await.unwrap(),
            tonguetyped::coordinator::CoordinatorResponse::RecordingStarted
        ));
        wait_for_state(&coordinator, "processing").await;
        runtime.release_transcription();
        wait_for_state(&coordinator, "idle").await;

        assert!(matches!(
            dispatch(&coordinator, Request::Start).await,
            Response::RecordingStarted
        ));
        tokio::time::sleep(Duration::from_millis(35)).await;
        assert!(matches!(
            coordinator.handle_activation(false).await.unwrap(),
            tonguetyped::coordinator::CoordinatorResponse::Ok
        ));
        assert!(matches!(
            coordinator.handle_activation(true).await.unwrap(),
            tonguetyped::coordinator::CoordinatorResponse::Ignored(_)
        ));
        assert!(matches!(
            dispatch(&coordinator, Request::Status).await,
            Response::Status { ref state, .. } if state == "recording"
        ));

        assert!(matches!(
            coordinator.handle_activation(false).await.unwrap(),
            tonguetyped::coordinator::CoordinatorResponse::Ok
        ));
        tokio::time::sleep(Duration::from_millis(60)).await;
        assert!(matches!(
            dispatch(&coordinator, Request::Status).await,
            Response::Status { ref state, .. } if state == "recording"
        ));
        dispatch(&coordinator, Request::Cancel).await;
    }
}

#[tokio::test]
async fn whitespace_transcript_is_not_published() {
    let runtime = Arc::new(TestRuntime::default());
    runtime.empty_transcript.store(true, Ordering::SeqCst);
    let coordinator = coordinator(runtime.clone(), 2);

    dispatch(&coordinator, Request::Start).await;
    tokio::time::sleep(Duration::from_millis(35)).await;
    dispatch(&coordinator, Request::Stop).await;
    wait_for_state(&coordinator, "idle").await;

    assert!(runtime.outputs.lock().unwrap().is_empty());
    assert!(coordinator.get_last_result().is_none());
}
