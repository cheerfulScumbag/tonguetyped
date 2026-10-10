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
`nix develop -c`. If test binaries fail with ``GLIBC_2.43' not found`` from
`libasound.so.2`, the host shell's `LD_LIBRARY_PATH` is leaking a newer
alsa-lib into the devshell; run `env -u LD_LIBRARY_PATH nix develop -c ...`.

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
That chain is `config.model.preferred_backend = "auto"`; any other value
(`inference::BACKEND_PREFERENCES`) pins `load()` to that one backend and fails with
a named reason (not compiled in vs. no usable device) instead of falling back. The
coordinator's engine cache key includes the preference, so a reload with a changed
backend reloads the engine.
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

The typing helpers are runtime dependencies of the packaged app, not optional
discoveries: `flake.nix`'s `packages.default` `postFixup` `wrapProgram` prepends
`wtype`, `dotool`, and `wl-clipboard` to the installed binary's PATH (the
devShell carries them too), the `PKGBUILD` depends on all three, and
`Cargo.toml`'s deb metadata recommends `wtype`/`wl-clipboard` only (Debian has
no `dotool` package; `xdotool` already covers the built-in X11 `enigo` backend).
`output::typing_helper_warning()` is compositor-aware, not "install wtype on
Wayland": `wtype` types through the virtual-keyboard protocol only wlroots
compositors implement, so `output::desktop_names_are_wlroots` maps
`XDG_CURRENT_DESKTOP`/`XDG_SESSION_DESKTOP`/`DESKTOP_SESSION` (sway, Hyprland,
niri, ...) to a `wtype` hint and every other session - KDE's KWin above all -
to `dotool`. `output::dotool_available()` is a real usability check, not a
binary-exists check: it also requires `/dev/uinput` to be writable, since a
`dotool` that cannot open that device (no `input` group or udev rule) types
nothing; the open is side-effect-free. Binary presence (`dotool_installed`) is
a PATH lookup, not a `dotool` self-test: running `dotool` opens `/dev/uinput`
at startup and exits non-zero when that fails, so it would report an
installed-but-forbidden `dotool` as missing and make the permission remedy
below unreachable. When no helper works but `dotool` is
already installed and only `/dev/uinput` is unwritable, `typing_helper_warning()`
names that permission remedy (add the user to the `input` group or add a udev
rule) instead of telling the user to install a binary they already have. A
protocol-based typing path that would
avoid `/dev/uinput` entirely (the KDE Wayland RemoteDesktop portal) is a
possible future direction, deliberately not implemented here. When no helper
actually works, the
shared configuration UIs never silently drop the "Type into the focused
application" choice: `Capabilities::typing_helper_warning()` (`src/setup.rs`)
renders as a warning line in the dashboard's Transcript output and Typing
backend screens and as the setup console's Output-step footer, and choosing
type without a working helper is refused (dashboard `OutputScreen::apply`
returns `Err` for the result line; both setup flows stay on the step) instead of
saving a method that can never type. A helper installed while TongueTyped is
running is not picked up until a restart - `cached_auto_backend` and the
dashboard's `Capabilities` memoize the probe forever - so the warning says so.
`tests/tui_dashboard.rs`'s PTY sandbox isolates PATH and pins
`XDG_SESSION_TYPE=wayland` plus `XDG_CURRENT_DESKTOP=KDE`; a test that wants a
helper drops a stub in via `Sandbox::install_typing_helper` (a bare `dotool`
stub would no longer count, since the probe also checks `/dev/uinput`).

Running bare `tonguetyped` (no subcommand) opens the dashboard (`src/tui/`,
`CLI`'s `command` field is `Option<Commands>` - see `data/tt-tui-dashboard-1/report.md`
for the original SuperDesign process and `data/tt-add-dashboard-settings-menu-77/report.md`
for the settings-menu redesign). The home screen is two stacked panels sharing one
selection cursor: a fixed eleven-row Settings panel (Model, Inference backend,
Microphone, Activation, Shortcut, Transcript output, Typing backend, Transcript
folder, History retention, Startup, Overlay) showing each area's current value,
above a Commands panel. The "Inference backend" row shows
`config.model.preferred_backend` raw (`auto` or the pinned name) and opens a
dedicated settings screen (`screens::BackendScreen`) that is the same
`inference::backend_choices()` list the Model screen's backend panel shows
(shared verbatim through `screens::backend_panel_lines`), but persists through
the shared `save_settings_config` path instead of the Model screen's
activate-and-confirm flow. `src/cli.rs` holds the clap
`Cli`/`Commands` definitions as a library module specifically so `src/tui/mod.rs`'s
`home_items()` can read the Commands panel straight off clap's own metadata
(`Cli::command().get_subcommands()`) instead of a second, driftable copy - it can
never disagree with `--help`; `model`/`autostart` are clap commands deliberately
filtered out of `home_items()` because they are Settings-panel rows instead. The
settings screens (`src/tui/screens.rs`) only mutate the in-memory `Config` via
`apply`; `App::save_settings_config` is the shared feedback path those screens
report through, so success/failure feedback stays uniform, though the Shortcut
outcome handlers save directly and the Startup path saves via
`autostart::update`. `src/commands.rs` is the one shared
command-implementation layer both `main.rs`'s CLI dispatch and the dashboard call
into (IPC send/format, model catalog rows, `activate_model`'s
download+save+reload+confirm composition) - add new shared command logic there, not
in either caller. `setup` is NOT reimplemented in the dashboard - it suspends its
own alternate screen, runs the pre-existing `setup::run_console()`
(`src/setup.rs`), then resumes, since a terminal tracks one alternate-screen
buffer, not a stack.

Transcript persistence has two independent, always-available settings (neither
branches on the output method). History retention lives in `[history]`:
`max_entries` and `max_age_days` (either `0` = no limit on that dimension; the
pair replaced the old fixed 500-entry cap, default 100 entries / 30 days) are
both applied on every write by `HistoryStore::prune(max_entries, max_age_days)`
and cover the SQLite DB only. `[history].transcript_folder`, when non-empty,
writes each finished transcript as a plain-text file via
`history::export_transcript` (timestamped filename, `-2`/`-3`... on a same-second
collision, `0600`, leading `~/` expanded) - write-once: TongueTyped never reads,
monitors, or prunes those files, so they outlive the DB retention. Both settings
are surfaced as their own dashboard Settings rows (screens in
`src/tui/screens.rs`'s `TextFieldsScreen`) and as console wizard steps (after
Startup, before Overlay). Free-text/numeric entry is the one input the two
configuration UIs otherwise lacked: `crate::text_input::TextField` is the shared
single-line editor (char-index cursor, insert/backspace/home/end, `split_at_cursor`
for the reversed-cell rendering), used by both `tui::screens` and `setup::console`.

Misconfiguration-prone preview/choice logic is extracted once and shared by the
wizard and the dashboard rather than copied: `audio::MicMonitor` owns the
microphone-preview stream (settle delay after rapid navigation, async stream-error
routing, restart) for both `setup/console.rs`'s Microphone step and the dashboard's
Microphone screen; `audio::level_to_ratio` is the one gauge mapping; the overlay
position/style value lists (`overlay::POSITION_VALUES`/`STYLE_VALUES`/
`STYLE_LABELS`/`STREAMING_LABELS`) and the activation/startup choice labels
(`config::ACTIVATION_MODE_LABELS`/`STARTUP_LABELS`) are imported by both UIs. Add a
new choice/value there, not as a second copy in either UI. The dashboard's Shortcut
screen and the console's shortcut step have no editable key field: TongueTyped
registers the `"Start or stop dictation"` action with the desktop and never chooses
a key, so the only way to set one is the desktop's own dialog. `activation::
bind_activation_shortcut` therefore passes no `preferred_trigger` (passing one made
KDE store it as the action's "Default shortcut" and display the app-chosen `Meta+O`
beside the user's real binding); `config.activation.keybind` now holds only what the
portal reports is bound, `activation::listen` refreshes it after binding (via
`Coordinator::record_activation_binding`), and both UIs render it through
`activation::keybind_display` (which turns KDE's `Meta` into the app's `Super`). The
portal test and native reconfigure dialog run through `setup::shortcut_test_async`/
`reconfigure_shortcut_async` handles polled by `App::poll_shortcut_handles` each
frame, and only the dialog's reported trigger is what gets persisted.

A ratatui app that ever loads an inference model (Doctor, Model-activation) while
holding raw mode/the alternate screen must keep **two** independent things off the
live terminal, not just one: (1) `tracing` output - redirect its writer
(`with_writer(std::io::sink)`) rather than relying on `EnvFilter`, since any
`tracing::warn!`/`info!` line written straight to stderr lands askew of ratatui's
cursor-positioned redraws; and (2) `transcribe-cpp`'s underlying C/C++ GGUF loader,
which logs backend-fallback and load-failure lines straight to the process's real
stderr **file descriptor**, bypassing `tracing` entirely - suppressing (1) alone does
not stop this. `src/tui/mod.rs`'s `redirect_stderr_to_devnull`/`restore_stderr`
(`libc::dup2` fd 2 to `/dev/null`, restored by the same `TerminalGuard` that restores
raw mode on every exit path including a panic) is the fix; any new interactive screen
that can trigger a model load inherits this for free by living under `tui::run`, but
a *new* top-level entry point that also loads models would need the same pattern.

PTY-driven behavior tests for a ratatui screen (`tests/tui_dashboard.rs`, via
`portable-pty`) must reconstruct the screen with `vt100::Parser`, not by stripping
ANSI escapes from the raw byte stream and concatenating it: ratatui only rewrites the
cells that changed between frames and jumps the cursor directly between them, so a
naive strip-and-concatenate approach silently merges unrelated rows from different
redraws into one run-on string with no whitespace between them. `vt100::Parser::
process` plus `.screen().contents()` tracks real cursor/cell state and returns the
actual current screen text. `Cargo.toml` enables ratatui's
`unstable-rendered-line-info` feature only so the dashboard's shared Info/result pane
(`src/tui/mod.rs::render_info`, backing last-result/status/doctor/activation output)
can call `Paragraph::line_count` to clamp its scroll offset to the *wrapped* content
height - command output can be one very long transcript line, so wrapping alone is
not enough and the pane is scrollable (Up/Down/j/k/PageUp/PageDown/Home/End). The
offset lives in `App::info_scroll` (`Cell<u16>`) and is clamped inside `render_info`
each frame, so the key handler never needs the pane geometry. The one PTY test that
starts a real daemon (`tests/tui_dashboard.rs::open_last_result`) must pass a short
sandbox tag: a Unix-domain socket path has to fit under `SUN_LEN` (~108 bytes), which
the longer human-readable tags the other sandboxes use would exceed under Nix's
already-long `TMPDIR`.

The setup console's step wizard (`src/setup/console.rs`) derives
`next_step`/`prev_step`/`step_index`/`step_total` from one `step_sequence()` method
that builds the actual ordered list of steps for the current conditional state
(download queue, output backend, overlay enabled), rather than four separately
hand-maintained arithmetic functions - add a new conditional step there, not as a
fourth place to keep in sync. Overlay settings (enable/disable, position, style,
streaming indicator) and the other settings areas now have their own dashboard
screens (`src/tui/screens.rs`), so the dashboard runs `setup::run_console()` only
for the `setup` command itself, suspending its alternate screen
(`App::run_setup_console`). The legacy
line-based `configure()` flow in `setup.rs` (kept for scripted/piped `tonguetyped
setup`, see `tests/setup_cli.rs`) has never prompted for overlay settings at all -
it leaves whatever `Config::reload()` loaded untouched - so it needed no changes
when overlay got its console step.

`OverlayConfig::style` (`badge`/`minimal`/`pill`/`blob`) and `streaming_indicator`
(bool) follow `position`/`monitor`'s existing convention of plain, unvalidated
strings/bools with a tolerant-fallback parser in `src/overlay.rs` (`style_for`,
`anchor_for`) rather than a strict `serde` enum - an unrecognized `style` value
falls back to `Badge`. `blob` is a bright, glowing phase-colored orb that slowly
breathes (`paint_blob`/`blob_edge`); it is the only style with its own animation
period (`BLOB_PULSE_PERIOD`, via the dedicated `blob_breath_fraction`, which keeps
the silhouette's slow breath independent of `animation_fraction`'s faster spinner
fraction while transcribing), and
the captain reviewed it through `examples/overlay_style_png.rs` - an offline
renderer (no compositor needed) that dumps every style/phase plus blob animation
frames to PNGs by calling the public `overlay::render_frame_pixels`. Use it for
any future overlay look, and note the canvas is wl_shm Argb8888 (little-endian
BGRA), so the example swaps channels when writing PNG.
`streaming_indicator` is a *synthetic* busier waveform animation (driven by the same
elapsed-time fraction every other phase already animates from), not a real
microphone-reactive one: there is no live audio-level feed wired from the
coordinator's recording stream into the overlay actor, and wiring one would mean
changing the `CoordinatorRuntime::record` trait signature - out of scope for what
this is (the Handy dictation app's real streaming-transcription overlay inspired
this - reviewed as Superdesign mockups and approved by the captain - and why a
literal equivalent isn't buildable without a streaming inference backend this project
doesn't have). Each `Style` can request a different Wayland surface size
(`overlay::surface_size_for`; `Pill` widens and `Blob` enlarges) - like
`position`/`monitor`, that size is fixed at first-ever overlay creation for the
daemon's lifetime, so a style change that affects surface shape needs a daemon
restart to take visual effect, exactly like a position/monitor change already does.

`tonguetyped daemon` (`src/cli.rs`'s `Commands::Daemon { command: Option<DaemonCommand> }`)
keeps bare invocation meaning exactly what it always has (foreground start, used
unchanged by `flake.nix`'s autostart entry) - `stop`/`restart` are a nested
`DaemonCommand`, not new top-level `Commands` variants, so they don't appear as
separate dashboard home items (`src/tui/mod.rs` only lists top-level subcommands).
Selecting "Daemon" there instead opens a small `Screen::Daemon` sub-screen
(`src/tui/screens.rs`'s `DaemonScreen`) listing Start/Stop/Restart, the same
three operations the CLI exposes, so the dashboard is never missing a way to
stop or restart a daemon it can start. `Request::Shutdown` (`src/ipc.rs`,
handled in `daemon::dispatch`) cancels any in-flight recording/processing the same
way a client `cancel` would (not a hard kill): `Coordinator::cancel` reaches an
in-flight transcription through the engine's `transcribe-cpp` `CancelToken`
(`InferenceEngine::set_cancel_token`, published per run in
`ProductionRuntime::transcribe`, never through the inference lock the run holds),
so the native decoder stops between steps. It then waits
(`Coordinator::wait_for_worker`) for that worker to finish before replying, but
*bounded* (`SHUTDOWN_WORKER_TIMEOUT`) so a worker that ignores cancellation cannot
hang `stop`/`restart`. Then `daemon::run_daemon`'s accept
loop (`tokio::select!` against a `tokio::sync::Notify`) stops taking new
connections, calls `Coordinator::shutdown()` - which refuses while a
recording/transcription worker is still active (a worker in its VAD phase holds
no inference lock, yet would still re-acquire it to run inference, so acquiring
the lock alone must not be read as "done"), and otherwise stops and JOINS the
detached `tonguetyped-idle-unload` timer thread, then drops the loaded
inference engine on the shutdown thread with a bounded lock acquisition - releases the instance lock, and only
then removes the control socket - in that order, since `commands::stop_daemon` treats the socket's
disappearance as proof the old process (and its lock) is gone, and
`restart_daemon` chains straight into `spawn_daemon` right after. If
`Coordinator::shutdown()` returns an error - a worker still active after the
bounded wait, or the bounded engine release failing to acquire the inference
lock in time - `run_daemon` removes the socket and calls `std::process::exit`
*without* freeing the engine, so neither the crash below nor an unbounded hang
can happen.
`commands::
stop_daemon` polls for the socket to actually disappear after sending
`Shutdown` (so it fails fast with a clear error when no daemon is running,
instead of hanging) and `restart_daemon` chains that into the existing
`spawn_daemon` (the same detached background launch the dashboard already
used) rather than blocking in the foreground. The dashboard's `start`/`stop`
home items (single-shot recording start/stop) stay hidden from the home list
too - `toggle` and `cancel` are the dashboard's recording controls, while
`start`/`stop` remain reachable only as CLI commands.

A daemon killed outside its own graceful shutdown (so the control socket never
gets unlinked) leaves a stale socket *file* behind - `commands::
daemon_socket_exists` (and the private `socket_is_alive` it and `stop_daemon`
share) treats that as "not running" by attempting a connect, the same
liveness probe `daemon::acquire_instance_lock_at` already performs before a
fresh daemon binds the socket, and removes the stale file on a failed
connect. `Path::exists()` alone is not enough: `stop_daemon`/`restart_daemon`
used to treat the leftover file as proof a daemon was running and tried (and
failed) to send it `Shutdown`, surfacing a raw connection-refused error
instead of proceeding straight to `spawn_daemon`.

`IdleUnloadTimer` (`src/coordinator.rs`) spawns a detached
`tonguetyped-idle-unload` thread that holds an `Arc` clone of the loaded-engine
mutex and drops the engine on idle timeout, freeing Vulkan buffers through the
NVIDIA driver. Because that thread was never stopped or joined, the daemon could
`exit()` while it was still freeing, racing the driver's `exit()`-time teardown
and segfaulting (reproduced 10/10 by restarting a Vulkan-loaded daemon;
coredump shows the idle thread in `ggml_vk_destroy_buffer` and the main thread
in `__run_exit_handlers`). `Coordinator::shutdown` now makes this deterministic:
once no worker is active it sends a `Shutdown` command the timer handles before
any expiry, joins the thread, then drops the engine on the shutting-down thread
with a bounded lock acquisition (`ENGINE_RELEASE_TIMEOUT`); if a worker is still
active or that bound expires because a transcription still owns the lock,
`run_daemon` exits via `std::process::exit` without freeing the engine, so
teardown cannot race live GPU work. `IdleUnloadTimer`'s
`Drop` joins as a fallback, and `apply_idle_unload_policy` refuses to re-arm the
timer once `shutdown_started` is set (the `reload` race). Any new background
thread that can drop a loaded engine needs the same join-on-shutdown treatment.

The Cargo version rarely changes, so build identity is the git commit:
`build.rs` captures it (`-dirty` suffix for uncommitted changes) into
`src/build_info.rs`, which `--version`, the daemon's `Response::Status.build`, and
`doctor`'s daemon-vs-binary comparison all read. The Nix flake's source copy has
no `.git`, so `flake.nix` passes the commit in via `TONGUETYPED_BUILD_COMMIT`; any
other git-less build reports `unknown`. Because `build.rs` declares
`rerun-if-changed`, Cargo no longer reruns it on every package file change - add
any new input it reads to that list.

## Release

`.github/workflows/release.yml` builds Linux (.deb variants + plain binary),
an Arch package, and macOS (Apple Silicon + Intel) assets on a `v*` tag, then
publishes a GitHub release. The `arch` job builds the AUR `tonguetyped-bin`
package (root `PKGBUILD`, a "-bin" style package repackaging the already-built
default-variant Linux binary, the same one `cargo-deb` repackages for the
.deb - not rebuilt from source) inside an `archlinux:latest` container, since
GitHub-hosted runners are Ubuntu and `makepkg` needs a real Arch environment;
`options=('!debug')` is required there, or `makepkg` emits a useless
`tonguetyped-bin-debug` split package from a binary that already shipped
stripped. The root `PKGBUILD` is a template only - `pkgver`/`sha256sums` are
resolved per-release by the `arch` job, not hand-edited here. The
`aur-publish` job pushes the resolved PKGBUILD/.SRCINFO to
`ssh://aur@aur.archlinux.org/tonguetyped-bin.git` using the
`AUR_SSH_PRIVATE_KEY` repo secret; it runs after the `publish` job (not
alongside it) because the PKGBUILD's `source` URL points at that release's
now-live GitHub asset, and it skips cleanly (not a failure) when the secret
is absent, so the rest of the release never depends on it. The macOS job
builds both `aarch64` (macos-14, Apple Silicon) and `x86_64` (Intel) legs
natively rather than cross-compiling, since transcribe-cpp's cmake build is
more reliable built natively per-arch; GitHub retired the old `macos-13`
Intel runner image on 2025-12-04, so the Intel leg uses `macos-15-intel`
(GitHub's current native x86_64 macOS runner label) - re-check GitHub's
runner-images deprecation notices before assuming that label still exists.
