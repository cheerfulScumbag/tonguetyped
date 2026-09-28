use crate::config::Config;
use crate::feedback::{DesktopFeedback, Feedback, FeedbackEvent, NoFeedback};
use crate::latency::{
    Clock, LatencyOperation, LatencySink, MonotonicClock, Phase, TracingLatencySink,
};
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
    fn transcribe(&self, samples: &[f32], config: &Config) -> TranscriptionAttempt;
    fn output(&self, text: &str, config: &Config) -> anyhow::Result<()>;
    fn suspend_idle_unload(&self) -> anyhow::Result<()> {
        Ok(())
    }
    fn apply_idle_unload_policy(
        &self,
        _config: &Config,
        _transcription_attempted: bool,
    ) -> anyhow::Result<()> {
        Ok(())
    }
}

#[derive(Debug, Default)]
pub struct TranscriptionTimings {
    pub vad: Duration,
    pub model_load: Duration,
    pub inference: Duration,
    pub cold_model_load: bool,
}

pub struct TranscriptionAttempt {
    pub result: anyhow::Result<String>,
    pub timings: TranscriptionTimings,
}

struct ProductionRuntime {
    inference: Arc<Mutex<EngineLifecycle<crate::inference::InferenceEngine>>>,
    idle_unload: IdleUnloadTimer,
}

struct IdleUnloadTimer {
    command_tx: mpsc::Sender<IdleUnloadCommand>,
}

enum IdleUnloadCommand {
    Schedule(u64, Instant),
    Cancel(mpsc::Sender<()>),
    #[cfg(test)]
    Expire(mpsc::Sender<()>),
}

impl IdleUnloadTimer {
    fn new<E: Send + 'static>(lifecycle: Arc<Mutex<EngineLifecycle<E>>>) -> anyhow::Result<Self> {
        let (command_tx, command_rx) = mpsc::channel();
        std::thread::Builder::new()
            .name("tonguetyped-idle-unload".to_string())
            .spawn(move || {
                let mut deadline: Option<(u64, Instant)> = None;
                loop {
                    let received = match deadline {
                        Some((_, at)) => {
                            command_rx.recv_timeout(at.saturating_duration_since(Instant::now()))
                        }
                        None => match command_rx.recv() {
                            Ok(IdleUnloadCommand::Schedule(generation, at)) => {
                                deadline = Some((generation, at));
                                continue;
                            }
                            Ok(IdleUnloadCommand::Cancel(done)) => {
                                let _ = done.send(());
                                continue;
                            }
                            #[cfg(test)]
                            Ok(IdleUnloadCommand::Expire(done)) => {
                                let _ = done.send(());
                                continue;
                            }
                            Err(_) => return,
                        },
                    };
                    match received {
                        Ok(IdleUnloadCommand::Schedule(generation, at)) => {
                            deadline = Some((generation, at));
                        }
                        Ok(IdleUnloadCommand::Cancel(done)) => {
                            deadline = None;
                            let _ = done.send(());
                        }
                        #[cfg(test)]
                        Ok(IdleUnloadCommand::Expire(done)) => {
                            if let Some((generation, _)) = deadline.take() {
                                lifecycle.lock().unwrap().unload_if_idle(generation);
                            }
                            let _ = done.send(());
                        }
                        Err(mpsc::RecvTimeoutError::Timeout) => {
                            let (generation, _) = deadline.take().unwrap();
                            lifecycle.lock().unwrap().unload_if_idle(generation);
                        }
                        Err(mpsc::RecvTimeoutError::Disconnected) => return,
                    }
                }
            })?;
        Ok(Self { command_tx })
    }

    fn schedule(&self, generation: u64, delay: Duration) -> anyhow::Result<()> {
        let deadline = Instant::now()
            .checked_add(delay)
            .ok_or_else(|| anyhow::anyhow!("idle unload timeout is too large"))?;
        self.command_tx
            .send(IdleUnloadCommand::Schedule(generation, deadline))
            .map_err(|_| anyhow::anyhow!("idle unload timer stopped"))
    }

    fn cancel(&self) -> anyhow::Result<()> {
        let (done_tx, done_rx) = mpsc::channel();
        self.command_tx
            .send(IdleUnloadCommand::Cancel(done_tx))
            .map_err(|_| anyhow::anyhow!("idle unload timer stopped"))?;
        done_rx
            .recv()
            .map_err(|_| anyhow::anyhow!("idle unload timer stopped"))
    }

    #[cfg(test)]
    fn expire(&self) {
        let (done_tx, done_rx) = mpsc::channel();
        self.command_tx
            .send(IdleUnloadCommand::Expire(done_tx))
            .unwrap();
        done_rx.recv().unwrap();
    }
}

impl ProductionRuntime {
    fn new() -> anyhow::Result<Self> {
        let inference = Arc::new(Mutex::new(EngineLifecycle::default()));
        let idle_unload = IdleUnloadTimer::new(Arc::clone(&inference))?;
        Ok(Self {
            inference,
            idle_unload,
        })
    }

    fn apply_policy(&self, config: &Config, transcription_attempted: bool) -> anyhow::Result<()> {
        let idle_unload = self.inference.lock().unwrap().apply_policy(
            &config.model.idle_unload.policy,
            config.model.idle_unload.timeout_minutes,
            transcription_attempted,
        );
        if let Some((generation, delay)) = idle_unload {
            self.idle_unload.schedule(generation, delay)
        } else {
            self.idle_unload.cancel()
        }
    }
}

struct EngineLifecycle<E> {
    engine: Option<(String, E)>,
    generation: u64,
}

impl<E> Default for EngineLifecycle<E> {
    fn default() -> Self {
        Self {
            engine: None,
            generation: 0,
        }
    }
}

impl<E> EngineLifecycle<E> {
    fn has_model(&self, model: &str) -> bool {
        self.engine
            .as_ref()
            .is_some_and(|(loaded_model, _)| loaded_model == model)
    }

    fn ensure(
        &mut self,
        model: &str,
        load: impl FnOnce() -> anyhow::Result<E>,
    ) -> anyhow::Result<&mut E> {
        if self
            .engine
            .as_ref()
            .is_some_and(|(loaded_model, _)| loaded_model != model)
        {
            self.engine = None;
        }
        if self.engine.is_none() {
            self.engine = Some((model.to_string(), load()?));
        }
        self.generation = self.generation.wrapping_add(1);
        Ok(&mut self.engine.as_mut().unwrap().1)
    }

    fn apply_policy(
        &mut self,
        policy: &crate::config::IdleUnloadPolicy,
        timeout_minutes: u64,
        transcription_attempted: bool,
    ) -> Option<(u64, Duration)> {
        self.suspend_idle_unload();
        match policy {
            crate::config::IdleUnloadPolicy::Never => None,
            crate::config::IdleUnloadPolicy::AfterTranscription => {
                if transcription_attempted {
                    self.engine = None;
                }
                None
            }
            crate::config::IdleUnloadPolicy::AfterIdle => self.engine.as_ref().map(|_| {
                (
                    self.generation,
                    Duration::from_secs(timeout_minutes.saturating_mul(60)),
                )
            }),
        }
    }

    fn suspend_idle_unload(&mut self) {
        self.generation = self.generation.wrapping_add(1);
    }

    fn unload_if_idle(&mut self, generation: u64) {
        if self.generation == generation {
            self.engine = None;
        }
    }

    #[cfg(test)]
    fn is_loaded(&self) -> bool {
        self.engine.is_some()
    }
}

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

    fn transcribe(&self, samples: &[f32], config: &Config) -> TranscriptionAttempt {
        let mut timings = TranscriptionTimings::default();
        let samples = if config.transcription.vad_enabled {
            let vad_started = Instant::now();
            match crate::inference::InferenceEngine::models_dir().and_then(|directory| {
                crate::vad::filter_audio(samples, &directory.join("silero_vad_v4.onnx"))
            }) {
                Ok(samples) => {
                    timings.vad = vad_started.elapsed();
                    samples
                }
                Err(error) => {
                    timings.vad = vad_started.elapsed();
                    return TranscriptionAttempt {
                        result: Err(error),
                        timings,
                    };
                }
            }
        } else {
            samples.to_vec()
        };
        if samples.is_empty() {
            return TranscriptionAttempt {
                result: Ok(String::new()),
                timings,
            };
        }
        let mut lifecycle = self.inference.lock().unwrap();
        timings.cold_model_load = !lifecycle.has_model(&config.model.selected);
        let load_started = Instant::now();
        let engine = match lifecycle.ensure(&config.model.selected, || {
            let mut engine = crate::inference::InferenceEngine::new(
                crate::model::ModelCatalog::model_path(&config.model.selected)?,
            );
            engine.load()?;
            Ok(engine)
        }) {
            Ok(engine) => engine,
            Err(error) => {
                timings.model_load = load_started.elapsed();
                return TranscriptionAttempt {
                    result: Err(error),
                    timings,
                };
            }
        };
        if timings.cold_model_load {
            timings.model_load = load_started.elapsed();
        }
        let inference_started = Instant::now();
        let result = engine.transcribe(&samples, &config.transcription.language);
        timings.inference = inference_started.elapsed();
        TranscriptionAttempt { result, timings }
    }

    fn output(&self, text: &str, config: &Config) -> anyhow::Result<()> {
        crate::output::output_text(
            text,
            &config.output.method,
            &config.output.typing_backend,
            config.output.auto_submit,
        )
    }

    fn suspend_idle_unload(&self) -> anyhow::Result<()> {
        self.inference.lock().unwrap().suspend_idle_unload();
        self.idle_unload.cancel()
    }

    fn apply_idle_unload_policy(
        &self,
        config: &Config,
        transcription_attempted: bool,
    ) -> anyhow::Result<()> {
        self.apply_policy(config, transcription_attempted)
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

struct CoordinatorStateInner {
    state: State,
    config: Config,
    last_action: Option<Instant>,
    pending_release: Option<(u64, Instant, Duration)>,
    generation: u64,
    signal_tx: Option<mpsc::Sender<RecordingSignal>>,
    physical_press: Option<PhysicalPress>,
    last_error: Option<String>,
    runtime_error: Option<String>,
    shortcut_status: crate::ipc::ShortcutStatus,
    worker_active: bool,
    active_timing: Option<Arc<Mutex<LatencyOperation>>>,
}

#[derive(Clone)]
enum PhysicalPress {
    Active(crate::config::ActivationMode),
    Detached,
}

#[derive(Clone)]
pub struct Coordinator {
    state: Arc<Mutex<CoordinatorStateInner>>,
    output_lock: Arc<Mutex<()>>,
    last_result: Arc<Mutex<Option<(String, u64)>>>,
    runtime: Arc<dyn CoordinatorRuntime>,
    feedback: Arc<dyn Feedback>,
    clock: Arc<dyn Clock>,
    latency_sink: Arc<dyn LatencySink>,
}

impl Coordinator {
    pub fn new(config: Config) -> anyhow::Result<Self> {
        Self::with_runtime_and_feedback(
            config,
            Arc::new(ProductionRuntime::new()?),
            Arc::new(DesktopFeedback),
        )
    }

    pub fn with_runtime(
        config: Config,
        runtime: Arc<dyn CoordinatorRuntime>,
    ) -> anyhow::Result<Self> {
        Self::with_runtime_and_feedback(config, runtime, Arc::new(NoFeedback))
    }

    pub fn with_runtime_and_feedback(
        config: Config,
        runtime: Arc<dyn CoordinatorRuntime>,
        feedback: Arc<dyn Feedback>,
    ) -> anyhow::Result<Self> {
        Self::with_runtime_feedback_and_latency(
            config,
            runtime,
            feedback,
            Arc::new(MonotonicClock::new()),
            Arc::new(TracingLatencySink),
        )
    }

    pub fn with_runtime_feedback_and_latency(
        config: Config,
        runtime: Arc<dyn CoordinatorRuntime>,
        feedback: Arc<dyn Feedback>,
        clock: Arc<dyn Clock>,
        latency_sink: Arc<dyn LatencySink>,
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
                physical_press: None,
                last_error: None,
                runtime_error: None,
                shortcut_status: crate::ipc::ShortcutStatus::Initializing,
                worker_active: false,
                active_timing: None,
            })),
            output_lock: Arc::new(Mutex::new(())),
            last_result: Arc::new(Mutex::new(last_result)),
            runtime,
            feedback,
            clock,
            latency_sink,
        })
    }

    pub async fn handle_command(
        &self,
        cmd: CoordinatorCommand,
    ) -> anyhow::Result<CoordinatorResponse> {
        self.handle_command_inner(cmd, true).await
    }

    async fn handle_command_inner(
        &self,
        cmd: CoordinatorCommand,
        detach_physical_press: bool,
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
                CoordinatorCommand::Start
                | CoordinatorCommand::Toggle
                | CoordinatorCommand::HoldPress
                    if inner.worker_active && inner.state == State::Idle =>
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
                        if detach_physical_press {
                            detach_physical_press_for_ipc(&mut inner);
                        }
                        start = Some(reserve_recording(
                            self.runtime.as_ref(),
                            &mut inner,
                            now,
                            Arc::clone(&self.clock),
                        )?);
                        CoordinatorResponse::RecordingStarted
                    }
                    State::Recording => CoordinatorResponse::Ignored("already recording".into()),
                    State::Processing => CoordinatorResponse::Busy,
                },
                CoordinatorCommand::Stop => stop_recording(self.feedback.as_ref(), &mut inner, now),
                CoordinatorCommand::Toggle => match inner.state {
                    State::Idle => {
                        if detach_physical_press {
                            detach_physical_press_for_ipc(&mut inner);
                        }
                        start = Some(reserve_recording(
                            self.runtime.as_ref(),
                            &mut inner,
                            now,
                            Arc::clone(&self.clock),
                        )?);
                        CoordinatorResponse::RecordingStarted
                    }
                    State::Recording => stop_recording(self.feedback.as_ref(), &mut inner, now),
                    State::Processing => CoordinatorResponse::Busy,
                },
                CoordinatorCommand::HoldPress => {
                    if !debounce_elapsed(&inner, now) {
                        CoordinatorResponse::Ignored("debounce".into())
                    } else {
                        hold_press(
                            self.runtime.as_ref(),
                            &mut inner,
                            now,
                            &mut start,
                            Arc::clone(&self.clock),
                        )?
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

        if let Some((generation, signal_rx, config, timing)) = start {
            let started = self.spawn_worker(generation, signal_rx, config, timing);
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
            self.feedback.send(FeedbackEvent::Recording, &inner.config);
        }
        Ok(response)
    }

    pub fn reload_config(&self, config: Config) -> anyhow::Result<()> {
        self.validate_reload(&config)?;
        let mut inner = self.state.lock().unwrap();
        if inner.worker_active {
            inner.config = config;
            return Ok(());
        }
        self.runtime.apply_idle_unload_policy(&config, false)?;
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
        let (mode, release, new_press) = {
            let mut inner = self.state.lock().unwrap();
            if pressed {
                if let Some((generation, released_at, timing_at)) = inner.pending_release {
                    if Instant::now().duration_since(released_at) < Duration::from_millis(50) {
                        inner.pending_release = None;
                        return Ok(CoordinatorResponse::Ignored("auto-repeat defense".into()));
                    }
                    inner.pending_release = None;
                    complete_activation_release(
                        self.feedback.as_ref(),
                        &mut inner,
                        generation,
                        timing_at,
                    );
                }
                if matches!(inner.physical_press, Some(PhysicalPress::Detached)) {
                    return Ok(CoordinatorResponse::Ignored(
                        "detached physical press".into(),
                    ));
                }
                let mode = match &inner.physical_press {
                    Some(PhysicalPress::Active(mode)) => mode.clone(),
                    Some(PhysicalPress::Detached) => unreachable!(),
                    None => inner.config.activation.mode.clone(),
                };
                let new_press = inner.physical_press.is_none();
                inner.physical_press = Some(PhysicalPress::Active(mode.clone()));
                (mode, None, new_press)
            } else {
                let Some(physical_press) = inner.physical_press.clone() else {
                    return Ok(CoordinatorResponse::Ok);
                };
                let released_at = Instant::now();
                let timing_at = self.clock.now();
                let generation = inner.generation;
                inner.pending_release = Some((generation, released_at, timing_at));
                let mode = match physical_press {
                    PhysicalPress::Active(mode) => mode,
                    PhysicalPress::Detached => {
                        self.schedule_activation_release(generation, released_at, timing_at);
                        return Ok(CoordinatorResponse::Ok);
                    }
                };
                (mode, Some((generation, released_at, timing_at)), false)
            }
        };
        let response = match (&mode, pressed) {
            (crate::config::ActivationMode::Toggle, true) => {
                self.handle_command_inner(CoordinatorCommand::Toggle, false)
                    .await
            }
            (crate::config::ActivationMode::Hold, true) => {
                self.handle_command_inner(CoordinatorCommand::HoldPress, false)
                    .await
            }
            (crate::config::ActivationMode::Hold, false) => {
                self.handle_command_inner(CoordinatorCommand::HoldRelease, false)
                    .await
            }
            (crate::config::ActivationMode::Toggle, false) => Ok(CoordinatorResponse::Ok),
        }?;
        if let Some((generation, released_at, timing_at)) = release {
            self.schedule_activation_release(generation, released_at, timing_at);
        }
        if new_press
            && matches!(mode, crate::config::ActivationMode::Hold)
            && !matches!(response, CoordinatorResponse::RecordingStarted)
        {
            let mut inner = self.state.lock().unwrap();
            if matches!(
                inner.physical_press,
                Some(PhysicalPress::Active(crate::config::ActivationMode::Hold))
            ) {
                inner.physical_press = Some(PhysicalPress::Detached);
            }
        }
        Ok(response)
    }

    fn cancel(&self) -> CoordinatorResponse {
        let _output_guard = self.output_lock.lock().unwrap();
        let mut inner = self.state.lock().unwrap();
        match inner.state {
            State::Idle => CoordinatorResponse::Ignored("idle".into()),
            State::Recording | State::Processing => {
                if let Some(timing) = &inner.active_timing {
                    timing.lock().unwrap().mark_stop_received();
                }
                inner.generation += 1;
                if let Some(sender) = inner.signal_tx.take() {
                    let _ = sender.send(RecordingSignal::Cancel);
                }
                inner.state = State::Idle;
                if inner.physical_press.is_some() {
                    inner.physical_press = Some(PhysicalPress::Detached);
                }
                inner.last_action = Some(Instant::now());
                self.feedback.send(FeedbackEvent::Cancelled, &inner.config);
                let timing = inner.active_timing.take();
                drop(inner);
                if let Some(timing) = timing {
                    self.emit_timing(&timing, "cancelled");
                }
                CoordinatorResponse::Cancelled
            }
        }
    }

    fn schedule_activation_release(
        &self,
        generation: u64,
        released_at: Instant,
        timing_at: Duration,
    ) {
        let coordinator = self.clone();
        tokio::spawn(async move {
            tokio::time::sleep(Duration::from_millis(50)).await;
            let mut inner = coordinator.state.lock().unwrap();
            if matches!(inner.pending_release, Some((token, pending_at, _)) if token == generation && pending_at == released_at)
            {
                inner.pending_release = None;
                complete_activation_release(
                    coordinator.feedback.as_ref(),
                    &mut inner,
                    generation,
                    timing_at,
                );
            }
        });
    }

    fn spawn_worker(
        &self,
        generation: u64,
        signal_rx: mpsc::Receiver<RecordingSignal>,
        config: Config,
        timing: Arc<Mutex<LatencyOperation>>,
    ) -> tokio::sync::oneshot::Receiver<Result<(), String>> {
        let (started_tx, started_rx) = tokio::sync::oneshot::channel();
        let coordinator = self.clone();
        tokio::task::spawn_blocking(move || {
            let mut completion = WorkerCompletion {
                coordinator: coordinator.clone(),
                generation,
                transcription_attempted: false,
            };
            let duration = Duration::from_secs(config.transcription.max_recording_seconds);
            let recording = coordinator.runtime.record(
                &config.audio.microphone,
                duration,
                signal_rx,
                started_tx,
            );
            {
                let mut timing = timing.lock().unwrap();
                timing.mark_stop_received();
                let duration = timing.elapsed_since_stop();
                timing.set_phase(Phase::AudioFinalization, duration);
            }
            let samples = match recording {
                Ok(samples) => samples,
                Err(error) => {
                    tracing::error!("recording failed: {}", error);
                    coordinator.settle_error(
                        generation,
                        error.to_string(),
                        &timing,
                        "recording_error",
                    );
                    return;
                }
            };

            {
                let mut inner = coordinator.state.lock().unwrap();
                if inner.generation != generation {
                    return;
                }
                if inner.state == State::Recording {
                    inner.state = State::Processing;
                    coordinator
                        .feedback
                        .send(FeedbackEvent::Processing, &inner.config);
                }
                inner.signal_tx = None;
            }

            completion.transcription_attempted = true;
            let attempt = coordinator.runtime.transcribe(&samples, &config);
            {
                let mut timing = timing.lock().unwrap();
                timing.set_phase(Phase::Vad, attempt.timings.vad);
                timing.set_phase(Phase::ModelLoad, attempt.timings.model_load);
                timing.set_phase(Phase::Inference, attempt.timings.inference);
                timing.set_cold_model_load(attempt.timings.cold_model_load);
            }
            let transcript = match attempt.result {
                Ok(text) => text,
                Err(error) => {
                    tracing::error!("transcription failed: {}", error);
                    coordinator.settle_error(
                        generation,
                        format!("transcription failed: {error}"),
                        &timing,
                        "transcription_error",
                    );
                    return;
                }
            };
            let trimmed = transcript.trim().to_string();
            if trimmed.is_empty() {
                coordinator.settle(generation, &timing, "empty");
                return;
            }
            let _output_guard = coordinator.output_lock.lock().unwrap();
            if coordinator.state.lock().unwrap().generation != generation {
                return;
            }

            let timestamp = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap_or_default()
                .as_secs();
            *coordinator.last_result.lock().unwrap() = Some((trimmed.clone(), timestamp));
            let output_started = timing.lock().unwrap().phase_started();
            let output_error = if config.transcription.trailing_space {
                coordinator
                    .runtime
                    .output(&format!("{} ", trimmed), &config)
            } else {
                coordinator.runtime.output(&trimmed, &config)
            }
            .err()
            .map(|error| format!("output failed: {error}"));
            timing
                .lock()
                .unwrap()
                .finish_phase(Phase::Output, output_started);
            if let Some(error) = &output_error {
                tracing::error!("{error}");
            }
            let mut history_failed = false;
            if config.history.enabled {
                let history_started = timing.lock().unwrap().phase_started();
                match crate::history::history_db_path()
                    .and_then(|path| crate::history::HistoryStore::new(&path))
                {
                    Ok(store) => {
                        if let Err(error) = store
                            .insert(&trimmed, None, &config.transcription.language)
                            .and_then(|_| store.prune(config.history.max_entries))
                        {
                            tracing::error!("history update failed: {}", error);
                            history_failed = true;
                        }
                    }
                    Err(error) => {
                        tracing::error!("history unavailable: {}", error);
                        history_failed = true;
                    }
                }
                timing
                    .lock()
                    .unwrap()
                    .finish_phase(Phase::History, history_started);
            }
            if let Some(error) = output_error {
                coordinator.settle_error(generation, error, &timing, "output_error");
            } else if history_failed {
                coordinator.settle(generation, &timing, "history_error");
            } else {
                coordinator.settle(generation, &timing, "success");
            }
        });
        started_rx
    }

    fn settle(&self, generation: u64, timing: &Arc<Mutex<LatencyOperation>>, outcome: &str) {
        let mut inner = self.state.lock().unwrap();
        if inner.generation == generation {
            inner.state = State::Idle;
            inner.signal_tx = None;
            inner.active_timing = None;
            self.feedback.send(FeedbackEvent::Success, &inner.config);
            drop(inner);
            self.emit_timing(timing, outcome);
        }
    }

    fn settle_error(
        &self,
        generation: u64,
        error: String,
        timing: &Arc<Mutex<LatencyOperation>>,
        outcome: &str,
    ) {
        let mut inner = self.state.lock().unwrap();
        if inner.generation == generation {
            inner.state = State::Idle;
            inner.signal_tx = None;
            inner.active_timing = None;
            inner.last_error = Some(error);
            self.feedback.send(FeedbackEvent::Error, &inner.config);
            drop(inner);
            self.emit_timing(timing, outcome);
        }
    }

    fn emit_timing(&self, timing: &Arc<Mutex<LatencyOperation>>, outcome: &str) {
        if let Some(record) = timing.lock().unwrap().finish(outcome) {
            self.latency_sink.emit(record);
        }
    }

    pub fn get_last_result(&self) -> Option<(String, u64)> {
        self.last_result.lock().unwrap().clone()
    }

    pub fn activation_keybind(&self) -> String {
        self.state.lock().unwrap().config.activation.keybind.clone()
    }

    pub fn set_runtime_error(&self, error: String) {
        let mut inner = self.state.lock().unwrap();
        inner.shortcut_status = crate::ipc::ShortcutStatus::Failed;
        inner.runtime_error = Some(error);
    }

    pub fn set_runtime_ready(&self) {
        let mut inner = self.state.lock().unwrap();
        inner.shortcut_status = crate::ipc::ShortcutStatus::Available;
        inner.runtime_error = None;
    }

    fn worker_finished(&self, generation: u64, transcription_attempted: bool) {
        let mut inner = self.state.lock().unwrap();
        if inner.generation != generation && inner.state == State::Idle {
            inner.signal_tx = None;
        }
        if inner.state == State::Idle {
            if let Err(error) = self
                .runtime
                .apply_idle_unload_policy(&inner.config, transcription_attempted)
            {
                inner.last_error = Some(format!("failed to apply model unload policy: {error}"));
            }
        }
        inner.worker_active = false;
    }
}

struct WorkerCompletion {
    coordinator: Coordinator,
    generation: u64,
    transcription_attempted: bool,
}

impl Drop for WorkerCompletion {
    fn drop(&mut self) {
        self.coordinator
            .worker_finished(self.generation, self.transcription_attempted);
    }
}

type ReservedRecording = (
    u64,
    mpsc::Receiver<RecordingSignal>,
    Config,
    Arc<Mutex<LatencyOperation>>,
);

fn reserve_recording(
    runtime: &dyn CoordinatorRuntime,
    inner: &mut CoordinatorStateInner,
    now: Instant,
    clock: Arc<dyn Clock>,
) -> anyhow::Result<ReservedRecording> {
    runtime.suspend_idle_unload()?;
    let (signal_tx, signal_rx) = mpsc::channel();
    inner.generation += 1;
    inner.state = State::Recording;
    inner.last_action = Some(now);
    inner.signal_tx = Some(signal_tx);
    inner.last_error = None;
    inner.worker_active = true;
    let timing = Arc::new(Mutex::new(LatencyOperation::new(
        inner.config.model.selected.clone(),
        clock,
    )));
    inner.active_timing = Some(Arc::clone(&timing));
    Ok((inner.generation, signal_rx, inner.config.clone(), timing))
}

fn detach_physical_press_for_ipc(inner: &mut CoordinatorStateInner) {
    if inner.physical_press.is_some() {
        inner.physical_press = Some(PhysicalPress::Detached);
    }
}

fn hold_press(
    runtime: &dyn CoordinatorRuntime,
    inner: &mut CoordinatorStateInner,
    now: Instant,
    start: &mut Option<ReservedRecording>,
    clock: Arc<dyn Clock>,
) -> anyhow::Result<CoordinatorResponse> {
    match inner.state {
        State::Idle => {
            *start = Some(reserve_recording(runtime, inner, now, clock)?);
            Ok(CoordinatorResponse::RecordingStarted)
        }
        State::Recording => Ok(CoordinatorResponse::Ignored("already recording".into())),
        State::Processing => Ok(CoordinatorResponse::Busy),
    }
}

fn stop_recording(
    feedback: &dyn Feedback,
    inner: &mut CoordinatorStateInner,
    now: Instant,
) -> CoordinatorResponse {
    match inner.state {
        State::Recording => {
            if let Some(timing) = &inner.active_timing {
                timing.lock().unwrap().mark_stop_received();
            }
            if let Some(sender) = inner.signal_tx.take() {
                let _ = sender.send(RecordingSignal::Stop);
            }
            inner.state = State::Processing;
            inner.last_action = Some(now);
            feedback.send(FeedbackEvent::Processing, &inner.config);
            CoordinatorResponse::RecordingStopped
        }
        State::Processing => CoordinatorResponse::Busy,
        State::Idle => CoordinatorResponse::Ignored("not recording".into()),
    }
}

fn complete_activation_release(
    feedback: &dyn Feedback,
    inner: &mut CoordinatorStateInner,
    generation: u64,
    timing_at: Duration,
) {
    if matches!(inner.physical_press, Some(PhysicalPress::Detached)) {
        inner.physical_press = None;
        return;
    }
    if inner.generation != generation {
        return;
    }
    match inner.physical_press.clone() {
        Some(PhysicalPress::Active(crate::config::ActivationMode::Hold))
            if inner.state == State::Recording =>
        {
            inner.physical_press = None;
            if let Some(timing) = &inner.active_timing {
                timing.lock().unwrap().mark_stop_received_at(timing_at);
            }
            stop_recording(feedback, inner, Instant::now());
        }
        _ => inner.physical_press = None,
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
        operation_error: inner.last_error.clone(),
        shortcut_status: inner.shortcut_status.clone(),
        activation_error: inner.runtime_error.clone(),
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
        operation_error: Option<String>,
        shortcut_status: crate::ipc::ShortcutStatus,
        activation_error: Option<String>,
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

    #[test]
    fn engine_lifecycle_honors_unload_policies() {
        let mut lifecycle = EngineLifecycle::default();
        let mut loads = 0;

        lifecycle
            .ensure("model", || {
                loads += 1;
                Ok::<_, anyhow::Error>(())
            })
            .unwrap();
        assert!(lifecycle
            .apply_policy(&crate::config::IdleUnloadPolicy::Never, 15, true)
            .is_none());
        lifecycle
            .ensure("model", || {
                loads += 1;
                Ok::<_, anyhow::Error>(())
            })
            .unwrap();
        assert_eq!(loads, 1);

        lifecycle.apply_policy(
            &crate::config::IdleUnloadPolicy::AfterTranscription,
            15,
            true,
        );
        lifecycle
            .ensure("model", || {
                loads += 1;
                Ok::<_, anyhow::Error>(())
            })
            .unwrap();
        assert_eq!(loads, 2);

        let idle = lifecycle
            .apply_policy(&crate::config::IdleUnloadPolicy::AfterIdle, 15, true)
            .unwrap();
        lifecycle.unload_if_idle(idle.0);
        assert!(!lifecycle.is_loaded());
    }

    #[test]
    fn stale_idle_timer_cannot_unload_reused_engine() {
        let mut lifecycle = EngineLifecycle::default();
        lifecycle
            .ensure("model", || Ok::<_, anyhow::Error>(()))
            .unwrap();
        let stale = lifecycle
            .apply_policy(&crate::config::IdleUnloadPolicy::AfterIdle, 15, true)
            .unwrap();
        lifecycle
            .ensure("model", || Ok::<_, anyhow::Error>(()))
            .unwrap();
        lifecycle.unload_if_idle(stale.0);
        assert!(lifecycle.is_loaded());
    }

    #[test]
    fn idle_unload_timer_resets_to_latest_deadline() {
        let lifecycle = Arc::new(Mutex::new(EngineLifecycle::default()));
        let timer = IdleUnloadTimer::new(Arc::clone(&lifecycle)).unwrap();
        let first_generation = {
            let mut lifecycle = lifecycle.lock().unwrap();
            lifecycle
                .ensure("model", || Ok::<_, anyhow::Error>(()))
                .unwrap();
            lifecycle.generation
        };
        timer
            .schedule(first_generation, Duration::from_secs(60))
            .unwrap();
        let second_generation = {
            let mut lifecycle = lifecycle.lock().unwrap();
            lifecycle
                .ensure("model", || Ok::<_, anyhow::Error>(()))
                .unwrap();
            lifecycle.generation
        };
        timer
            .schedule(second_generation, Duration::from_secs(60))
            .unwrap();
        timer.expire();
        assert!(!lifecycle.lock().unwrap().is_loaded());
    }

    #[test]
    fn recording_suspends_idle_unloading_until_recording_finishes() {
        let lifecycle = Arc::new(Mutex::new(EngineLifecycle::default()));
        let timer = IdleUnloadTimer::new(Arc::clone(&lifecycle)).unwrap();
        let scheduled = {
            let mut lifecycle = lifecycle.lock().unwrap();
            lifecycle
                .ensure("model", || Ok::<_, anyhow::Error>(()))
                .unwrap();
            lifecycle.apply_policy(&crate::config::IdleUnloadPolicy::AfterIdle, 15, true)
        }
        .unwrap();
        timer.schedule(scheduled.0, scheduled.1).unwrap();

        lifecycle.lock().unwrap().suspend_idle_unload();
        timer.cancel().unwrap();
        timer.expire();
        assert!(lifecycle.lock().unwrap().is_loaded());

        let resumed = lifecycle
            .lock()
            .unwrap()
            .apply_policy(&crate::config::IdleUnloadPolicy::AfterIdle, 15, false)
            .unwrap();
        timer.schedule(resumed.0, resumed.1).unwrap();
        timer.expire();
        assert!(!lifecycle.lock().unwrap().is_loaded());
    }

    #[test]
    fn policy_reload_cancels_or_schedules_idle_unloading() {
        let lifecycle = Arc::new(Mutex::new(EngineLifecycle::default()));
        let timer = IdleUnloadTimer::new(Arc::clone(&lifecycle)).unwrap();
        let scheduled = {
            let mut lifecycle = lifecycle.lock().unwrap();
            lifecycle
                .ensure("model", || Ok::<_, anyhow::Error>(()))
                .unwrap();
            lifecycle.apply_policy(&crate::config::IdleUnloadPolicy::AfterIdle, 15, true)
        }
        .unwrap();
        timer.schedule(scheduled.0, scheduled.1).unwrap();

        assert!(lifecycle
            .lock()
            .unwrap()
            .apply_policy(&crate::config::IdleUnloadPolicy::Never, 15, false)
            .is_none());
        timer.cancel().unwrap();
        timer.expire();
        assert!(lifecycle.lock().unwrap().is_loaded());

        let rescheduled = lifecycle
            .lock()
            .unwrap()
            .apply_policy(&crate::config::IdleUnloadPolicy::AfterIdle, 15, false)
            .unwrap();
        timer.schedule(rescheduled.0, rescheduled.1).unwrap();
        timer.expire();
        assert!(!lifecycle.lock().unwrap().is_loaded());
    }
}
