use std::process::Command;

struct Env {
    root: std::path::PathBuf,
    config_home: std::path::PathBuf,
    data_home: std::path::PathBuf,
    runtime_dir: std::path::PathBuf,
}

impl Env {
    fn new(tag: &str) -> Self {
        let root = std::env::temp_dir().join(format!(
            "tt-model-cli-{tag}-{}-{}",
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
        Env {
            root,
            config_home,
            data_home,
            runtime_dir,
        }
    }

    fn command(&self, args: &[&str]) -> Command {
        let mut command = Command::new(env!("CARGO_BIN_EXE_tonguetyped"));
        command
            .args(args)
            .env("XDG_CONFIG_HOME", &self.config_home)
            .env("XDG_DATA_HOME", &self.data_home)
            .env("XDG_RUNTIME_DIR", &self.runtime_dir)
            .env("DBUS_SESSION_BUS_ADDRESS", "unix:path=/nonexistent");
        command
    }

    fn models_dir(&self) -> std::path::PathBuf {
        self.data_home.join("tonguetyped/models")
    }

    fn config_toml(&self) -> String {
        std::fs::read_to_string(self.config_home.join("tonguetyped/config.toml")).unwrap()
    }

    /// Places an empty file where a catalog model would live once installed,
    /// without performing a real network download.
    fn fake_install(&self, id: &str) {
        let entry = tonguetyped::catalog::find(id).expect("known catalog id");
        std::fs::write(self.models_dir().join(entry.filename), []).unwrap();
    }
}

impl Drop for Env {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.root);
    }
}

#[test]
fn list_shows_catalog_entries_with_install_status() {
    let env = Env::new("list");
    env.fake_install("whisper-tiny-q5_k_m");

    let output = env.command(&["model", "list"]).output().unwrap();
    assert!(output.status.success());
    let stdout = String::from_utf8(output.stdout).unwrap();

    assert!(stdout.contains("whisper-tiny-q5_k_m"));
    let installed_line = stdout
        .lines()
        .find(|line| line.starts_with("whisper-tiny-q5_k_m"))
        .unwrap();
    assert!(installed_line.contains("installed"));
    let uninstalled_line = stdout
        .lines()
        .find(|line| line.starts_with("whisper-base-q5_k_m"))
        .unwrap();
    assert!(!uninstalled_line.contains("installed"));
    assert!(!uninstalled_line.contains("active"));
    // The default active model (not yet installed in this fresh config) is
    // marked accordingly rather than "active", since it isn't installed.
    let default_line = stdout
        .lines()
        .find(|line| line.starts_with(tonguetyped::catalog::DEFAULT_MODEL_ID))
        .unwrap();
    assert!(!default_line.contains("active"));
}

#[test]
fn use_requires_the_model_to_already_be_installed() {
    let env = Env::new("use-not-installed");

    let output = env
        .command(&["model", "use", "whisper-tiny-q5_k_m"])
        .output()
        .unwrap();
    assert!(!output.status.success());
    let stderr = String::from_utf8(output.stderr).unwrap();
    assert!(stderr.contains("not installed"));
    assert!(stderr.contains("tonguetyped model install whisper-tiny-q5_k_m"));
}

#[test]
fn use_rejects_an_unknown_catalog_id() {
    let env = Env::new("use-unknown");

    let output = env
        .command(&["model", "use", "not-a-real-model"])
        .output()
        .unwrap();
    assert!(!output.status.success());
    let stderr = String::from_utf8(output.stderr).unwrap();
    assert!(stderr.contains("unknown catalog model"));
}

#[test]
fn use_persists_the_selection_and_list_marks_it_active() {
    let env = Env::new("use-persists");
    env.fake_install("whisper-tiny-q5_k_m");

    let use_output = env
        .command(&["model", "use", "whisper-tiny-q5_k_m"])
        .output()
        .unwrap();
    assert!(use_output.status.success());
    assert!(env
        .config_toml()
        .contains("active_model = \"whisper-tiny-q5_k_m\""));

    let list_output = env.command(&["model", "list"]).output().unwrap();
    let stdout = String::from_utf8(list_output.stdout).unwrap();
    let active_line = stdout
        .lines()
        .find(|line| line.starts_with("whisper-tiny-q5_k_m"))
        .unwrap();
    assert!(active_line.contains("active"));
}

#[test]
fn remove_refuses_to_delete_the_active_model() {
    let env = Env::new("remove-active");
    env.fake_install("whisper-tiny-q5_k_m");
    assert!(env
        .command(&["model", "use", "whisper-tiny-q5_k_m"])
        .output()
        .unwrap()
        .status
        .success());

    let output = env
        .command(&["model", "remove", "whisper-tiny-q5_k_m"])
        .output()
        .unwrap();
    assert!(!output.status.success());
    let stderr = String::from_utf8(output.stderr).unwrap();
    assert!(stderr.contains("is the active model"));
    assert!(env.models_dir().join("whisper-tiny-Q5_K_M.gguf").exists());
}

#[test]
fn remove_deletes_an_installed_non_active_model() {
    let env = Env::new("remove-non-active");
    env.fake_install("whisper-tiny-q5_k_m");
    let path = env.models_dir().join("whisper-tiny-Q5_K_M.gguf");
    assert!(path.exists());

    let output = env
        .command(&["model", "remove", "whisper-tiny-q5_k_m"])
        .output()
        .unwrap();
    assert!(output.status.success());
    assert!(!path.exists());
}

#[test]
fn remove_reports_when_a_model_was_never_installed() {
    let env = Env::new("remove-missing");

    let output = env
        .command(&["model", "remove", "whisper-tiny-q5_k_m"])
        .output()
        .unwrap();
    assert!(output.status.success());
    let stdout = String::from_utf8(output.stdout).unwrap();
    assert!(stdout.contains("is not installed"));
}
