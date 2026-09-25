use std::path::PathBuf;
use std::time::Instant;

use tonguetyped::inference::InferenceEngine;

fn main() -> anyhow::Result<()> {
    let mut args = std::env::args_os().skip(1);
    let model_path = args
        .next()
        .map(PathBuf::from)
        .ok_or_else(|| anyhow::anyhow!("usage: transcribe_benchmark MODEL WAV [LANGUAGE]"))?;
    let wav_path = args
        .next()
        .map(PathBuf::from)
        .ok_or_else(|| anyhow::anyhow!("usage: transcribe_benchmark MODEL WAV [LANGUAGE]"))?;
    let language = args
        .next()
        .map(|value| value.to_string_lossy().into_owned())
        .unwrap_or_else(|| "auto".to_string());
    if args.next().is_some() {
        anyhow::bail!("usage: transcribe_benchmark MODEL WAV [LANGUAGE]");
    }

    let samples = transcribe_rs::audio::read_wav_samples(&wav_path)?;
    let audio_seconds = samples.len() as f64 / 16_000.0;

    let mut engine = InferenceEngine::new(model_path);
    let started = Instant::now();
    engine.load()?;
    let load_time = started.elapsed();

    let started = Instant::now();
    let transcript = engine.transcribe(&samples, &language)?;
    let transcription_time = started.elapsed();

    println!("audio_seconds={audio_seconds:.3}");
    println!("model_load_seconds={:.3}", load_time.as_secs_f64());
    println!(
        "transcription_seconds={:.3}",
        transcription_time.as_secs_f64()
    );
    println!(
        "realtime_factor={:.3}",
        transcription_time.as_secs_f64() / audio_seconds
    );
    println!("cpu_threads={}", InferenceEngine::cpu_threads());
    println!("transcript={transcript}");
    Ok(())
}
