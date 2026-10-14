//! Per-family load/inference smoke test.
//!
//! The catalog spans whisper, parakeet, moonshine, canary, and cohere, but
//! every other test exercises it with an empty placeholder file instead of a
//! real GGUF. Nothing else would catch a family that the pinned transcribe-cpp
//! binding cannot load and run through the same `InferenceEngine` the daemon
//! uses, so this test loads the smallest quantization of one variant per
//! non-whisper family and transcribes a one-second clip.
//!
//! Each model is hundreds of megabytes, so the test stays off unless
//! `TONGUETYPED_SMOKE_MODELS=1` is set and huggingface.co is reachable. Without
//! that it skips and prints why, rather than passing as if it had checked.

use tonguetyped::config::TranscriptionConfig;
use tonguetyped::inference::{BackendPreference, InferenceEngine};
use tonguetyped::model::DownloadManager;

/// One non-whisper family and the smallest catalog entry that exercises it.
const SMOKE_MODELS: &[(&str, &str)] = &[
    ("parakeet", "parakeet-tdt-0.6b-v2-q4_k_m"),
    ("moonshine", "moonshine-base-q8_0"),
    ("canary", "canary-1b-v2-q4_k_m"),
    ("cohere_asr", "cohere-transcribe-03-2026-q4_k_m"),
];

fn smoke_enabled() -> bool {
    std::env::var("TONGUETYPED_SMOKE_MODELS").is_ok_and(|value| value == "1")
}

/// A deterministic one-second 16 kHz mono tone: long enough to decode, short
/// enough that every family accepts it, and never dependent on a fixture file.
fn one_second_tone() -> Vec<f32> {
    let sample_rate = 16_000.0_f32;
    (0..16_000)
        .map(|n| (2.0 * std::f32::consts::PI * 440.0 * n as f32 / sample_rate).sin() * 0.1)
        .collect()
}

#[test]
fn every_non_whisper_family_loads_and_transcribes() {
    if !smoke_enabled() {
        eprintln!(
            "skipping model-family smoke test: set TONGUETYPED_SMOKE_MODELS=1 to fetch the models"
        );
        return;
    }

    // Keep the downloads (and the engine's model directory) out of the real
    // user data dir. This single test owns the process so the env change cannot
    // race another test.
    let root = std::env::temp_dir().join(format!(
        "tt-family-smoke-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    std::fs::create_dir_all(&root).unwrap();
    std::env::set_var("XDG_DATA_HOME", &root);

    let runtime = tokio::runtime::Runtime::new().unwrap();
    let manager = DownloadManager::new().unwrap();
    let samples = one_second_tone();
    let config = TranscriptionConfig::default();

    for (family, id) in SMOKE_MODELS {
        let entry =
            tonguetyped::catalog::find(id).unwrap_or_else(|| panic!("{id} is not in the catalog"));
        assert_eq!(
            entry.info().family,
            *family,
            "{id} should be a {family} model"
        );

        runtime
            .block_on(manager.install_catalog_model(id, None))
            .unwrap_or_else(|error| panic!("failed to fetch {id}: {error:#}"));
        let path = tonguetyped::catalog::model_path(id).unwrap();

        let mut engine = InferenceEngine::new(path, BackendPreference::Auto);
        engine
            .load()
            .unwrap_or_else(|error| panic!("{family} model {id} failed to load: {error:#}"));
        engine
            .transcribe(&samples, &config)
            .unwrap_or_else(|error| panic!("{family} model {id} failed to run: {error:#}"));
        engine.unload();
    }

    std::fs::remove_dir_all(&root).ok();
}

#[test]
fn smoke_model_entries_are_the_smallest_for_each_family() {
    // Guards the `SMOKE_MODELS` picks against a catalog change that would make
    // the opt-in run download a larger file than intended: each listed id must
    // exist, belong to its family, and be that family's smallest entry.
    for (family, id) in SMOKE_MODELS {
        let chosen = tonguetyped::catalog::find(id).unwrap();
        assert_eq!(chosen.info().family, *family);
        let smallest = tonguetyped::catalog::ENTRIES
            .iter()
            .filter(|entry| entry.info().family == *family)
            .min_by_key(|entry| entry.size_bytes)
            .unwrap();
        assert_eq!(
            smallest.id, *id,
            "{id} is no longer the smallest {family} entry; it is now {}",
            smallest.id
        );
    }
}

/// Keeps the helper honest about its output shape without fetching anything.
#[test]
fn the_smoke_clip_is_one_second_of_16khz_audio() {
    let samples = one_second_tone();
    assert_eq!(samples.len(), 16_000);
    assert!(samples.iter().all(|sample| sample.abs() <= 0.1));
}
