<div align="center">
<pre>
████████╗ ██████╗ ███╗   ██╗ ██████╗ ██╗   ██╗███████╗████████╗██╗   ██╗██████╗ ███████╗██████╗
╚══██╔══╝██╔═══██╗████╗  ██║██╔════╝ ██║   ██║██╔════╝╚══██╔══╝╚██╗ ██╔╝██╔══██╗██╔════╝██╔══██╗
   ██║   ██║   ██║██╔██╗ ██║██║  ███╗██║   ██║█████╗     ██║    ╚████╔╝ ██████╔╝█████╗  ██║  ██║
   ██║   ██║   ██║██║╚██╗██║██║   ██║██║   ██║██╔══╝     ██║     ╚██╔╝  ██╔═══╝ ██╔══╝  ██║  ██║
   ██║   ╚██████╔╝██║ ╚████║╚██████╔╝╚██████╔╝███████╗   ██║      ██║   ██║     ███████╗██████╔╝
   ╚═╝    ╚═════╝ ╚═╝  ╚═══╝ ╚═════╝  ╚═════╝ ╚══════╝   ╚═╝      ╚═╝   ╚═╝     ╚══════╝╚═════╝
</pre>
<strong>S P E A K . &nbsp; T Y P E . &nbsp; R E P E A T .</strong>
</div>

<p align="center">
  <a href="https://github.com/cheerfulScumbag/tonguetyped/blob/main/LICENSE"><img src="https://img.shields.io/badge/license-MIT-blue.svg" alt="License: MIT"></a>
  <img src="https://img.shields.io/badge/platform-Linux-informational.svg" alt="Platform: Linux">
  <img src="https://img.shields.io/badge/rust-2021-orange.svg?logo=rust" alt="Rust 2021">
  <img src="https://img.shields.io/badge/offline--first-success.svg" alt="Offline-first">
</p>

**TongueTyped** is a privacy-focused offline dictation app for Linux. Speak, and
your words are transcribed on your own machine and typed into whichever
application already has focus - or written straight to text files. There is no
account, no cloud, and no audio ever leaves the computer.

It is built for the terminal: one daemon owns the microphone and the speech
model, a small CLI drives it, and running `tonguetyped` with no arguments opens
an interactive dashboard for every setting.

---

## Highlights

- **Fully local inference.** Speech recognition runs through
  [transcribe.cpp](https://github.com/handy-computer/transcribe.cpp) on GGUF
  models - CPU by default, or Vulkan, CUDA, ROCm, or Metal when you build them
  in. Nothing is uploaded.
- **Keyboard-first.** Bind "Start or stop dictation" once in your desktop's
  shortcut dialog. Hold to talk, or press once to start and once to stop.
- **Types where you are.** The transcript is typed into the focused app through
  `wtype`, `dotool`, or `enigo` - whichever actually works on your compositor.
- **Dictate into files.** Point `transcript_folder` at a directory and every
  finished transcript is also written there as a plain-text file, independent of
  the focused app or the clipboard.
- **Unobtrusive feedback.** A click-through overlay shows recording,
  transcribing, success, and failure - six styles, on any number of screens.
- **Terminal-native control.** Script it, or use the dashboard. Every command is
  a plain subcommand over a local Unix socket.
- **Your transcripts stay yours.** History is a local SQLite database with
  retention you control, plus an optional plain-text export folder.

---

## Quick start

Install through whichever channel matches your system, then run the two setup
commands:

```sh
# 1. Install - pick one (every channel is detailed under Installation):
paru -S tonguetyped-bin                                 # Arch Linux (AUR)
nix profile install github:cheerfulScumbag/tonguetyped  # Nix
sudo apt install ./tonguetyped_*.deb                    # Debian / Ubuntu
cargo build --release                                   # build from source

# 2. Configure and start on login:
tonguetyped setup
tonguetyped autostart enable
```

`tonguetyped setup` does not choose a dictation key for you. The "Start or stop
dictation" action is registered with your desktop by the daemon, and the wizard's
Shortcut step only opens the desktop's own shortcut dialog when you press Ctrl+R
there - the dashboard's Shortcut screen offers an explicit action to open that
same dialog. If you skip it, setup finishes with no key bound. See
[the desktop shortcut](#the-desktop-shortcut) for why.

Sign out and back in after the first install so your desktop's portal discovers
the new application entry, then press your key and speak.

---

## Installation

TongueTyped targets Linux desktops. Pick the channel that matches your system.

### Nix (flakes)

```sh
# Install a user-profile package (Vulkan-accelerated by default):
nix profile install github:cheerfulScumbag/tonguetyped

# Upgrade in place:
nix profile upgrade tonguetyped

# Try without installing:
nix run github:cheerfulScumbag/tonguetyped -- daemon
```

From a local checkout, use `nix profile install .` instead of the flake URL. The
package ships the `tonguetyped` binary and the
`io.github.cheerfulScumbag.tonguetyped.desktop` entry, and wraps the binary with
the `wtype`, `dotool`, `wl-clipboard`, and `canberra-gtk-play` helpers it needs.

### Arch Linux (AUR)

```sh
paru -S tonguetyped-bin   # or: yay -S tonguetyped-bin
```

`tonguetyped-bin` repackages the prebuilt Linux binary and depends on `wtype`,
`dotool`, `wl-clipboard`, and `xdotool`, so typing works out of the box.

### Debian / Ubuntu (.deb)

Download the `.deb` for your variant from the
[latest release](https://github.com/cheerfulScumbag/tonguetyped/releases/latest)
and install it:

```sh
sudo apt install ./tonguetyped_*.deb   # or the tonguetyped-gpu-*.deb variant
```

The `.deb` recommends `wtype` and `wl-clipboard`. Debian does not package
`dotool`, so KDE Wayland users must build it from source (see
[platform notes](#platform-notes)).

### Build from source

You need Rust, CMake, libclang, OpenSSL, pkg-config, ALSA development files,
libxdo development files, and Wayland client development files:

```sh
cargo build --release
./target/release/tonguetyped setup
./target/release/tonguetyped daemon
```

The Nix flake provides the whole toolchain: `nix develop` then `cargo build
--release`. On NixOS, run build commands through `nix develop -c`.

### GPU acceleration

A plain `cargo build` links transcribe.cpp CPU-only. Enable one accelerator at
a time with a Cargo feature:

```sh
cargo build --release --features gpu-vulkan  # or gpu-cuda, gpu-rocm, gpu-metal
```

`model.preferred_backend` (`auto` by default) controls which backend the engine
loads at runtime. `auto` tries CUDA, ROCm, Vulkan, then Metal before always
falling back to CPU; setting any single name (`cpu`, `vulkan`, `cuda`, `rocm`,
`metal`) pins the engine to it and fails loudly instead of silently switching.

---

## Using TongueTyped

### The dashboard

Run `tonguetyped` with no subcommand:

```sh
tonguetyped
```

The dashboard opens on a settings overview - one row each for Model, Inference
backend, Microphone, Activation, Shortcut, Transcript output, Typing backend,
Transcript folder, History retention, Startup, and Overlay - above a list of the
remaining commands. Selecting a row opens a screen that edits just that area,
with the same live microphone preview and shortcut dialog the setup wizard uses.

### Commands

| Command | What it does |
| --- | --- |
| `tonguetyped` | Open the interactive dashboard |
| `tonguetyped setup` | Configure everything interactively |
| `tonguetyped daemon` | Start the daemon (foreground) |
| `tonguetyped daemon stop` / `restart` | Gracefully stop, or stop and start fresh |
| `tonguetyped start` / `stop` / `toggle` | Control recording |
| `tonguetyped cancel` | Cancel recording or discard in-flight processing |
| `tonguetyped status` | Show daemon state, activation mode, and shortcut health |
| `tonguetyped reload` | Validate and reload the configuration |
| `tonguetyped last-result` | Print the most recent transcription |
| `tonguetyped doctor` | Check runtime dependencies, mic, model, and backends |
| `tonguetyped shortcut-test` | Bind the shortcut and wait for you to press it |
| `tonguetyped autostart enable` / `disable` | Toggle desktop-session autostart |
| `tonguetyped model list` / `install` / `use` / `remove` | Manage GGUF models |

The daemon listens on `$XDG_RUNTIME_DIR/tonguetyped/control.sock` and refuses to
start a second instance.

### The desktop shortcut

TongueTyped registers the action **"Start or stop dictation"** with your
desktop's global-shortcuts portal but deliberately never chooses a key for it.
The binding belongs to the desktop, so you set it in your own shortcut dialog -
KDE's System Settings, GNOME's Keyboard settings, and so on. The dashboard's
Shortcut screen offers an explicit action to open that dialog for you.

This means a fresh install starts **unbound**: nothing responds until you assign
a key. `tonguetyped shortcut-test` binds the action and waits up to 15 seconds
for you to press whatever key you have bound, reporting whether the press was
actually detected.

Activation comes in two modes, set from the dashboard or `config.toml`:

- **Hold** (default) - record while the key is held, stop when released.
- **Toggle** - press once to start, press again to stop.

### Window managers

On a wlroots compositor you can skip the desktop portal entirely and bind a key
to the CLI, which talks to the daemon over its local socket. Every recording
command is a valid bind target:

- `tonguetyped toggle` - start or stop on one key (best for a single binding)
- `tonguetyped start` / `tonguetyped stop` - explicit start and stop
- `tonguetyped cancel` - abort the current recording or in-flight processing
- `tonguetyped last-result` - print the latest transcript

Bind your key to `tonguetyped toggle`:

```ini
# Hyprland (hyprland.conf)
bind = SUPER, S, exec, tonguetyped toggle

# Sway
bindsym $mod+s exec tonguetyped toggle

# mango / mangowc
bind=SUPER,s,spawn,tonguetyped toggle
```

Because the key runs the command directly, this is effectively
press-to-toggle regardless of the `activation.mode` setting; the portal's
hold/toggle distinction only applies to the desktop shortcut above.

### Overlay

Visual feedback is on by default and never takes keyboard focus or pointer
input. It identifies listening, transcribing, success, cancellation, failure,
and a no-speech outcome (an empty capture). `overlay.style` picks the look:

| Style | Description |
| --- | --- |
| `badge` | The default small rounded badge with an icon and phase colour |
| `minimal` | A bare dot, no chrome |
| `pill` | A wider pill-shaped badge with a live microphone waveform |
| `blob` | A bright, glowing orb that stays soft and round in silence and spikes with your voice |
| `half-circle` | A gleaming semicircle docked to the top-centre edge |
| `border` | A full-screen frame that glows along every edge, brightest in the corners |
| `long-pill` | A wide dark bar with a squiggly live waveform and a round status button |

Every style's recording animation follows your live microphone level: `badge`,
`minimal`, and `pill` show a scrolling multi-bar waveform when
`overlay.live_waveform` is on (the default), or a pulsing dot that swells with
your voice when it is off; `long-pill` always shows its squiggly trace; `blob`
sprouts sharp spikes on voice peaks and settles into a soft, round, slowly
breathing ball in silence; and `border`/`half-circle` pulse and sweep with
your voice. Loud speech is a big response; silence is calm, with the
waveform's quiet stretches rendered as tiny dots.

`border` and `half-circle` ignore `overlay.position`; the other styles honour
`overlay.position` (`top-left`, `top-right`, `bottom-left`, `bottom-right`,
`top`, `bottom`, or `center`).

By default the overlay draws on **every connected screen**: `overlay.monitor`
is `all` (the legacy `active` is an alias for it). Set a specific output name
from `tonguetyped doctor` to pin it to one monitor. Hotplugging outputs is
handled without restarting the daemon.

The overlay is a `wlr-layer-shell` surface, which is available on wlroots-based
compositors (sway, Hyprland, niri) and on modern KWin (validated on KDE Plasma
6.7+), but not on X11 or older compositors. Where it is unavailable, TongueTyped
falls back to Plasma's native OSD on KDE, then to a transient freedesktop
notification - in that fallback tier your desktop controls placement, so
`overlay.position` and `overlay.monitor` no longer apply. Run `tonguetyped
doctor` to see which tier is active.

`overlay.live_waveform` (`true` by default) picks the busier recording glyph:
the live multi-bar microphone waveform instead of the simple pulsing dot.
Enable sound feedback separately:

```toml
[overlay]
enabled = true
style = "pill"
live_waveform = true

[audio]
feedback_sounds = true
feedback_volume = 0.7
```

### Transcript output

`output.method` defaults to `none`, so a fresh install transcribes and stores
but types nothing - a safe default while you test. Choose **Type into the
focused application** in the dashboard (or set `output.method = "type"`) to type
transcripts into whatever has focus. `output.auto_submit` presses Enter after
typing, useful for chat boxes and prompts. TongueTyped never uses the clipboard
as a typing fallback.

### History and the transcript folder

When history is enabled, transcripts are stored in
`$XDG_DATA_HOME/tonguetyped/history.db` with user-only permissions. Two
retention limits are applied on every write, independently of the output method:

```toml
[history]
enabled = true
max_entries = 100          # 0 = no entry limit
max_age_days = 30          # 0 = no age limit
transcript_folder = ""     # e.g. "/home/you/Documents/tonguetyped"
```

Setting `transcript_folder` also writes each finished transcript to that folder
as a plain-text file stamped with the date and time (one file per dictation,
written `0600`, `~/` expanded). Those files are write-once - TongueTyped never
reads, monitors, or prunes them, so they outlive the database retention.
TongueTyped never stores audio.

### Models

Models are GGUF files managed from a reviewed catalog:

```sh
tonguetyped model list                          # catalog + install status
tonguetyped model install whisper-small-q5_k_m  # resumable, SHA-256-verified
tonguetyped model install whisper-tiny-q5_k_m --use
tonguetyped model use whisper-small-q5_k_m
tonguetyped model remove whisper-small-q5_k_m   # delete a non-active install
```

Downloads resume from a partial file when the server supports HTTP range
requests, and are re-verified against a pinned SHA-256 before going live; a
corrupt download is deleted, not kept. The active model lives in
`model.active_model` and is used by every backend alike - there is no separate
CPU or GPU model.

---

## Platform notes

Typing into the focused application uses one of three backends. Automatic
selection tries `wtype`, then `enigo` on X11, then `dotool`, and only ever uses
a backend that actually works.

| Session | Typing helper | Notes |
| --- | --- | --- |
| wlroots Wayland (sway, Hyprland, niri) | `wtype` | Types through the Wayland virtual-keyboard protocol |
| X11 | `enigo` / `xdotool` | Built in; `xdotool` provides the X11 backend |
| KDE KWin and other Wayland compositors | `dotool` | `wtype` cannot type here; `dotool` writes through `/dev/uinput` |

`dotool` needs write access to `/dev/uinput`, which is not granted by default.
Add your user to the `input` group or install a matching udev rule; a present
`dotool` that cannot open the device is treated as unavailable. **Debian has no
`dotool` package** - KDE Wayland users installing from the `.deb` must build
`dotool` from source (`go build` from its upstream repository) to type on KWin.

The Nix package and the AUR package both carry the helpers, so a normal install
works out of the box. When no helper is available, the dashboard and the setup
wizard say so and name what to install for your compositor, and refuse to save a
typing method that could never work.

### Other requirements

An ALSA-compatible input device is required for recording. Desktop-shortcut
activation requires an XDG Global Shortcuts portal; every terminal command keeps
working without one.

---

## Diagnostics

`tonguetyped doctor` checks the compositor, microphone, model, and typing
backends without injecting text or requesting shortcut authorization. It reports
the selected model, the configured backend preference, the backend and device
actually in use, and its own build identifier - the git commit it was built from
- comparing it against the running daemon so a stale daemon is flagged.

After each dictation, the daemon logs one structured `tonguetyped::latency`
event at `info` level with separate durations for audio finalization, VAD, model
loading, inference, output, and history. Latency events contain no transcript
text and no audio.

---

## Configuration

TongueTyped reads `$XDG_CONFIG_HOME/tonguetyped/config.toml`. Almost everything
is editable from `tonguetyped setup` and the dashboard; the file is there when
you want to script it. A representative slice:

```toml
[activation]
mode = "hold"              # "hold" or "toggle"

[model]
active_model = "whisper-small-q5_k_m"
preferred_backend = "auto"

[model.idle_unload]
policy = "after_idle"      # "never", "after_transcription", or "after_idle"
timeout_minutes = 15

[output]
method = "type"            # "none" or "type"
typing_backend = "auto"    # "auto", "wtype", "enigo", or "dotool"
auto_submit = false

[overlay]
enabled = true
position = "top-right"
style = "badge"
monitor = "all"
```

By default the model unloads after 15 minutes of inactivity to free memory; the
next dictation then pays a cold-load cost. Set `policy = "never"` to keep it
resident, or `"after_transcription"` to free it after every dictation.

---

<details>
<summary><strong>Under the hood</strong> - inference, models, and benchmarking</summary>

**One engine, every backend.** A single module, `src/inference.rs`, handles CPU,
Vulkan, CUDA, ROCm, and Metal through one Rust binding for transcribe.cpp (the
`transcribe-cpp` crate). `InferenceEngine::load` always requests an explicit
backend and never lets the library auto-select, so a fallback never silently
substitutes a different model or family.

**A curated catalog.** `src/catalog.rs` tracks every reviewed `family =
"whisper"` GGUF model published by
[handy-computer](https://huggingface.co/handy-computer), each pinned to a commit
with an expected byte size and SHA-256.

**Benchmarking.** `examples/transcribe_benchmark.rs` measures cold load and warm
inference on a 16 kHz mono WAV:

```sh
cargo run --release --example transcribe_benchmark -- \
  "$HOME/.local/share/tonguetyped/models/whisper-small-Q5_K_M.gguf" \
  recording.wav --runs 5
```

Pass `--vad-model PATH` to include production VAD, and `--backend NAME` to force
one backend. It writes a JSON record with raw runs, medians, p95, hashes, host
CPU, and device - never transcript text.

See `data/tt-transcribe-cpp-1/report.md` for the CPU/Vulkan/CUDA consolidation
evidence and `data/tt-model-catalog-19/report.md` for the catalog derivation.

</details>

---

## License

MIT - see [LICENSE](LICENSE).
