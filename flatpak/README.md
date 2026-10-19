# Local Flatpak test build

`io.github.cheerfulScumbag.tonguetyped.yml` is a local verification manifest,
**not a Flathub submission**. It uses a local `dir` source and Microsoft's
prebuilt ONNX Runtime archive. Do not open a Flathub PR or copy this manifest
into one: Flathub's Generative AI policy requires a human maintainer to author
the submitted manifest and its pull request, and the ONNX Runtime source-build
policy decision is still open.

The build installs the app icon so local desktop integration works, but it
intentionally leaves out AppStream metainfo and screenshots. Those still need
real product assets and are required before submission.

## Prerequisites

- Flatpak and `flatpak-builder`
- The `flathub` remote
- `org.freedesktop.Platform//26.08`, `org.freedesktop.Sdk//26.08`,
  `org.freedesktop.Sdk.Extension.rust-stable//26.08`, and
  `org.freedesktop.Sdk.Extension.llvm22//26.08`
- Python 3 and the dependencies for
  [`flatpak-cargo-generator.py`](https://github.com/flatpak/flatpak-builder-tools/tree/master/cargo)

## Build

Generate the Cargo sources into this directory, then build from the repository
root:

```sh
python3 flatpak-cargo-generator.py Cargo.lock -o flatpak/cargo-sources.json
flatpak-builder --user --force-clean \
  --state-dir="$PWD/flatpak/.state" \
  --repo="$PWD/flatpak/.repo" \
  "$PWD/flatpak/.build" \
  flatpak/io.github.cheerfulScumbag.tonguetyped.yml
```

At the current Cargo version, the generated git source key for `vad-rs` may
need the `git+` prefix and the `rev` query. Check `flatpak/cargo-sources.json`
if Cargo reports that it cannot update the `vad-rs` git dependency offline.

Install and run the local build:

```sh
flatpak install --user "$PWD/flatpak/.repo" io.github.cheerfulScumbag.tonguetyped
flatpak run --user io.github.cheerfulScumbag.tonguetyped
flatpak run --user --command=tonguetyped io.github.cheerfulScumbag.tonguetyped doctor
```

Older builds of installed software are overwritten with `flatpak install
--user --reinstall ...` after a rebuild.

## Behavior inside the sandbox

The desktop entry (`Exec=tonguetyped`, `Terminal=true`) opens the TUI dashboard.
`tonguetyped autostart enable` inside a Flatpak uses the
`org.freedesktop.portal.Background` portal with `autostart=true` and the
command line `tonguetyped daemon`, because the host desktop ignores autostart
files written inside the sandbox. A non-Flatpak install keeps writing the XDG
autostart entry.

The helper modules (`wl-clipboard`, `xclip`, `wtype`, `xdotool`) are bundled
for the local test. `dotool` is not included: `/dev/uinput` and `/dev/input`
are unavailable in the sandbox, and the libei RemoteDesktop path replaces it.
