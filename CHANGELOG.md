# Changelog

All notable changes to TongueTyped are recorded here.

## 0.3.2

### Fixed

- The autostart warning test is hermetic under Nix. `buildRustPackage`
  exports `RUST_LOG` as an empty string, and an empty `EnvFilter` silenced
  every tracing event, so the setup child never printed the systemd autostart
  warning the test asserts on. The test now drops any inherited `RUST_LOG`
  for that child so the app's default filter applies, as it does in a normal
  user session. The assertions are unchanged, and `nix build` of the package
  check passes on x86_64 and aarch64 again.

## 0.3.1

### Added

- The Speech models table in the setup console is filterable and sortable.
  `i` cycles the installed-state filter through any, installed, and not
  installed, `f` cycles model family, and `t` cycles quantization; `s`
  cycles the sort order through catalog order, size ascending and
  descending, id, quantization, and live-first, and `c` clears the filters.
  The table header names the active sort, a status line shows the active
  filter chips and how to clear them, and the pane title always carries the
  entry count so a filtered view never looks like an empty catalog. Sorting
  keeps the cursor on the same model and clamps it to a visible row, and a
  filter that hides the active model names it instead of dropping its
  marker.

### Changed

- Feedback sounds play in-process from embedded audio assets. Each feedback
  event has one small mono WAV compiled into the binary and played through
  cpal on an output stream at the configured `feedback_volume` (a linear
  gain, so 0.7 is still -3.1 dB), through `feedback_device` when that names
  a device. Turning on Feedback sounds in the setup console now plays the
  Success sound immediately and shows a real playback error there, naming
  whether the output device is missing or failed and including the
  underlying error, instead of leaving a stale "install canberra-gtk-play"
  warning in the daemon journal. The external `canberra-gtk-play` player,
  its freedesktop theme ids, the Nix wrapper entry, and the packaging
  dependency are gone.

### Fixed

- Both daemon service units set explicit start limits
  (`StartLimitIntervalSec=300`, `StartLimitBurst=5`), so any repeated start
  failure stops instead of restarting forever; the duplicate-daemon exit
  status stays in `RestartPreventExitStatus`. The systemd unit is now the
  only autostart owner: it removes the XDG entry that `tonguetyped autostart
  enable` writes, and that command refuses to create one while the unit
  exists, naming the unit. Saving settings still succeeds when the entry is
  owned elsewhere, but it never writes a competing entry, and the saved
  config records autostart as off instead of claiming an entry the app
  refused to write.

## 0.3.0

### Added

- A four-tab settings console: Audio & Capture, Model & Engine, Output &
  Delivery, and Interface & System. Running `tonguetyped` with no arguments
  opens it, and `tonguetyped setup` opens the same console. It reaches every
  setting the old wizard had, adds pause-media and feedback-sound switches,
  starts, stops, and restarts the daemon, runs a Start/Stop Dictation card
  over daemon IPC, and shows the real layer-shell overlay for four seconds as
  a desktop test. Edits apply in memory, and Ctrl+S saves only the changed
  fields.
- Live text in the overlay for streaming models. Partial words stream across
  the live-text look while you speak, and the finalized text is typed when
  the recording stops. A model-aware rollover keeps a long dictation
  streaming past the model's output window.
- New catalog entries: Parakeet TDT 0.6B v2, Moonshine base, Canary 1B v2,
  Cohere Transcribe, Qwen3-ASR 1.7B, and the streaming ports
  moonshine-streaming-medium, parakeet-unified-en-0.6b,
  nemotron-3.5-asr-streaming-0.6b, and voxtral-mini-4b-realtime-2602. Every
  entry is pinned to a repository commit with its size and SHA-256.
- `tonguetyped transcribe <file>` transcribes a WAV file without the daemon.
  It downmixes and resamples to 16 kHz mono, applies VAD when enabled, and
  reuses one model load across chunks.
- Long dictation support. The recording cap defaults to 600 seconds, and
  audio longer than `transcription.chunk_seconds` is decoded in windows
  bounded by the active model's own limit. A boundary snaps to a quiet point
  when the audio has one.
- Transcript post-processing between transcription and output: replacement
  pairs, spoken punctuation, English-only filler removal, and an optional
  external cleanup command with a timeout.
- Clipboard and paste output methods, plus a clipboard fallback when typing
  or the paste keys fail.
- A native libei typing backend for KDE and GNOME Wayland. It types through
  the XDG RemoteDesktop portal's Emulated Input session and needs no
  `/dev/uinput` access.
- JSON status for status bars (`status --format json`), with `--follow` for
  a line per change and `--extended` for model, backend, device, and build.
- Optional media pause while dictating (`audio.pause_media`). It pauses
  playing MPRIS players and resumes the same players afterwards.
- NixOS and home-manager modules (`nixosModules.default` and
  `homeManagerModules.default`) with backend, package, autostart, and typed
  settings options.
- A local Flatpak build with portal-based autostart. The manifest builds ONNX
  Runtime from source, so the sandbox no longer needs a prebuilt runtime.
- Release packaging additions: bash, zsh, and fish completions and man pages
  generated from the CLI, per-backend RPMs, a CPU-only aarch64 build,
  `SHA256SUMS.txt`, and detached signatures when signing keys are configured.
- A tilted speech-bubble app icon, shipped in the .deb, .rpm, AUR, and Nix
  packages.

### Changed

- The previous dashboard TUI is removed. Its settings and actions live in the
  four-tab console that bare `tonguetyped` now opens, and the console header
  draws a lowercase `tonguetyped` wordmark with both `t` glyphs in green.
- Transcript history saving is opt-in and off by default. Turning it off
  writes neither the history database nor transcript files.
- The daemon watches `config.toml` and applies settled edits without a reload
  command or a restart. A rejected edit leaves the running config in place
  and reports the error in status and the overlay.
- Overlay configuration splits into a per-style look and a waveform shape.
  Each style offers only the looks it can paint, and the configured style is
  authoritative even when a streaming model is active.
- The Whisper decode options are exposed in `[transcription]`: vocabulary
  prompt, translation, temperature, and the temperature fallback thresholds.

### Fixed

- The daemon is the sole owner of the desktop shortcut session. Shortcut
  tests and reconfigures go through daemon IPC instead of opening a competing
  session, and the listener rebinds after a session close or stream loss. A
  duplicate daemon exits 3, and both Nix modules list that status in
  `RestartPreventExitStatus` so systemd does not restart-loop behind a
  manually started daemon.
- `daemon stop` and `restart` stay bounded while a transcript is being typed,
  including when the typing helper is wedged.
- The setup console no longer writes a stale config snapshot over a newer
  file, so it cannot revert the saved output method or reset the typing
  backend and auto-submit.
- The transcript-folder export writes files only when keeping transcripts is
  enabled.
- The live-text caption sizes to its measured content, so a short transcript
  no longer sits in a tall card with a dead band underneath.
- The libei typing path tolerates an already-registered portal application
  id, so it no longer falls back to the clipboard on every KDE dictation.
- Typing backends are probed for availability at daemon load and on every
  reload. A pinned backend that cannot work is replaced by one that can, the
  substitution is logged, and status reports it along with output health.
  The real round-trip typing test stays in `doctor --test-type`, in a surface
  the tool owns.
- The Silero VAD download is pinned and verified by SHA-256.
