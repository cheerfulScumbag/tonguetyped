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

Key crates: cpal (audio capture), rubato (resampling), transcribe-cpp (inference, every
backend), vad-rs (Silero VAD), rusqlite (history). See Cargo.toml for full dep list.

Inference is one module, `InferenceEngine` (`src/inference.rs`), built entirely on the
`transcribe-cpp` binding (github.com/handy-computer/transcribe.cpp) - there is no
separate CPU-only library or legacy GGML model anymore (removed in
`data/tt-transcribe-cpp-1/report.md`; see it for the full before/after story and real
CPU/Vulkan/CUDA benchmark+transcript evidence). `load()` always requests an explicit
backend (never `Backend::Auto`), trying `Cuda`, `Rocm`, `Vulkan`, `Metal`, then the
unconditional, always-available `Cpu`, all against the one configured GGUF file
(`config.model.active_model`, resolved via `catalog::model_path`) - a backend whose
Cargo feature (`gpu-vulkan`/`gpu-cuda`/`gpu-rocm`/`gpu-metal`) wasn't compiled in simply
isn't natively satisfiable (`Error::Backend`), so this fallback chain needs no `cfg`
gating and is correct on every build by construction. None of the four features is
enabled by Cargo's default set, while `flake.nix`'s `packages.default` enables
`gpu-vulkan` for `nix build`/`nix profile install`.
Report backend/device by reading the *loaded* model (`Model::device()`), not by
re-probing: prefer `Device::kind` over `Model::backend()` for the category label -
the latter returns device-indexed strings in practice (`"Vulkan0"`, uppercase `"CPU"`),
not the clean lowercase vocabulary (`"cpu"`, `"vulkan"`, ...) its own doc comment
suggests and `Device::kind` actually documents.
`backend_info()`/`cached_backend_info()` (the *hardware-capability* probe, used only
before any model has loaded - `doctor`'s fallback, the daemon startup log) keep the
prior blocking-vs-non-blocking split: a fresh `backend_available`/`devices()` call is
not free (~100ms first call) and must not land on the stop-to-idle hot path. Once an
engine has actually loaded, though, `active_backend_info()` is cheap enough to call
directly from the hot path (`coordinator.rs`) - the expensive part is backend
*initialization*, already paid by `Model::load_with`, not the metadata read after.

Building `gpu-cuda` on NixOS needs `cudaPackages.cudatoolkit` and the driver's
`/run/opengl-driver/lib` (`libcuda.so`) on the link path; `flake.nix`'s devShell and
the crate-root `build.rs` (scoped to `CARGO_FEATURE_GPU_CUDA`) handle this - `rustc`'s
linker (`rust-lld`, invoked directly) does not honor `LIBRARY_PATH`, only explicit
`-L`/`-l`, so `cargo:rustc-link-search`/`-lib` in `build.rs` is the fix, not env vars.

The model catalog (`src/catalog.rs`, managed with `tonguetyped model
{list,install,remove,use}`) is scoped to `family = 'whisper'` GGUF models only, not
transcribe.cpp's full catalog.db. The CPU path can now load any family architecturally
(that's the whole point of the consolidation in `data/tt-transcribe-cpp-1/report.md`),
but other families' output/chunking semantics (diarization, streaming-only, non-whisper
long-form strategies) have never been exercised against this project's single-shot
`Session::run` usage. See `data/tt-model-catalog-19/report.md` before widening the
catalog to a new family.

Neither `setup.rs`'s line-based flow nor the `nix build`/`nix profile install` default
made the configured GGUF model show up without a manual `tonguetyped model install` -
`data/tt-transcribe-cpp-gpu-18/report.md` documents that as a deliberate choice to keep
daemon startup non-blocking, but it meant a fresh install silently ran CPU-only
inference with no indication why. The setup console (`src/setup.rs`'s
`model_requirements`/`provision_model_async`, wired into `src/setup/console.rs`'s
`Downloading` step) fetches it when missing, with progress and errors visible before
the wizard finishes; `daemon.rs::prepare_dependencies` also fetches it synchronously at
daemon startup if absent (there is only the one file to fetch now, so this is no longer
the eager-GPU-prefetch tradeoff that report weighed against).
`model::DownloadManager`'s download methods take an `Option<ProgressCallback>`:
`None` keeps the existing indicatif terminal bar (CLI, scripted `configure()`); the
Ratatui console passes `Some` and draws its own `Gauge` instead, since indicatif and
ratatui's alternate screen would otherwise fight over the same terminal.

`setup::run` (`src/setup.rs`) picks between two UIs by checking whether both
stdin and stdout are a terminal (`IsTerminal`): an interactive Ratatui console
(`src/setup/console.rs`) when both are a TTY, else the original line-based
prompt flow in `setup.rs` itself (kept so scripts/tests/CI can still drive
`tonguetyped setup` with piped newline-separated answers - see
`tests/setup_cli.rs`). The console's microphone step opens a live
`audio::AudioRecorder` (with a `level_callback`) on whichever device remains
highlighted after a short navigation settle period, not just the confirmed
choice. The delay prevents rapid stream teardown and recreation while retaining
the live VU preview.

A stack-allocated buffer inside an `async fn` is embedded inline in the generated
state machine across every `.await` point in that function (not heap-allocated), and
compounds when awaited from other async functions - `src/model.rs`'s SHA-256
verifier hit a real stack overflow from a 1 MiB buffer for exactly this reason
(fixed at 64 KiB, matching `examples/transcribe_benchmark.rs`'s synchronous hasher).
Keep buffers inside `async fn` bodies small, or hash on a blocking thread instead.

The visual feedback overlay (`src/overlay.rs`, wired from `feedback.rs`'s
`DesktopFeedback`) is a `zwlr_layer_shell_v1` surface built on
`smithay-client-toolkit` (calloop feature only, `xkbcommon` off - no seat/keyboard
use). `zwlr_layer_shell_v1` is a wlroots-originated protocol, but modern KWin
advertises it too (confirmed on KWin 6.7 via `wayland-info`); `OverlayHandle::
try_send` probes for the global once per process (`OnceLock`-cached, same
probe-then-cache shape as the inference module's `backend_info`/
`cached_backend_info` split above) and `feedback.rs` falls back to the
existing Plasma OSD / notification / sound chain whenever it's absent (X11
sessions, compositors that never added
it). The layer surface is created once, lazily, on the first event and kept
transparent-but-mapped between dictations rather than being torn down, so
there's no per-dictation Wayland round trip on the stop-to-idle hot path.
`examples/overlay_preview.rs` cycles or holds each semantic state for manual
validation on a given compositor (`cargo run --example overlay_preview --
top-right recording`); screenshotting it requires a compositor-native tool
(`grim` needs wlr-screencopy, which KWin doesn't have - use `spectacle -b -f -n
-o out.png` there) and, on a multi-monitor KWin session, checking every
output's corner, since `output: None` (config's `monitor = "active"`) places
the surface on whichever output KWin currently considers focused, not
necessarily the one at the top-left of the combined virtual screen.

`config::OverlayConfig::enabled` defaults to `true` (changed from `false`): this is
the only visual dictation feedback there is - `feedback.rs::DesktopFeedback::send`
skips its visual branch entirely when `overlay.enabled` is false, regardless of
whether the layer-shell overlay or its OSD/notification fallback would have worked,
so a `false` default meant a correctly-configured KDE Wayland install still showed
nothing on screen while recording. Changing the Rust default does not touch an
already-written `config.toml`; `tonguetyped setup` is how an existing install picks
up the new default.

`output::type_text`'s `typing_backend = "auto"` path now caches which helper
(`wtype`/`enigo`/`dotool`) actually works via `output::cached_auto_backend`
(`OnceLock`, same probe-then-cache shape as the inference and overlay probes above) -
`probe_type_backend()`'s subprocess self-tests were re-run on every single
dictation's output phase before this, measured at just over 2s in the same real
`journalctl` latency line referenced above.
