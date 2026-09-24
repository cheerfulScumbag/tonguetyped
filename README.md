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

For development outside the installed package, start the daemon from a graphical
desktop session:

```sh
cargo run -- daemon
```

The first run creates `$XDG_CONFIG_HOME/tonguetyped/config.toml` and downloads
the selected Whisper model to `$XDG_DATA_HOME/tonguetyped/models`. If the
corresponding XDG variables are unset, the standard user config and data
directories are used. The desktop portal may ask you to approve the configured
shortcut, which defaults to `Super+O` in hold mode.

The default output method is `none`, so transcription does not type or copy
anything. Retrieve the latest result with:

```sh
cargo run -- last-result
```

Set `output.method = "type"` in the generated config to type transcripts into
the focused application. To test typing explicitly, focus a disposable text
field and run `cargo run -- doctor --test-type`. The regular `doctor` command
checks the compositor, microphone, model, and typing backends without injecting
text or requesting shortcut authorization. Run `cargo run -- shortcut-test`
explicitly to validate desktop shortcut authorization and binding.

## Commands

The CLI provides these commands:

```text
tonguetyped daemon         Start the daemon
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
