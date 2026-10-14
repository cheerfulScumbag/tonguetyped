//! One-shot transcription of an audio file - the `tonguetyped transcribe
//! <file>` command.
//!
//! The file is decoded to the same 16 kHz mono `f32` stream the live capture
//! path produces, then handed to the exact same `InferenceEngine`
//! load/transcribe code the daemon uses (`crate::inference`), so file
//! transcription shares one decode path with dictation rather than carrying a
//! second inference implementation. The engine is loaded once and reused for
//! every chunk of a long file; the chunking itself lives in
//! `InferenceEngine::transcribe`, driven by `transcription.chunk_seconds` and
//! always by the active model's own limit - a hard input cap (canary, cohere)
//! or, for a model whose cap is on its output (moonshine), a window clamped to
//! half the `max_audio_ms` the GGUF advertises.

use crate::config::Config;
use crate::inference::{BackendPreference, InferenceEngine, Transcription};
use anyhow::Context;
use std::path::Path;

/// Decode `path` (a WAV of integer or float samples, any channel count or
/// sample rate) to 16 kHz mono `f32` samples in `[-1, 1]`.
pub fn read_audio_file(path: &Path) -> anyhow::Result<Vec<f32>> {
    let mut reader = hound::WavReader::open(path)
        .with_context(|| format!("failed to open audio file {}", path.display()))?;
    let spec = reader.spec();
    anyhow::ensure!(spec.channels > 0, "audio file has no channels");
    anyhow::ensure!(spec.sample_rate > 0, "audio file has no sample rate");

    let interleaved: Vec<f32> = match spec.sample_format {
        hound::SampleFormat::Int => {
            anyhow::ensure!(
                (1..=32).contains(&spec.bits_per_sample),
                "unsupported integer WAV bit depth: {}",
                spec.bits_per_sample
            );
            let scale = (1_u64 << (spec.bits_per_sample - 1)) as f32;
            reader
                .samples::<i32>()
                .map(|sample| sample.map(|value| value as f32 / scale))
                .collect::<Result<_, hound::Error>>()?
        }
        hound::SampleFormat::Float => reader
            .samples::<f32>()
            .collect::<Result<_, hound::Error>>()?,
    };

    let mono = crate::audio::downmix_to_mono(&interleaved, spec.channels as usize);
    crate::audio::resample_mono(&mono, spec.sample_rate, 16_000)
        .with_context(|| format!("failed to resample {} to 16 kHz", path.display()))
}

/// Transcribe `path` and return the transcript text plus whether the model cut
/// it short.
///
/// VAD runs first when `transcription.vad_enabled` is set, exactly as the live
/// dictation path does, so silence-only audio yields an empty string without
/// loading the model. The inference engine is then loaded once for the whole
/// file - including every chunk of a long one - and driven through the shared
/// `InferenceEngine::transcribe` path.
pub fn transcribe_file(path: &Path, config: &Config) -> anyhow::Result<Transcription> {
    let samples = read_audio_file(path)?;
    let samples = if config.transcription.vad_enabled {
        let models_dir = InferenceEngine::models_dir()?;
        crate::vad::filter_audio(
            &samples,
            &models_dir.join(crate::catalog::VAD_MODEL.filename),
        )
        .context("voice activity detection failed")?
    } else {
        samples
    };
    if samples.is_empty() {
        return Ok(Transcription::default());
    }

    let model_path = crate::catalog::model_path(&config.model.active_model)?;
    anyhow::ensure!(
        model_path.exists(),
        "model file not found: {} (run `tonguetyped model install {}` first)",
        model_path.display(),
        config.model.active_model
    );
    let mut engine = InferenceEngine::new(
        model_path,
        BackendPreference::parse(&config.model.preferred_backend)?,
    );
    engine.load()?;
    engine.transcribe(&samples, &config.transcription)
}

/// Write a finished transcript to `out` and, when the model cut it short, a
/// clear warning to `err`. The CLI keeps stdout as pure transcript data, so
/// the cut-short notice goes to stderr - the file-path counterpart of the live
/// path's truncated cue, so a partial transcript is never silently emitted.
pub fn write_transcript(
    transcription: &Transcription,
    out: &mut impl std::io::Write,
    err: &mut impl std::io::Write,
) -> std::io::Result<()> {
    writeln!(out, "{}", transcription.text.trim())?;
    if transcription.truncated {
        writeln!(
            err,
            "warning: the active model's decode limit cut the transcript short; \
             only a partial transcript was produced"
        )?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn write_wav(path: &Path, spec: hound::WavSpec, samples: &[i16]) {
        let mut writer = hound::WavWriter::create(path, spec).unwrap();
        for sample in samples {
            writer.write_sample(*sample).unwrap();
        }
        writer.finalize().unwrap();
    }

    fn temp_path(label: &str) -> std::path::PathBuf {
        std::env::temp_dir().join(format!(
            "tonguetyped-transcribe-{label}-{}-{}.wav",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ))
    }

    fn spec(channels: u16, sample_rate: u32) -> hound::WavSpec {
        hound::WavSpec {
            channels,
            sample_rate,
            bits_per_sample: 16,
            sample_format: hound::SampleFormat::Int,
        }
    }

    #[test]
    fn reads_mono_16k_samples() {
        let path = temp_path("mono-16k");
        write_wav(&path, spec(1, 16_000), &[0, i16::MAX, i16::MIN + 1, 0]);
        let samples = read_audio_file(&path).unwrap();
        std::fs::remove_file(&path).unwrap();
        assert_eq!(samples.len(), 4);
        assert_eq!(samples[0], 0.0);
        assert!((samples[1] - 1.0).abs() < 1e-4);
        assert!((samples[2] + 1.0).abs() < 1e-4);
    }

    #[test]
    fn downmixes_stereo_and_resamples_to_16k() {
        let path = temp_path("stereo-8k");
        let interleaved: Vec<i16> = (0..8_000)
            .flat_map(|_| [i16::MAX / 4, i16::MAX / 4])
            .collect();
        write_wav(&path, spec(2, 8_000), &interleaved);
        let samples = read_audio_file(&path).unwrap();
        std::fs::remove_file(&path).unwrap();
        // One second of 8 kHz audio resamples to one second of 16 kHz audio.
        assert!((samples.len() as i64 - 16_000).unsigned_abs() <= 1);
        assert!(samples.iter().any(|sample| sample.abs() > 0.1));
    }

    #[test]
    fn a_missing_file_is_a_clear_error() {
        let error = read_audio_file(Path::new("/nonexistent/tonguetyped-missing.wav"))
            .unwrap_err()
            .to_string();
        assert!(error.contains("failed to open audio file"), "{error}");
    }

    #[test]
    fn write_transcript_warns_only_when_the_model_cut_it_short() {
        let complete = Transcription {
            text: "  hello world  ".to_string(),
            truncated: false,
        };
        let mut out = Vec::new();
        let mut err = Vec::new();
        write_transcript(&complete, &mut out, &mut err).unwrap();
        assert_eq!(String::from_utf8(out).unwrap(), "hello world\n");
        assert!(err.is_empty(), "a complete transcript must not warn");

        let partial = Transcription {
            text: "hello".to_string(),
            truncated: true,
        };
        let mut out = Vec::new();
        let mut err = Vec::new();
        write_transcript(&partial, &mut out, &mut err).unwrap();
        assert_eq!(String::from_utf8(out).unwrap(), "hello\n");
        let warning = String::from_utf8(err).unwrap();
        assert!(warning.contains("partial transcript"), "{warning}");
    }
}
