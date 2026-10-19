//! Deterministic coverage for the persistence gate: a completed dictation only
//! writes to the history database and the transcript folder when saving
//! transcripts is enabled. The microphone and speech model are stubbed because
//! a headless test has neither; the coordinator's real persistence branch,
//! `HistoryStore`, and `history::export_transcript` all run for real.
//!
//! The off and on cases live in separate tests so each side fails on its own,
//! and both serialize on `XDG_DATA_HOME` because that environment variable is
//! process-global.

use std::path::{Path, PathBuf};
use std::sync::{mpsc, Arc, Mutex, MutexGuard};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use tonguetyped::config::Config;
use tonguetyped::coordinator::{
    Coordinator, CoordinatorRuntime, RecordingAttempt, RecordingSignal, TranscriptionAttempt,
    TranscriptionTimings,
};
use tonguetyped::daemon::dispatch;
use tonguetyped::ipc::{Request, Response};
use tonguetyped::latency::Clock;

static XDG_DATA_HOME_LOCK: Mutex<()> = Mutex::new(());

struct StubRuntime {
    transcript: String,
}

impl CoordinatorRuntime for StubRuntime {
    fn record(
        &self,
        _microphone: &str,
        max_duration: Duration,
        signal_rx: mpsc::Receiver<RecordingSignal>,
        started: tokio::sync::oneshot::Sender<Result<(), String>>,
        _clock: Arc<dyn Clock>,
    ) -> RecordingAttempt {
        let _ = started.send(Ok(()));
        match signal_rx.recv_timeout(max_duration) {
            Ok(RecordingSignal::Stop) => ok_attempt(),
            Ok(RecordingSignal::Cancel) => RecordingAttempt {
                result: Err(anyhow::anyhow!("cancelled")),
                automatic_stop_at: None,
                streamed: None,
            },
            Err(_) => ok_attempt(),
        }
    }

    fn transcribe(&self, _samples: &[f32], _config: &Config) -> TranscriptionAttempt {
        TranscriptionAttempt {
            result: Ok(self.transcript.clone()),
            truncated: false,
            timings: TranscriptionTimings::default(),
        }
    }

    fn output(
        &self,
        _text: &str,
        _config: &Config,
    ) -> anyhow::Result<tonguetyped::output::Delivery> {
        Ok(tonguetyped::output::Delivery::Inserted)
    }
}

fn ok_attempt() -> RecordingAttempt {
    RecordingAttempt {
        result: Ok(vec![0.1; 512]),
        automatic_stop_at: None,
        streamed: None,
    }
}

fn unique_dir(tag: &str) -> PathBuf {
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    let path = std::env::temp_dir().join(format!(
        "tonguetyped-history-persistence-{tag}-{}-{nanos}",
        std::process::id()
    ));
    std::fs::create_dir_all(&path).unwrap();
    path
}

/// Points the coordinator's data directory at an isolated root for as long as
/// the returned guard lives; no other test in this binary may run meanwhile.
fn isolate_data_dir(root: &Path) -> MutexGuard<'static, ()> {
    let guard = XDG_DATA_HOME_LOCK.lock().unwrap();
    std::env::set_var("XDG_DATA_HOME", root);
    guard
}

async fn wait_for_idle(coordinator: &Arc<Coordinator>) {
    for _ in 0..300 {
        if let Response::Status { state, .. } = dispatch(coordinator, Request::Status).await {
            if state == "idle" {
                return;
            }
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    panic!("coordinator did not return to idle");
}

/// Drives one full dictation (Start -> Stop) through the real coordinator.
async fn dictate(coordinator: &Arc<Coordinator>) {
    // The coordinator debounces Start/Stop by 30ms.
    tokio::time::sleep(Duration::from_millis(35)).await;
    assert!(matches!(
        dispatch(coordinator, Request::Start).await,
        Response::RecordingStarted
    ));
    tokio::time::sleep(Duration::from_millis(35)).await;
    assert!(matches!(
        dispatch(coordinator, Request::Stop).await,
        Response::RecordingStopped
    ));
    wait_for_idle(coordinator).await;
}

fn history_row_texts(db: &Path) -> Vec<String> {
    let conn = rusqlite::Connection::open(db).unwrap();
    let mut stmt = conn
        .prepare("SELECT transcript_text FROM history ORDER BY timestamp ASC")
        .unwrap();
    let rows = stmt
        .query_map([], |row| row.get::<_, String>(0))
        .unwrap()
        .map(Result::unwrap)
        .collect();
    rows
}

fn exported_files(folder: &Path) -> Vec<PathBuf> {
    if !folder.exists() {
        return Vec::new();
    }
    let mut files: Vec<PathBuf> = std::fs::read_dir(folder)
        .unwrap()
        .filter_map(Result::ok)
        .map(|entry| entry.path())
        .filter(|path| path.extension().map(|e| e == "txt").unwrap_or(false))
        .collect();
    files.sort();
    files
}

fn coordinator(config: Config, transcript: &str) -> Arc<Coordinator> {
    Arc::new(
        Coordinator::with_runtime(
            config,
            Arc::new(StubRuntime {
                transcript: transcript.to_string(),
            }),
        )
        .unwrap(),
    )
}

fn base_config(folder: &Path) -> Config {
    let mut config = Config::default();
    config.history.transcript_folder = folder.to_string_lossy().into_owned();
    config.transcription.max_recording_seconds = 5;
    config
}

/// With saving off (the fresh default), a completed dictation writes neither a
/// history row nor a transcript file, even when a folder is configured.
#[tokio::test]
async fn completed_dictation_writes_nothing_when_history_is_off() {
    let data_root = unique_dir("off-data");
    let folder = unique_dir("off-root").join("nested/dictations");
    let _guard = isolate_data_dir(&data_root);

    let config = base_config(&folder);
    assert!(
        !config.history.enabled,
        "a fresh config must not save transcripts by default"
    );
    let coordinator = coordinator(config, "off transcript");

    dictate(&coordinator).await;

    let db = tonguetyped::history::history_db_path().unwrap();
    assert!(
        !db.exists(),
        "history off must not create the history database: {db:?}"
    );
    assert!(
        !folder.exists(),
        "history off must not create the transcript folder: {folder:?}"
    );
    assert!(
        exported_files(&folder).is_empty(),
        "history off must not write transcript files: {:?}",
        exported_files(&folder)
    );

    let data_parent = folder.parent().unwrap().parent().unwrap();
    let _ = std::fs::remove_dir_all(&data_root);
    let _ = std::fs::remove_dir_all(data_parent);
}

/// With saving on, the same completed dictation writes both the history row and
/// the transcript file.
#[tokio::test]
async fn completed_dictation_writes_both_when_history_is_on() {
    let data_root = unique_dir("on-data");
    let folder = unique_dir("on-root").join("nested/dictations");
    let _guard = isolate_data_dir(&data_root);

    let mut config = base_config(&folder);
    config.history.enabled = true;
    let coordinator = coordinator(config, "on transcript");

    dictate(&coordinator).await;

    let db = tonguetyped::history::history_db_path().unwrap();
    assert_eq!(
        history_row_texts(&db),
        vec!["on transcript".to_string()],
        "history on must write the transcript row"
    );
    let files = exported_files(&folder);
    assert_eq!(
        files.len(),
        1,
        "history on must write exactly one transcript file: {files:?}"
    );
    assert_eq!(
        std::fs::read_to_string(&files[0]).unwrap(),
        "on transcript\n",
        "the transcript file must hold the dictated text"
    );

    let data_parent = folder.parent().unwrap().parent().unwrap();
    let _ = std::fs::remove_dir_all(&data_root);
    let _ = std::fs::remove_dir_all(data_parent);
}
