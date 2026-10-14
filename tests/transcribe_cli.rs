//! Regression coverage for `tonguetyped transcribe <file>`: the subcommand
//! reads a WAV fixture and drives the shared inference path. There is no
//! speech model in CI, so these assert the file is decoded and the command
//! reaches the model-loading step with a clear, actionable message rather
//! than failing on the audio or the arguments.

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
            "tt-transcribe-cli-{tag}-{}-{}",
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

    /// Writes a config with VAD off, so the subcommand reaches the model load
    /// with no Silero model present rather than stopping at VAD.
    fn write_config(&self, body: &str) {
        std::fs::write(self.config_home.join("tonguetyped/config.toml"), body).unwrap();
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

    fn fixture_wav(&self, label: &str) -> std::path::PathBuf {
        let path = self.root.join(format!("{label}.wav"));
        let spec = hound::WavSpec {
            channels: 1,
            sample_rate: 16_000,
            bits_per_sample: 16,
            sample_format: hound::SampleFormat::Int,
        };
        let mut writer = hound::WavWriter::create(&path, spec).unwrap();
        // Half a second of a quiet tone, enough to exercise decode + resample.
        for i in 0..8_000 {
            let sample = ((i as f32) * 0.05).sin() * 0.2;
            writer
                .write_sample((sample * i16::MAX as f32) as i16)
                .unwrap();
        }
        writer.finalize().unwrap();
        path
    }
}

impl Drop for Env {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.root);
    }
}

#[test]
fn transcribe_reports_a_missing_file_without_touching_the_model() {
    let env = Env::new("missing-file");
    env.write_config("[transcription]\nvad_enabled = false\n");
    let missing = env.root.join("does-not-exist.wav");

    let output = env
        .command(&["transcribe", missing.to_str().unwrap()])
        .output()
        .unwrap();

    assert!(!output.status.success());
    let stderr = String::from_utf8(output.stderr).unwrap();
    assert!(stderr.contains("failed to open audio file"), "{stderr}");
}

#[test]
fn transcribe_reads_a_short_fixture_and_reaches_the_model_load() {
    let env = Env::new("short-fixture");
    env.write_config("[transcription]\nvad_enabled = false\n");
    let wav = env.fixture_wav("short");

    let output = env
        .command(&["transcribe", wav.to_str().unwrap()])
        .output()
        .unwrap();

    assert!(!output.status.success());
    let stderr = String::from_utf8(output.stderr).unwrap();
    // The WAV decoded cleanly; the only missing piece is the speech model,
    // and the error says exactly how to install it.
    assert!(!stderr.contains("failed to open audio file"), "{stderr}");
    assert!(stderr.contains("model file not found"), "{stderr}");
    assert!(stderr.contains("tonguetyped model install"), "{stderr}");
}

#[test]
fn transcribe_rejects_a_non_wav_file() {
    let env = Env::new("not-wav");
    env.write_config("[transcription]\nvad_enabled = false\n");
    let path = env.root.join("not-audio.txt");
    std::fs::write(&path, b"this is not a wav file").unwrap();

    let output = env
        .command(&["transcribe", path.to_str().unwrap()])
        .output()
        .unwrap();

    assert!(!output.status.success());
    let stderr = String::from_utf8(output.stderr).unwrap();
    assert!(stderr.contains("failed to open audio file"), "{stderr}");
}
