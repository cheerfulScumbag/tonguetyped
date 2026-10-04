# TongueTyped

TongueTyped is a Linux dictation application controlled from the terminal or a
desktop-wide keyboard shortcut. It records microphone audio, transcribes it
locally with whisper.cpp, and can type the result into the focused application.

## Requirements

TongueTyped requires Linux and an ALSA-compatible input device. Desktop shortcut
activation also requires an XDG Global Shortcuts portal; terminal IPC commands
remain available without one. Building from source requires Rust, CMake,
libclang, OpenSSL, pkg-config, ALSA development files, libxdo development
files, and Wayland client development files (for the optional layer-shell
overlay described under [Desktop feedback](#desktop-feedback); the daemon
still runs without a Wayland session, falling back as described there). The
Nix flake provides these build dependencies:

```sh
nix develop
cargo build --release
```

For direct typing, install at least one supported backend: `wtype`, an X11
environment supported by `enigo`, or `dotool`. Automatic backend selection tries
them in that order. TongueTyped never uses the clipboard as a typing fallback.

## Installation

Install the Nix package into your user profile:

```sh
nix profile add .
tonguetyped autostart enable
```

The package installs the binary and the
`io.github.cheerfulScumbag.tonguetyped.desktop` application entry.
The second command installs that same entry in your user autostart directory, so
KDE starts TongueTyped when you next sign in. Sign out and back in after the
first installation so the desktop portal discovers the Nix profile application
entry. KDE displays an authorization dialog for the default `Super+O` shortcut
when TongueTyped starts.

Upgrade the profile package without changing the autostart setting:

```sh
nix profile upgrade tonguetyped
```

Autostart can be disabled and re-enabled without removing the package:

```sh
tonguetyped autostart disable
tonguetyped autostart enable
```

Disable autostart before uninstalling so KDE does not retain a stale entry:

```sh
tonguetyped autostart disable
nix profile remove tonguetyped
```

Uninstalling leaves configuration, downloaded models, and transcription history
in the XDG config and data directories. Remove those directories separately only
if their contents are no longer needed.

## First run

Configure the microphone, model, shortcut, typing output, and autostart behavior:

```sh
tonguetyped setup
```

The setup flow discovers microphones and typing backends before presenting choices.
When both stdin and stdout are a terminal, `setup` opens an interactive Ratatui
console: arrow keys or `j`/`k` move the selection, `Enter` confirms a step, `Esc`
returns to the previous one, and `q` quits without saving. The microphone step
shows a live input level meter for whichever microphone is currently
highlighted after navigation settles, so you can compare devices without
restarting audio capture for every keypress. After the model step, the
console fetches any selected model file (CPU, and GPU on a GPU-feature
build) that isn't already installed, with progress shown before continuing.
Piped or non-interactive stdin/stdout (scripts, tests, CI) falls back to the
original line-based prompts, reading newline-separated answers from stdin
and leaving model downloads to the daemon or `tonguetyped model install`.

Either flow validates the complete configuration before replacing
`$XDG_CONFIG_HOME/tonguetyped/config.toml`. Enabling autostart installs
`$XDG_CONFIG_HOME/autostart/tonguetyped.desktop`; disabling it removes that entry.
Cancelling (`q`/`Esc` in the console, or `q` at any line-based prompt) leaves the
configuration and autostart entry unchanged.

For development outside the installed package, start the daemon from a graphical
desktop session:

```sh
cargo run -- daemon
```

Debug builds install or update the hidden
`$XDG_DATA_HOME/applications/io.github.cheerfulScumbag.tonguetyped.Devel.desktop`
entry before requesting shortcut authorization so the desktop portal can
identify the source build. Starting the daemon downloads the selected Whisper
model to `$XDG_DATA_HOME/tonguetyped/models` if needed. If the corresponding XDG
variables are unset, the standard user config and data directories are used. The
desktop portal may ask you to approve the configured shortcut, which defaults to
`Super+O` in hold mode.

The default output method is `none`, so transcription does not type or copy
anything. Retrieve the latest result with:

```sh
cargo run -- last-result
```

Run `tonguetyped setup` and choose `Type into the focused application` to type
transcripts directly. To test typing explicitly, focus a disposable text field
and run `cargo run -- doctor --test-type`. The regular `doctor` command checks
the compositor, microphone, model, and typing backends without injecting text
or requesting shortcut authorization. Run `cargo run -- shortcut-test`
explicitly to validate desktop shortcut authorization and binding.

## Desktop feedback

Visual feedback (the overlay) is enabled by default; sound feedback is not.
Disable the overlay or enable sound in `config.toml` without changing the
terminal workflow:

```toml
[overlay]
enabled = false

[audio]
feedback_sounds = true
feedback_volume = 0.7
feedback_device = "default"
```

The visual feedback identifies listening, transcription, completion,
cancellation, and failure without taking keyboard focus or accepting pointer
input. It prefers a small, click-through `wlr-layer-shell` overlay badge
positioned by `overlay.position` (`top-left`, `top-right`, `bottom-left`,
`bottom-right`, `top`, `bottom`, or `center`) and `overlay.monitor` (`active`,
or a specific output name from `tonguetyped doctor`). `zwlr_layer_shell_v1` is
a wlroots-originated protocol; it is available on wlroots-based compositors
(code-reviewed against Mango, but not live-validated) and on modern KWin
(validated against KDE Plasma 6.7+), but not on X11 sessions or older/other
compositors that never advertise it. Where it is unavailable, TongueTyped falls back to Plasma's
native OSD on KDE, then a transient freedesktop notification -- in that
fallback tier, the desktop environment controls placement and monitor
selection, so `overlay.position`/`overlay.monitor` have no effect. Run
`tonguetyped doctor` to see which tier is active.

Sound feedback uses the desktop sound theme through `canberra-gtk-play` and
accepts a volume from `0.0` through `1.0`. The Nix package includes this helper;
source and development builds require it on `PATH`. A non-default
`feedback_device` is passed as `PULSE_SINK`, which works with PulseAudio and
PipeWire's PulseAudio compatibility service. A missing sound helper disables
only sound feedback. Visual feedback is unavailable only when neither the
layer-shell overlay, Plasma's OSD, nor a desktop notification service can be
reached. Recording and terminal commands continue to work. TongueTyped does
not inject text or move focus when it reports state.

## Commands

The CLI provides these commands:

```text
tonguetyped daemon         Start the daemon
tonguetyped setup          Configure TongueTyped interactively
tonguetyped start          Start recording
tonguetyped stop           Stop and transcribe the recording
tonguetyped toggle         Start or stop recording
tonguetyped cancel         Cancel recording or discard in-flight processing
tonguetyped status         Show daemon state, activation mode, and shortcut health
tonguetyped reload         Validate and reload the config
tonguetyped last-result    Print the latest transcription
tonguetyped doctor         Check runtime dependencies
tonguetyped shortcut-test  Interactively test shortcut authorization and binding
tonguetyped autostart      Enable or disable desktop-session autostart
tonguetyped model          Manage the GGUF speech model catalog (GPU backend)
```

The daemon listens on `$XDG_RUNTIME_DIR/tonguetyped/control.sock` and refuses to
start a second instance. Recordings stop at `transcription.max_recording_seconds`
even if no stop command arrives. Voice activity detection trims retained audio;
it does not stop a recording automatically.

When history is enabled, TongueTyped stores transcript text in
`$XDG_DATA_HOME/tonguetyped/history.db` with user-only permissions. Disable
`history.enabled` to keep only the current daemon's latest result in memory.

## Latency diagnostics

`tonguetyped doctor` reports the selected model ID and the detected inference
backend and device. The daemon logs the same inference configuration at startup.

After each dictation finishes or is cancelled, the daemon writes one structured
`tonguetyped::latency` event at the `info` level. The event identifies the model,
backend, device, outcome, and whether model loading was cold. It reports separate
durations for audio finalization, VAD, model loading, inference, output, and
history, plus the total time from the earliest stop boundary until the daemon is
idle. For hold-mode activation, the total begins when the key is released, while
audio finalization begins when TongueTyped sends the stop signal after its
50-millisecond auto-repeat check. Disabled or unreached phases are zero. Latency
events contain no transcript text or audio.

## Transcription benchmark

Measure cold model loading and repeated warm inference with a 16 kHz mono WAV
file:

```sh
cargo run --release --example transcribe_benchmark -- \
  "$HOME/.local/share/tonguetyped/models/ggml-small-q5_1.bin" \
  recording.wav --runs 5
```

Pass `--vad-model PATH` to include production VAD before each inference run. The
benchmark writes one JSON record containing raw runs, median, p95, model and
audio SHA-256 hashes, host CPU, thread count, backend, device, and a competing
load warning. It does not print transcript text. Compare builds with the same
model, WAV, release profile, and otherwise idle machine. Do not use a result as
a release threshold when `competing_load_warning` is non-null.

A recovery run on an AMD Ryzen 7 9700X (8 physical cores, 16 logical CPUs) used
the `small-q5_1` model. Its 8.597-second synthetic speech WAV was generated with
FFmpeg's `flite` source:

```sh
ffmpeg -f lavfi \
  -i "flite=text='Today I am testing local speech recognition. The microphone records my voice and the computer converts each sentence into written text.'" \
  -ar 16000 -ac 1 recording.wav
```

The repository includes a benchmark source compatible with baseline commit
`a91a123`. Copy it into an archive of that revision, then run both examples with
the same input, language, timing boundaries, and default model-loading behavior:

```sh
baseline_dir="$(mktemp -d)"
git archive a91a1236d26f15f972e7e30089cbb4acbf5f578f | \
  tar -x -C "$baseline_dir"
mkdir "$baseline_dir/examples"
cp examples/transcribe_benchmark_baseline.rs \
  "$baseline_dir/examples/transcribe_benchmark.rs"
cargo run --release --manifest-path "$baseline_dir/Cargo.toml" \
  --example transcribe_benchmark -- \
  "$HOME/.local/share/tonguetyped/models/ggml-small-q5_1.bin" \
  "$(pwd)/recording.wav"
rm -rf "$baseline_dir"

cargo run --release --example transcribe_benchmark -- \
  "$HOME/.local/share/tonguetyped/models/ggml-small-q5_1.bin" \
  recording.wav
```

The release build of baseline commit `a91a123` took 30.917 seconds. The final
8-thread build, using the same default load parameters, took 19.512 seconds.
Whisper.cpp reported that flash attention was enabled, no GPU was available,
and the CPU backend was used. Both runs produced this exact transcript:

```text
Today I am testing local speech recognition, the microphone records my voice and the computer converts each sentence into written text.
```

## GPU inference backends

Two optional Cargo features add GPU inference on top of the tested CPU path,
using the official Rust binding for
[transcribe.cpp](https://github.com/handy-computer/transcribe.cpp)
(the `transcribe-cpp` crate):

```sh
cargo build --release --features gpu-vulkan   # or --features gpu-cuda
```

Neither feature is enabled by Cargo's default feature set, so a plain
`cargo build` never links against Vulkan, CUDA, or transcribe-cpp. The flake's
`packages.default` enables `gpu-vulkan`, which gives `nix build` and
`nix profile install` GPU acceleration on Vulkan-capable systems while keeping
the tested CPU fallback on systems without a usable GPU or installed GGUF
model. If both features are enabled, CUDA takes priority. Building `gpu-cuda`
on NixOS additionally needs
`cudaPackages.cudatoolkit` and the driver's `/run/opengl-driver/lib` on the
link path; the flake's devShell and `build.rs` set this up automatically.

When a GPU feature is compiled in, `InferenceEngine::load` tries that backend
first, against a separately downloaded GGUF model - by default
`whisper-small-Q5_K_M.gguf` from
[handy-computer/whisper-small-gguf](https://huggingface.co/handy-computer/whisper-small-gguf),
chosen to match the CPU path's `small` model at a comparable quantization.
If the GGUF file is missing, or the backend fails to load or run, it logs a
warning and falls back to the same tested CPU path used by a plain build -
this fallback forces `use_gpu: false` explicitly, since whisper-rs otherwise
opportunistically uses whatever GPU backend it was linked against, which is
not what "tested CPU fallback" should mean. `tonguetyped doctor` and the
daemon's startup log report whichever backend is actually active
(`whisper.cpp/cpu`, `transcribe.cpp/vulkan`, or `transcribe.cpp/cuda`) and its
device. The GPU model is not eagerly downloaded at daemon startup (that would
block every launch on a large synchronous fetch); the interactive `tonguetyped
setup` console fetches it (and the CPU model) if either is missing, or
provision it directly with `tonguetyped model install <id>` before using the
GPU path or running the benchmark with a GPU feature - a missing file
silently falls back to the CPU path.

### GGUF model catalog (`tonguetyped model`)

The GPU backend isn't limited to that one default model. `src/catalog.rs`
holds a reviewed catalog of every `family = "whisper"` GGUF model published by
[handy-computer](https://huggingface.co/handy-computer), derived from
[transcribe.cpp](https://github.com/handy-computer/transcribe.cpp)'s release
`catalog.db` and cross-verified against HuggingFace's own metadata for each
file (see `data/tt-model-catalog-19/report.md` for the derivation process and
why other catalog.db families - canary, parakeet, voxtral, moonshine,
sortformer diarization, and so on - are left out: they're real transcribe.cpp
models, just not yet integration-tested against this project's single-shot
usage). Every entry is pinned to a specific commit (not `main`) with an
expected byte size and SHA-256, so what actually downloads can't drift from
what was reviewed.

```sh
tonguetyped model list                          # catalog + install status
tonguetyped model install whisper-tiny-q5_k_m   # resumable, verified download
tonguetyped model install whisper-tiny-q5_k_m --use   # and select it
tonguetyped model use whisper-tiny-q5_k_m       # switch the active GPU model
tonguetyped model remove whisper-tiny-q5_k_m    # delete a non-active install
```

`install` resumes an interrupted download from its partial `.download` file
when the server supports HTTP range requests, and always re-verifies the full
file's SHA-256 before making it live - a corrupt or mismatched download is
deleted rather than left in place. The selected model is stored in
`model.gpu_model` in `config.toml`; it's ignored by builds without a GPU
feature enabled, and the CPU model (`model.selected`, still just
`whisper-small-q5_1`) is unaffected by any of this.

### Benchmark: Vulkan and CUDA vs. the CPU baseline

The same `transcribe_benchmark` example used for the CPU baseline reports
whichever backend the build and hardware actually select, so it doubles as
the GPU benchmark - just build it with a GPU feature and it hashes and
reports the GGUF file it actually ran against
(`InferenceEngine::active_model_path`), not the CPU model path still passed
on the CLI:

```sh
cargo build --release --example transcribe_benchmark --features gpu-vulkan
./target/release/examples/transcribe_benchmark \
  "$HOME/.local/share/tonguetyped/models/ggml-small-q5_1.bin" \
  recording.wav --runs 3
```

Measured on an AMD Ryzen 7 9700X (8 physical cores) with an NVIDIA GeForce
RTX 4080 SUPER (16 GB VRAM), using the exact same 8.597-second input WAV as
the CPU baseline above (SHA-256 `2de0423a...f90c78`):

| Backend | Device | Median inference | Realtime factor | Cold first inference |
| --- | --- | ---: | ---: | ---: |
| `whisper.cpp/cpu` | CPU | 18.01 s | 2.10x (slower than realtime) | same as median |
| `transcribe.cpp/vulkan` | RTX 4080 SUPER | 0.057 s | 0.0066x (~320x faster than CPU) | 0.05-8.2 s (see note) |
| `transcribe.cpp/cuda` | RTX 4080 SUPER | 0.053 s | 0.0062x (~340x faster than CPU) | 0.09-0.13 s |

Both GPU backends produce the exact same transcript as the CPU path
(byte-for-byte, verified separately from the benchmark tool, which discards
transcript text). Full raw JSON reports, including per-run timings and the
`competing_load_warning` field, are in
`data/tt-transcribe-cpp-gpu-18/report.md`.

**Vulkan's first-ever inference on a given machine pays a one-time shader
compilation cost** (observed once at 8.18 s; every run after that, including
across separate process launches, was 0.05-0.09 s) - the NVIDIA driver
caches compiled Vulkan pipelines to disk, so this is a single per-machine
cost, not a per-process or per-dictation one. CUDA's cold start was
consistently under 0.13 s with no such spike. If a deployment relies on
`model.idle_unload` with a short timeout on a machine whose shader cache gets
cleared (e.g. driver updates, cache eviction), Vulkan's reload cost is worth
being aware of; CUDA does not have this characteristic on the hardware
tested.

**Competing load, honestly**: this machine runs several concurrent build
lanes, so most runs above show a non-null `competing_load_warning` (CPU load
average or GPU free-memory pressure from another process). Every backend's
*median inference time* was nonetheless stable within a few percent across
contended and quiet runs (contention mostly costs a slower first/cold run,
not the steady-state number) - but per the pipeline's own release-threshold
guidance, none of these numbers should be read as a clean baseline. A truly
idle run of the CUDA case did occur (`competing_load_warning: null`) and its
numbers match the contended runs closely, which is the best available
evidence that the contention here did not meaningfully distort the
comparison.
