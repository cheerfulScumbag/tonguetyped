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
```

The package installs the binary and a `TongueTyped` desktop entry. Launch
TongueTyped from your desktop's application menu so the Global Shortcuts portal
can identify it. KDE displays an authorization dialog for the default `Super+O`
shortcut on first launch.

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
  recording.wav en
```

Omit the language argument to include automatic language detection. The output
includes audio duration, model-load time, transcription time, real-time factor,
CPU thread count, and transcript text. Compare builds with the same model, WAV,
language, release profile, and otherwise idle machine. CPU model, core count,
temperature, power policy, and competing load affect absolute timing.

The original 8-second representative microphone sample improved from about
35.8 seconds to 16.5 seconds, with identical transcript text. A recovery run on
an AMD Ryzen 7 9700X (8 physical cores, 16 logical CPUs) used the `small-q5_1`
model and fixed English. Its 8.597-second synthetic speech WAV was generated
with FFmpeg's `flite` source:

```sh
ffmpeg -f lavfi \
  -i "flite=text='Today I am testing local speech recognition. The microphone records my voice and the computer converts each sentence into written text.'" \
  -ar 16000 -ac 1 recording.wav
```

The baseline took 31.825 seconds and the 8-thread build took 13.081 seconds.
Both runs produced this exact transcript:

```text
Today I am testing local speech recognition, the microphone records my voice and the computer converts each sentence into written text.
```
