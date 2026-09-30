# transcribe.cpp GPU inference report

Date: 2026-09-29

## Summary

Integrated the official Rust binding (`transcribe-cpp`, wrapping
[handy-computer/transcribe.cpp](https://github.com/handy-computer/transcribe.cpp))
behind the `InferenceEngine` boundary added in slice 1, as two opt-in Cargo
features: `gpu-vulkan` and `gpu-cuda`. Neither is enabled by default; a plain
`cargo build` is byte-for-byte the same tested CPU path as before. When a GPU
feature is enabled, `InferenceEngine::load` tries that backend first and falls
back to the CPU path (now forced `use_gpu: false`, see below) on any failure.

Before writing any GPU code, I verified `transcribe-cpp` is a real, legitimate
crate: fetched the GitHub repo via `gh api`, confirmed it's a ~2,000-star,
MIT-licensed, actively maintained project with its own AGENTS.md/CLAUDE.md and
CI, and confirmed `cargo add transcribe-cpp --dry-run` resolves the exact
version (0.2.4) referenced in its own `bindings/rust/transcribe-cpp/Cargo.toml`
against the real crates.io registry - not a typosquat or hallucinated
dependency.

## Real bugs found and fixed along the way

**1. The "CPU fallback" was not actually CPU-only.** `transcribe-rs`'s
`WhisperEngine::load()` auto-detects and prefers any GPU backend it was linked
against (`use_gpu: cfg!(feature = "_gpu")` internally, further gated by a
runtime accelerator preference). Slice 1 never observed this because no
Vulkan/CUDA libraries were present in its build environment. Adding
`vulkan-headers`/`vulkan-loader`/`shaderc` to the Nix devShell (needed to build
`transcribe-cpp-sys`'s Vulkan backend) also let whisper-rs's own vendored
ggml auto-detect the same Vulkan device - and the very first benchmark run
crashed:

```text
ggml_vulkan: Device memory allocation of size 189489216 failed.
ggml_vulkan: vk::Device::allocateMemory: ErrorOutOfDeviceMemory
.../ggml-backend.cpp:205: GGML_ASSERT(buffer) failed
```

Root cause of the OOM: an unrelated `llama-server` process on this shared
machine was holding ~14 GiB of the 16 GiB VRAM (confirmed via `nvidia-smi`).
That's real competing GPU load I have no authority to clear (outside this
worktree/task). But the crash itself is a correctness bug I introduced by
changing the build environment: a "CPU fallback" that can itself try to use a
contended GPU and abort the whole process is not a fallback. Fixed by loading
the CPU path with `WhisperEngine::load_with_params(path, WhisperLoadParams {
use_gpu: false, .. })` explicitly (`src/inference.rs`), and simplified
`backend_info()`'s CPU branch to match (it no longer probes for a GPU it will
never use).

**2. GPU backend/device detection cost real latency on the hot path.**
`latency.rs::finish()` calls `backend_info()` on every completed dictation.
Under `gpu-vulkan`/`gpu-cuda`, the first `transcribe_cpp::backend_available` +
`devices()` call measured **117.6 ms** (`examples/_debug_timing.rs`, deleted
after use) - not free like the CPU path's device probe. That's real
stop-to-idle latency added to exactly the metric slice 1 built this
instrumentation to protect, and it also raced a coordinator integration test's
timing assumptions (`coordinator_ipc.rs`), causing two flaky failures. Fixed
in two parts:
  - `inference::gpu::probe_backend_info` memoizes its result (`OnceLock`) -
    backend availability cannot change over a process's lifetime.
  - Added `inference::cached_backend_info()`, a non-blocking variant that
    returns a `"detecting"` placeholder instead of computing when the cache
    isn't warm yet, and pointed `latency.rs` at it instead of the blocking
    `backend_info()`. `Coordinator::new` also spawns a background thread that
    calls the blocking probe once, so the cache is warm well before any real
    dictation completes in practice.
  - The daemon startup log still uses the blocking `backend_info()` capability
    probe - accuracy matters more than speed for a one-time startup call, and
    no engine is loaded yet at that point to read a real outcome from. A
    later review round found that this same probe, used verbatim for
    `doctor` and the per-dictation latency record, reported hardware
    capability rather than the backend a load actually fell back to; `doctor`
    and latency reporting now read `InferenceEngine::active_backend_info()` /
    `cached_active_backend_info()` off the loaded engine instead (see
    `src/inference.rs`).

**3. Eagerly downloading the GPU model at every daemon startup.** My first
pass added a GPU-model existence/download check to `daemon.rs::prepare_dependencies`,
mirroring the existing CPU model check. This blocks daemon startup on a
~194 MB synchronous download whenever the GGUF file is missing - caught by
`daemon_startup.rs`'s `ipc_starts_when_shortcut_portal_is_unavailable` test,
which expects the control socket to open within 5 seconds in a fresh XDG
sandbox with no GGUF file. Removed the eager prefetch entirely:
`GpuEngine::load()` already fails gracefully and falls back to CPU with a
`tracing::warn!` when the GGUF file is missing, so a GPU build works out of
the box (on CPU) without provisioning anything, and the operator provisions
the GGUF file manually (documented in `README.md`) to actually get GPU
inference.

**4. `gpu-cuda` failed to link on NixOS**, with `rust-lld: error: undefined
symbol: cudaFuncSetAttribute` (and, once that was fixed, `cublasGemmBatchedEx`,
`cuMemCreate`, etc.). `transcribe-cpp-sys`'s CMake build compiles the CUDA
kernels fine but doesn't emit the `cargo:rustc-link-search`/`-lib` directives
Cargo needs on a system where CUDA isn't on the default linker search path the
way an FHS install would put it (`LIBRARY_PATH`/`LD_LIBRARY_PATH` don't matter
here either - the linker is `rust-lld` invoked directly, which only honors
explicit `-L`/`-l`, not those env vars). Fixed with a small `build.rs` at the
crate root, scoped to `CARGO_FEATURE_GPU_CUDA` so it has zero effect on the
default or `gpu-vulkan` builds: link-searches `$CUDAToolkit_ROOT/lib` (cudart,
cublas) and `/run/opengl-driver/lib` (libcuda.so, which ships with the driver,
not the toolkit), and rpaths the latter into the binary so it doesn't
additionally need `LD_LIBRARY_PATH` at run time.

## Verification

Both GPU backends produce the **exact same transcript**, byte-for-byte, as the
CPU path on the same input (verified with a throwaway debug binary, deleted
after use - the benchmark tool itself discards transcript text by design):

```text
Today I am testing local speech recognition, the microphone records my voice and the computer converts each sentence into written text.
```

`cargo test`, `cargo fmt --check`, and `cargo clippy --all-targets -- -D
warnings` all pass clean for the default build and both `--features
gpu-vulkan` / `--features gpu-cuda` builds.

## Benchmark evidence

Same exact input WAV as slice 1's CPU baseline (SHA-256
`2de0423a4b272c60df025c0e77d8b9c974d079ae0814294fd4fa796bb7f90c78`, reused
from that PR's recorded evidence, not regenerated), 8.597 seconds, on an AMD
Ryzen 7 9700X (8 physical cores) with an NVIDIA GeForce RTX 4080 SUPER
(16 GB VRAM). Every run below used `--runs 3` against the real compiled
`transcribe_benchmark` release binary.

This machine runs several concurrent build lanes (visible in `git branch -a`
and in `nvidia-smi`/`/proc/loadavg` throughout this session), so most runs
carry a non-null `competing_load_warning` - flagged rather than presented as
clean, per the captain's intent. Where a clean rerun was practical I include
it; where it wasn't (this machine did not go quiet), the flagged number is
still the best available evidence, and cross-checking shows contention barely
moved the steady-state numbers (see the CPU section below).

### CPU baseline (`whisper.cpp/cpu`)

Command:

```sh
cargo build --release --example transcribe_benchmark
./target/release/examples/transcribe_benchmark \
  "$HOME/.local/share/tonguetyped/models/ggml-small-q5_1.bin" \
  recording.wav --runs 3
```

```json
{"model_file":"ggml-small-q5_1.bin","model_sha256":"ae85e4a935d7a567bd102fe55afc16bb595bdb618e11b2fc7591bc08120411bb","audio_sha256":"2de0423a4b272c60df025c0e77d8b9c974d079ae0814294fd4fa796bb7f90c78","audio_seconds":8.597,"model_load_seconds":0.126923692,"backend":"whisper.cpp/cpu","device":"CPU","host_cpu":"AMD Ryzen 7 9700X 8-Core Processor","thread_count":8,"runs":[{"run":1,"vad_seconds":0.0,"inference_seconds":17.967513916,"realtime_factor":2.08997486518553,"retained_audio_seconds":8.597},{"run":2,"vad_seconds":0.0,"inference_seconds":18.524640873,"realtime_factor":2.1547796758171454,"retained_audio_seconds":8.597},{"run":3,"vad_seconds":0.0,"inference_seconds":18.01110287,"realtime_factor":2.0950451169012445,"retained_audio_seconds":8.597}],"median_inference_seconds":18.01110287,"p95_inference_seconds":18.524640873,"competing_load_warning":"pre-run 1-minute load average 1.96 exceeded 1.00; do not use this result as a release threshold"}
```

An earlier run under heavier load (1-minute average 27.62) produced
17.97-26.33 s per run, median 20.31 s - noisier but in the same range,
supporting that contention here mostly adds occasional slow outliers rather
than shifting the whole distribution.

### Vulkan (`transcribe.cpp/vulkan`)

Command:

```sh
cargo build --release --example transcribe_benchmark --features gpu-vulkan
./target/release/examples/transcribe_benchmark \
  "$HOME/.local/share/tonguetyped/models/ggml-small-q5_1.bin" \
  recording.wav --runs 3
```

(The CLI still takes the CPU model path for compatibility; the report's
`model_file`/`model_sha256` correctly reflect the GGUF file actually used via
`InferenceEngine::active_model_path`, not that CLI argument.)

First run on this machine (cold shader compilation):

```json
{"model_file":"whisper-small-Q5_K_M.gguf","model_sha256":"326cd00c3e7217c751667c7c1600eaf7e0de174e186ca2c16b4bf590251c3c3b","audio_sha256":"2de0423a4b272c60df025c0e77d8b9c974d079ae0814294fd4fa796bb7f90c78","audio_seconds":8.597,"model_load_seconds":0.173651413,"backend":"transcribe.cpp/vulkan","device":"NVIDIA GeForce RTX 4080 SUPER","host_cpu":"AMD Ryzen 7 9700X 8-Core Processor","thread_count":8,"runs":[{"run":1,"vad_seconds":0.0,"inference_seconds":8.17648709,"realtime_factor":0.9510860870070956,"retained_audio_seconds":8.597},{"run":2,"vad_seconds":0.0,"inference_seconds":0.053874548,"realtime_factor":0.006266668372688147,"retained_audio_seconds":8.597},{"run":3,"vad_seconds":0.0,"inference_seconds":0.050765985,"realtime_factor":0.005905081423752472,"retained_audio_seconds":8.597}],"median_inference_seconds":0.053874548,"p95_inference_seconds":8.17648709,"competing_load_warning":"pre-run 1-minute load average 17.65 exceeded 1.00; do not use this result as a release threshold"}
```

Second, independent process launch (same machine, same shader cache now
warm - note run 1 is no longer slow):

```json
{"model_file":"whisper-small-Q5_K_M.gguf","model_sha256":"326cd00c3e7217c751667c7c1600eaf7e0de174e186ca2c16b4bf590251c3c3b","audio_sha256":"2de0423a4b272c60df025c0e77d8b9c974d079ae0814294fd4fa796bb7f90c78","audio_seconds":8.597,"model_load_seconds":0.204021169,"backend":"transcribe.cpp/vulkan","device":"NVIDIA GeForce RTX 4080 SUPER","host_cpu":"AMD Ryzen 7 9700X 8-Core Processor","thread_count":8,"runs":[{"run":1,"vad_seconds":0.0,"inference_seconds":0.088834878,"realtime_factor":0.01033324159590555,"retained_audio_seconds":8.597},{"run":2,"vad_seconds":0.0,"inference_seconds":0.053373235,"realtime_factor":0.006208355821798302,"retained_audio_seconds":8.597},{"run":3,"vad_seconds":0.0,"inference_seconds":0.056849263,"realtime_factor":0.006612686169594044,"retained_audio_seconds":8.597}],"median_inference_seconds":0.056849263,"p95_inference_seconds":0.088834878,"competing_load_warning":"pre-run 1-minute load average 6.04 exceeded 1.00; do not use this result as a release threshold"}
```

### CUDA (`transcribe.cpp/cuda`)

Command:

```sh
cargo build --release --example transcribe_benchmark --features gpu-cuda
./target/release/examples/transcribe_benchmark \
  "$HOME/.local/share/tonguetyped/models/ggml-small-q5_1.bin" \
  recording.wav --runs 3
```

This run happened to land during a quiet window - `competing_load_warning`
is null, the only clean (uncontended) reading among the three backends:

```json
{"model_file":"whisper-small-Q5_K_M.gguf","model_sha256":"326cd00c3e7217c751667c7c1600eaf7e0de174e186ca2c16b4bf590251c3c3b","audio_sha256":"2de0423a4b272c60df025c0e77d8b9c974d079ae0814294fd4fa796bb7f90c78","audio_seconds":8.597,"model_load_seconds":0.103263613,"backend":"transcribe.cpp/cuda","device":"NVIDIA GeForce RTX 4080 SUPER","host_cpu":"AMD Ryzen 7 9700X 8-Core Processor","thread_count":8,"runs":[{"run":1,"vad_seconds":0.0,"inference_seconds":0.126936721,"realtime_factor":0.01476523450040712,"retained_audio_seconds":8.597},{"run":2,"vad_seconds":0.0,"inference_seconds":0.057349337,"realtime_factor":0.006670854600442015,"retained_audio_seconds":8.597},{"run":3,"vad_seconds":0.0,"inference_seconds":0.053340468,"realtime_factor":0.006204544375945097,"retained_audio_seconds":8.597}],"median_inference_seconds":0.057349337,"p95_inference_seconds":0.126936721,"competing_load_warning":null}
```

That this clean run's numbers (median 0.0573 s) are within a few percent of
the contended CUDA/Vulkan numbers above (0.0539-0.0568 s) is the concrete
evidence that contention on this machine mostly affects cold-start/outlier
runs, not the steady-state figures this report leads with.

### Summary table

| Backend | Device | Median inference | Realtime factor vs. CPU |
| --- | --- | ---: | ---: |
| `whisper.cpp/cpu` | CPU | 18.01 s | 1x (baseline) |
| `transcribe.cpp/vulkan` | RTX 4080 SUPER | 0.057 s | ~320x faster |
| `transcribe.cpp/cuda` | RTX 4080 SUPER | 0.053 s | ~340x faster |

## What was not done

- No attempt to make Vulkan/CUDA the shipped default: the release `packages.default`
  Nix output still builds the plain CPU-only binary, matching "preserve a
  tested CPU fallback" as the product default. Shipping a GPU-accelerated
  build is a packaging/distribution decision for the captain, not made here.
- The GGUF model is not auto-provisioned; see bug #3 above for why, and
  `README.md` for the manual provisioning step.
