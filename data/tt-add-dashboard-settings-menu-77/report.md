# Dashboard settings menu

Adds the overview/settings navigation the captain asked for: the dashboard
home screen is now two stacked panels sharing one selection cursor - a fixed
eight-row **Settings** panel (Model, Microphone, Activation, Shortcut,
Transcript output, Typing backend, Startup, Overlay) showing each area's
current value, above the **Commands** panel (the CLI commands that are not
settings areas). Every one of the eight areas opens its own screen exposing
exactly the settings the Setup wizard already configures for it. The
underlying settings themselves are unchanged; only how they are reached and
edited from the dashboard is new.

The design went through the same SuperDesign + captain sign-off process as
the original dashboard and the overlay work. Captain approved **Option B** -
the two-section home with unified selection and no separate hub screen - see
`design-brief.md` for both mockup options, the preview links, and the
approved per-screen decisions.

## Home screen (Option B)

- `src/tui/mod.rs`'s `home_items()` still derives the Commands panel from
  clap metadata (`Cli::command().get_subcommands()`) and now also filters out
  `model` and `autostart`: those two are settings areas, so they live in the
  Settings panel and each area has exactly one home-screen entry point.
- `SettingId::ALL` is the eight areas in the captain's order; the Settings
  panel renders each row as `{name:<18} {current value}` where the value is
  computed from the same `Config` fields the settings screen edits (model id,
  resolved microphone label, activation mode, bound shortcut or keybind,
  output method, typing backend, startup choice, overlay summary).
- One selection cursor spans both panels: `home_selected < 8` selects a
  Settings row, otherwise it selects
  `home_selected - 8` in `command_items`. The Commands panel only scrolls
  while the cursor is inside it, so the Settings panel never nudges the view.

## Settings screens (`src/tui/screens.rs`)

All follow the existing dashboard pattern (bordered list, `> ` cyan-bold
selection, Enter applies, Esc back, own one-line footer) and only mutate the
in-memory `Config` via `apply`; `App::save_settings_config` is the single
place that calls `Config::save`, so every screen reports the same green
saved/red failed result line.

| Area | Screen behavior |
| ---- | --------------- |
| Model | Existing `ModelScreen` (catalog list, Enter activates, activation confirmation), unchanged. |
| Microphone | Device list **plus the live input-level gauge** (same thresholds/colors as Setup). Highlighting a device previews it after a 250 ms settle delay; Enter applies `config.audio.microphone`. |
| Activation | Hold vs Toggle (`config::ACTIVATION_MODE_LABELS`). |
| Shortcut | Editable keybinding string, "currently bound" status, and the two portal actions: "Test shortcut - press it now" (`activation::test_shortcut_binding`) and "Set via system dialog (Ctrl+R)" (`activation::reconfigure_shortcut` via `setup::reconfigure_shortcut_async`). A successful test or dialog saves the typed keybind; merely typing and leaving does not. `q` is a literal character here, same as the Setup console's shortcut step. |
| Transcript output | Keep in TongueTyped vs Type into the focused application; choosing keep resets the typing backend to `auto` exactly like the wizard. |
| Typing backend | `auto` plus every detected helper, with the "only used when transcripts are typed..." hint. |
| Startup | Existing `AutostartScreen` (Enter writes/removes the autostart entry and saves config), unchanged. |
| Overlay | One screen: Disabled/Enabled always, plus Position/Style/Streaming rows while enabled. Enter toggles the enable choice and cycles position through the seven real values, style through `badge`/`minimal`/`pill`, and flips the streaming indicator. A disabled overlay never shows the three configuration rows. |

Async work stays off the input path: `App::refresh_screens` polls the
microphone preview and the shortcut test/dialog handles each 66 ms tick, the
same non-blocking shape the Setup console uses. Leaving the Shortcut screen
while its dialog is open is allowed (the background thread finishes
unobserved, exactly like the console's in-flight reconfigure).

## Shared logic extracted rather than duplicated

- `audio::MicMonitor` now owns the microphone-preview stream for both the
  Setup console's Microphone step and the dashboard's Microphone screen:
  the settle delay after rapid navigation, background stream-error routing,
  and restart. `audio::level_to_ratio` is the one gauge mapping. The console
  was refactored onto both, with its existing behavior tests preserved
  (rewritten against the monitor's test helpers).
- `Capabilities` (`src/setup.rs`) now exposes `discover_or_default`,
  `microphone_label`, `output_labels` and `typing_backend_values`; the
  console and dashboard build their mic/output/backend lists from the same
  methods.
- The overlay value/label lists moved to `src/overlay.rs`
  (`POSITION_VALUES`, `STYLE_VALUES`, `STYLE_LABELS`, `STREAMING_LABELS`) and
  the activation/startup choice labels to `src/config.rs`
  (`ACTIVATION_MODE_LABELS`, `STARTUP_LABELS`); both UIs import them, so a
  new choice/value is added once.
- `setup::shortcut_test_async` mirrors `reconfigure_shortcut_async` (own
  background thread + single-threaded runtime reporting through an
  `Arc<Mutex<Option<...>>>`) so the dashboard can keep drawing while the
  portal waits for a press.

## Tests

- `src/tui/screens.rs` unit tests: overlay position/style cycling visits
  every configured value and wraps, disabling hides the sub-rows, the
  streaming row uses the shared labels, output switching to "keep" resets
  the backend, the typing-backend screen starts on the configured backend,
  and the activation screen reflects/updates the mode.
- `src/tui/mod.rs` unit tests: the Commands panel is exactly the nine
  non-settings commands in declared order, and the Settings panel is the
  captain's eight areas in order.
- `src/audio.rs` unit tests: the gauge mapping bounds (moved from the
  console tests), plus the console's mic-monitor behavior tests now run
  against `MicMonitor`.
- `tests/tui_dashboard.rs` real-PTY tests, driving the compiled binary
  through a PTY with `vt100::Parser` screen reconstruction (same harness as
  before):
  - the home screen shows all eight settings rows with real current values
    and all nine commands, no `start`/`stop`;
  - one cursor crosses from the Settings panel into the Commands panel;
  - arrow keys move the selection, Esc is a no-op on home;
  - activating Activation, Transcript output / Typing backend, Startup and
    Overlay settings through their screens writes the change to
    `config.toml` and the Settings panel reflects it;
  - the Overlay screen starts with only Disabled/Enabled when the config
    disables it, shows the three configuration rows once enabled, and
    cycling Position persists `position = "center"`;
  - the Shortcut screen edits the raw key string, rejects an invalid value
    inline, and surfaces a portal failure inline (the sandbox has no session
    bus) while staying usable;
  - the Microphone screen renders the device list and the Input level panel;
  - Startup writes the autostart desktop entry in the sandbox.
  - the pre-existing tests (model activation flow, daemon screen, IPC error
    path, logo reflow at wide/narrow sizes, non-TTY failure) were updated
    for the new home indexes and still pass.

## Validation

- `cargo fmt --check` - clean.
- `cargo clippy --all-targets -- -D warnings` - clean.
- `cargo test` - 197 passed, 0 failed (112 lib, 3 cli, 4 daemon-startup, 37
  coordinator/IPC, 7 model-cli, 8 model-activation, 4 setup-cli, 2 + 3 setup
  console, 15 tui dashboard, 2 autostart).

## Real TTY evidence

Captured with `tmux` against the compiled debug binary in an isolated XDG
sandbox, saved under `tty-evidence/`:

- `home-100x32.txt` - the new two-section home at a normal size: Settings
  panel with current values on top, Commands below, one `>` cursor.
- `home-narrow-40x32.txt` - compact wordmark, both panels still readable,
  long values clipped inside their borders (no wrap/crash).
- `home-overlay-selected-100x32.txt` - the cursor sitting on the Overlay
  settings row.
- `microphone-100x32.txt`, `activation-100x32.txt`, `shortcut-100x32.txt`,
  `transcript-output-100x32.txt`, `typing-backend-100x32.txt`,
  `startup-100x32.txt`, `overlay-100x32.txt` - each settings screen as
  rendered by the real binary.

Chrome is not installed in this environment, so no screenshots of the
SuperDesign preview pages were possible (same blocker the original dashboard
task recorded); the fetched draft HTML plus the real TTY evidence above stand
in.
