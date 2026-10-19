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
- **Types where you are.** The transcript is typed or pasted into the focused
  app through `wtype`, `dotool`, `enigo`, or a built-in native `libei` session -
  whichever actually works on your compositor. When typing fails, the transcript
  is left on the clipboard.
- **Dictate into files.** Point `transcript_folder` at a directory and, while
  keeping transcripts is on, every finished transcript is also written there as
  a plain-text file, independent of the focused app or the clipboard.
- **Unobtrusive feedback.** A click-through overlay shows recording,
  transcribing, success, and failure - nine styles, on any number of screens.
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
sudo apt install ./tonguetyped-gpu-vulkan_*.deb         # Debian / Ubuntu
sudo dnf install ./tonguetyped-gpu-vulkan-*.rpm         # Fedora / RHEL
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

Every package carries the CPU backend as a fallback plus one GPU backend. The
default is Vulkan; NVIDIA and AMD users can pick CUDA or ROCm instead (x86_64
only):

```sh
nix profile install github:cheerfulScumbag/tonguetyped#gpu-cuda
nix profile install github:cheerfulScumbag/tonguetyped#gpu-rocm
```

From a local checkout, use `nix profile install .` instead of the flake URL. The
package ships the `tonguetyped` binary, its bash/zsh/fish shell completions and
man pages, and the `io.github.cheerfulScumbag.tonguetyped.desktop` entry, and
wraps the binary with the `wtype`, `dotool`, `wl-clipboard`, `xclip`, and
`canberra-gtk-play` helpers it needs.

### NixOS and home-manager modules

The flake also exports `nixosModules.default` and `homeManagerModules.default`
(also `homeModules.default`). Both provide the same `programs.tonguetyped`
options. Add the flake as an input:

```nix
inputs.tonguetyped.url = "github:cheerfulScumbag/tonguetyped";
```

then import one module. With home-manager:

```nix
{ inputs, ... }:
{
  imports = [ inputs.tonguetyped.homeManagerModules.default ];

  programs.tonguetyped = {
    enable = true;
    backend = "cuda";         # "vulkan" (default), "cuda", or "rocm"
    autostart = "systemd";    # "systemd" (default), "xdg", or "none"
    settings = {
      activation.mode = "toggle";
      model.active_model = "whisper-small-q5_k_m";
      output.method = "type";
      overlay.style = "pill";
    };
  };
}
```

On NixOS, import `inputs.tonguetyped.nixosModules.default` and set the same
options. The NixOS module installs the package for every user and starts the
daemon in each user's graphical session.

- `backend` picks the package build. Every build carries the CPU fallback;
  `cuda` and `rocm` are x86_64 only. Set `package` to use a build of your own.
- `autostart = "systemd"` runs a `tonguetyped.service` user unit tied to
  `graphical-session.target`, which KDE, GNOME, and compositors started through
  systemd or uwsm reach. `"xdg"` installs an XDG autostart entry instead;
  `"none"` leaves starting the daemon to you (for example `exec-once` in
  Hyprland). Do not also run `tonguetyped autostart enable`, which adds a
  second autostart entry.
- `settings` takes any key from [Configuration](#configuration). Common keys
  are typed, so a misspelled value fails at evaluation time.

`config.toml` stays a normal writable file, because the dashboard, `tonguetyped
setup`, and the daemon itself all write it. The modules merge the keys you set
over it: those keys are reset to your value, and every other key keeps what the
app last wrote. Removing a key from `settings` leaves its last value in the
file. home-manager applies settings on every activation and runs `tonguetyped
reload` if the daemon is up; the NixOS module applies them each time the
daemon starts, so on NixOS `settings` needs `autostart` set to `"systemd"` or
`"xdg"`.

`dotool`, used on KDE KWin and other non-wlroots Wayland compositors, needs
`/dev/uinput` access (see [platform notes](#platform-notes) for the built-in
`libei` alternative). On NixOS, set `hardware.uinput.enable = true` and add your
user to the `uinput` group.

### Arch Linux (AUR)

```sh
paru -S tonguetyped-bin   # or: yay -S tonguetyped-bin
```

`tonguetyped-bin` repackages the prebuilt Vulkan Linux binary (CPU fallback
included), its shell completions and man pages, and depends on `wtype`,
`dotool`, `wl-clipboard`, and `xdotool`, so typing works out of the box.

### Debian / Ubuntu (.deb)

Each release ships one `.deb` per GPU backend, and every one also carries the
CPU fallback: `tonguetyped-gpu-vulkan` (any Vulkan-capable GPU),
`tonguetyped-gpu-cuda` (NVIDIA), and `tonguetyped-gpu-rocm` (AMD). Download the
one for your GPU from the
[latest release](https://github.com/cheerfulScumbag/tonguetyped/releases/latest)
and install it:

```sh
sudo apt install ./tonguetyped-gpu-vulkan_*.deb   # or -gpu-cuda / -gpu-rocm
```

The variants replace one another, so installing a different one swaps the
backend. Every `.deb` installs the `tonguetyped` binary plus its bash, zsh, and
fish shell completions and man pages.

The `.deb` recommends `wtype` and `wl-clipboard`. Debian does not package
`dotool`, but KDE and GNOME Wayland still type out of the box through the
built-in `libei` backend (see [platform notes](#platform-notes)).

### Fedora / RHEL (.rpm)

The same GPU-backend builds ship as RPMs: `tonguetyped-gpu-vulkan`,
`tonguetyped-gpu-cuda`, and `tonguetyped-gpu-rocm`, each also carrying the CPU
fallback and the shell completions and man pages. Install the one for your GPU:

```sh
sudo dnf install ./tonguetyped-gpu-vulkan-*.rpm   # or -gpu-cuda / -gpu-rocm
```

As with the `.deb` variants, each provides, conflicts with, and obsoletes the
virtual `tonguetyped` name, so installing a different backend swaps it out.

### Other Linux builds

The release also attaches plain binaries for anyone installing by hand:
`tonguetyped-<tag>-linux-x86_64-gpu-<backend>` for the three x86_64 backends and
`tonguetyped-<tag>-linux-aarch64-cpu` for 64-bit ARM (CPU-only, with its own
`.deb` and `.rpm`). A `tonguetyped-<tag>-completions-and-man.tar.gz` bundle
carries the shell completions and man pages for install methods that want them
separately.

### Verifying a download

Every release artifact is listed in `SHA256SUMS.txt`. When the repository's GPG
signing secrets are configured, each artifact and the manifest itself also ship
a detached, ASCII-armored `<file>.asc` signature:

```sh
sha256sum --check --ignore-missing SHA256SUMS.txt
gpg --verify <artifact>.asc <artifact>
```

### Build from source

You need Rust, CMake, libclang, OpenSSL, pkg-config, ALSA development files,
libxdo development files, libxkbcommon development files, and Wayland client
development files:

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

The dashboard opens on a settings overview, grouped into four sections -
**Language & model**, **Capture & activation**, **Output**, and **Behaviour** -
with one row each for Model, Inference backend, Translation, Decoding, Text
processing, Microphone, Activation, Shortcut, Transcript output, Typing backend,
Transcript folder, History retention, Startup, Overlay, and Media pause. A
Commands row below them opens a separate screen listing the remaining commands,
where Up/Down selects a command and Enter runs it. Selecting a settings row
opens a screen that edits just that area, with the same live microphone preview
and shortcut dialog the setup wizard uses.

The value steppers and choice lists share one navigation grammar: Up/Down
moves between rows, Left/Right changes the highlighted choice in either
direction, Enter saves, and Esc (or Backspace) goes back. On Microphone,
Activation, Translation, Typing backend, Startup, Media pause, Overlay, and the
text forms, backing out also commits the highlighted value. Model, Inference
backend, Transcript output, Shortcut, and Daemon apply only on Enter. Free-text
rows keep Left/Right for the cursor and Backspace for deleting, with Esc as the
way back. The Overlay screen previews the highlighted style, look, waveform
shape, and position as you step them, so a choice is visible before Enter or
backing out saves it. The setup wizard walks most of the same settings one step
at a time with the same Overlay preview: Enter advances and Esc (or Backspace on
its list steps) goes back.

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
| `tonguetyped status --format json [--follow] [--extended]` | Machine-readable status for status bars |
| `tonguetyped reload` | Validate and reload the configuration |
| `tonguetyped last-result` | Print the most recent transcription |
| `tonguetyped transcribe <file>` | Transcribe a WAV file and print the transcript |
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

### Status bars

`tonguetyped status --format json` prints the daemon state as one JSON object
in the shape Waybar's custom modules read. Add `--follow` to keep running and
print a new line each time the state changes; a daemon that stops or restarts
shows as `stopped` and is picked up again when it comes back, so the stream
never ends while the bar runs.

```json
{"text":"recording","alt":"recording","class":"recording","tooltip":"TongueTyped: recording"}
```

The state is `idle`, `recording`, `processing`, or `stopped`, repeated in
`text`, `alt` and `class` so you can show it as text, map it to an icon, or
style it. `--extended` adds `model`, `backend`, `device` and `build` fields;
the tooltip lists `model`, `backend` and `device`, while `build` appears only in
the JSON.

```jsonc
// Waybar (config.jsonc)
"custom/tonguetyped": {
    "exec": "tonguetyped status --format json --follow",
    "return-type": "json",
    "format": "{icon}",
    "format-icons": {
        "idle": "",
        "recording": "●",
        "processing": "…",
        "stopped": ""
    },
    "on-click": "tonguetyped toggle"
}
```

### Pausing media while dictating

Set `audio.pause_media = true` (or pick it on the dashboard's Media pause
screen) to pause music and videos while you dictate. When a recording starts,
every MPRIS media player that is playing is paused; when the recording ends,
whether stopped, cancelled, or failed, those same players are resumed. A
player you stopped yourself in the meantime is left alone. It is off by
default.

```toml
[audio]
pause_media = true
```

### Overlay

Visual feedback is on by default and never takes keyboard focus or pointer
input. It identifies listening, transcribing, success, cancellation, failure,
and a no-speech outcome (an empty capture). `overlay.style` picks the style:

| Style | Description |
| --- | --- |
| `badge` | The default small rounded badge |
| `minimal` | A thin ring, no filled background |
| `pill` | A wider capsule |
| `blob` | A bright, glowing orb that stays soft and round in silence and spikes with your voice |
| `half-circle` | A gleaming semicircle docked to the top-centre edge |
| `border` | A full-screen frame that glows along every edge, brightest in the corners |
| `long-pill` | A wide dark glass bar with a waveform and a round status button |
| `live-text` | The streaming caption (glass): a dark glass card with a leading phase chip and one line of a streaming model's live words (see "Live text") |
| `live-text-box` | The streaming caption as a wrapped card that grows with its lines and scrolls internally, for long passages |

Each style offers a short menu of **looks** (`overlay.look`): its resting shape
(`plain`), its voice-reactive **waveform** (`waveform`, the default), or **live
text** (`live-text`). Which looks a style offers depends on the style, so the
settings screens never offer a pair that cannot be painted:

- `badge`, `minimal`, `pill`, `half-circle`, and `border` offer `plain` or
  `waveform`.
- `blob` has no options; it reacts in its own way.
- `long-pill` and the glass `live-text` caption offer `waveform` or `live-text`.
- `live-text-box` is `live-text` only.

A stored look a style does not offer is clamped to the style's first option.

The `waveform` look can be drawn as uniform `bars` or a continuous `squiggle`
(the long pill's line) via `overlay.waveform_shape` (`bars` by default, and
`squiggle` for the long pill, which has always drawn its continuous line). Only
the wide styles - `pill`, `long-pill`, and the glass caption - offer the shape
choice; the round styles and the live-text look ignore it.

Every waveform look follows your live microphone level: `badge`, `minimal`, and
`pill` show a scrolling multi-bar waveform; `long-pill` shows its squiggly trace
(or bars); `blob` sprouts sharp spikes on voice peaks and settles into a soft,
round, slowly breathing ball in silence; and `border`/`half-circle` pulse and
sweep with your voice. The `plain` look is the resting shape with no waveform.
Loud speech is a big response; silence is calm, with the waveform's quiet
stretches rendered as tiny dots.

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

Enable sound feedback separately:

```toml
[overlay]
enabled = true
style = "long-pill"
look = "waveform"
waveform_shape = "squiggle"

[audio]
feedback_sounds = true
feedback_volume = 0.7
```

### Transcript output

`output.method` defaults to `none`, so a fresh install transcribes and stores
but types nothing - a safe default while you test. Pick another method in the
dashboard's **Transcript output** screen or in `tonguetyped setup`:

| `output.method` | What happens to the transcript |
| --- | --- |
| `none` | Kept in TongueTyped only (history and `last-result`) |
| `type` | Typed into the focused application |
| `clipboard` | Copied to the clipboard; nothing is typed |
| `paste` | Copied to the clipboard, then pasted with `output.paste_keys` |

`output.paste_keys` is `"ctrl-v"` (most applications) or `"shift-insert"`
(terminals, which bind Ctrl+V to something else). `output.auto_submit` presses
Enter after the text is typed or pasted, useful for chat boxes and prompts; it
applies to `type` and `paste` only.

If the typing helper fails during `type` or `paste`, the transcript is left on
the clipboard instead and the overlay shows "Typing failed, copied to
clipboard", so you can paste it yourself. If the text was inserted but the
`auto_submit` Enter could not be pressed, it is not copied - it is already in
the application - and the overlay shows a distinct "Text entered, but Enter
failed" cue. The clipboard helper is `wl-copy` from
`wl-clipboard` on Wayland and `xclip` on X11. `tonguetyped doctor --test-type`
sends a test line through the configured method and says where it went.

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

Setting `transcript_folder` writes each finished transcript to that folder as a
plain-text file stamped with the date and time (one file per dictation, written
`0600`, `~/` expanded), but only while history is enabled - a folder on its own
writes nothing. Those files are write-once: TongueTyped never reads, monitors,
or prunes them, so they outlive the database retention. TongueTyped never
stores audio.

### Vocabulary and translation

Whisper misspells names, jargon and product terms it has rarely heard. The
**Decoding** setting takes a vocabulary prompt: text the decoder reads as if it
came just before your recording, which biases it toward the words it contains.
Whisper also copies the prompt's style, so write a punctuated sentence ("I
dictate in TongueTyped on KWin with dotool.") rather than a bare word list, or
the transcript loses its punctuation too. **Translation** turns speech in any
language into English text; the English-only `.en` models always transcribe.

```toml
[transcription]
initial_prompt = "I dictate in TongueTyped on KWin with dotool."
translate = false
# Optional Whisper decode knobs; leave them out to keep the model defaults.
temperature = 0.0                  # 0-1, first decoding temperature
temperature_increment = 0.2        # 0-1, retry step; 0 turns the retry off
compression_ratio_threshold = 2.4  # retry above this; inf turns it off
logprob_threshold = -1.0           # retry below this; -inf turns it off
no_speech_threshold = 0.6          # 0-1, treat as silence above this; inf turns it off
```

A running daemon reloads these automatically when the file changes; run
`tonguetyped reload` to apply an edit immediately instead of waiting for the
watch.

### Text processing

A finished transcript can be cleaned up before it is typed, copied, or
recorded. It is all local and off by default:

```toml
[text]
spoken_punctuation = false           # "comma" -> ",", "new line" -> a newline, ...
remove_fillers = false               # drop "um", "uh", ... (English dictation only)
post_process_command = ""            # command: transcript on stdin, cleaned text on stdout
post_process_timeout_seconds = 10    # kill the command after this and keep the text

[text.replacements]
"vox type" = "Voxtype"               # fix the same mistranscription every time
```

The replacement table is applied case-insensitively and only on whole words, so
`vox type` never rewrites part of `vox typed`. Spoken punctuation turns spoken
words into symbols (`comma`, `period`, `open paren`, `close brace`, `new line`,
`equals`, and so on) for dictating code. Filler removal drops unambiguous
English interjections (`um`, `uh`, `hmm`, ...), and runs only when
`transcription.language` names English (`en`, `english`, or a regional form) -
it is skipped for `auto` and every other language, so it can never delete a real
word in a non-English dictation. The optional `post_process_command` runs last
with the transcript on its stdin and the cleaned text on its stdout (a local LLM
cleanup pipe, a `sed` script, ...); any error or a timeout past
`post_process_timeout_seconds` falls back to the text without it. History and
the transcript-folder export record exactly the processed text that was output.

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

Qwen3-ASR auto-detects the spoken language across its 30 languages and accepts
no explicit language hint: `transcription.language` is ignored for it and the
engine runs in auto-detect mode (the model screen says so, and the daemon logs
the dropped hint once per load). Every other catalog model takes the configured
hint as before.

### Live text (streaming models)

Some models transcribe *as you speak*: while a recording is running their words
can stream across the overlay, and the finalized text is typed when you stop.
Whether a model can do this is a property of the model, not a setting, so the
live decode runs the way Handy does it. Whether the words are *shown* is your
call: choose a live-capable style's `live-text` look (see "Overlay" above) to
see them. `tonguetyped model list` marks the streaming models with a `live` tag
under **STREAMING** (or `live (en)` for the English-only ones), and the
dashboard's model screen marks them `live` in its LIVE column; the rest of the
catalog produces no live text and shows only the recording indicator.

Four things to know before choosing a streaming model:

- **Live text appears in the overlay only.** Partial words are preview: they are
  never typed into the focused application, and the transcript that is actually
  typed is the model's finalized result. Rewrites mid-recording therefore never
  reach your editor.
- **Live text is a choice of overlay look, not a model behaviour.** On a
  live-capable style, set `overlay.look = "live-text"` to see it: **Long Pill**
  and the glass **Live Text** caption (`overlay.style = "live-text"`) offer
  `waveform` or `live-text`, and **Live Text (box)** (`live-text-box`, a wrapped
  card that grows with its lines and scrolls, for long passages) is live-text
  only. The caption
  is a dark glass card with a leading phase chip, the committed text bright, the
  still-revisable tail dimmed, and a caret; a listening pad shows before the
  first word, and the words hold through the terminal phases. The single-line
  caption sizes its pill to its content - a small card while empty, growing only
  as words arrive (the box grows a wrapped line at a time and scrolls past its
  line cap). With any other style, or a live-capable style set to `plain` or
  `waveform` (for example `blob`, or `pill` on its default), the overlay shows
  that look and no live text at all, even with a streaming model active.
- **Live text and the waveform never appear together.** A style set to the
  `live-text` look paints words (or the listening pad) and never a waveform;
  every other look paints its own recording animation and never live words.
- **Most streaming models are English-only, but not all.** The small, fast
  Moonshine-streaming models and the English Parakeet port stream English only;
  the multilingual Nemotron 3.5 port and the Voxtral-realtime port stream their
  own language sets and are tagged `live` (no `(en)`) in `model list`. Pick a
  Whisper (or another multilingual offline) model when a streaming model does
  not cover your language; you keep the full transcript, just without live
  text.

Streaming models are recorded through the model's own native stream (nothing is
re-decoded on a timer), so the feature costs no extra CPU beyond the model
itself. A streaming model has no separate batch decode: the finalized text is
the transcript.

Some streaming models have a fixed output window - Moonshine-streaming caps its
decoder at roughly 48 s of English - and once one stream reaches that cap it
stops emitting words. Those are decoded as a sequence of segments: when the
running segment reaches the model-aware window (half the model's advertised
ceiling, about 24 s), its text is committed and a fresh stream starts on the
audio that follows, keeping the live text flowing for a long dictation. Each
segment boundary is placed in a pause where the audio has one, and the finished
segments are joined with a single space, so nothing at the seam is dropped or
duplicated. The unbounded streaming ports (Parakeet, Nemotron 3.5,
Voxtral-realtime) advertise no model cap, but each live segment is still bounded
by the decode window (`transcription.chunk_seconds`, 30 s by default), so they
roll over the same way; set `chunk_seconds = 0` to run a single stream to the end
of the recording instead.

### Long recordings and file transcription

A single live recording is capped by `transcription.max_recording_seconds`
(600 s by default). Decoding is bounded separately: audio longer than
`transcription.chunk_seconds` (30 s by default) is transcribed in back-to-back
chunks that each fit that window, with each chunk boundary snapped back to the
quietest point before its limit so a split lands in a pause rather than through
a word. This keeps memory and latency bounded for a long dictation instead of
sending one unbounded buffer through the decoder.

The window is model-aware. A model that advertises an audio limit is decoded in
windows at most half that limit, so its own ceiling is never overrun: Moonshine's
cap is on its *output* (a 194-token decoder budget, roughly 48 s of English), so
it is chunked at about 24 s rather than the configured 30 s. A hard-capped model
(canary, cohere, Qwen3-ASR) is likewise split into windows within its per-call
limit. Set `chunk_seconds = 0` to decode the whole buffer in one call for a
model with no practical limit; a model that advertises one is still bounded.

The same inference path transcribes an existing audio file:

```sh
tonguetyped transcribe recording.wav
```

`transcribe` reads a WAV (integer or float samples, any channel count or
sample rate), downmixes it to mono, resamples it to 16 kHz, applies VAD when
`transcription.vad_enabled` is set, and prints the transcript to stdout. It
loads the configured model once and reuses it for every chunk of a long file,
and does not need the daemon to be running.

---

## Platform notes

Typing into the focused application uses one of four backends. Automatic
selection tries `wtype`, then `enigo` on X11, then `dotool`, then the built-in
`libei` backend, and only ever uses one that actually works.

| Session | Typing helper | Notes |
| --- | --- | --- |
| wlroots Wayland (sway, Hyprland, niri) | `wtype` | Types through the Wayland virtual-keyboard protocol |
| X11 | `enigo` / `xdotool` | Built in; `xdotool` provides the X11 backend |
| KDE Plasma 6.1+ / GNOME 45+ Wayland | `libei` | Built in; types through the XDG RemoteDesktop portal's Emulated Input session, needing no `/dev/uinput` access |
| KDE KWin and other Wayland compositors with `/dev/uinput` | `dotool` | `wtype` cannot type here; `dotool` writes through `/dev/uinput` |

`dotool` needs write access to `/dev/uinput`, which is not granted by default.
Add your user to the `input` group or install a matching udev rule; a present
`dotool` that cannot open the device is treated as unavailable. The built-in
`libei` backend covers KDE and GNOME Wayland without that access, so **Debian
has no `dotool` package** no longer leaves KDE Wayland users unable to type:
`auto` picks `libei` there. The first dictation that uses `libei` shows the
portal's authorization dialog (naming TongueTyped) and stores a restore token so
later runs are silent; the daemon then holds one Emulated Input session for its
lifetime, so there is no per-dictation handshake. `typing_backend = "libei"`
selects it explicitly. If the portal is unavailable (an older KDE or GNOME, or a
denied request), typing falls back to leaving the transcript on the clipboard.

The Nix package and the AUR package both carry the helpers, so a normal install
works out of the box. When a method's helper is missing, the dashboard and the
setup wizard say so and name the remedy for your compositor - install the missing
helper, or switch to the built-in `libei` backend where it works - and refuse to
save an output method that could never work. `paste` needs both the clipboard
helper and a typing helper, which presses the paste keys.

### Other requirements

An ALSA-compatible input device is required for recording. Desktop-shortcut
activation requires an XDG Global Shortcuts portal; every terminal command keeps
working without one.

---

## Diagnostics

`tonguetyped doctor` checks the compositor, microphone, model, typing backends,
clipboard helper, and whether the configured output method can work, without
injecting text or requesting shortcut authorization. It reports
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
you want to script it. The running daemon watches this file and applies a valid
edit live, so you do not have to run `tonguetyped reload` or restart it; an
invalid edit is rejected, the overlay cues that the edit was rejected, and
`tonguetyped status` carries the detail, leaving the last good config in
effect. A representative slice:

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
method = "type"            # "none", "type", "clipboard", or "paste"
typing_backend = "auto"    # "auto", "wtype", "enigo", "dotool", or "libei"
auto_submit = false        # press Enter after "type" or "paste"
paste_keys = "ctrl-v"      # "ctrl-v" or "shift-insert", for "paste"

[text]
spoken_punctuation = false # "comma" -> ",", "new line" -> a newline, ...
remove_fillers = false     # drop "um", "uh", ... (English dictation only)
post_process_command = ""  # command: transcript on stdin, cleaned text on stdout

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

**A curated catalog.** `src/catalog.rs` tracks reviewed GGUF models from the
offline families this project has checked against its VAD-then-single-buffer
pipeline (whisper, parakeet, moonshine, canary, cohere, and Qwen3-ASR), plus
the streaming ports that feed live text (Moonshine-streaming, the Parakeet
ports, and Voxtral-realtime; see "Live text" above), all published by
[handy-computer](https://huggingface.co/handy-computer). Each entry is pinned to
a commit with an expected byte size and SHA-256, and `tonguetyped model list`
labels it with its family and language coverage (and its `live`/`live (en)`
streaming tag). Families differ in how much audio one call accepts: whisper
and parakeet take any length, moonshine's decoder has a fixed output budget
(about 48 s of English, kept partially if a run still hits it), canary and
cohere reject input past about 400 s, and Qwen3-ASR rejects input past its
5,218 s decoder-context cap (about 87 minutes), so
`InferenceEngine::transcribe` always keeps each call within the active model's
advertised limit - splitting any such model's input into windows within that
limit even when `chunk_seconds = 0` - and `Config::validate` additionally
refuses a config whose per-call buffer
(`min(max_recording_seconds, chunk_seconds)`, or just `max_recording_seconds`
with chunking off) would exceed it. A streaming model bypasses that offline path:
`ProductionRuntime::record_streaming` feeds it the live capture and takes the
joined finalized text as the transcript, so it is never handed a single offline
buffer.

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
