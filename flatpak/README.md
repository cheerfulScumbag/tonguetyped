# Local Flatpak test build

`io.github.cheerfulScumbag.tonguetyped.yml` is a local verification manifest,
**not a Flathub submission**. It uses a local `dir` source and a source-built
ONNX Runtime. Do not open a Flathub PR or copy this manifest into one:
Flathub's Generative AI policy requires a human maintainer to author the
submitted manifest and its pull request.

The build installs the app icon so local desktop integration works, but it
intentionally leaves out AppStream metainfo and screenshots. Those still need
real product assets and are required before submission.

## ONNX Runtime source build

The app's VAD path (`vad-rs` -> `ort` -> `ort-sys`) needs ONNX Runtime.
`ort-sys` 2.0.0-rc.12 targets ONNX Runtime 1.24, so
`modules/onnxruntime.yml` builds ONNX Runtime v1.24.1 from source and installs
`libonnxruntime.so` into `/app/lib`. The app links it dynamically through
`ORT_LIB_LOCATION=/app/lib` and `ORT_PREFER_DYNAMIC_LINK=1`, exactly as it did
with the prebuilt archive.

The module builds ONNX Runtime with `-Donnxruntime_USE_VCPKG=ON` and
`-DFETCHCONTENT_FULLY_DISCONNECTED=ON`. That makes ONNX Runtime use
`find_package()` for every dependency, so each dependency is built first as a
nested module from a pinned archive and no network access is needed during the
build. Versions follow `cmake/deps.txt` in ONNX Runtime v1.24.1: Abseil
20250814.0, RE2 2024-07-02, protobuf 21.12, date 3.0.1, Boost 1.82 headers,
nlohmann/json 3.11.3, cpuinfo `403d652`, Microsoft GSL 4.0.0, SafeInt 3.0.28,
FlatBuffers 23.5.26, ONNX 1.20.1, and Eigen `1d8b82b`.

Three build details are deliberate:

- Abseil is configured with `CMAKE_CXX_STANDARD=17`. The 26.08 SDK's GCC 16
  defaults to C++23, so Abseil would otherwise pin its headers to C++20
  `std::ordering` types while ONNX Runtime 1.24 compiles as C++17.
- `modules/onnxruntime-no-werror.patch` turns off ONNX Runtime's
  `COMPILE_WARNING_AS_ERROR` property. GCC 16 adds warnings that ONNX Runtime
  1.24 did not expect; the warnings stay enabled, but they no longer fail the
  build.
- ONNX is configured with `ONNX_DISABLE_STATIC_REGISTRATION=ON`. ONNX Runtime
  registers the ONNX operator schemas itself, so leaving ONNX's static
  registration on registers every schema twice and floods stderr.

The top-level manifest cleans headers, CMake files, pkg-config files, static
archives, the dependency build tools (`flatc`, `protoc`, and the cpuinfo
tools), and the unused protobuf libraries (`libprotoc`, `libprotobuf-lite`)
out of the final app. Only the shared libraries the app needs remain.

## Prerequisites

- Flatpak and `flatpak-builder`. If `flatpak-builder` is not on the host,
  `flatpak run org.flatpak.Builder` provides it.
- The `flathub` remote
- `org.freedesktop.Platform//26.08`, `org.freedesktop.Sdk//26.08`,
  `org.freedesktop.Sdk.Extension.rust-stable//26.08`, and
  `org.freedesktop.Sdk.Extension.llvm22//26.08`
- Python 3 and the dependencies for
  [`flatpak-cargo-generator.py`](https://github.com/flatpak/flatpak-builder-tools/tree/master/cargo)
- Enough disk and time for the ONNX Runtime build. It compiles ONNX Runtime and
  its dependencies from source; the first build is much slower than the old
  prebuilt-archive build.

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

If `flatpak-builder` is not installed, use the Flatpak builder app instead.
The app sandbox sets `XDG_DATA_HOME`, which makes `flatpak-builder` look for
the SDK in the Builder app's private data directory instead of the host's
Flatpak installations. Clear that variable inside the sandbox so the SDK and
runtime installed with `flatpak install --user` are found:

```sh
flatpak run --command=sh org.flatpak.Builder -c \
  'env -u XDG_DATA_HOME flatpak-builder \
    --user --force-clean \
    --state-dir="$PWD/flatpak/.state" \
    --repo="$PWD/flatpak/.repo" \
    "$PWD/flatpak/.build" \
    flatpak/io.github.cheerfulScumbag.tonguetyped.yml'
```

Install and run the local build:

```sh
flatpak install --user "$PWD/flatpak/.repo" io.github.cheerfulScumbag.tonguetyped
flatpak run --user io.github.cheerfulScumbag.tonguetyped
flatpak run --user --command=tonguetyped io.github.cheerfulScumbag.tonguetyped doctor
```

Older builds of installed software are overwritten with `flatpak install
--user --reinstall ...` after a rebuild.

## Verification performed

The source manifest built, installed, and ran with
`org.freedesktop.Platform//26.08` on x86_64:

- The Flatpak build completed with exit code 0, including ONNX Runtime 1.24.1
  and the app with the `gpu-vulkan` feature.
- `doctor` reported `model: ready`, `backend: transcribe.cpp/vulkan`,
  `device: NVIDIA GeForce RTX 4080 SUPER`, `overlay: layer-shell available`,
  `audio: available`, `clipboard: wl-copy found`, and `helpers: libei`.
- `ldd /app/bin/tonguetyped` resolves `libonnxruntime.so.1` to
  `/app/lib/libonnxruntime.so.1`, and the packaged app no longer ships the
  prebuilt ONNX Runtime libraries.
- A VAD-only run loads the Silero model through the source-built library:
  transcribing a 16 kHz silence WAV returns an empty transcript and exit code
  0 without loading the whisper model or printing errors. This exercises
  `vad-rs` -> `ort` -> ONNX Runtime and the pinned `silero_vad_v4.onnx` model.
  The model's SHA-256 matches `catalog::VAD_MODEL`, and the packaged
  `libonnxruntime.so.1.24.1` is the library `ldd` reports.

`cargo test` also passes on the host for the same tree.

## Behavior inside the sandbox

The desktop entry (`Exec=tonguetyped`, `Terminal=true`) opens the setup console.
`tonguetyped autostart enable` inside a Flatpak uses the
`org.freedesktop.portal.Background` portal with `autostart=true` and the
command line `tonguetyped daemon`, because the host desktop ignores autostart
files written inside the sandbox. A non-Flatpak install keeps writing the XDG
autostart entry.

The helper modules (`wl-clipboard`, `xclip`, `wtype`, `xdotool`) are bundled
for the local test. `dotool` is not included: `/dev/uinput` and `/dev/input`
are unavailable in the sandbox, and the libei RemoteDesktop path replaces it.

## Remaining Flathub submission work

This manifest is still a local test artifact. A human maintainer must do the
submission work, including:

- Author the final manifest and PR. Flathub's Generative AI policy does not
  allow an AI agent to open or automate the submission PR.
- Replace the local `dir` source with a tagged `git` source from the release
  commit, and commit the generated `cargo-sources.json`.
- Add the AppStream metainfo file and real screenshots. The icon is present;
  metainfo and screenshots are not.
- Add license files for every bundled module under
  `/app/share/licenses/$FLATPAK_ID/<module>` (TongueTyped MIT, ONNX Runtime
  MIT, xdotool BSD-3-Clause, xclip GPL-2.0, wl-clipboard GPL-3.0, wtype MIT,
  libXmu MIT, SPIRV-Headers Apache-2.0/MIT, and the ONNX Runtime source
  dependencies).
- Build and test aarch64 as well as x86_64. The source module is
  architecture-independent, but it has only been verified on x86_64 here.
- Run `flatpak-builder-lint manifest` and `flatpak-builder-lint repo`, and
  resolve the AppStream and permission warnings.
- Decide the final permissions. The test manifest keeps the MPRIS talk-name
  and network access for model downloads; the submission task owns those
  decisions.
