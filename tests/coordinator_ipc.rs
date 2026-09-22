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
    transcription_error: AtomicBool,
    output_error: AtomicBool,
    transcription_gate: (Mutex<bool>, Condvar),
    startup_gate: (Mutex<bool>, Condvar),
    microphone_error_gate: (Mutex<bool>, Condvar),
    outputs: Mutex<Vec<String>>,
    idle_unload_suspensions: AtomicUsize,
    idle_policy_applications: Mutex<Vec<(String, bool)>>,
    block_idle_policy: AtomicBool,
    idle_policy_invocations: AtomicUsize,
    idle_policy_started: (Mutex<bool>, Condvar),
    second_idle_policy_started: (Mutex<bool>, Condvar),
    idle_policy_gate: (Mutex<bool>, Condvar),
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

    fn release_idle_policy(&self) {
        *self.idle_policy_gate.0.lock().unwrap() = true;
        self.idle_policy_gate.1.notify_all();
    }

    fn wait_for_idle_policy_start(&self) {
        let started = self.idle_policy_started.0.lock().unwrap();
        let (started, timeout) = self
            .idle_policy_started
            .1
            .wait_timeout_while(started, Duration::from_secs(1), |started| !*started)
            .unwrap();
        assert!(!timeout.timed_out() && *started, "idle policy did not start");
    }

    fn wait_for_second_idle_policy_start(&self) {
        let started = self.second_idle_policy_started.0.lock().unwrap();
        let (_started, _timeout) = self
            .second_idle_policy_started
            .1
            .wait_timeout_while(started, Duration::from_secs(1), |started| !*started)
            .unwrap();
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
        } else if self.transcription_error.load(Ordering::SeqCst) {
            anyhow::bail!("simulated transcription failure")
        } else {
            Ok("test transcript".to_string())
        }
    }

    fn output(&self, text: &str, _config: &Config) -> anyhow::Result<()> {
        if self.output_error.load(Ordering::SeqCst) {
            anyhow::bail!("simulated output failure");
        }
        self.outputs.lock().unwrap().push(text.to_string());
        Ok(())
    }

    fn suspend_idle_unload(&self) -> anyhow::Result<()> {
        self.idle_unload_suspensions.fetch_add(1, Ordering::SeqCst);
        Ok(())
    }

    fn apply_idle_unload_policy(
        &self,
        config: &Config,
        transcription_attempted: bool,
    ) -> anyhow::Result<()> {
        let invocation = self.idle_policy_invocations.fetch_add(1, Ordering::SeqCst) + 1;
        if self.block_idle_policy.swap(false, Ordering::SeqCst) {
            *self.idle_policy_started.0.lock().unwrap() = true;
            self.idle_policy_started.1.notify_all();
            let mut released = self.idle_policy_gate.0.lock().unwrap();
            while !*released {
                released = self.idle_policy_gate.1.wait(released).unwrap();
            }
        }
        self.idle_policy_applications.lock().unwrap().push((
            config.model.idle_unload.policy.to_string(),
            transcription_attempted,
        ));
        if invocation == 2 {
            *self.second_idle_policy_started.0.lock().unwrap() = true;
            self.second_idle_policy_started.1.notify_all();
        }
        Ok(())
    }
}

#[tokio::test]
async fn shortcut_listener_health_is_explicit() {
    let runtime = Arc::new(TestRuntime::default());
    let coordinator = coordinator(runtime, 2);

    assert!(matches!(
        dispatch(&coordinator, Request::Status).await,
        Response::Status {
            shortcut_status: tonguetyped::ipc::ShortcutStatus::Initializing,
            activation_error: None,
            ..
        }
    ));

    coordinator.set_runtime_ready();
    assert!(matches!(
        dispatch(&coordinator, Request::Status).await,
        Response::Status {
            shortcut_status: tonguetyped::ipc::ShortcutStatus::Available,
            activation_error: None,
            ..
        }
    ));

    coordinator.set_runtime_error("portal failed".to_string());
    assert!(matches!(
        dispatch(&coordinator, Request::Status).await,
        Response::Status {
            shortcut_status: tonguetyped::ipc::ShortcutStatus::Failed,
            activation_error: Some(ref error),
            ..
        } if error == "portal failed"
    ));
}

#[tokio::test]
async fn transcription_failure_is_reported_in_status() {
    let runtime = Arc::new(TestRuntime::default());
    runtime.transcription_error.store(true, Ordering::SeqCst);
    let coordinator = coordinator(runtime, 2);

    assert!(matches!(
        dispatch(&coordinator, Request::Start).await,
        Response::RecordingStarted
    ));
    assert!(matches!(
        dispatch(&coordinator, Request::Stop).await,
        Response::RecordingStopped
    ));
    wait_for_state(&coordinator, "idle").await;

    assert!(matches!(
        dispatch(&coordinator, Request::Status).await,
        Response::Status { operation_error: Some(ref error), .. }
            if error.contains("transcription failed")
    ));
}

#[tokio::test]
async fn output_failure_preserves_transcript_and_is_reported_in_status() {
    let runtime = Arc::new(TestRuntime::default());
    runtime.output_error.store(true, Ordering::SeqCst);
    let coordinator = coordinator(runtime, 2);

    assert!(matches!(
        dispatch(&coordinator, Request::Start).await,
        Response::RecordingStarted
    ));
    assert!(matches!(
        dispatch(&coordinator, Request::Stop).await,
        Response::RecordingStopped
    ));
    wait_for_state(&coordinator, "idle").await;

    assert!(matches!(
        dispatch(&coordinator, Request::GetLastResult).await,
        Response::LastResult { ref text, .. } if text == "test transcript"
    ));
    assert!(matches!(
        dispatch(&coordinator, Request::Status).await,
        Response::Status { operation_error: Some(ref error), .. }
            if error.contains("output failed")
    ));
}

#[tokio::test]
async fn successful_recording_does_not_hide_activation_listener_failure() {
    let runtime = Arc::new(TestRuntime::default());
    runtime.output_error.store(true, Ordering::SeqCst);
    let coordinator = coordinator(runtime, 2);
    coordinator.set_runtime_error("activation listener failed: portal unavailable".to_string());

    assert!(matches!(
        dispatch(&coordinator, Request::Start).await,
        Response::RecordingStarted
    ));
    tokio::time::sleep(Duration::from_millis(35)).await;
    assert!(matches!(
        dispatch(&coordinator, Request::Stop).await,
        Response::RecordingStopped
    ));
    wait_for_state(&coordinator, "idle").await;

    assert!(matches!(
        dispatch(&coordinator, Request::Status).await,
        Response::Status {
            operation_error: Some(ref operation_error),
            activation_error: Some(ref activation_error),
            ..
        } if operation_error.contains("output failed")
            && activation_error.contains("activation listener failed")
    ));
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
async fn recording_suspends_idle_unload_and_completion_reapplies_policy() {
    let runtime = Arc::new(TestRuntime::default());
    let coordinator = coordinator(runtime.clone(), 2);
    let owner_count = Arc::strong_count(&runtime);

    assert!(matches!(
        dispatch(&coordinator, Request::Start).await,
        Response::RecordingStarted
    ));
    assert_eq!(runtime.idle_unload_suspensions.load(Ordering::SeqCst), 1);

    assert!(matches!(
        dispatch(&coordinator, Request::Cancel).await,
        Response::Cancelled
    ));
    wait_for_worker_completion(&runtime, owner_count).await;
    assert_eq!(
        runtime.idle_policy_applications.lock().unwrap().as_slice(),
        &[("after_idle".to_string(), false)]
    );
}

#[tokio::test]
async fn reload_applies_idle_policy_immediately_or_suspends_during_recording() {
    let runtime = Arc::new(TestRuntime::default());
    let coordinator = coordinator(runtime.clone(), 2);
    let owner_count = Arc::strong_count(&runtime);

    let mut config = Config::default();
    config.history.enabled = false;
    config.model.idle_unload.policy = tonguetyped::config::IdleUnloadPolicy::Never;
    coordinator.reload_config(config.clone()).unwrap();
    config.model.idle_unload.policy = tonguetyped::config::IdleUnloadPolicy::AfterIdle;
    coordinator.reload_config(config.clone()).unwrap();
    assert_eq!(
        runtime.idle_policy_applications.lock().unwrap().as_slice(),
        &[
            ("never".to_string(), false),
            ("after_idle".to_string(), false),
        ]
    );

    assert!(matches!(
        dispatch(&coordinator, Request::Start).await,
        Response::RecordingStarted
    ));
    config.model.idle_unload.policy = tonguetyped::config::IdleUnloadPolicy::Never;
    coordinator.reload_config(config).unwrap();
    assert_eq!(runtime.idle_unload_suspensions.load(Ordering::SeqCst), 2);
    assert_eq!(runtime.idle_policy_applications.lock().unwrap().len(), 2);

    dispatch(&coordinator, Request::Cancel).await;
    wait_for_worker_completion(&runtime, owner_count).await;
    assert_eq!(
        runtime.idle_policy_applications.lock().unwrap().last(),
        Some(&("never".to_string(), false))
    );
}

#[tokio::test]
async fn worker_completion_serializes_idle_policy_with_reload() {
    let runtime = Arc::new(TestRuntime::default());
    runtime.block_idle_policy.store(true, Ordering::SeqCst);
    let coordinator = coordinator(runtime.clone(), 2);

    assert!(matches!(
        dispatch(&coordinator, Request::Start).await,
        Response::RecordingStarted
    ));
    assert!(matches!(
        dispatch(&coordinator, Request::Cancel).await,
        Response::Cancelled
    ));
    runtime.wait_for_idle_policy_start();

    let mut config = Config::default();
    config.history.enabled = false;
    config.model.idle_unload.policy = tonguetyped::config::IdleUnloadPolicy::Never;
    let release_runtime = runtime.clone();
    let release = std::thread::spawn(move || {
        release_runtime.wait_for_second_idle_policy_start();
        release_runtime.release_idle_policy();
    });
    coordinator.reload_config(config).unwrap();
    release.join().unwrap();
    assert_eq!(
        runtime.idle_policy_applications.lock().unwrap().as_slice(),
        &[
            ("after_idle".to_string(), false),
            ("never".to_string(), false),
        ]
    );
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
    tokio::time::sleep(Duration::from_millis(35)).await;
    assert!(matches!(
        dispatch(&coordinator, Request::Start).await,
        Response::Busy
    ));
    assert_eq!(runtime.recordings.load(Ordering::SeqCst), 1);
    runtime.release_startup();

    assert!(matches!(start.await.unwrap(), Response::Error { .. }));
    wait_for_worker_completion(&runtime, 2).await;
    tokio::time::sleep(Duration::from_millis(35)).await;
    assert!(matches!(
        dispatch(&coordinator, Request::Start).await,
        Response::RecordingStarted
    ));
    assert_eq!(runtime.recordings.load(Ordering::SeqCst), 2);
    assert!(matches!(
        dispatch(&coordinator, Request::Cancel).await,
        Response::Cancelled
    ));
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
        Response::Status { operation_error: Some(error), .. } if error.contains("simulated microphone disconnect")
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
async fn cancel_absorbs_held_activation_autorepeat_until_final_release() {
    for mode in [
        tonguetyped::config::ActivationMode::Hold,
        tonguetyped::config::ActivationMode::Toggle,
    ] {
        let runtime = Arc::new(TestRuntime::default());
        let mut config = Config::default();
        config.history.enabled = false;
        config.activation.mode = mode;
        let coordinator = Arc::new(Coordinator::with_runtime(config, runtime.clone()).unwrap());

        assert!(matches!(
            coordinator.handle_activation(true).await.unwrap(),
            tonguetyped::coordinator::CoordinatorResponse::RecordingStarted
        ));
        assert!(matches!(
            dispatch(&coordinator, Request::Cancel).await,
            Response::Cancelled
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
        assert_eq!(runtime.recordings.load(Ordering::SeqCst), 1);

        assert!(matches!(
            coordinator.handle_activation(false).await.unwrap(),
            tonguetyped::coordinator::CoordinatorResponse::Ok
        ));
        tokio::time::sleep(Duration::from_millis(60)).await;
        assert!(matches!(
            coordinator.handle_activation(true).await.unwrap(),
            tonguetyped::coordinator::CoordinatorResponse::RecordingStarted
        ));
        dispatch(&coordinator, Request::Cancel).await;
    }
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
async fn toggle_stop_absorbs_autorepeat_after_transcription_settles() {
    let runtime = Arc::new(TestRuntime::default());
    let mut config = Config::default();
    config.history.enabled = false;
    config.activation.mode = tonguetyped::config::ActivationMode::Toggle;
    let coordinator = Arc::new(Coordinator::with_runtime(config, runtime.clone()).unwrap());

    assert!(matches!(
        coordinator.handle_activation(true).await.unwrap(),
        tonguetyped::coordinator::CoordinatorResponse::RecordingStarted
    ));
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
    tokio::time::sleep(Duration::from_millis(35)).await;
    assert!(matches!(
        coordinator.handle_activation(false).await.unwrap(),
        tonguetyped::coordinator::CoordinatorResponse::Ok
    ));
    assert!(matches!(
        coordinator.handle_activation(true).await.unwrap(),
        tonguetyped::coordinator::CoordinatorResponse::Ignored(_)
    ));
    assert_eq!(runtime.recordings.load(Ordering::SeqCst), 1);
    assert!(matches!(
        dispatch(&coordinator, Request::Status).await,
        Response::Status { ref state, .. } if state == "idle"
    ));

    assert!(matches!(
        coordinator.handle_activation(false).await.unwrap(),
        tonguetyped::coordinator::CoordinatorResponse::Ok
    ));
    tokio::time::sleep(Duration::from_millis(60)).await;
    assert!(matches!(
        coordinator.handle_activation(true).await.unwrap(),
        tonguetyped::coordinator::CoordinatorResponse::RecordingStarted
    ));
    dispatch(&coordinator, Request::Cancel).await;
}

#[tokio::test]
async fn completed_hold_release_does_not_detach_the_next_activation() {
    let runtime = Arc::new(TestRuntime::default());
    let coordinator = coordinator(runtime.clone(), 2);

    assert!(matches!(
        coordinator.handle_activation(true).await.unwrap(),
        tonguetyped::coordinator::CoordinatorResponse::RecordingStarted
    ));
    assert!(matches!(
        coordinator.handle_activation(false).await.unwrap(),
        tonguetyped::coordinator::CoordinatorResponse::RecordingStopped
    ));
    wait_for_state(&coordinator, "idle").await;
    tokio::time::sleep(Duration::from_millis(35)).await;

    assert!(matches!(
        dispatch(&coordinator, Request::Start).await,
        Response::RecordingStarted
    ));
    tokio::time::sleep(Duration::from_millis(35)).await;
    assert!(matches!(
        dispatch(&coordinator, Request::Stop).await,
        Response::RecordingStopped
    ));
    wait_for_state(&coordinator, "idle").await;

    let mut config = Config::default();
    config.history.enabled = false;
    config.activation.mode = tonguetyped::config::ActivationMode::Toggle;
    coordinator.reload_config(config).unwrap();
    tokio::time::sleep(Duration::from_millis(35)).await;

    assert!(matches!(
        coordinator.handle_activation(true).await.unwrap(),
        tonguetyped::coordinator::CoordinatorResponse::RecordingStarted
    ));
    assert_eq!(runtime.recordings.load(Ordering::SeqCst), 3);
    dispatch(&coordinator, Request::Cancel).await;
}

#[tokio::test]
async fn rejected_hold_press_cannot_stop_an_ipc_recording() {
    let runtime = Arc::new(TestRuntime::default());
    let coordinator = coordinator(runtime.clone(), 2);

    assert!(matches!(
        dispatch(&coordinator, Request::Start).await,
        Response::RecordingStarted
    ));
    tokio::time::sleep(Duration::from_millis(35)).await;
    assert!(matches!(
        coordinator.handle_activation(true).await.unwrap(),
        tonguetyped::coordinator::CoordinatorResponse::Ignored(_)
    ));

    assert!(matches!(
        coordinator.handle_activation(false).await.unwrap(),
        tonguetyped::coordinator::CoordinatorResponse::Ok
    ));
    assert!(matches!(
        coordinator.handle_activation(true).await.unwrap(),
        tonguetyped::coordinator::CoordinatorResponse::Ignored(_)
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
    assert_eq!(runtime.recordings.load(Ordering::SeqCst), 1);
    dispatch(&coordinator, Request::Cancel).await;
}

#[tokio::test]
async fn ipc_start_preserves_pending_release_before_mode_reload() {
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
async fn repeated_ipc_start_preserves_detached_autorepeat_until_final_release() {
    for mode in [
        tonguetyped::config::ActivationMode::Hold,
        tonguetyped::config::ActivationMode::Toggle,
    ] {
        let runtime = Arc::new(TestRuntime::default());
        let mut config = Config::default();
        config.history.enabled = false;
        config.activation.mode = mode;
        config.transcription.max_recording_seconds = 1;
        let coordinator = Arc::new(Coordinator::with_runtime(config, runtime).unwrap());

        coordinator.handle_activation(true).await.unwrap();
        wait_for_state(&coordinator, "idle").await;
        assert!(matches!(
            dispatch(&coordinator, Request::Start).await,
            Response::RecordingStarted
        ));
        tokio::time::sleep(Duration::from_millis(35)).await;
        assert!(matches!(
            dispatch(&coordinator, Request::Stop).await,
            Response::RecordingStopped
        ));
        wait_for_state(&coordinator, "idle").await;
        tokio::time::sleep(Duration::from_millis(35)).await;

        assert!(matches!(
            coordinator.handle_activation(false).await.unwrap(),
            tonguetyped::coordinator::CoordinatorResponse::Ok
        ));
        assert!(matches!(
            dispatch(&coordinator, Request::Start).await,
            Response::RecordingStarted
        ));
        tokio::time::sleep(Duration::from_millis(35)).await;
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
