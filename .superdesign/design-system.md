# TongueTyped terminal design system

Source of truth: `src/setup/console.rs` (the only existing full-screen interactive
surface in the app today, reached via `tonguetyped setup` in a real terminal) plus
the line-based fallback UI in `src/setup.rs` and the plain-text output of
`doctor`/`status`/`model list` in `src/main.rs`. This is a **terminal UI (TUI)**
product rendered with `ratatui` 0.30 + `crossterm` 0.29 - not a web app. Every
token below maps directly onto a `ratatui::style::Color` / `Modifier`, not a CSS
value; the HTML reference and SuperDesign drafts use these as a terminal-emulator
approximation only (see `replica_html_template/` for how the mapping works).

## Product context

TongueTyped is a local, offline Linux dictation daemon: hold or toggle a
keyboard shortcut, speak, and the transcript is typed into the focused
application (or kept in history). The terminal surface is the entire control
plane - there is no GUI window. Today a user reaches every feature through
individual CLI subcommands (`tonguetyped setup`, `tonguetyped status`, ...);
`tonguetyped setup` is the only one of those subcommands that currently opens a
full-screen interactive console instead of printing text and exiting.

## Rendering model

- **Alternate screen + raw mode.** `crossterm::terminal::{enable_raw_mode,
  EnterAlternateScreen}` on entry, always paired with a `Drop` guard
  (`TerminalGuard`) that calls `disable_raw_mode` + `LeaveAlternateScreen` even on
  an error/panic unwind path. Any new interactive screen must follow the same
  guard pattern - terminal corruption on exit is a correctness bug, not a
  cosmetic one.
- **Redraw loop.** `terminal.draw(...)` once per iteration of a loop that polls
  input with a 66ms timeout (`event::poll(Duration::from_millis(66))`), i.e.
  ~15fps. This is fast enough for live meters (the mic level gauge) without
  burning CPU when idle.
- **Full-bleed layout.** `frame.area()` is split top-to-bottom into fixed-height
  chrome (logo, status line) and a flexible body (`Constraint::Min(5)`), with a
  one-line footer pinned to the bottom. No sidebars, no floating panels, no
  modals - everything is one full-width vertical stack.

## Color palette (ratatui `Color` names, used as semantic roles)

| Role                | Color                               | Usage                                            |
| ------------------- | ------------------------------------ | ------------------------------------------------ |
| Brand / accent       | `Color::Cyan` + `Modifier::BOLD`     | Logo, selected list row, progress gauge fill, titles |
| Success              | `Color::Green`                      | Completed download rows, "All required files are ready.", success banners |
| Error / danger       | `Color::Red`                        | Footer error line, failed download rows, mic unavailable, >85% input level |
| Warning              | `Color::Yellow`                      | 50-85% input level (mic gauge mid-range)         |
| Default foreground   | terminal default (no explicit color) | Unselected list rows, body text, hint footer      |
| Borders              | terminal default, `Borders::ALL`     | Every panel (`Block::default().borders(Borders::ALL).title(...)`) |

There is no purple/magenta/blue/orange anywhere in the existing surface. A new
screen must stay inside this five-role palette; do not invent new hues.

Colors are **never** the only signal: the selected row also gets a leading `"> "`
marker + bold weight, and the mic gauge's color threshold is paired with its
numeric ratio, so the same information survives a `NO_COLOR` / monochrome
terminal (see Accessibility below).

## Typography

Monospace only (the terminal's own font - there is no font selection). No
italics. Two weight states: `Modifier::BOLD` (brand / selection / emphasis) and
regular. No underline, no strikethrough, no dim/faint text anywhere in the
current surface.

## The logo

A fixed ASCII/box-drawing wordmark (`const LOGO` in `console.rs`), 8 lines tall
(`LOGO_HEIGHT = 8`, enforced by a unit test), rendered in `Color::Cyan` +
`Modifier::BOLD`:

```
████████╗ ██████╗ ███╗   ██╗ ██████╗ ██╗   ██╗███████╗████████╗██╗   ██╗██████╗ ███████╗██████╗
╚══██╔══╝██╔═══██╗████╗  ██║██╔════╝ ██║   ██║██╔════╝╚══██╔══╝╚██╗ ██╔╝██╔══██╗██╔════╝██╔══██╗
   ██║   ██║   ██║██╔██╗ ██║██║  ███╗██║   ██║█████╗     ██║    ╚████╔╝ ██████╔╝█████╗  ██║  ██║
   ██║   ██║   ██║██║╚██╗██║██║   ██║██║   ██║██╔══╝     ██║     ╚██╔╝  ██╔═══╝ ██╔══╝  ██║  ██║
   ██║   ╚██████╔╝██║ ╚████║╚██████╔╝╚██████╔╝███████╗   ██║      ██║   ██║     ███████╗██████╔╝
   ╚═╝    ╚═════╝ ╚═╝  ╚═══╝ ╚═════╝  ╚═════╝ ╚══════╝   ╚═╝      ╚═╝   ╚═╝     ╚══════╝╚═════╝

                              ░▒▓  S P E A K .  T Y P E .  R E P E A T .  ▓▒░
```

It is drawn as a single `Paragraph` (left-aligned text, not centered) occupying
a fixed-height chunk at the very top of the frame - the console today does
**not** center it; centering the logo using the full terminal width is new
required behavior for the dashboard (see the redesign brief), not something to
copy from this baseline. The wordmark is ~97 columns wide at its natural size;
any screen narrower than that needs an explicit narrow-terminal fallback (the
current console has no such fallback - it simply clips, which is a gap the
redesign must close).

## Layout primitives

- **Bordered panel**: `Block::default().borders(Borders::ALL).title(<step
  name>)` around every content area - the list, the shortcut panel, the
  confirmation summary, the download log, the microphone panel.
- **Selectable list**: one line per option, `"> "` prefix + cyan bold when
  selected, two-space indent + default style otherwise; scrolls via
  `.scroll((offset, 0))` once the selection would leave the visible region
  (`list_paragraph` in `console.rs`).
- **Gauge**: `ratatui::widgets::Gauge` with a bordered block and a title,
  colored by threshold (mic input level) or fixed cyan (download progress).
- **Step indicator**: a single plain line above the body, `"Step {i} of {n}:
  {title}"` - no progress bar, no breadcrumbs, just text.
- **Footer hint bar**: one line, bottom-pinned, either the contextual key hint
  (`"↑/↓ choose  Enter continue  Esc back  q quit"`) or, if present, the current
  error in red. Only one of the two is shown at a time - errors replace the
  hint rather than appending to it.

## Interaction model

- **Navigation**: `Up`/`k` and `Down`/`j` move a list selection, clamped (not
  wrapped) to the option range.
- **Confirm / advance**: `Enter`, `Right`, or `l`.
- **Back / cancel current step**: `Esc`, `Left`, or `h` - steps backward
  through a linear wizard; from the first step it exits.
- **Quit**: `q` - available on every step; no step captures raw text input.
- **Global cancel**: `Ctrl+C` is caught explicitly and treated as "finish
  cancelled" from any step, bypassing the normal step state machine.
- **Async work surfaces synchronously in the render loop**: background threads
  (model download, microphone level meter) report into `Arc<Mutex<T>>` state
  that the main render loop polls every frame - the UI thread itself never
  blocks on I/O. Any new screen that performs a slow action (daemon
  start/stop, model activation) must follow this same non-blocking,
  poll-reported-state shape rather than awaiting the action inline in the
  input-handling path.

## Accessibility notes (carry into the redesign)

- Every color-coded state (selection, success, error, warning, mic level) is
  paired with a non-color cue: a marker glyph, explicit text ("Unavailable:
  ...", "failed - ..."), or numeric value. Preserve this pairing in new
  screens; do not add a state that is color-only.
- All interaction is keyboard-only; there is no mouse support anywhere in the
  app, and none should be assumed.
- The footer hint bar is the only on-screen legend for available keys -
  every screen must keep one current and accurate for its own key set.

## What is explicitly NOT part of the current visual language

- No centered text/logo anywhere (everything is left-aligned).
- No multi-column / grid layouts - every screen is a single vertical stack.
- No icons beyond the box-drawing glyphs already in the logo.
- No animation beyond the live mic-level gauge and progress gauge.
- No modals/popups/overlays within the TUI itself (the Wayland layer-shell
  "overlay" in `src/overlay.rs` is a separate, non-interactive OS-level
  surface shown *outside* this console, for passive recording feedback - not
  part of this console's own widget vocabulary).
