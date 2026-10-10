//! End-to-end coverage for transcript persistence as an end user experiences
//! it: driving the real `Coordinator` through `dispatch` (Start/Stop) with a
//! stub hardware/inference runtime, then inspecting the *actual* history
//! SQLite database and the *actual* plain-text files the coordinator wrote
//! into the configured transcript folder.
//!
//! The runtime stub only replaces the microphone and the speech model - the
//! two things a headless test cannot provide - while the code under test
//! (the coordinator's persistence branch, `HistoryStore::prune`, and
//! `history::export_transcript`) is the real product code. `XDG_DATA_HOME` is
//! pointed at a throwaway directory so the operator's real history database
//! and configured folders are never touched.

use std::path::{Path, PathBuf};
use std::sync::{mpsc, Arc};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use tonguetyped::config::Config;
use tonguetyped::coordinator::{
    Coordinator, CoordinatorRuntime, RecordingAttempt, RecordingSignal, TranscriptionAttempt,
    TranscriptionTimings,
};
use tonguetyped::daemon::dispatch;
use tonguetyped::ipc::{Request, Response};
use tonguetyped::latency::Clock;

/// The microphone + model stub. Blocks on `record` until the coordinator sends
/// Stop, then returns fixed audio; `transcribe` always returns a transcript
/// unique to this runtime so a written file/row can be attributed.
struct StubRuntime {
    transcript: String,
    outputs: std::sync::Mutex<Vec<String>>,
}

impl StubRuntime {
    fn new(transcript: &str) -> Self {
        Self {
            transcript: transcript.to_string(),
            outputs: std::sync::Mutex::new(Vec::new()),
        }
    }

    fn outputs(&self) -> Vec<String> {
        self.outputs.lock().unwrap().clone()
    }
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
            Ok(RecordingSignal::Stop) => RecordingAttempt {
                result: Ok(vec![0.1; 512]),
                automatic_stop_at: None,
            },
            Ok(RecordingSignal::Cancel) => RecordingAttempt {
                result: Err(anyhow::anyhow!("cancelled")),
                automatic_stop_at: None,
            },
            Err(_) => RecordingAttempt {
                result: Ok(vec![0.1; 512]),
                automatic_stop_at: None,
            },
        }
    }

    fn transcribe(&self, _samples: &[f32], _config: &Config) -> TranscriptionAttempt {
        TranscriptionAttempt {
            result: Ok(self.transcript.clone()),
            timings: TranscriptionTimings::default(),
        }
    }

    fn output(&self, text: &str, _config: &Config) -> anyhow::Result<()> {
        self.outputs.lock().unwrap().push(text.to_string());
        Ok(())
    }
}

fn unique_dir(tag: &str) -> PathBuf {
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    let path = std::env::temp_dir().join(format!(
        "tonguetyped-history-e2e-{tag}-{}-{nanos}",
        std::process::id()
    ));
    std::fs::create_dir_all(&path).unwrap();
    path
}

/// Points the process's data directory at an isolated throwaway root *before*
/// the coordinator computes `history_db_path()`.
fn isolate_data_dir(root: &Path) {
    std::env::set_var("XDG_DATA_HOME", root);
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

/// Drives one full dictation (Start -> Stop) and waits for it to finish.
async fn dictate(coordinator: &Arc<Coordinator>) {
    // The coordinator debounces Start/Stop by 30ms; wait past it before
    // starting so the command is not ignored.
    tokio::time::sleep(Duration::from_millis(35)).await;
    let start = dispatch(coordinator, Request::Start).await;
    assert!(
        matches!(start, Response::RecordingStarted),
        "start returned {start:?}"
    );
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

/// The whole retention + export surface, exercised through the real
/// coordinator against real SQLite and real files, in one test so the
/// process-global `XDG_DATA_HOME` never races a parallel test.
#[tokio::test]
async fn coordinator_applies_retention_and_exports_transcripts_to_a_folder() {
    // Scenario A: a plain DB write with the entry cap applied on every write.
    // max_entries = 1 means three dictations must leave exactly the newest row.
    let data_root = unique_dir("cap");
    isolate_data_dir(&data_root);

    let runtime = Arc::new(StubRuntime::new("first transcript"));
    let mut config = Config::default();
    config.history.enabled = true;
    config.history.max_entries = 1;
    config.history.max_age_days = 0;
    config.transcription.max_recording_seconds = 5;
    let coordinator = Arc::new(Coordinator::with_runtime(config.clone(), runtime.clone()).unwrap());

    dictate(&coordinator).await;
    dictate(&coordinator).await;
    dictate(&coordinator).await;

    let db = tonguetyped::history::history_db_path().unwrap();
    let rows = history_row_texts(&db);
    assert_eq!(
        rows,
        vec!["first transcript".to_string()],
        "the entry cap must keep only the newest row"
    );

    // Scenario B: an age cap prunes a row that predates the cutoff, while the
    // freshly dictated row survives.
    let old_cutoff = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_secs() as i64
        - 90 * 86_400;
    let conn = rusqlite::Connection::open(&db).unwrap();
    conn.execute(
        "INSERT INTO history (timestamp, transcript_text, recording_path, language)
         VALUES (?1, 'stale transcript', NULL, 'auto')",
        [old_cutoff],
    )
    .unwrap();
    drop(conn);
    assert_eq!(history_row_texts(&db).len(), 2);

    let runtime = Arc::new(StubRuntime::new("fresh transcript"));
    let mut config = Config::default();
    config.history.enabled = true;
    config.history.max_entries = 0;
    config.history.max_age_days = 30;
    config.transcription.max_recording_seconds = 5;
    let coordinator = Arc::new(Coordinator::with_runtime(config, runtime).unwrap());
    dictate(&coordinator).await;

    let rows = history_row_texts(&db);
    assert!(
        !rows.iter().any(|row| row == "stale transcript"),
        "the 90-day-old row must be pruned by the 30-day age limit: {rows:?}"
    );
    assert!(
        rows.iter().any(|row| row == "fresh transcript"),
        "the just-dictated row must remain: {rows:?}"
    );

    // Scenario C: the folder export is independent of the history database.
    // With history disabled but a folder set, each dictation still lands as a
    // file and no row is written; the files accumulate one-per-dictation and
    // are never overwritten or pruned. The folder does not exist yet, as when
    // a user types a fresh path, and the product must create it.
    let folder = unique_dir("export-root").join("nested/dictations");
    let export_data_root = unique_dir("export-data");
    isolate_data_dir(&export_data_root);
    let runtime = Arc::new(StubRuntime::new("keep this text"));
    let mut config = Config::default();
    config.history.enabled = false;
    config.history.transcript_folder = folder.to_string_lossy().into_owned();
    config.transcription.max_recording_seconds = 5;
    let coordinator = Arc::new(Coordinator::with_runtime(config, runtime).unwrap());

    dictate(&coordinator).await;
    dictate(&coordinator).await;

    let files = exported_files(&folder);
    assert_eq!(
        files.len(),
        2,
        "one file per dictation, with no overwrite: {files:?}"
    );
    for file in &files {
        assert_eq!(std::fs::read_to_string(file).unwrap(), "keep this text\n");
        assert!(
            file.file_name()
                .unwrap()
                .to_str()
                .unwrap()
                .ends_with(".txt"),
            "exported filenames are date-stamped .txt: {file:?}"
        );
    }
    let export_db = tonguetyped::history::history_db_path().unwrap();
    assert!(
        !export_db.exists(),
        "history disabled must not create the database when only the folder is set"
    );

    // Scenario D: the exported files outlive the database retention. With both
    // the database and the folder on, the entry cap prunes the database to one
    // row while every dictation's file is still on disk, proving the folder is
    // written once and never pruned or monitored again.
    let both_folder = unique_dir("both-root").join("transcripts");
    let both_data_root = unique_dir("both-data");
    isolate_data_dir(&both_data_root);
    let runtime = Arc::new(StubRuntime::new("retained text"));
    let mut config = Config::default();
    config.history.enabled = true;
    config.history.max_entries = 1;
    config.history.max_age_days = 0;
    config.history.transcript_folder = both_folder.to_string_lossy().into_owned();
    config.transcription.max_recording_seconds = 5;
    let coordinator = Arc::new(Coordinator::with_runtime(config, runtime).unwrap());

    for _ in 0..3 {
        dictate(&coordinator).await;
    }

    let both_db = tonguetyped::history::history_db_path().unwrap();
    assert_eq!(
        history_row_texts(&both_db).len(),
        1,
        "the entry cap must have pruned the database to a single row"
    );
    assert_eq!(
        exported_files(&both_folder).len(),
        3,
        "all three files must survive the database retention"
    );

    // Scenario E: an unusable folder must not swallow the dictation. Pointing
    // the folder at a path whose parent is a regular file makes the export
    // fail deterministically; the transcript must still be delivered to the
    // focused application and reported as the last result.
    let blocker_root = unique_dir("blocker-root");
    let blocker = blocker_root.join("not-a-directory");
    std::fs::write(&blocker, b"file, not a folder").unwrap();
    let broken_folder = blocker.join("sub");
    let broken_data_root = unique_dir("broken-data");
    isolate_data_dir(&broken_data_root);
    let runtime = Arc::new(StubRuntime::new("still delivered"));
    let mut config = Config::default();
    config.history.enabled = false;
    config.history.transcript_folder = broken_folder.to_string_lossy().into_owned();
    config.transcription.max_recording_seconds = 5;
    let coordinator = Arc::new(Coordinator::with_runtime(config, runtime.clone()).unwrap());
    dictate(&coordinator).await;

    assert_eq!(
        runtime
            .outputs()
            .iter()
            .map(|o| o.trim())
            .collect::<Vec<_>>(),
        vec!["still delivered"],
        "the transcript must still reach the output even when the export fails"
    );
    assert!(
        matches!(
            dispatch(&coordinator, Request::GetLastResult).await,
            Response::LastResult { ref text, .. } if text == "still delivered"
        ),
        "the failed export must not lose the transcript"
    );

    let _ = std::fs::remove_dir_all(&data_root);
    let _ = std::fs::remove_dir_all(&export_data_root);
    let _ = std::fs::remove_dir_all(folder.parent().unwrap().parent().unwrap());
    let _ = std::fs::remove_dir_all(&both_data_root);
    let _ = std::fs::remove_dir_all(both_folder.parent().unwrap());
    let _ = std::fs::remove_dir_all(&broken_data_root);
    let _ = std::fs::remove_dir_all(&blocker_root);
}
