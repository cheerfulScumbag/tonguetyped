use crate::config::Config;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
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

struct CoordinatorStateInner {
    state: State,
    recording_start: Option<Instant>,
    config: Config,
    last_action: Option<Instant>,
    release_time: Option<Instant>,
}

pub struct Coordinator {
    state: Arc<Mutex<CoordinatorStateInner>>,
    cancel_flag: Arc<AtomicBool>,
    generation: Arc<AtomicU64>,
    output_lock: Arc<Mutex<()>>,
    last_result: Arc<Mutex<Option<(String, u64)>>>,
    audio_sender: Arc<Mutex<Option<tokio::sync::mpsc::Sender<()>>>>,
}

impl Coordinator {
    pub fn new(config: Config) -> Self {
        Coordinator {
            state: Arc::new(Mutex::new(CoordinatorStateInner {
                state: State::Idle,
                recording_start: None,
                config,
                last_action: None,
                release_time: None,
            })),
            cancel_flag: Arc::new(AtomicBool::new(false)),
            generation: Arc::new(AtomicU64::new(0)),
            output_lock: Arc::new(Mutex::new(())),
            last_result: Arc::new(Mutex::new(None)),
            audio_sender: Arc::new(Mutex::new(None)),
        }
    }

    pub async fn handle_command(
        &self,
        cmd: CoordinatorCommand,
    ) -> anyhow::Result<CoordinatorResponse> {
        let need_start;
        let need_toggle;
        let need_hold_press;
        let response;
        let config;
        let mic_name;

        {
            let mut inner = self.state.lock().unwrap();
            need_start = matches!(cmd, CoordinatorCommand::Start);
            need_toggle = matches!(cmd, CoordinatorCommand::Toggle);
            need_hold_press = matches!(cmd, CoordinatorCommand::HoldPress);
            config = inner.config.clone();
            mic_name = inner.config.audio.microphone.clone();

            response = match &cmd {
                CoordinatorCommand::Start => {
                    if !self.check_debounce(&inner) {
                        Some(CoordinatorResponse::Ignored("debounce".into()))
                    } else {
                        match inner.state {
                            State::Processing => Some(CoordinatorResponse::Busy),
                            State::Recording => {
                                Some(CoordinatorResponse::Ignored("already recording".into()))
                            }
                            State::Idle => None,
                        }
                    }
                }
                CoordinatorCommand::Stop => {
                    match inner.state {
                        State::Recording => {
                            self.stop_recording(&mut inner);
                            Some(CoordinatorResponse::RecordingStopped)
                        }
                        State::Processing => Some(CoordinatorResponse::Busy),
                        State::Idle => Some(CoordinatorResponse::Ignored("not recording".into())),
                    }
                }
                CoordinatorCommand::Toggle => {
                    if !self.check_debounce(&inner) {
                        Some(CoordinatorResponse::Ignored("debounce".into()))
                    } else {
                        match inner.state {
                            State::Processing => Some(CoordinatorResponse::Busy),
                            State::Recording => {
                                self.stop_recording(&mut inner);
                                Some(CoordinatorResponse::RecordingStopped)
                            }
                            State::Idle => None,
                        }
                    }
                }
                CoordinatorCommand::Cancel => {
                    self.cancel_flag.store(true, Ordering::SeqCst);
                    match inner.state {
                        State::Recording => {
                            self.reset_recording(&mut inner);
                            Some(CoordinatorResponse::Cancelled)
                        }
                        State::Processing => Some(CoordinatorResponse::Cancelled),
                        State::Idle => Some(CoordinatorResponse::Ignored("idle".into())),
                    }
                }
                CoordinatorCommand::HoldPress => {
                    if !self.check_debounce(&inner) {
                        Some(CoordinatorResponse::Ignored("debounce".into()))
                    } else {
                        match inner.state {
                            State::Processing => Some(CoordinatorResponse::Busy),
                            State::Recording => {
                                Some(CoordinatorResponse::Ignored("already recording".into()))
                            }
                            State::Idle => None,
                        }
                    }
                }
                CoordinatorCommand::HoldRelease => {
                    let now = Instant::now();
                    if let Some(rel_time) = inner.release_time {
                        if now.duration_since(rel_time) < Duration::from_millis(50) {
                            return Ok(CoordinatorResponse::Ignored(
                                "auto-repeat defense".into(),
                            ));
                        }
                    }
                    inner.release_time = Some(now);
                    match inner.state {
                        State::Recording => {
                            self.stop_recording(&mut inner);
                            Some(CoordinatorResponse::RecordingStopped)
                        }
                        State::Processing => Some(CoordinatorResponse::Busy),
                        State::Idle => {
                            Some(CoordinatorResponse::Ignored("not recording".into()))
                        }
                    }
                }
                CoordinatorCommand::GetStatus => Some(self.build_status(&inner)),
            };
        }

        if let Some(resp) = response {
            return Ok(resp);
        }

        if need_start || need_toggle || need_hold_press {
            self.start_recording_async(&mic_name, &config).await?;
            return Ok(CoordinatorResponse::RecordingStarted);
        }

        Ok(CoordinatorResponse::Ok)
    }

    fn check_debounce(&self, inner: &CoordinatorStateInner) -> bool {
        if let Some(last) = inner.last_action {
            Instant::now().duration_since(last) >= Duration::from_millis(30)
        } else {
            true
        }
    }

    async fn start_recording_async(&self, mic_name: &str, config: &Config) -> anyhow::Result<()> {
        self.cancel_flag.store(false, Ordering::SeqCst);
        let gen = self.generation.fetch_add(1, Ordering::SeqCst) + 1;

        let (tx, mut rx) = tokio::sync::mpsc::channel::<()>(1);
        *self.audio_sender.lock().unwrap() = Some(tx);

        {
            let mut inner = self.state.lock().unwrap();
            inner.state = State::Recording;
            inner.recording_start = Some(Instant::now());
            inner.last_action = Some(Instant::now());
        }

        let cancel = self.cancel_flag.clone();
        let gen_flag = self.generation.clone();
        let output_lock = self.output_lock.clone();
        let last_result = self.last_result.clone();
        let config = config.clone();
        let mic = mic_name.to_string();

        tokio::task::spawn_blocking(move || {
            let recorder = crate::audio::AudioRecorder::new(&mic, 16000, None);
            let mut recorder = match recorder {
                Ok(r) => r,
                Err(e) => {
                    tracing::error!("failed to create audio recorder: {}", e);
                    return;
                }
            };

            if let Err(e) = recorder.start() {
                tracing::error!("failed to start recording: {}", e);
                return;
            }

            let _ = rx.blocking_recv();

            recorder.stop();
            let samples = recorder.take_buffer();

            if cancel.load(Ordering::SeqCst) {
                tracing::debug!("cancelled before inference started");
                return;
            }

            let transcript = Self::run_inference_sync(&samples, &config);

            if cancel.load(Ordering::SeqCst) {
                tracing::debug!("cancelled after inference, discarding result");
                return;
            }

            if gen != gen_flag.load(Ordering::SeqCst) {
                tracing::debug!("stale generation {}, discarding result", gen);
                return;
            }

            let _output_guard = output_lock.lock().unwrap();

            if gen != gen_flag.load(Ordering::SeqCst) {
                return;
            }

            if let Ok(ref text) = transcript {
                let timestamp = std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .unwrap()
                    .as_secs();
                let trimmed = text.trim().to_string();
                if !trimmed.is_empty() {
                    let output_text = if config.transcription.trailing_space {
                        format!("{} ", trimmed)
                    } else {
                        trimmed.clone()
                    };
                    if let Err(e) = crate::output::output_text(
                        &output_text,
                        &config.output.method,
                        &config.output.typing_backend,
                    ) {
                        tracing::error!("output failed: {}", e);
                    }
                }
                *last_result.lock().unwrap() = Some((trimmed, timestamp));
            }
        });

        Ok(())
    }

    fn stop_recording(&self, inner: &mut CoordinatorStateInner) {
        if let Some(sender) = self.audio_sender.lock().unwrap().take() {
            let _ = sender.try_send(());
        }
        inner.state = State::Processing;
        inner.recording_start = None;
    }

    fn reset_recording(&self, inner: &mut CoordinatorStateInner) {
        if let Some(sender) = self.audio_sender.lock().unwrap().take() {
            let _ = sender.try_send(());
        }
        inner.state = State::Idle;
        inner.recording_start = None;
    }

    fn run_inference_sync(samples: &[f32], config: &Config) -> anyhow::Result<String> {
        let mut engine = crate::inference::InferenceEngine::new(
            crate::model::ModelCatalog::model_path(&config.model.selected),
        );
        engine.load()?;
        let result = engine.transcribe(samples)?;
        engine.unload();
        Ok(result)
    }

    fn build_status(&self, inner: &CoordinatorStateInner) -> CoordinatorResponse {
        CoordinatorResponse::Status {
            state: match inner.state {
                State::Idle => "idle",
                State::Recording => "recording",
                State::Processing => "processing",
            }
            .to_string(),
            recording: matches!(inner.state, State::Recording),
            processing: matches!(inner.state, State::Processing),
            activation_mode: inner.config.activation.mode.to_string(),
        }
    }

    pub fn get_last_result(&self) -> Option<(String, u64)> {
        self.last_result.lock().unwrap().clone()
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
        recording: bool,
        processing: bool,
        activation_mode: String,
    },
}

#[cfg(test)]
mod tests {
    use super::*;

    fn test_config() -> Config {
        Config::default()
    }

    #[tokio::test]
    async fn test_coordinator_initial_state() {
        let coord = Coordinator::new(test_config());
        let resp = coord
            .handle_command(CoordinatorCommand::GetStatus)
            .await
            .unwrap();
        if let CoordinatorResponse::Status {
            state,
            recording,
            processing,
            ..
        } = resp
        {
            assert_eq!(state, "idle");
            assert!(!recording);
            assert!(!processing);
        } else {
            panic!("expected status response");
        }
    }

    #[tokio::test]
    async fn test_cancel_in_idle() {
        let coord = Coordinator::new(test_config());
        let resp = coord
            .handle_command(CoordinatorCommand::Cancel)
            .await
            .unwrap();
        assert!(matches!(resp, CoordinatorResponse::Ignored(_)));
    }

    #[tokio::test]
    async fn test_debounce() {
        let coord = Coordinator::new(test_config());
        let _ = coord
            .handle_command(CoordinatorCommand::Toggle)
            .await
            .unwrap();
        std::thread::sleep(Duration::from_millis(5));
        let resp = coord
            .handle_command(CoordinatorCommand::Toggle)
            .await
            .unwrap();
        assert!(matches!(resp, CoordinatorResponse::Ignored(_)));
    }

    #[tokio::test]
    async fn test_hold_autorepeat_defense() {
        let coord = Coordinator::new(test_config());
        let _ = coord
            .handle_command(CoordinatorCommand::HoldPress)
            .await
            .unwrap();
        let _ = coord
            .handle_command(CoordinatorCommand::HoldRelease)
            .await
            .unwrap();
        let resp = coord
            .handle_command(CoordinatorCommand::HoldRelease)
            .await
            .unwrap();
        assert!(matches!(resp, CoordinatorResponse::Ignored(_)));
    }
}