use clap::Parser;
use serde::Serialize;
use sha2::{Digest, Sha256};
use std::io::Read;
use std::path::{Path, PathBuf};
use std::time::Instant;

use tonguetyped::inference::InferenceEngine;

#[derive(Parser)]
#[command(about = "Benchmark cold model loading and repeated warm inference")]
struct Args {
    /// Whisper model file
    model: PathBuf,
    /// 16 kHz mono WAV input
    wav: PathBuf,
    /// Number of warm inference runs
    #[arg(long, default_value_t = 1)]
    runs: usize,
    /// Apply production VAD with this Silero model before each run
    #[arg(long)]
    vad_model: Option<PathBuf>,
}

#[derive(Serialize)]
struct BenchmarkRun {
    run: usize,
    vad_seconds: f64,
    inference_seconds: f64,
    realtime_factor: f64,
    retained_audio_seconds: f64,
}

#[derive(Serialize)]
struct BenchmarkReport {
    model_file: String,
    model_sha256: String,
    audio_sha256: String,
    audio_seconds: f64,
    model_load_seconds: f64,
    backend: String,
    device: String,
    host_cpu: String,
    thread_count: i32,
    runs: Vec<BenchmarkRun>,
    median_inference_seconds: f64,
    p95_inference_seconds: f64,
    competing_load_warning: Option<String>,
}

fn main() -> anyhow::Result<()> {
    let args = Args::parse();
    if args.runs == 0 {
        anyhow::bail!("--runs must be at least 1");
    }

    let competing_load_warning = competing_load_warning();
    let samples = transcribe_rs::audio::read_wav_samples(&args.wav)?;
    let audio_seconds = samples.len() as f64 / 16_000.0;

    let mut engine = InferenceEngine::new(args.model.clone());
    let started = Instant::now();
    engine.load()?;
    let load_time = started.elapsed();

    let mut runs = Vec::with_capacity(args.runs);
    for run in 1..=args.runs {
        let vad_started = Instant::now();
        let run_samples = match &args.vad_model {
            Some(path) => tonguetyped::vad::filter_audio(&samples, path)?,
            None => samples.clone(),
        };
        let vad_seconds = args
            .vad_model
            .as_ref()
            .map(|_| vad_started.elapsed().as_secs_f64())
            .unwrap_or_default();

        let started = Instant::now();
        let _transcript = engine.transcribe(&run_samples, "en")?;
        let inference_seconds = started.elapsed().as_secs_f64();
        runs.push(BenchmarkRun {
            run,
            vad_seconds,
            inference_seconds,
            realtime_factor: inference_seconds / audio_seconds,
            retained_audio_seconds: run_samples.len() as f64 / 16_000.0,
        });
    }

    let mut sorted: Vec<f64> = runs.iter().map(|run| run.inference_seconds).collect();
    sorted.sort_by(f64::total_cmp);
    let backend = tonguetyped::inference::backend_info();
    let report = BenchmarkReport {
        model_file: args
            .model
            .file_name()
            .and_then(|name| name.to_str())
            .unwrap_or("unknown")
            .to_string(),
        model_sha256: sha256(&args.model)?,
        audio_sha256: sha256(&args.wav)?,
        audio_seconds,
        model_load_seconds: load_time.as_secs_f64(),
        backend: backend.backend,
        device: backend.device,
        host_cpu: host_cpu(),
        thread_count: InferenceEngine::cpu_threads(),
        median_inference_seconds: percentile(&sorted, 0.5),
        p95_inference_seconds: percentile(&sorted, 0.95),
        runs,
        competing_load_warning,
    };

    println!("{}", serde_json::to_string(&report)?);
    Ok(())
}

fn sha256(path: &Path) -> anyhow::Result<String> {
    let mut file = std::fs::File::open(path)?;
    let mut digest = Sha256::new();
    let mut buffer = [0_u8; 64 * 1024];
    loop {
        let read = file.read(&mut buffer)?;
        if read == 0 {
            break;
        }
        digest.update(&buffer[..read]);
    }
    Ok(format!("{:x}", digest.finalize()))
}

fn percentile(sorted: &[f64], quantile: f64) -> f64 {
    let rank = (quantile * sorted.len() as f64).ceil() as usize;
    sorted[rank.saturating_sub(1).min(sorted.len() - 1)]
}

fn host_cpu() -> String {
    std::fs::read_to_string("/proc/cpuinfo")
        .ok()
        .and_then(|contents| {
            contents.lines().find_map(|line| {
                line.strip_prefix("model name")
                    .and_then(|value| value.split_once(':'))
                    .map(|(_, value)| value.trim().to_string())
            })
        })
        .unwrap_or_else(|| "unknown".to_string())
}

fn competing_load_warning() -> Option<String> {
    let load = std::fs::read_to_string("/proc/loadavg")
        .ok()?
        .split_whitespace()
        .next()?
        .parse::<f64>()
        .ok()?;
    let threshold = (InferenceEngine::cpu_threads() as f64 * 0.125).max(1.0);
    (load > threshold).then(|| {
        format!(
            "pre-run 1-minute load average {load:.2} exceeded {threshold:.2}; do not use this result as a release threshold"
        )
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn nearest_rank_percentiles_are_stable_for_five_runs() {
        let runs = [1.0, 2.0, 3.0, 4.0, 5.0];
        assert_eq!(percentile(&runs, 0.5), 3.0);
        assert_eq!(percentile(&runs, 0.95), 5.0);
    }
}
