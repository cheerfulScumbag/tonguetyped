# Dashboard settings menu - design brief (sign-off requested)

## What this is

The bare-invocation dashboard's home screen currently lists only top-level CLI
commands (`src/tui/mod.rs`'s `home_items()`), which gives standalone dashboard
screens for Model and Autostart (Startup) only. Mic, Activation, Shortcut,
Transcript output, Typing backend and Overlay settings exist only as steps
inside the linear first-run Setup wizard (`src/setup/console.rs`); there is no
overview menu that jumps straight to any one settings area.

This brief covers the SuperDesign mockup phase for a new settings
overview/menu. Per the task's firstmate spec, implementation waits for captain
sign-off on this brief.

## SuperDesign artifacts

Project: **TongueTyped Settings Menu** (extended from the existing
TongueTyped TUI Dashboard project)
<https://superdesign.dev/teams/45d90506-9cb8-4e5a-a285-fa9ccb916130/projects/a70a2971-b823-498c-9dea-c04b0d55aa51>

- Baseline "before" replica of today's home screen, authored by hand from a
  real 100x32 render: committed at
  `.superdesign/replica_html_template/dashboard_home.html`
  (draft preview: <https://p.superdesign.dev/draft/fabb452a-b28c-4fec-a1b7-f147a1da4426>)
- **Option A - Settings hub (recommended)**:
  "TongueTyped Settings Design Board" -
  <https://p.superdesign.dev/draft/02cf133f-7637-4e72-a844-be0146503a87>
- **Option B - Two-section home (alternative)**:
  "TongueTyped Alternative Dashboard Redesign - Corrected" -
  <https://p.superdesign.dev/draft/6883b829-6de3-43ad-8e25-f32a7cfab700>

Both drafts are design boards: one HTML page stacking labeled 100x32 terminal
frames, one frame per screen. Chrome is not installed in this environment
(same blocker the original dashboard design task hit), so no live screenshots
were captured; the generated HTML itself is the character-exact reference.

## The decision asked

**Where does the settings overview live, and do Model/Startup move into it?**

- **Option A (recommended)**: a new `settings` row is the first item in the
  home Commands panel; it opens a dedicated **Settings overview** screen
  listing all eight areas with their current values. Model and Autostart are
  removed from the home command list and reached through Settings (their CLI
  commands are unchanged). Home becomes: settings, setup, daemon, toggle,
  cancel, status, reload, last-result, doctor, shortcut-test.
  Rationale: exact reading of "an overview menu for these eight things";
  keeps home as actions and Settings as configuration; scales by adding hub
  rows; fits a 24-row terminal comfortably.
- **Option B (alternative)**: the home screen itself becomes two stacked
  panels - Settings (eight rows with current values) above Commands (nine
  rows) - with one selection cursor spanning both and no separate hub screen.
  Rationale: zero extra hop, every setting visible on first screen; costs a
  busier home screen and mixes configuration into the command list. Draft B's
  settings screens are otherwise identical to A's.

## What each option's frames settle (not open questions)

- **Eight areas, eight screens**, in the captain's order: Model, Microphone,
  Activation, Shortcut, Transcript output, Typing backend, Startup, Overlay.
- **Transcript output and Typing backend stay two separate menu entries and
  two screens** (as the captain listed them); the Typing backend screen
  carries the "only used when transcripts are typed into the focused
  application" hint.
- **Overlay is one screen**: Enabled/Disabled plus Position, Style and
  Streaming rows with the current values inline (not wizard sub-steps).
- **Microphone screen**: device list plus the existing live Input level gauge
  (same thresholds/colors as the Setup console), applied on Enter.
- **Shortcut screen**: editable key string, "currently bound" status, and two
  actions - "Test shortcut - press it now" (reuses
  `activation::test_shortcut_binding`) and "Set via system dialog (Ctrl+R)"
  (reuses `setup::reconfigure_shortcut_async` / `activation::reconfigure_shortcut`).
- **Model and Startup screens** keep their existing dashboard designs; they
  are only re-parented behind the hub.
- Settings screens are dashboard sub-screens (no logo, full-frame bordered
  panel), matching the existing Model/Autostart/Daemon screens; only home
  keeps the centered logo and status strip.
- Every settings change applies immediately with a green save/feedback line
  (same shape as Autostart's existing "Autostart setting saved." result line).

## Non-goals

- No change to what any setting does, to config fields, to the Setup wizard,
  or to CLI commands (`model`, `autostart` keep working exactly as today).
- No new CLI subcommand is required; this is dashboard navigation only.
