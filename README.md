# TongueTyped

TongueTyped is a Linux dictation application controlled from the terminal or a
desktop-wide keyboard shortcut. It records microphone audio, transcribes it
locally with [transcribe.cpp](https://github.com/handy-computer/transcribe.cpp)
(GGUF models, CPU or an accelerator), and can type the result into the focused
application.

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

Typing into the focused application uses one of three backends: `wtype`,
`enigo` in an X11 session, or `dotool`. Automatic backend selection tries
them in that order, and only a backend that actually works is ever used.
`wtype` types through the Wayland virtual-keyboard protocol, which only
wlroots-based compositors (sway, Hyprland, niri) implement; KDE's KWin and
other Wayland compositors do not, so `wtype` can never type there. `dotool`
works on any compositor by typing through `/dev/uinput`, which needs write
access to that device - membership in the `input` group or a matching udev
rule - and is not granted by default, so a present `dotool` that cannot open
`/dev/uinput` is treated as unavailable. The packaged installs carry the
helpers with them: the Nix package bundles `wtype`, `dotool`, and
`wl-clipboard` into its wrapper and the Arch package depends on all three, so
a normal install can type out of the box and has the Wayland clipboard tools
available; the .deb recommends `wtype` and `wl-clipboard` only - Debian does
not package `dotool`, so a KDE Wayland user installing from the .deb must
build `dotool` from source (`go build` from its upstream repository) to type
on KWin. When no typing helper is available, the dashboard and the
setup wizard say so and name what to install for this compositor (`dotool` on
KDE and other non-wlroots compositors, `wtype` on wlroots) instead of hiding
the typing options. TongueTyped never uses the clipboard as a typing fallback.

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
entry. TongueTyped registers the "Start or stop dictation" action with your
desktop but does not choose a shortcut for it: set the key from your desktop's
own shortcut dialog (or the dashboard's Shortcut screen, which opens it for
you).

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

Configure the microphone, model, shortcut, typing output, overlay, and autostart
behavior:

```sh
tonguetyped setup
```

The setup flow discovers microphones, typing backends, and inference backends
before presenting choices.
When both stdin and stdout are a terminal, `setup` opens an interactive Ratatui
console: arrow keys or `j`/`k` move the selection, `Enter` confirms a step, `Esc`
returns to the previous one, and `q` quits without saving. The microphone step
shows a live input level meter for whichever microphone is currently
highlighted after navigation settles, so you can compare devices without
restarting audio capture for every keypress. On the shortcut step, typing a
new combination only takes effect the first time a shortcut is ever bound;
to change an already-bound shortcut, press `Ctrl+R` to open the desktop's
own native "press your new shortcut" dialog instead. After the model step, the
console fetches the selected GGUF model file if it isn't already installed,
with progress shown before continuing - the same file every backend (CPU or
an accelerator) requests, so there is only ever one model to fetch. It then
offers the inference backend step, listing the backends this build and host
can use; a backend that can't run here is shown with its reason instead of
being selectable, except a configured backend that has become unusable, which
stays in the list marked unavailable so stepping through the step can't
silently replace it.
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
identify the source build. Starting the daemon downloads the selected GGUF
model to `$XDG_DATA_HOME/tonguetyped/models` if needed. If the corresponding XDG
variables are unset, the standard user config and data directories are used. The
desktop portal may ask you to approve the "Start or stop dictation" action; it
ships with no default key, so set your own in the desktop's shortcut dialog (the
dashboard's Shortcut screen opens it).

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
explicitly, then press the configured shortcut; the command waits up to 15
seconds and reports whether the press was actually detected.

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
or a specific output name from `tonguetyped doctor`). `overlay.style`
(`badge`, `minimal`, or `pill`) picks the overlay's look, and
`overlay.streaming_indicator` (`false` by default) swaps the plain pulsing
dot for a busier multi-bar animation during recording. `tonguetyped setup`
prompts for all of these. `zwlr_layer_shell_v1` is a wlroots-originated
protocol; it is available on wlroots-based compositors
(code-reviewed against Mango, but not live-validated) and on modern KWin
(validated against KDE Plasma 6.7+), but not on X11 sessions or older/other
compositors that never advertise it. Where it is unavailable, TongueTyped
falls back to Plasma's native OSD on KDE, then a transient freedesktop
notification - in that fallback tier, the desktop environment controls
placement and monitor selection, so `overlay.position`/`overlay.monitor` have
no effect. Run `tonguetyped doctor` to see which tier is active.

Sound feedback uses the desktop sound theme through `canberra-gtk-play` and
accepts a volume from `0.0` through `1.0`. The Nix package includes this helper;
source and development builds require it on `PATH`. A non-default
`feedback_device` is passed as `PULSE_SINK`, which works with PulseAudio and
PipeWire's PulseAudio compatibility service. A missing sound helper disables
only sound feedback. Visual feedback is unavailable only when neither the
layer-shell overlay, Plasma's OSD, nor a desktop notification service can be
reached. Recording and terminal commands continue to work. TongueTyped does
not inject text or move focus when it reports state.

## Dashboard

Running `tonguetyped` with no subcommand opens an interactive terminal dashboard.
Its home screen is a settings overview - one row each for Model, Microphone,
Activation, Shortcut, Transcript output, Typing backend, Startup, and Overlay,
showing the current value - above a list of the remaining commands below.
Selecting a settings row opens a screen that edits just that area, with the same
live microphone preview and shortcut test/native dialog as `tonguetyped setup`,
and the Model screen also selects the inference backend (`Tab` switches between
the model catalog and the backend list). `start` and `stop` are
omitted from the dashboard's list in favor of `toggle` and `cancel` for recording
control, but every subcommand remains directly invocable on its own, and
`tonguetyped --help` still lists them all.

## Commands

The CLI provides these commands:

```text
tonguetyped daemon         Start the daemon
tonguetyped daemon stop    Gracefully stop the running daemon
tonguetyped daemon restart Stop the running daemon, then start a fresh one
tonguetyped setup          Configure TongueTyped interactively
tonguetyped start          Start recording
tonguetyped stop           Stop and transcribe the recording
tonguetyped toggle         Start or stop recording
tonguetyped cancel         Cancel recording or discard in-flight processing
tonguetyped status         Show daemon state, activation mode, and shortcut health
tonguetyped reload         Validate and reload the config
tonguetyped last-result    Print the latest transcription
tonguetyped doctor         Check runtime dependencies
tonguetyped shortcut-test  Bind the desktop shortcut and wait for you to press it
tonguetyped autostart      Enable or disable desktop-session autostart
tonguetyped model          Manage the GGUF speech model catalog
```

The daemon listens on `$XDG_RUNTIME_DIR/tonguetyped/control.sock` and refuses to
start a second instance. Recordings stop at `transcription.max_recording_seconds`
even if no stop command arrives. Voice activity detection trims retained audio;
it does not stop a recording automatically.

When history is enabled, TongueTyped stores transcript text in
`$XDG_DATA_HOME/tonguetyped/history.db` with user-only permissions. Disable
`history.enabled` to keep only the current daemon's latest result in memory.

## Latency diagnostics

`tonguetyped doctor` reports the selected model ID, the configured backend
preference, and the detected inference backend and device. It also prints its
own build identifier - the git commit it
was built from - and compares it with the build the running daemon reports,
flagging a daemon left running from an older build. `tonguetyped --version`
prints this binary's identifier, and `tonguetyped status` the daemon's. The
daemon logs the same inference configuration at startup.

After each dictation finishes or is cancelled, the daemon writes one structured
`tonguetyped::latency` event at the `info` level. The event identifies the model,
backend, device, outcome, and whether model loading was cold. It reports separate
durations for audio finalization, VAD, model loading, inference, output, and
history, plus the total time from the earliest stop boundary until the daemon is
idle. For hold-mode activation, the total begins when the key is released, while
audio finalization begins when TongueTyped sends the stop signal after its
50-millisecond auto-repeat check. Disabled or unreached phases are zero. Latency
events contain no transcript text or audio.

A load is cold on the daemon's first dictation, and again whenever the model has
been unloaded from memory since the last one. By default the daemon unloads the
model after 15 minutes of inactivity, so the next dictation after a quiet period
pays that cold-load cost again:

```toml
[model.idle_unload]
policy = "after_idle"       # "never", "after_transcription", or "after_idle"
timeout_minutes = 15
```

Set `policy` to `"after_transcription"` to free the memory after every dictation,
or `"never"` to keep the model loaded for the daemon's entire lifetime.
`timeout_minutes` only applies to `"after_idle"`.

## Transcription benchmark

Measure cold model loading and repeated warm inference with a 16 kHz mono WAV
file:

```sh
cargo run --release --example transcribe_benchmark -- \
  "$HOME/.local/share/tonguetyped/models/whisper-small-Q5_K_M.gguf" \
  recording.wav --runs 5
```

Pass `--vad-model PATH` to include production VAD before each inference run.
Pass `--backend NAME` (`auto`, `cpu`, `vulkan`, `cuda`, `rocm`, or `metal`;
default `auto`) to force one inference backend. The
benchmark writes one JSON record containing raw runs, median, p95, model and
audio SHA-256 hashes, host CPU, thread count, backend, device, and a competing
load warning. It does not print transcript text. Compare builds with the same
model, WAV, release profile, and otherwise idle machine. Do not use a result as
a release threshold when `competing_load_warning` is non-null.

A recovery run on an AMD Ryzen 7 9700X (8 physical cores, 16 logical CPUs) used
the default `whisper-small-q5_k_m` model. Its 8.597-second synthetic speech WAV
was generated with FFmpeg's `flite` source:

```sh
ffmpeg -f lavfi \
  -i "flite=text='Today I am testing local speech recognition. The microphone records my voice and the computer converts each sentence into written text.'" \
  -ar 16000 -ac 1 recording.wav
```

See `data/tt-transcribe-cpp-1/report.md` for the historical
whisper.cpp/transcribe-rs CPU baseline this project started from, and for
this consolidation's own before/after evidence (dual-path reproduction, then
the single transcribe.cpp module on CPU, Vulkan, and CUDA, all producing the
exact same transcript from the exact same GGUF file).

## Inference backends

One module, `src/inference.rs`, handles every backend - CPU, Vulkan, CUDA,
ROCm, and Metal - through a single Rust binding for
[transcribe.cpp](https://github.com/handy-computer/transcribe.cpp) (the
`transcribe-cpp` crate). A plain `cargo build` links transcribe.cpp with no
accelerator compiled in (CPU only); four optional Cargo features additionally
compile in one accelerator each:

```sh
cargo build --release --features gpu-vulkan   # or gpu-cuda, gpu-rocm, gpu-metal
```

`InferenceEngine::load` always requests an explicit backend (never letting the
library auto-select). By default (`model.preferred_backend = "auto"`), it tries
accelerators in a fixed priority order (CUDA, ROCm, Vulkan, Metal) before an
unconditional, always-available explicit CPU request. Set any other value -
`cpu`, `vulkan`, `cuda`, `rocm`, or `metal` - to pin `load` to that one backend
instead: if it can't run here (the feature wasn't compiled in, or no usable
device or driver was found), the load fails with a named error rather than
quietly running on a different backend. `tonguetyped setup` and the dashboard's
Model screen both offer this choice, showing any backend this build and host
can't use as unavailable. Every attempt loads the exact same configured
GGUF file, so a fallback never substitutes a different model or model family.
The flake's `packages.default` enables `gpu-vulkan`, which gives `nix build` and
`nix profile install` accelerated inference on Vulkan-capable systems while
keeping the tested CPU fallback on systems without a usable GPU or an
installed GGUF model. Building `gpu-cuda` on NixOS additionally needs
`cudaPackages.cudatoolkit` and the driver's `/run/opengl-driver/lib` on the
link path; the flake's devShell and `build.rs` set this up automatically.

If the configured model's GGUF file is missing, or the backend in play fails to
load or run it, `InferenceEngine::load` returns an error (no silent partial
success). `tonguetyped doctor` reports the configured preference and whichever
backend actually ended up active (`transcribe.cpp/cpu`,
`transcribe.cpp/vulkan`, `transcribe.cpp/cuda`, `transcribe.cpp/rocm`, or
`transcribe.cpp/metal`) and its device - read directly off the loaded model,
not guessed from which Cargo features were compiled in. With no model loaded,
doctor and the daemon's startup log report the pinned backend, or the `auto`
chain's capability probe. `doctor` separately lists every backend kind this
build and host can use (CUDA, ROCm, Vulkan,
Metal, CPU) in inference priority order, so a compiled-in accelerator with no
usable device shows as unavailable next to the always-available CPU fallback.
`daemon.rs::prepare_dependencies` fetches the configured model synchronously
at startup if it is missing, matching the historical CPU path's zero-config
behavior (now also covering
every accelerator, since they all share the one file). Provision it ahead of
time with `tonguetyped model install <id>` to avoid that first-launch wait,
or let `tonguetyped setup` fetch it as part of the interactive flow.

### GGUF model catalog (`tonguetyped model`)

`src/catalog.rs` holds a reviewed catalog of every `family = "whisper"` GGUF
model published by [handy-computer](https://huggingface.co/handy-computer),
derived from transcribe.cpp's release `catalog.db` and cross-verified against
HuggingFace's own metadata for each file (see
`data/tt-model-catalog-19/report.md` for the original derivation, and
`data/tt-transcribe-cpp-1/report.md` for why other catalog.db families -
canary, parakeet, voxtral, moonshine, sortformer diarization, and so on -
remain left out even though the CPU path can now load any of them too: their
output/chunking semantics have not been integration-tested against this
project's single-shot usage). Every entry is pinned to a specific commit (not
`main`) with an expected byte size and SHA-256, so what actually downloads
can't drift from what was reviewed.

```sh
tonguetyped model list                          # catalog + install status
tonguetyped model install whisper-tiny-q5_k_m   # resumable, verified download
tonguetyped model install whisper-tiny-q5_k_m --use   # and select it
tonguetyped model use whisper-tiny-q5_k_m       # switch the active model
tonguetyped model remove whisper-tiny-q5_k_m    # delete a non-active install
```

`install` resumes an interrupted download from its partial `.download` file
when the server supports HTTP range requests, and always re-verifies the full
file's SHA-256 before making it live - a corrupt or mismatched download is
deleted rather than left in place. The selected model is stored in
`model.active_model` in `config.toml` and used by every backend alike - there
is no separate CPU or GPU model anymore. A config written by an older version
(`model.selected`/`model.gpu_model`) is migrated to `active_model`
automatically the first time it loads, with no edits required.
