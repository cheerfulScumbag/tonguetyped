# Project agent memory

This file is the project's committed home for project-intrinsic agent knowledge: build, test, release, architecture, and sharp-edge notes that should travel with the code.

- Add durable project-specific notes here as they are discovered through real work.

## Maintaining this file

Keep this file for knowledge useful to almost every future agent session in this project.
Do not repeat what the codebase already shows; point to the authoritative file or command instead.
Prefer rewriting or pruning existing entries over appending new ones.
When updating this file, preserve this bar for all agents and keep entries concise.

## Build

Uses Cargo with system deps (alsa, openssl, cmake, libclang). On NixOS, run build
commands through `nix develop -c`.

Format: `cargo fmt --check`  |  Lint: `cargo clippy -- -D warnings`  |  Test: `cargo test`

## Architecture

Single binary `tonguetyped`. CLI via clap dispatches to subcommands. Daemon owns the
coordinator state machine. Clients connect via Unix-domain socket (newline-delimited JSON).

Key crates: cpal (audio capture), rubato (resampling), transcribe-rs/whisper-cpp (inference),
vad-rs (Silero VAD), rusqlite (history). See Cargo.toml for full dep list.
