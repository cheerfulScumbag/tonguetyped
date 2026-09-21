# TongueTyped

TongueTyped is a Linux dictation application controlled from the terminal or a
desktop-wide keyboard shortcut. It records microphone audio, transcribes it
locally with whisper.cpp, and can type the result into the focused application.

## Requirements

TongueTyped requires a Linux desktop with an XDG Global Shortcuts portal and an
ALSA-compatible input device. Building from source requires Rust, CMake,
libclang, OpenSSL, pkg-config, and ALSA development files. The Nix flake provides
these build dependencies:

```sh
nix develop
cargo build --release
```

For direct typing, install at least one supported backend: `wtype`, an X11
environment supported by `enigo`, or `dotool`. Automatic backend selection tries
them in that order. TongueTyped never uses the clipboard as a typing fallback.

## First run

Start the daemon from a graphical desktop session:

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
text.

## Commands

With the daemon running, use:

```text
tonguetyped start        Start recording
tonguetyped stop         Stop and transcribe the recording
tonguetyped toggle       Start or stop recording
tonguetyped cancel       Cancel recording or discard in-flight processing
tonguetyped status       Show daemon state and activation mode
tonguetyped reload       Validate and reload the config
tonguetyped last-result  Print the latest transcription
tonguetyped doctor       Check runtime dependencies
```

The daemon listens on `$XDG_RUNTIME_DIR/tonguetyped/control.sock` and refuses to
start a second instance. Recordings stop at `transcription.max_recording_seconds`
even if no stop command arrives. Voice activity detection trims retained audio;
it does not stop a recording automatically.

When history is enabled, TongueTyped stores transcript text in
`$XDG_DATA_HOME/tonguetyped/history.db` with user-only permissions. Disable
`history.enabled` to keep only the current daemon's latest result in memory.
