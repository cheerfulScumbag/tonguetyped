use crate::config::Config;
use std::sync::{mpsc, Arc, Mutex};
use std::time::{Duration, Instant};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum State {
    Idle,
    Recording,
    Processing,
}

pub enum CoordinatorCommand {
    Start,
    Stop,
    Toggle,
    Cancel,
    HoldPress,
    HoldRelease,
    GetStatus,
}

#[derive(Debug)]
pub enum RecordingSignal {
    Stop,
    Cancel,
}

pub trait CoordinatorRuntime: Send + Sync {
    fn record(
        &self,
        microphone: &str,
        max_duration: Duration,
        signal_rx: mpsc::Receiver<RecordingSignal>,
        started: tokio::sync::oneshot::Sender<Result<(), String>>,
    ) -> anyhow::Result<Vec<f32>>;
    fn transcribe(&self, samples: &[f32], config: &Config) -> anyhow::Result<String>;
    fn output(&self, text: &str, config: &Config) -> anyhow::Result<()>;
}

struct ProductionRuntime;

impl CoordinatorRuntime for ProductionRuntime {
    fn record(
        &self,
        microphone: &str,
        max_duration: Duration,
        signal_rx: mpsc::Receiver<RecordingSignal>,
        started: tokio::sync::oneshot::Sender<Result<(), String>>,
    ) -> anyhow::Result<Vec<f32>> {
        let (error_tx, error_rx) = mpsc::channel();
        let error_callback = Arc::new(move |message: String| {
            let _ = error_tx.send(message);
        });
        let mut recorder = match crate::audio::AudioRecorder::new(
            microphone,
            16_000,
            Some(Arc::new(|level| tracing::trace!(level, "microphone level"))),
            Some(error_callback),
        ) {
            Ok(recorder) => recorder,
            Err(error) => {
                let message = error.to_string();
                let _ = started.send(Err(message.clone()));
                anyhow::bail!(message);
            }
        };
        if let Err(error) = recorder.start() {
            let message = error.to_string();
            let _ = started.send(Err(message.clone()));
            anyhow::bail!(message);
        }
        let deadline = match Instant::now().checked_add(max_duration) {
            Some(deadline) => deadline,
            None => {
                let message = "max recording duration is too large".to_string();
                let _ = started.send(Err(message.clone()));
                anyhow::bail!(message);
            }
        };
        let _ = started.send(Ok(()));
        wait_for_recording_end(&signal_rx, &error_rx, deadline, || recorder.stop())?;
        recorder.take_buffer()
    }

    fn transcribe(&self, samples: &[f32], config: &Config) -> anyhow::Result<String> {
        let samples = if config.transcription.vad_enabled {
            apply_vad(samples)?
        } else {
            samples.to_vec()
        };
        if samples.is_empty() {
            return Ok(String::new());
        }
        let mut engine = crate::inference::InferenceEngine::new(
            crate::model::ModelCatalog::model_path(&config.model.selected)?,
        );
        engine.load()?;
        let result = engine.transcribe(&samples, &config.transcription.language);
        engine.unload();
        result
    }

    fn output(&self, text: &str, config: &Config) -> anyhow::Result<()> {
        crate::output::output_text(
            text,
            &config.output.method,
            &config.output.typing_backend,
            config.output.auto_submit,
        )
    }
}

fn wait_for_recording_end(
    signal_rx: &mpsc::Receiver<RecordingSignal>,
    error_rx: &mpsc::Receiver<String>,
    deadline: Instant,
    stop: impl FnOnce(),
) -> anyhow::Result<()> {
    let result = loop {
        if let Ok(message) = error_rx.try_recv() {
            break Err(anyhow::anyhow!("microphone stream failed: {}", message));
        }
        let remaining = deadline.saturating_duration_since(Instant::now());
        if remaining.is_zero() {
            break Ok(());
        }
        match signal_rx.recv_timeout(remaining.min(Duration::from_millis(50))) {
            Ok(RecordingSignal::Stop) => break Ok(()),
            Ok(RecordingSignal::Cancel) => break Err(anyhow::anyhow!("recording cancelled")),
            Err(mpsc::RecvTimeoutError::Timeout) => {}
            Err(mpsc::RecvTimeoutError::Disconnected) => break Ok(()),
        }
    };

    stop();
    if let Ok(message) = error_rx.try_recv() {
        anyhow::bail!("microphone stream failed: {}", message);
    }
    result
}

fn apply_vad(samples: &[f32]) -> anyhow::Result<Vec<f32>> {
    let model_path = crate::inference::InferenceEngine::models_dir()?.join("silero_vad_v4.onnx");
    let mut detector = crate::vad::VadDetector::new(model_path.to_string_lossy().as_ref(), 16_000)?;
    detector.reset();
    let mut speech = Vec::new();
    for chunk in samples.chunks(512) {
        let mut padded = [0.0; 512];
        padded[..chunk.len()].copy_from_slice(chunk);
        if let Some(segment) = detector.process_window(&padded, chunk.len())? {
            speech.extend(segment);
        }
    }
    if let Some(segment) = detector.finish() {
        speech.extend(segment);
    }
    detector.reset();
    Ok(speech)
}

struct CoordinatorStateInner {
    state: State,
    config: Config,
    last_action: Option<Instant>,
    pending_release: Option<(u64, Instant)>,
    generation: u64,
    signal_tx: Option<mpsc::Sender<RecordingSignal>>,
    active_press_mode: Option<crate::config::ActivationMode>,
    last_error: Option<String>,
}

#[derive(Clone)]
pub struct Coordinator {
    state: Arc<Mutex<CoordinatorStateInner>>,
    output_lock: Arc<Mutex<()>>,
    last_result: Arc<Mutex<Option<(String, u64)>>>,
    runtime: Arc<dyn CoordinatorRuntime>,
}

impl Coordinator {
    pub fn new(config: Config) -> anyhow::Result<Self> {
        Self::with_runtime(config, Arc::new(ProductionRuntime))
    }

    pub fn with_runtime(
        config: Config,
        runtime: Arc<dyn CoordinatorRuntime>,
    ) -> anyhow::Result<Self> {
        let last_result = if config.history.enabled {
            crate::history::HistoryStore::new(&crate::history::history_db_path()?)?
                .get_last_result()?
        } else {
            None
        };
        Ok(Self {
            state: Arc::new(Mutex::new(CoordinatorStateInner {
                state: State::Idle,
                config,
                last_action: None,
                pending_release: None,
                generation: 0,
                signal_tx: None,
                active_press_mode: None,
                last_error: None,
            })),
            output_lock: Arc::new(Mutex::new(())),
            last_result: Arc::new(Mutex::new(last_result)),
            runtime,
        })
    }

    pub async fn handle_command(
        &self,
        cmd: CoordinatorCommand,
    ) -> anyhow::Result<CoordinatorResponse> {
        if matches!(cmd, CoordinatorCommand::Cancel) {
            return Ok(self.cancel());
        }

        let mut start = None;
        let response = {
            let mut inner = self.state.lock().unwrap();
            let now = Instant::now();
            match cmd {
                CoordinatorCommand::Start | CoordinatorCommand::Toggle
                    if inner.state == State::Processing =>
                {
                    CoordinatorResponse::Busy
                }
                CoordinatorCommand::Start | CoordinatorCommand::Toggle
                    if !debounce_elapsed(&inner, now) =>
                {
                    CoordinatorResponse::Ignored("debounce".into())
                }
                CoordinatorCommand::Start => match inner.state {
                    State::Idle => {
                        start = Some(reserve_recording(&mut inner, now));
                        CoordinatorResponse::RecordingStarted
                    }
                    State::Recording => CoordinatorResponse::Ignored("already recording".into()),
                    State::Processing => CoordinatorResponse::Busy,
                },
                CoordinatorCommand::Stop => stop_recording(&mut inner, now),
                CoordinatorCommand::Toggle => match inner.state {
                    State::Idle => {
                        start = Some(reserve_recording(&mut inner, now));
                        CoordinatorResponse::RecordingStarted
                    }
                    State::Recording => stop_recording(&mut inner, now),
                    State::Processing => CoordinatorResponse::Busy,
                },
                CoordinatorCommand::HoldPress => {
                    if !debounce_elapsed(&inner, now) {
                        CoordinatorResponse::Ignored("debounce".into())
                    } else {
                        hold_press(&mut inner, now, &mut start)
                    }
                }
                CoordinatorCommand::HoldRelease => match inner.state {
                    State::Recording => {
                        inner.last_action = Some(now);
                        CoordinatorResponse::RecordingStopped
                    }
                    State::Processing => CoordinatorResponse::Busy,
                    State::Idle => CoordinatorResponse::Ignored("not recording".into()),
                },
                CoordinatorCommand::GetStatus => build_status(&inner),
                CoordinatorCommand::Cancel => unreachable!(),
            }
        };

        if let Some((generation, signal_rx, config)) = start {
            let started = self.spawn_worker(generation, signal_rx, config);
            match started.await {
                Ok(Ok(())) => {}
                Ok(Err(message)) => return Err(anyhow::anyhow!(message)),
                Err(_) => return Err(anyhow::anyhow!("recording worker exited during startup")),
            }
            let inner = self.state.lock().unwrap();
            if inner.generation != generation || inner.state != State::Recording {
                return Ok(CoordinatorResponse::Ignored(
                    "recording cancelled during startup".into(),
                ));
            }
        }
        Ok(response)
    }

    pub fn reload_config(&self, config: Config) -> anyhow::Result<()> {
        self.validate_reload(&config)?;
        let mut inner = self.state.lock().unwrap();
        inner.config = config;
        Ok(())
    }

    pub fn validate_reload(&self, config: &Config) -> anyhow::Result<()> {
        config.validate()?;
        if self.state.lock().unwrap().config.activation.keybind != config.activation.keybind {
            anyhow::bail!("changing activation.keybind requires a daemon restart");
        }
        Ok(())
    }

    pub async fn handle_activation(&self, pressed: bool) -> anyhow::Result<CoordinatorResponse> {
        let (mode, release) = {
            let mut inner = self.state.lock().unwrap();
            if pressed {
                if let Some((generation, released_at)) = inner.pending_release {
                    if Instant::now().duration_since(released_at) < Duration::from_millis(50) {
                        inner.pending_release = None;
                        return Ok(CoordinatorResponse::Ignored("auto-repeat defense".into()));
                    }
                    inner.pending_release = None;
                    complete_activation_release(&mut inner, generation);
                }
                let mode = inner
                    .active_press_mode
                    .clone()
                    .unwrap_or_else(|| inner.config.activation.mode.clone());
                inner.active_press_mode = Some(mode.clone());
                (mode, None)
            } else {
                let Some(mode) = inner.active_press_mode.clone() else {
                    return Ok(CoordinatorResponse::Ok);
                };
                let released_at = Instant::now();
                let generation = inner.generation;
                inner.pending_release = Some((generation, released_at));
                (mode, Some((generation, released_at)))
            }
        };
        let response = match (&mode, pressed) {
            (crate::config::ActivationMode::Toggle, true) => {
                self.handle_command(CoordinatorCommand::Toggle).await
            }
            (crate::config::ActivationMode::Hold, true) => {
                self.handle_command(CoordinatorCommand::HoldPress).await
            }
            (crate::config::ActivationMode::Hold, false) => {
                self.handle_command(CoordinatorCommand::HoldRelease).await
            }
            (crate::config::ActivationMode::Toggle, false) => Ok(CoordinatorResponse::Ok),
        }?;
        if let Some((generation, released_at)) = release {
            self.schedule_activation_release(generation, released_at);
        }
        Ok(response)
    }

    fn cancel(&self) -> CoordinatorResponse {
        let _output_guard = self.output_lock.lock().unwrap();
        let mut inner = self.state.lock().unwrap();
        match inner.state {
            State::Idle => CoordinatorResponse::Ignored("idle".into()),
            State::Recording | State::Processing => {
                inner.generation += 1;
                if let Some(sender) = inner.signal_tx.take() {
                    let _ = sender.send(RecordingSignal::Cancel);
                }
                inner.state = State::Idle;
                inner.pending_release = None;
                inner.active_press_mode = None;
                inner.last_action = Some(Instant::now());
                CoordinatorResponse::Cancelled
            }
        }
    }

    fn schedule_activation_release(&self, generation: u64, released_at: Instant) {
        let coordinator = self.clone();
        tokio::spawn(async move {
            tokio::time::sleep(Duration::from_millis(50)).await;
            let mut inner = coordinator.state.lock().unwrap();
            if matches!(inner.pending_release, Some((token, pending_at)) if token == generation && pending_at == released_at)
            {
                inner.pending_release = None;
                complete_activation_release(&mut inner, generation);
            }
        });
    }

    fn spawn_worker(
        &self,
        generation: u64,
        signal_rx: mpsc::Receiver<RecordingSignal>,
        config: Config,
    ) -> tokio::sync::oneshot::Receiver<Result<(), String>> {
        let (started_tx, started_rx) = tokio::sync::oneshot::channel();
        let coordinator = self.clone();
        tokio::task::spawn_blocking(move || {
            let duration = Duration::from_secs(config.transcription.max_recording_seconds);
            let samples = match coordinator.runtime.record(
                &config.audio.microphone,
                duration,
                signal_rx,
                started_tx,
            ) {
                Ok(samples) => samples,
                Err(error) => {
                    tracing::error!("recording failed: {}", error);
                    coordinator.settle_error(generation, error.to_string());
                    return;
                }
            };

            {
                let mut inner = coordinator.state.lock().unwrap();
                if inner.generation != generation {
                    return;
                }
                inner.state = State::Processing;
                inner.signal_tx = None;
            }

            let transcript = match coordinator.runtime.transcribe(&samples, &config) {
                Ok(text) => text,
                Err(error) => {
                    tracing::error!("transcription failed: {}", error);
                    coordinator.settle(generation);
                    return;
                }
            };
            let trimmed = transcript.trim().to_string();
            if trimmed.is_empty() {
                coordinator.settle(generation);
                return;
            }
            let _output_guard = coordinator.output_lock.lock().unwrap();
            if coordinator.state.lock().unwrap().generation != generation {
                return;
            }

            if !trimmed.is_empty() {
                let output_text = if config.transcription.trailing_space {
                    format!("{} ", trimmed)
                } else {
                    trimmed.clone()
                };
                if let Err(error) = coordinator.runtime.output(&output_text, &config) {
                    tracing::error!("output failed: {}", error);
                }
            }

            let timestamp = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap_or_default()
                .as_secs();
            *coordinator.last_result.lock().unwrap() = Some((trimmed.clone(), timestamp));
            if config.history.enabled {
                match crate::history::history_db_path()
                    .and_then(|path| crate::history::HistoryStore::new(&path))
                {
                    Ok(store) => {
                        if let Err(error) = store
                            .insert(&trimmed, None, &config.transcription.language)
                            .and_then(|_| store.prune(config.history.max_entries))
                        {
                            tracing::error!("history update failed: {}", error);
                        }
                    }
                    Err(error) => tracing::error!("history unavailable: {}", error),
                }
            }
            coordinator.settle(generation);
        });
        started_rx
    }

    fn settle(&self, generation: u64) {
        let mut inner = self.state.lock().unwrap();
        if inner.generation == generation {
            inner.state = State::Idle;
            inner.signal_tx = None;
        }
    }

    fn settle_error(&self, generation: u64, error: String) {
        let mut inner = self.state.lock().unwrap();
        if inner.generation == generation {
            inner.state = State::Idle;
            inner.signal_tx = None;
            inner.last_error = Some(error);
        }
    }

    pub fn get_last_result(&self) -> Option<(String, u64)> {
        self.last_result.lock().unwrap().clone()
    }

    pub fn activation_keybind(&self) -> String {
        self.state.lock().unwrap().config.activation.keybind.clone()
    }
}

type ReservedRecording = (u64, mpsc::Receiver<RecordingSignal>, Config);

fn reserve_recording(inner: &mut CoordinatorStateInner, now: Instant) -> ReservedRecording {
    let (signal_tx, signal_rx) = mpsc::channel();
    inner.generation += 1;
    inner.state = State::Recording;
    inner.last_action = Some(now);
    inner.pending_release = None;
    inner.signal_tx = Some(signal_tx);
    inner.last_error = None;
    (inner.generation, signal_rx, inner.config.clone())
}

fn hold_press(
    inner: &mut CoordinatorStateInner,
    now: Instant,
    start: &mut Option<ReservedRecording>,
) -> CoordinatorResponse {
    match inner.state {
        State::Idle => {
            *start = Some(reserve_recording(inner, now));
            CoordinatorResponse::RecordingStarted
        }
        State::Recording => CoordinatorResponse::Ignored("already recording".into()),
        State::Processing => CoordinatorResponse::Busy,
    }
}

fn stop_recording(inner: &mut CoordinatorStateInner, now: Instant) -> CoordinatorResponse {
    match inner.state {
        State::Recording => {
            if let Some(sender) = inner.signal_tx.take() {
                let _ = sender.send(RecordingSignal::Stop);
            }
            inner.state = State::Processing;
            inner.pending_release = None;
            inner.active_press_mode = None;
            inner.last_action = Some(now);
            CoordinatorResponse::RecordingStopped
        }
        State::Processing => CoordinatorResponse::Busy,
        State::Idle => CoordinatorResponse::Ignored("not recording".into()),
    }
}

fn complete_activation_release(inner: &mut CoordinatorStateInner, generation: u64) {
    if inner.generation != generation {
        return;
    }
    if inner.state == State::Recording
        && inner.active_press_mode == Some(crate::config::ActivationMode::Hold)
    {
        stop_recording(inner, Instant::now());
    } else {
        inner.active_press_mode = None;
    }
}

fn debounce_elapsed(inner: &CoordinatorStateInner, now: Instant) -> bool {
    inner
        .last_action
        .is_none_or(|last| now.duration_since(last) >= Duration::from_millis(30))
}

fn build_status(inner: &CoordinatorStateInner) -> CoordinatorResponse {
    CoordinatorResponse::Status {
        state: match inner.state {
            State::Idle => "idle",
            State::Recording => "recording",
            State::Processing => "processing",
        }
        .to_string(),
        activation_mode: inner.config.activation.mode.to_string(),
        error: inner.last_error.clone(),
    }
}

#[derive(Debug, Clone)]
pub enum CoordinatorResponse {
    Ok,
    Busy,
    RecordingStarted,
    RecordingStopped,
    Cancelled,
    Ignored(String),
    Status {
        state: String,
        activation_mode: String,
        error: Option<String>,
    },
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn microphone_error_at_stop_rejects_recording() {
        let (signal_tx, signal_rx) = mpsc::channel();
        let (error_tx, error_rx) = mpsc::channel();
        signal_tx.send(RecordingSignal::Stop).unwrap();

        let result = wait_for_recording_end(
            &signal_rx,
            &error_rx,
            Instant::now() + Duration::from_secs(1),
            || error_tx.send("device disconnected".to_string()).unwrap(),
        );

        assert_eq!(
            result.unwrap_err().to_string(),
            "microphone stream failed: device disconnected"
        );
    }
}
