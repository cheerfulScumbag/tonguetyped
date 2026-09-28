use clap::Parser;
use serde::Serialize;
use std::path::PathBuf;
use std::time::Instant;

use tonguetyped::inference::InferenceEngine;

#[derive(Parser)]
#[command(about = "Benchmark model loading and inference on the baseline revision")]
struct Args {
    /// Whisper model file
    model: PathBuf,
    /// 16 kHz mono WAV input
    wav: PathBuf,
}

#[derive(Serialize)]
struct BenchmarkReport {
    audio_seconds: f64,
    model_load_seconds: f64,
    inference_seconds: f64,
}

fn main() -> anyhow::Result<()> {
    let args = Args::parse();
    let samples = transcribe_rs::audio::read_wav_samples(&args.wav)?;

    let mut engine = InferenceEngine::new(args.model);
    let started = Instant::now();
    engine.load()?;
    let model_load_seconds = started.elapsed().as_secs_f64();

    let started = Instant::now();
    let _transcript = engine.transcribe(&samples, "en")?;
    let inference_seconds = started.elapsed().as_secs_f64();

    println!(
        "{}",
        serde_json::to_string(&BenchmarkReport {
            audio_seconds: samples.len() as f64 / 16_000.0,
            model_load_seconds,
            inference_seconds,
        })?
    );
    Ok(())
}
