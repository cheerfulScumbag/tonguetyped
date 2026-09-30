# Project agent memory

This file is the project's committed home for project-intrinsic agent knowledge: build, test, release, architecture, and sharp-edge notes that should travel with the code.

- Add durable project-specific notes here as they are discovered through real work.

## Maintaining this file

Keep this file for knowledge useful to almost every future agent session in this project.
Do not repeat what the codebase already shows; point to the authoritative file or command instead.
Prefer rewriting or pruning existing entries over appending new ones.
When updating this file, preserve this bar for all agents and keep entries concise.

## Build

See README.md for system dependencies. On NixOS, run build commands through
`nix develop -c`.

Format: `cargo fmt --check`  |  Lint: `cargo clippy -- -D warnings`  |  Test: `cargo test`

## Architecture

Single binary `tonguetyped`. CLI via clap dispatches to subcommands. Daemon owns the
coordinator state machine. Clients connect via Unix-domain socket (newline-delimited JSON).

Key crates: cpal (audio capture), rubato (resampling), transcribe-rs/whisper-cpp (inference),
vad-rs (Silero VAD), rusqlite (history). See Cargo.toml for full dep list.

Inference has an optional GPU path behind `InferenceEngine` (`src/inference.rs`,
`src/inference/gpu.rs`): the `gpu-vulkan`/`gpu-cuda` Cargo features add the
`transcribe-cpp` binding (github.com/handy-computer/transcribe.cpp), tried first and
falling back to the tested CPU path on any failure. Neither is enabled by default.
`backend_info()` (blocking, for `doctor`/daemon startup) and `cached_backend_info()`
(non-blocking, for the per-dictation latency path in `latency.rs`) are deliberately
separate - the GPU device probe is not free (~100ms first call) and must not land on
the stop-to-idle hot path. See `README.md`'s "GPU inference backends" section and
`data/tt-transcribe-cpp-gpu-18/report.md` for the full story (including a whisper-rs
GPU auto-detection bug this surfaced) and real Vulkan/CUDA benchmark numbers.

Building `gpu-cuda` on NixOS needs `cudaPackages.cudatoolkit` and the driver's
`/run/opengl-driver/lib` (`libcuda.so`) on the link path; `flake.nix`'s devShell and
the crate-root `build.rs` (scoped to `CARGO_FEATURE_GPU_CUDA`) handle this - `rustc`'s
linker (`rust-lld`, invoked directly) does not honor `LIBRARY_PATH`, only explicit
`-L`/`-l`, so `cargo:rustc-link-search`/`-lib` in `build.rs` is the fix, not env vars.

The GPU backend's model catalog (`src/catalog.rs`, managed with `tonguetyped model
{list,install,remove,use}`) is scoped to `family = 'whisper'` GGUF models only, not
transcribe.cpp's full catalog.db - the CPU path's vendored whisper.cpp checks for the
legacy `GGML_FILE_MAGIC` and cannot load GGUF at all (any family), and other
families' output/chunking semantics (diarization, streaming-only, non-whisper
long-form strategies) have never been exercised against this project's single-shot
`Session::run` usage. See `data/tt-model-catalog-19/report.md` before widening the
catalog to a new family.

A stack-allocated buffer inside an `async fn` is embedded inline in the generated
state machine across every `.await` point in that function (not heap-allocated), and
compounds when awaited from other async functions - `src/model.rs`'s SHA-256
verifier hit a real stack overflow from a 1 MiB buffer for exactly this reason
(fixed at 64 KiB, matching `examples/transcribe_benchmark.rs`'s synchronous hasher).
Keep buffers inside `async fn` bodies small, or hash on a blocking thread instead.
