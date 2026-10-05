# transcribe.cpp CPU/GPU inference consolidation report

Date: 2026-10-04

## Summary

Replaced the dual-library inference setup (`transcribe-rs`'s whisper.cpp
binding on CPU, `transcribe-cpp` on Vulkan/CUDA) with one deep `transcribe-cpp`
module (`src/inference.rs`) that handles every backend - CPU, Vulkan, CUDA,
ROCm, and Metal - through the same code path. `InferenceEngine::load` always
requests an explicit backend (never `Backend::Auto`), in a fixed priority
order (CUDA, ROCm, Vulkan, Metal, then the unconditional, always-available
explicit CPU request), and every attempt loads the exact same configured GGUF
file - the dual-model problem (a GGML file for CPU, a separate GGUF file for
GPU) is gone. `transcribe-rs`, `whisper-rs`, the legacy GGML CPU model
download, and the separate `model.selected`/`model.gpu_model` config split are
all removed. A pre-consolidation config file is migrated to the new
`model.active_model` field automatically on first load (see
`config::migrate_legacy_model_config`), with no manual edits required.

## Reproducing the dual-path behavior before editing

Before changing any source, I built the pre-change tree (default features,
then `--features gpu-vulkan`) and ran the existing `transcribe_benchmark`
example against the project's own canonical fixture
(`~/.no-mistakes/evidence/01M3EG83QGQM19R7AV23C0SHYM/recording.wav`, SHA-256
`2de0423a4b272c60df025c0e77d8b9c974d079ae0814294fd4fa796bb7f90c78` - the same
fixture referenced in `data/tt-transcribe-cpp-gpu-18/report.md` and
`data/tt-model-catalog-19/report.md`), confirming the exact contracts the
intent calls out before touching them:

**CPU path (`whisper.cpp/cpu`, via `transcribe-rs`)**, loading the legacy
`ggml-small-q5_1.bin`:

```json
{"model_file":"ggml-small-q5_1.bin","model_sha256":"ae85e4a935d7a567bd102fe55afc16bb595bdb618e11b2fc7591bc08120411bb","audio_sha256":"2de0423a4b272c60df025c0e77d8b9c974d079ae0814294fd4fa796bb7f90c78","audio_seconds":8.597,"model_load_seconds":0.12196981,"backend":"whisper.cpp/cpu","device":"CPU","host_cpu":"AMD Ryzen 7 9700X 8-Core Processor","thread_count":8,"runs":[{"run":1,"vad_seconds":0.0,"inference_seconds":18.063594493,"realtime_factor":2.1011509239269515,"retained_audio_seconds":8.597}],"median_inference_seconds":18.063594493,"p95_inference_seconds":18.063594493,"competing_load_warning":"pre-run 1-minute load average 2.47 exceeded 1.00; do not use this result as a release threshold"}
```

`whisper_backend_init_gpu: no GPU found` confirmed in stderr - genuinely CPU,
matching the documented `use_gpu: false` contract.

**Vulkan path (`transcribe.cpp/vulkan`)**, loading
`whisper-small-Q5_K_M.gguf` (the GPU catalog's default, already installed on
this machine from prior work):

```json
{"model_file":"whisper-small-Q5_K_M.gguf","model_sha256":"326cd00c3e7217c751667c7c1600eaf7e0de174e186ca2c16b4bf590251c3c3b","audio_sha256":"2de0423a4b272c60df025c0e77d8b9c974d079ae0814294fd4fa796bb7f90c78","audio_seconds":8.597,"model_load_seconds":0.198936421,"backend":"transcribe.cpp/vulkan","device":"NVIDIA GeForce RTX 4080 SUPER","host_cpu":"AMD Ryzen 7 9700X 8-Core Processor","thread_count":8,"runs":[{"run":1,"vad_seconds":0.0,"inference_seconds":0.067446136,"realtime_factor":0.007845310689775504,"retained_audio_seconds":8.597}],"median_inference_seconds":0.067446136,"p95_inference_seconds":0.067446136,"competing_load_warning":"pre-run 1-minute load average 3.99 exceeded 1.00; do not use this result as a release threshold"}
```

This confirms, before any edit: the CPU path and the GPU path loaded **two
different model files** (`ggml-small-q5_1.bin` vs. `whisper-small-Q5_K_M.gguf`
- same family, different quantization scheme), the exact problem the intent
calls "the correct fix has one ... model downloader" to resolve, and the
observable backend strings (`whisper.cpp/cpu`, `transcribe.cpp/vulkan`) that
`tonguetyped doctor` and the daemon startup log report.

## The new module

`src/inference.rs` is now the only inference code in the project. Design
points, each a direct response to a spec requirement:

- **One model path, every backend.** `InferenceEngine::new(model_path)` takes
  a single `PathBuf`; `load()` tries `Backend::Cuda`, `Backend::Rocm`,
  `Backend::Vulkan`, `Backend::Metal`, then the unconditional
  `Backend::Cpu` - all against that same path. A backend whose Cargo feature
  wasn't compiled in simply isn't natively satisfiable
  (`transcribe_cpp::Model::load_with` returns `Error::Backend`), so the
  fallback chain needs no `cfg` gating at all; it is correct on every build by
  construction. Verified directly in the real stderr of a plain (no
  accelerator feature) build loading a real GGUF file:

  ```text
  whisper: cuda backend requested but not available
  whisper: rocm backend requested but not available
  whisper: vulkan backend requested but not available
  whisper: metal backend requested but not available
  whisper: using cpu backend (strict)
  ```

- **Never `Backend::Auto`.** Every request is explicit, matching the intent's
  "select an explicit backend internally." This is also what makes the
  fallback deterministic and testable: an unsatisfiable explicit request
  fails cleanly (`Error::Backend`) rather than silently rerouting.
- **Backend/device reporting reads the loaded model, not a guess.**
  `describe_loaded_backend` calls `Model::device()` and uses its `kind` field
  (the library's own clean vendor vocabulary: `"cpu"`, `"vulkan"`, `"cuda"`,
  `"rocm"`, `"metal"`, ...) for the backend label, and its `description`/`name`
  for the device string. A real bug found and fixed here: `Model::backend()`
  (the API the doc comment suggested using) returns device-indexed strings in
  practice - `"Vulkan0"`, uppercase `"CPU"` - not the clean lowercase values
  its own doc comment shows; using `Device::kind` instead gives a backend
  label (`"transcribe.cpp/vulkan"`, `"transcribe.cpp/cpu"`) consistent with
  the hardware-capability probe's own vocabulary.
- **Hardware-capability probe kept, simplified.** `backend_info()`/
  `cached_backend_info()` (used when no engine has loaded yet - daemon startup
  log, `doctor`'s fallback) keep the prior OnceLock-memoized,
  non-blocking-variant shape, now trying the same accelerator priority list
  via `transcribe_cpp::backend_available`/`devices()` instead of two
  hand-written per-backend probe modules.
- **No more `cached_active_backend_info` split for a loaded engine.** The
  prior code split `active_backend_info` (blocking) from
  `cached_active_backend_info` (non-blocking, for the stop-to-idle hot path)
  because a *fresh* backend/device enumeration was measured at ~118ms
  (`data/tt-transcribe-cpp-gpu-18/report.md`). That cost is paid once, by
  `Model::load_with` itself, when a backend is first initialized - reading
  `Model::device()` off an *already-loaded* model is a cheap struct read, not
  a fresh enumeration, so a single `active_backend_info()` is used from both
  `coordinator.rs`'s hot path and `doctor`.

## Config migration

`ModelConfig` now has one field, `active_model` (a `crate::catalog` GGUF id),
replacing `selected` (the legacy GGML CPU model name) and `gpu_model` (the
GPU catalog id). `config::migrate_legacy_model_config` rewrites a config
file's raw `toml::Value` before strict (`deny_unknown_fields`) deserialization:
present `gpu_model` wins (it already names a real catalog entry and preserves
exactly what a GPU build was running); otherwise the legacy GGML-only
`selected` field maps to `catalog::DEFAULT_MODEL_ID` (the GGUF equivalent of
its one possible value, `whisper-small-q5_1`). `Config::load` persists the
migrated file back to disk (so a second load sees the new format directly);
`Config::reload` migrates in-memory only, matching its existing read-only
contract. Covered by five tests in `src/config.rs` (both legacy field
combinations, the no-op cases, and an end-to-end `Config::load` round trip
through a real temp `XDG_CONFIG_HOME` proving the file is actually rewritten).

## Tests added

- `src/inference.rs`: `backend_candidates_end_with_explicit_cpu`,
  `backend_candidates_never_include_auto` (the selection-order invariants,
  independent of which Cargo features are compiled or what hardware is
  present), `load_reports_a_missing_model_file_without_trying_any_backend`,
  `unloaded_engine_reports_no_active_model_or_backend`.
- `src/config.rs`: the five migration tests above.
- `tests/daemon_startup.rs`: `doctor_reports_a_missing_model_with_no_error_and_no_file`
  (genuinely absent file, as opposed to the existing corrupt-but-present
  case), `doctor_reports_the_actually_configured_model_id_not_the_default`
  (proves the reported id/path reflect the real config, not a hardcoded
  default).
- Existing tests (`model_cli.rs`, `daemon_startup.rs`, `setup.rs`,
  `setup/console.rs`) updated for the unified `active_model` field and the
  single-catalog `tonguetyped model` CLI surface (no more separate "CPU model
  (fixed default)" section).

No test downloads a real model or depends on real hardware, matching this
project's existing convention (`vad.rs`'s `test_vad_creation_fails_without_model`
tests the error path with a nonexistent path rather than a real ONNX file);
real end-to-end inference is verified manually below, the same way slice 2's
GPU work was.

## Real end-to-end verification (post-change)

Using the exact same fixture and GGUF file as the before-editing reproduction
above, built and run on this machine's real NVIDIA GeForce RTX 4080 SUPER:

### CPU (`cargo build --release`, no accelerator feature)

```text
whisper: cuda backend requested but not available
whisper: rocm backend requested but not available
whisper: vulkan backend requested but not available
whisper: metal backend requested but not available
whisper: using cpu backend (strict)
```

```json
{"model_file":"whisper-small-Q5_K_M.gguf","model_sha256":"326cd00c3e7217c751667c7c1600eaf7e0de174e186ca2c16b4bf590251c3c3b","audio_sha256":"2de0423a4b272c60df025c0e77d8b9c974d079ae0814294fd4fa796bb7f90c78","audio_seconds":8.597,"model_load_seconds":0.119594896,"backend":"transcribe.cpp/cpu","device":"AMD Ryzen 7 9700X 8-Core Processor","host_cpu":"AMD Ryzen 7 9700X 8-Core Processor","thread_count":8,"runs":[{"run":1,"vad_seconds":0.0,"inference_seconds":5.342315581,"realtime_factor":0.6214162592764919,"retained_audio_seconds":8.597},{"run":2,"vad_seconds":0.0,"inference_seconds":5.285088962,"realtime_factor":0.6147596791904153,"retained_audio_seconds":8.597},{"run":3,"vad_seconds":0.0,"inference_seconds":5.222842082,"realtime_factor":0.6075191441200418,"retained_audio_seconds":8.597}],"median_inference_seconds":5.285088962,"p95_inference_seconds":5.342315581,"competing_load_warning":"pre-run 1-minute load average 12.67 exceeded 1.00; do not use this result as a release threshold"}
```

Transcript (captured with a throwaway debug binary, deleted after use, same
as slice 2's methodology - the committed benchmark tool discards transcript
text by design):

```text
Today I am testing local speech recognition, the microphone records my voice and the computer converts each sentence into written text.
```

Byte-for-byte identical to the pre-change CPU transcript. Note: the new CPU
path's median inference (~5.3s) is markedly faster than the pre-change
`whisper-rs` CPU path (18.06s) on the same hardware and thread count - likely
attributable to the different quantization scheme (GGUF Q5_K_M vs. the legacy
GGML Q5_1) rather than a transcribe.cpp-vs-whisper.cpp difference, since both
ultimately wrap the same underlying ggml CPU kernels; not a claim this task
set out to make, just an observed, welcome side effect of using one model
file for both paths.

### Vulkan (`--features gpu-vulkan`)

```json
{"model_file":"whisper-small-Q5_K_M.gguf","model_sha256":"326cd00c3e7217c751667c7c1600eaf7e0de174e186ca2c16b4bf590251c3c3b","audio_sha256":"2de0423a4b272c60df025c0e77d8b9c974d079ae0814294fd4fa796bb7f90c78","audio_seconds":8.597,"model_load_seconds":0.157438424,"backend":"transcribe.cpp/vulkan","device":"NVIDIA GeForce RTX 4080 SUPER","host_cpu":"AMD Ryzen 7 9700X 8-Core Processor","thread_count":8,"runs":[{"run":1,"vad_seconds":0.0,"inference_seconds":0.066087607,"realtime_factor":0.0076872870768872874,"retained_audio_seconds":8.597},{"run":2,"vad_seconds":0.0,"inference_seconds":0.055400036,"realtime_factor":0.006444112597417704,"retained_audio_seconds":8.597},{"run":3,"vad_seconds":0.0,"inference_seconds":0.056570852,"realtime_factor":0.006580301500523439,"retained_audio_seconds":8.597}],"median_inference_seconds":0.056570852,"p95_inference_seconds":0.066087607,"competing_load_warning":"pre-run 1-minute load average 12.67 exceeded 1.00; do not use this result as a release threshold"}
```

Transcript: byte-for-byte identical to the CPU run above. `backend` resolves
to the clean `transcribe.cpp/vulkan` label (see "Real bugs found" above for
why this needed a fix after the first attempt returned `transcribe.cpp/
vulkan0`).

### CUDA (`--features gpu-cuda`)

```text
ggml_cuda_init: found 1 CUDA devices (Total VRAM: 15937 MiB):
  Device 0: NVIDIA GeForce RTX 4080 SUPER, compute capability 8.9, VMM: yes, VRAM: 15937 MiB
whisper: using cuda backend: CUDA0
```

```json
{"model_file":"whisper-small-Q5_K_M.gguf","model_sha256":"326cd00c3e7217c751667c7c1600eaf7e0de174e186ca2c16b4bf590251c3c3b","audio_sha256":"2de0423a4b272c60df025c0e77d8b9c974d079ae0814294fd4fa796bb7f90c78","audio_seconds":8.597,"model_load_seconds":0.055978633,"backend":"transcribe.cpp/cuda","device":"NVIDIA GeForce RTX 4080 SUPER","host_cpu":"AMD Ryzen 7 9700X 8-Core Processor","thread_count":8,"runs":[{"run":1,"vad_seconds":0.0,"inference_seconds":0.105628135,"realtime_factor":0.012286627311852972,"retained_audio_seconds":8.597},{"run":2,"vad_seconds":0.0,"inference_seconds":0.056084895,"realtime_factor":0.006523775154123532,"retained_audio_seconds":8.597},{"run":3,"vad_seconds":0.0,"inference_seconds":0.05291975,"realtime_factor":0.006155606606955915,"retained_audio_seconds":8.597}],"median_inference_seconds":0.056084895,"p95_inference_seconds":0.105628135,"competing_load_warning":"pre-run 1-minute load average 13.31 exceeded 1.00; do not use this result as a release threshold"}
```

Transcript: byte-for-byte identical to the CPU and Vulkan runs above.

### Summary table

| Backend | Device | Median inference | Realtime factor vs. CPU |
| --- | --- | ---: | ---: |
| `transcribe.cpp/cpu` | AMD Ryzen 7 9700X | 5.29 s | 1x (baseline) |
| `transcribe.cpp/vulkan` | RTX 4080 SUPER | 0.057 s | ~93x faster |
| `transcribe.cpp/cuda` | RTX 4080 SUPER | 0.056 s | ~94x faster |

All three runs above (and the CUDA kernel compile itself) happened on a
shared machine running several concurrent build lanes - every
`competing_load_warning` is non-null, so per this project's own
release-threshold guidance none of these numbers should be read as a clean
baseline. The CPU/Vulkan/CUDA *relative* comparison and, above all, the
byte-for-byte identical transcript across all three backends from the one
configured GGUF file, are the load-bearing evidence here, not the absolute
timings.

## Verification commands run

- `cargo fmt --check` - clean (default, `--features gpu-vulkan`, and
  `--features gpu-cuda`).
- `cargo clippy --all-targets -- -D warnings` - clean (default and
  `--features gpu-vulkan`); one real finding fixed along the way
  (`clippy::explicit_auto_deref` in `detect_capability`).
- `cargo test` (default features): 134 passed, 0 failed.
- `cargo test --features gpu-vulkan` (real Vulkan hardware, this machine):
  134 passed, 0 failed - `doctor_distinguishes_invalid_model_from_missing_model`'s
  corrupt-stub-file scenario visibly exercises the real compiled Vulkan
  backend-probe path (`ggml_vulkan: Found 1 Vulkan devices: ... NVIDIA
  GeForce RTX 4080 SUPER`) before falling through to the same CPU error.
- `cargo build --release --features gpu-cuda` - builds clean on this
  machine's real CUDA 12.9 toolchain (`cudaPackages.cudatoolkit` via
  `flake.nix`'s devShell), linking against the real driver's `libcuda.so` at
  `/run/opengl-driver/lib` per `build.rs`'s `CARGO_FEATURE_GPU_CUDA`-scoped
  link directives - unchanged from slice 2, still correct. Real end-to-end
  load + transcribe verified above (CUDA debug-transcript run and the
  3-run benchmark), both against the real RTX 4080 SUPER.
- Nix: `flake.nix` defines no separate `checks` output; `nix build` (the
  default `gpu-vulkan` package) is the repository's native packaging
  validation. It built clean (`/nix/store/.../tonguetyped-0.1.0`), passed
  `installCheckPhase` (binary exists, `desktop-file-validate`), and the
  produced binary runs (`tonguetyped --version` → `tonguetyped 0.1.0`).
- `cargo clippy --all-targets --features gpu-cuda -- -D warnings` - clean.
- `cargo test --features gpu-cuda` (real CUDA hardware, this machine):
  134 passed, 0 failed - `coordinator_ipc.rs` visibly initializes the real
  compiled CUDA backend (`ggml_cuda_init: found 1 CUDA devices (Total VRAM:
  15937 MiB): Device 0: NVIDIA GeForce RTX 4080 SUPER, compute capability
  8.9`) while exercising the coordinator's inference path.
- `cargo fmt --check` passed again after all of the above (default,
  `gpu-vulkan`, `gpu-cuda`) - no formatting drift across any feature set.

## What was not done

- No new model-family catalog entries (Parakeet, Canary, Moonshine, ...).
  The CPU path can now load any transcribe.cpp-supported GGUF family
  architecturally (the module has no whisper-specific assumptions left -
  that was the whole point of the old CPU-side restriction, which this
  change removes), but their output/chunking semantics (diarization,
  streaming-only families, `hard-cap`/`soft-window` long-form strategies)
  remain unverified against this project's VAD-then-single-buffer pipeline.
  Validating one for real - a real download, a real single-shot dictation
  run, a real transcript - is a focused follow-up, not bundled into this
  consolidation per the spec's "add only model entries that can be validated"
  and "implement ... as a focused change" guidance.
- No `transcribe-cpp` version bump: 0.2.4 (the currently pinned version)
  already exposes an explicit `Backend::Cpu` and `Device::kind`, everything
  this consolidation needed.

## Hardware paths this worktree cannot exercise

This machine has one NVIDIA GeForce RTX 4080 SUPER (confirmed via `lspci`/
`nvidia-smi`) and no AMD GPU and no macOS target - `flake.nix` only builds
`x86_64-linux`/`aarch64-linux`. So:

- **ROCm** (`gpu-rocm` feature, forwarding to `transcribe-cpp-sys/rocm`):
  compiles as a Cargo feature (the native CMake build is `transcribe-cpp-sys`'s
  concern, the same as `gpu-cuda`), but this worktree has no AMD GPU to load a
  model against and no ROCm/HIP toolchain in `flake.nix`'s devShell to even
  attempt a real build. Unverified beyond "the Rust-level request path is
  identical to Vulkan/CUDA's, which are both verified."
- **Metal** (`gpu-metal` feature): transcribe.cpp's Metal backend only builds
  on Darwin; this project's Nix flake and every other subsystem (Wayland
  overlay, ALSA/PipeWire audio, XDG portals) already assume Linux, so there is
  no realistic way to validate this on any machine reachable from this task.
  The Cargo feature exists for completeness (mirrors the other three
  one-line forwards) but is unbuildable here by construction, not merely
  untested.

A captain/firstmate with ROCm hardware (or a Darwin machine, for Metal) can
validate those two paths directly; everything else in this report - CPU,
Vulkan, and CUDA compilation/loading/transcription - was verified for real on
this machine.
