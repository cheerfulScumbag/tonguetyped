# TongueTyped TUI dashboard

Refactors the TUI so running bare `tonguetyped` opens a polished, centered-logo
first page listing every existing command as a keyboard-accessible action,
while every direct CLI invocation (`setup`, `daemon`, `start`, `stop`,
`toggle`, `cancel`, `status`, `reload`, `last-result`, `doctor`,
`shortcut-test`, `autostart`, `model`, `help`, `--help`, `--version`) keeps
its exact existing behavior.

## SuperDesign process

Used the SuperDesign CLI (`npx @superdesign/cli@latest`, already authenticated
as team "Personal") per the product's own workflow, not the archived IDE
extension. This is a Rust TUI project with no frontend codebase, so the
normal repo-`init` (components.md/routes.md/theme.md/...) doesn't apply; the
existing terminal UI's styling and interaction patterns were captured by hand
instead.

1. **Design system** - `.superdesign/design-system.md`, derived from
   `src/setup/console.rs` (the only existing full-screen interactive surface
   today, reached via `tonguetyped setup`): the five-color semantic palette
   (cyan/green/red/yellow/default), monospace-only typography, box-drawing
   borders, the `"> "` selected-row convention, the non-centered logo, the
   footer hint bar, and the accessibility note that every color cue is
   paired with a non-color one.
2. **Replica (before-state only)** - `.superdesign/replica_html_template/index.html`,
   a faithful HTML/CSS reproduction of that console's first screen (the
   Model-selection step) as a terminal-emulator grid. Contains no redesign.
3. **Project + baseline draft** -
   `create-project --template .superdesign/replica_html_template/index.html`:
   - Project: **TongueTyped TUI Dashboard**, id `d107a1c9-eea4-4044-8d75-9bf51c3acb9a`
   - Canvas: https://superdesign.dev/teams/45d90506-9cb8-4e5a-a285-fa9ccb916130/projects/d107a1c9-eea4-4044-8d75-9bf51c3acb9a
   - Baseline/reproduction draft: `f8dbff09-0345-48f8-b480-4eaf60fae7d1`
     (preview: https://p.superdesign.dev/draft/f8dbff09-0345-48f8-b480-4eaf60fae7d1)
4. **Three branched dashboard drafts** (`iterate-design-draft --mode branch`
   from the baseline, `--context-file` on `design-system.md` + the replica,
   each prompt explicitly constrained to centered logo, full 13-command
   coverage, keyboard focus, narrow-terminal reflow, and "use only the
   colors/fonts/components in the design system"):

   | Draft | id | preview | verdict |
   | --- | --- | --- | --- |
   | TongueTyped Command Dashboard | `98aca049-b7a0-4e85-82e9-f495063f35ff` | https://p.superdesign.dev/draft/98aca049-b7a0-4e85-82e9-f495063f35ff | **chosen** |
   | TongueTyped Two-Column Dashboard | `cea50af5-6b66-4bf1-8f67-d8a6d60eb619` | https://p.superdesign.dev/draft/cea50af5-6b66-4bf1-8f67-d8a6d60eb619 | rejected |
   | TongueTyped Grid Dashboard | `506bc244-052f-4c85-a8fc-8a07aefb93fe` | https://p.superdesign.dev/draft/506bc244-052f-4c85-a8fc-8a07aefb93fe | rejected |

   Fetched each draft's generated HTML (`get-design --output`) and compared
   against the functional/accessibility requirements:
   - **Two-Column Dashboard**: invented an incomplete, wrong command set
     (`history`, `config`, `logs`, `update` don't exist; `daemon`, `cancel`,
     `reload`, `last-result`, `autostart`, `shortcut-test` are missing) -
     fails "every existing command available" outright. It also pulled in
     Tailwind's CDN script and a Google Fonts import (design-system
     fidelity violation; meaningless for a ratatui reimplementation anyway)
     and used `overflow-y: auto` scrolling for overflow, which the brief
     explicitly rules out ("without a hidden more page").
   - **Grid Dashboard**: got the full, correct 13-command set and handled
     narrow-terminal reflow explicitly (3→2→1 columns), but each command
     became a multi-line bordered tile (~4 rows each); 13 tiles at 3 columns
     is 5 rows of tiles, which risks exceeding a normal ~24-row terminal
     once the logo/status/footer chrome is added. It also loaded an unused
     external icon-font script, a design-system fidelity violation (no icons
     beyond the box-drawing glyphs already in the wordmark).
   - **Command Dashboard (chosen)**: correct, complete command set; a single
     dense vertical list (same `"> "` selected-row convention as the
     existing console, directly continuing its visual language rather than
     departing from it); naturally reflow-safe (a single column never needs
     column-count breakpoints); fits a normal terminal with room to spare.
     This is the strongest match for "compact list ... that shows all
     commands in a normal terminal without a hidden more page" and for
     preserving TongueTyped's existing visual identity.

   The chosen draft's HTML was used only as a layout/visual reference for the
   Rust implementation below - its browser-only CSS (flexbox centering,
   invented backend-name strings like "LibreWhisper (CUDA)", paraphrased
   command descriptions) was **not** copied; the Rust dashboard centers via a
   computed column margin and uses the project's real CLI descriptions and
   real `doctor`-reported backend/device strings.

   **Blocker**: `chrome-devtools-axi` could not find a Chrome executable in
   this environment (`Could not find Google Chrome executable for channel
   'stable'`), so no live-browser screenshot of the chosen draft could be
   captured. The generated HTML itself (fetched via `get-design --output`)
   was used as the complete visual reference instead - it fully specifies
   the terminal-grid content character-by-character, so this did not block
   the design decision, but a screenshot was not possible to attach here.

## Implementation

- **`src/cli.rs`**: `Cli`/`Commands`/`ModelCommand`/`AutostartCommand` moved
  out of `main.rs` into the library, with `Cli::command` changed from
  `Commands` to `Option<Commands>` - the only clap-level change. This is also
  what lets the dashboard's command list introspect clap's own
  name/`about` metadata (`Cli::command().get_subcommands()`) instead of
  maintaining a second, driftable copy - the home screen and `--help` can
  never disagree about a command's name or description.
- **`src/commands.rs`**: the "one internal command interface" - `send_ipc`,
  `format_response_lines`/`format_doctor_lines` (byte-identical to the
  pre-existing CLI `println!` text, now shared instead of duplicated),
  `model_rows`/`human_size`/`select_model`, `spawn_daemon`, and
  `activate_model` (download+verify, save selection, reload a running
  daemon, re-run diagnostics to confirm the model actually loads - composing
  `model::DownloadManager`, `config::Config`, and `doctor::run_doctor`,
  nothing reimplemented). `main.rs` now calls these same functions instead of
  duplicating the logic inline; its printed output is unchanged.
- **`src/tui/`**: the dashboard itself.
  - `mod.rs` - an async event loop (`tokio::select!` over a redraw ticker,
    `crossterm::event::EventStream`, and a possibly-pending background
    action's `JoinHandle`) so a long action (daemon launch, model download)
    never blocks input or rendering. `Screen::{Home, Info, Model,
    Autostart}`; `setup` is dispatched by suspending the dashboard's
    alternate screen, running the pre-existing `setup::console` unmodified,
    and resuming - no duplicated wizard logic.
  - `logo.rs` - centers the existing block-art wordmark using a computed
    margin when the terminal is wide enough, and falls back to a compact
    text wordmark ("TONGUETYPED") on a terminal narrower than the logo's
    natural width, so a narrow terminal never clips it into garbage (the
    existing console has no such fallback). Unit-tested at wide/exact/narrow/
    zero widths.
  - `screens.rs` - the Model catalog screen (list + activate) and the
    Autostart toggle screen.
- **Two pre-existing bugs this work surfaced and fixed**, both invisible
  until a ratatui screen could trigger them (the existing console never
  loads an inference model or hits a failing backend during its own flow):
  1. `tracing_subscriber` was unconditionally writing to stderr; a
     `tracing::warn!` from inference's backend-fallback logging (visible the
     moment Doctor or Model-activation tries to load a model) would land
     askew of ratatui's cursor-positioned redraws and visibly corrupt the
     display. Fixed by sinking tracing output when no subcommand is given
     (every other subcommand is unaffected).
  2. `transcribe-cpp`'s underlying C/C++ GGUF loader logs load failures
     straight to the process's real stderr file descriptor, bypassing
     `tracing` entirely - the above fix alone did not stop it. Fixed with an
     OS-level `dup2` of fd 2 to `/dev/null` for the dashboard's lifetime,
     restored by the same `TerminalGuard` that restores raw mode/alternate
     screen on every exit path (including a panic).

## Tests (real behavior, no source-grepping)

- `tests/cli_help.rs` - plain subprocess assertions: `--help` still lists
  every one of the 13 commands with its exact existing description plus
  `-h/--help`/`-V/--version`; every subcommand still parses; bare invocation
  no longer produces the old clap "missing subcommand" usage error.
- `tests/tui_dashboard.rs` - real PTY tests (`portable-pty`), with screen
  state reconstructed via `vt100::Parser` (ratatui only rewrites changed
  cells with cursor jumps between them - naive ANSI-stripping of the raw
  byte stream produces a corrupted run-on string, discovered while writing
  these tests; `vt100` parses the actual escape sequences into a real
  screen grid):
  - bare invocation opens the dashboard with the centered logo and all 13
    commands visible on one normal-sized screen (the "reproduction" of the
    old bare-command experience, now showing what replaced it);
  - non-TTY bare invocation fails fast instead of hanging;
  - arrow-key navigation moves the selection marker; Escape is a no-op on
    the home screen;
  - the logo recenters correctly at a wide (140-col), exact-fit (100-col),
    and narrow (40-col, compact-wordmark) terminal size, live across a real
    PTY resize;
  - selecting an IPC action (`status`) with no daemon running shows the
    actionable "daemon is not running" error inline, then Esc returns home;
  - the full model-activation flow end-to-end through the real TUI: navigate
    to Model, the catalog lists the default entry, activating it reports
    "NOT fully activated" against a pre-seeded fake (non-GGUF) stub file -
    proving activation refuses to claim success when the model doesn't
    actually load, deterministically and offline (no multi-gigabyte
    download in a test).
- `tests/model_activation.rs` - library-level coverage of
  `commands::activate_model`'s orchestration (download outcome, config
  persisted regardless of later confirmation, `succeeded()` false on load
  failure, unknown catalog id rejected) without a terminal.

## Validation

- `cargo fmt --check` - clean.
- `cargo clippy --all-targets -- -D warnings` - clean.
- `cargo test` (default features) - 153 passed, 0 failed, across the lib,
  both binaries, and every integration test file (including the new ones).
- `cargo test --features gpu-vulkan` - 153 passed, 0 failed; this host has a
  real Vulkan-capable GPU, and the inference tests' own log output confirms
  the Vulkan backend genuinely probed it (`ggml_vulkan: Found 1 Vulkan
  devices: 0 = NVIDIA GeForce RTX 4080 SUPER`), not just a feature-gated
  no-op.
- `nix flake check` - "all checks passed!" (devShell and `packages.default`
  both evaluate cleanly).
- `nix build .#default` - succeeds end to end (build, `checkPhase`,
  `installPhase`, `fixupPhase`, `installCheckPhase`), producing a working
  `gpu-vulkan` binary. One fix was needed: the Nix build sandbox's
  `checkPhase` has no usable pty/tty subsystem, so `tests/tui_dashboard.rs`'s
  real-PTY tests (which pass normally under `nix develop -c cargo test` and
  plain `cargo test` - confirmed above) failed there with ENOENT on the
  nested `spawn_command`. Renamed those tests with a `pty_` prefix and added
  `cargoTestFlags = ["--" "--skip" "pty_"]` to `packages.default`, documented
  in both `flake.nix` and the test file's module doc comment. This only
  changes what the *packaged binary's own build-time test run* executes;
  every PTY test still runs (and passes) under every other invocation.
- Real TTY evidence of the completed dashboard, captured via `tmux` against
  the compiled binary (an isolated XDG sandbox, no real daemon/config
  touched), saved under `data/tt-tui-dashboard-1/tty-evidence/`:
  - `home-normal-100x32.txt` - first page at a normal terminal size: centered
    logo, status strip, all 13 commands with their exact `--help`
    descriptions, footer hint, no clipping.
  - `home-wide-140x32.txt` - logo recenters with a wider margin; no layout
    break.
  - `home-narrow-40x32.txt` - compact "TONGUETYPED" wordmark fallback (the
    full block-art logo would not fit); command list clips its overflow text
    within the bordered panel rather than wrapping or crashing.
  - `model-screen-100x40.txt` - the Model catalog screen, scrolled so the
    currently active model (`whisper-small-q5_k_m`, marked `active`) is
    visible with the `>` selection marker, correct column alignment, and the
    screen's own footer hint.
