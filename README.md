# TongueTyped

TongueTyped is a Linux dictation application controlled from the terminal or a
desktop-wide keyboard shortcut. It records microphone audio, transcribes it
locally with whisper.cpp, and can type the result into the focused application.

## Requirements

TongueTyped requires Linux and an ALSA-compatible input device. Desktop shortcut
activation also requires an XDG Global Shortcuts portal; terminal IPC commands
remain available without one. Building from source requires Rust, CMake,
libclang, OpenSSL, pkg-config, ALSA development files, and libxdo development
files. The Nix flake provides these build dependencies:

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

The package installs the binary and the `tonguetyped.desktop` application entry.
The second command installs that same entry in your user autostart directory, so
KDE starts TongueTyped when you next sign in. Launch TongueTyped from your
desktop's application menu once after installation so the Global Shortcuts
portal can identify it. KDE displays an authorization dialog for the default
`Super+O` shortcut on first launch.

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
It validates the complete configuration before replacing
`$XDG_CONFIG_HOME/tonguetyped/config.toml`. Enabling autostart installs
`$XDG_CONFIG_HOME/autostart/tonguetyped.desktop`; disabling it removes that entry.
Enter `q` at any prompt to leave the configuration and autostart entry unchanged.

For development outside the installed package, start the daemon from a graphical
desktop session:

```sh
cargo run -- daemon
```

Starting the daemon downloads the selected Whisper model to
`$XDG_DATA_HOME/tonguetyped/models` if needed. If the corresponding XDG variables
are unset, the standard user config and data directories are used. The desktop
portal may ask you to approve the configured shortcut, which defaults to
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

Visual and sound feedback are disabled by default. Enable either one in
`config.toml` without changing the terminal workflow:

```toml
[overlay]
enabled = true

[audio]
feedback_sounds = true
feedback_volume = 0.7
feedback_device = "default"
```

The visual feedback identifies listening, transcription, completion,
cancellation, and failure without taking keyboard focus. On KDE it uses
Plasma's native OSD and falls back to a transient freedesktop notification if
the OSD service is unavailable. Plasma and the notification daemon control
placement and monitor selection, so the existing `overlay.position` and
`overlay.monitor` settings do not override desktop accessibility or
multi-monitor policy.

Sound feedback uses the desktop sound theme through `canberra-gtk-play` and
accepts a volume from `0.0` through `1.0`. A non-default `feedback_device` is
passed as `PULSE_SINK`, which works with PulseAudio and PipeWire's PulseAudio
compatibility service. Missing desktop helpers disable only the unavailable
sound channel; a missing desktop notification service disables visual feedback.
Recording and terminal commands continue to work. TongueTyped does not add
animation, inject text, or move focus when it reports state.

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
```

The daemon listens on `$XDG_RUNTIME_DIR/tonguetyped/control.sock` and refuses to
start a second instance. Recordings stop at `transcription.max_recording_seconds`
even if no stop command arrives. Voice activity detection trims retained audio;
it does not stop a recording automatically.

When history is enabled, TongueTyped stores transcript text in
`$XDG_DATA_HOME/tonguetyped/history.db` with user-only permissions. Disable
`history.enabled` to keep only the current daemon's latest result in memory.

## Transcription benchmark

Measure model loading and inference separately with a 16 kHz mono WAV file:

```sh
cargo run --release --example transcribe_benchmark -- \
  "$HOME/.local/share/tonguetyped/models/ggml-small-q5_1.bin" \
  recording.wav
```

The benchmark uses fixed English. Its output includes audio duration, model-load
time, transcription time, real-time factor, and transcript text. Compare builds
with the same model, WAV, release profile, and otherwise idle machine. CPU model,
core count, temperature, power policy, and competing load affect absolute timing.

A recovery run on an AMD Ryzen 7 9700X (8 physical cores, 16 logical CPUs) used
the `small-q5_1` model. Its 8.597-second synthetic speech WAV was generated with
FFmpeg's `flite` source:

```sh
ffmpeg -f lavfi \
  -i "flite=text='Today I am testing local speech recognition. The microphone records my voice and the computer converts each sentence into written text.'" \
  -ar 16000 -ac 1 recording.wav
```

The baseline predates the benchmark example. Run it with the final benchmark
source so both builds use the same input, language, timing boundaries, and
default model-loading behavior:

```sh
baseline_dir="$(mktemp -d)"
git archive a91a1236d26f15f972e7e30089cbb4acbf5f578f | \
  tar -x -C "$baseline_dir"
mkdir "$baseline_dir/examples"
cp examples/transcribe_benchmark.rs "$baseline_dir/examples/"
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
