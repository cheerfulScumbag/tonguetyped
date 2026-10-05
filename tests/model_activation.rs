//! Exercises `tonguetyped::commands::activate_model` - the dashboard's model
//! screen activation flow (download/verify, save selection, reload a running
//! daemon, confirm the model actually loads) - directly, without a terminal.
//! The real-PTY coverage of this same flow through the dashboard UI lives in
//! `tests/tui_dashboard.rs`.
//!
//! These use a pre-seeded stub file (same trick `tests/daemon_startup.rs`
//! uses) so the download step hits the already-verified `AlreadyInstalled`
//! fast path - no multi-gigabyte network fetch in a test. Since the stub
//! isn't a real GGUF, the confirmation step is expected to fail to load it,
//! which is exactly what proves `succeeded()` correctly refuses to claim
//! activation when the model doesn't actually load.

use std::sync::Mutex;

// `Config::load`/`select_model` resolve `XDG_CONFIG_HOME`/`XDG_DATA_HOME` via
// `directories::BaseDirs`, process-wide env vars `cargo test`'s default
// parallel threads would otherwise race on - same guard shape as
// `src/config.rs`'s own `XDG_CONFIG_HOME_LOCK`.
static ENV_LOCK: Mutex<()> = Mutex::new(());

struct EnvSandbox {
    _guard: std::sync::MutexGuard<'static, ()>,
    root: std::path::PathBuf,
    previous_config_home: Option<std::ffi::OsString>,
    previous_data_home: Option<std::ffi::OsString>,
    previous_runtime_dir: Option<std::ffi::OsString>,
}

impl EnvSandbox {
    fn new(tag: &str) -> Self {
        let guard = ENV_LOCK
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let root = std::env::temp_dir().join(format!(
            "tt-model-activation-{tag}-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        let config_home = root.join("config");
        let data_home = root.join("data");
        let runtime_dir = root.join("runtime");
        std::fs::create_dir_all(config_home.join("tonguetyped")).unwrap();
        std::fs::create_dir_all(data_home.join("tonguetyped/models")).unwrap();
        std::fs::create_dir_all(&runtime_dir).unwrap();

        let previous_config_home = std::env::var_os("XDG_CONFIG_HOME");
        let previous_data_home = std::env::var_os("XDG_DATA_HOME");
        let previous_runtime_dir = std::env::var_os("XDG_RUNTIME_DIR");
        std::env::set_var("XDG_CONFIG_HOME", &config_home);
        std::env::set_var("XDG_DATA_HOME", &data_home);
        std::env::set_var("XDG_RUNTIME_DIR", &runtime_dir);

        Self {
            _guard: guard,
            root,
            previous_config_home,
            previous_data_home,
            previous_runtime_dir,
        }
    }

    fn install_fake_model_file(&self, id: &str) {
        let entry = tonguetyped::catalog::find(id).unwrap();
        std::fs::write(
            self.root
                .join("data/tonguetyped/models")
                .join(entry.filename),
            b"not a real gguf file",
        )
        .unwrap();
    }
}

impl Drop for EnvSandbox {
    fn drop(&mut self) {
        match &self.previous_config_home {
            Some(value) => std::env::set_var("XDG_CONFIG_HOME", value),
            None => std::env::remove_var("XDG_CONFIG_HOME"),
        }
        match &self.previous_data_home {
            Some(value) => std::env::set_var("XDG_DATA_HOME", value),
            None => std::env::remove_var("XDG_DATA_HOME"),
        }
        match &self.previous_runtime_dir {
            Some(value) => std::env::set_var("XDG_RUNTIME_DIR", value),
            None => std::env::remove_var("XDG_RUNTIME_DIR"),
        }
        let _ = std::fs::remove_dir_all(&self.root);
    }
}

#[tokio::test]
async fn activation_saves_selection_and_reports_unconfirmed_when_the_file_does_not_load() {
    let sandbox = EnvSandbox::new("invalid-file");
    let id = tonguetyped::catalog::DEFAULT_MODEL_ID;
    sandbox.install_fake_model_file(id);

    let activation = tonguetyped::commands::activate_model(id, None)
        .await
        .expect("activate_model should not itself error for a known catalog id");

    assert_eq!(
        activation.download,
        tonguetyped::model::DownloadOutcome::AlreadyInstalled
    );
    // No daemon is running in this sandbox.
    assert!(activation.daemon_reload.is_none());
    // The config selection itself is saved regardless of whether the model
    // actually loads - `Config::load()` reflects it immediately.
    let saved = tonguetyped::config::Config::load().unwrap();
    assert_eq!(saved.model.active_model, id);
    // But the confirmation step must catch that the stub isn't a real GGUF,
    // so the overall action must not claim success.
    assert!(!activation.confirmation.model_ready);
    assert!(activation.confirmation.model_error.is_some());
    assert!(!activation.succeeded());
}

#[tokio::test]
async fn activation_rejects_an_unknown_catalog_id_without_touching_the_filesystem() {
    let _sandbox = EnvSandbox::new("unknown-id");
    let result = tonguetyped::commands::activate_model("not-a-real-model-id", None).await;
    assert!(result.is_err());
}
