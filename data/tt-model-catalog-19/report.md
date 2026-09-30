# GGUF model catalog and lifecycle commands report

Date: 2026-09-30

## Summary

Added a reviewed catalog of GGUF speech models published by
[handy-computer](https://huggingface.co/handy-computer) on HuggingFace
(`src/catalog.rs`), derived from
[transcribe.cpp](https://github.com/handy-computer/transcribe.cpp)'s release
`catalog.db`, with `tonguetyped model {list,install,remove,use}` lifecycle
commands and a resumable, SHA-256-verified `DownloadManager`. The existing
built-in CPU model (`whisper-small-q5_1`) is unchanged in behavior; the new
catalog and lifecycle commands govern which GGUF model the optional
`gpu-vulkan`/`gpu-cuda` backend (added in slice 2,
`data/tt-transcribe-cpp-gpu-18/report.md`) loads, replacing that slice's
single hardcoded model with a full, user-selectable catalog.

## Sourcing and reviewing the catalog

`transcribe-cpp = "=0.2.4"` is the pinned version in `Cargo.toml`, so its
matching GitHub release (`v0.2.4`) is the catalog source of truth:

```sh
gh api repos/handy-computer/transcribe.cpp/releases/tags/v0.2.4
```

lists a `catalog.db` asset alongside its own `catalog.db.sha256`. Both were
downloaded and the hash verified before opening the file:

```
$ sha256sum catalog.db
795cc0a31c325d53008e6a090ec41473054436319bc0a5bc979d005384105a6c  catalog.db
$ cat catalog.db.sha256
795cc0a31c325d53008e6a090ec41473054436319bc0a5bc979d005384105a6c  catalog.db
```

It's a SQLite database (schema: `models`, `downloads`, `languages`,
`capabilities`, `accuracy`, `speed`, ...) covering 66 model variants across
many architecture families (whisper, canary, parakeet, voxtral, moonshine,
granite_speech, sortformer diarization, and more) - each with its upstream
repo/commit, license, and a `downloads` table of published quantizations
(filename + byte size) per variant.

### Scoping decision: `family = 'whisper'` only

Before wiring any of this into the inference engine, I checked whether the
two backends this project actually compiles can load these GGUF files at
all - "supported GGUF models" has to mean models this build can run, not
just models transcribe.cpp's ecosystem knows about:

- **CPU path** (`transcribe-rs`'s `whisper_cpp` module, the only
  `transcribe-rs` feature this project enables): its vendored `whisper.cpp`
  (`whisper-rs-sys-0.15.0/whisper.cpp/src/whisper.cpp`) checks
  `magic != GGML_FILE_MAGIC` in `whisper_model_load` and rejects anything
  else - it does not understand the GGUF container format at all, despite
  vendoring a `ggml/src/gguf.cpp`. It can only load the legacy `ggml-*.bin`
  files already used for the CPU default, never a handy-computer GGUF file,
  regardless of architecture family.
- **GPU path** (`transcribe-cpp`, only compiled under `gpu-vulkan`/
  `gpu-cuda`): loads GGUF natively via `transcribe_cpp::Model::load_with`,
  and is architecturally general-purpose - that's the whole point of
  transcribe.cpp existing. But many catalog families have real behavioral
  differences this project's `Session::run(audio, options) -> {text}`
  single-shot usage has never been exercised against: diarization output
  (`sortformer`), streaming-only models (`moonshine_streaming`,
  `nemotron-*-streaming`), and long-form strategies other than what whisper
  already uses (`chunked-unbounded`) - `hard-cap` and `soft-window` appear on
  many non-whisper families in `models.long_form_strategy` and may need
  different audio chunking than this project's VAD-then-single-buffer
  pipeline provides.

Given that, the catalog only includes `family = 'whisper'` (13 variants:
`whisper-{tiny,base,small,medium,large,large-v2,large-v3,large-v3-turbo}`,
the `.en` variants, and `breeze-asr-25`, a whisper-architecture fine-tune) -
the one family already proven end-to-end in slice 2's byte-for-byte
CPU/GPU transcript comparison, and the same family the existing hardcoded GPU
model already used. This is a real, deliberate scope line, not a shortcut:
other families are genuine transcribe.cpp models, just not yet
integration-tested against this project's usage, and get a false sense of
support from being merely downloadable. A future slice can extend the
catalog to a new family once its output/chunking semantics are verified the
same way whisper's were here.

### Per-file metadata: pinned revision, size, SHA-256

`catalog.db`'s `downloads` table gives `(variant, quant, filename,
size_bytes)` but no SHA-256, and `models.upstream_commit` is the *upstream*
model's commit (e.g. `openai/whisper-small`), not a commit on the
*published* GGUF repo actually downloaded from
(`handy-computer/whisper-small-gguf`). Both gaps are exactly what "pinned
revisions ... and SHA-256 verification" in the intent calls for, so each of
the 76 `(variant, quant)` files was resolved directly against HuggingFace
with a `HEAD` request to its `resolve/main/<filename>` URL:

```sh
curl -sSI "https://huggingface.co/handy-computer/whisper-small-gguf/resolve/main/whisper-small-Q5_K_M.gguf"
# x-repo-commit: a2073177cb69bd74b9ca9460b852d17fbfd5d68c
# x-linked-size: 193749056
# x-linked-etag: "326cd00c3e7217c751667c7c1600eaf7e0de174e186ca2c16b4bf590251c3c3b"
```

`x-linked-etag`/`x-linked-size` are HuggingFace's own LFS-backed SHA-256 and
byte size for the file at that exact commit (`x-repo-commit`); all 76 sizes
matched `catalog.db`'s `size_bytes` exactly (0 mismatches), and the resolved
SHA-256 for `whisper-small-Q5_K_M.gguf` matches the value already recorded in
slice 2's GPU benchmark report (`326cd00c3e72...`), independently
cross-validating both derivations. Every catalog entry embeds this resolved
`(revision, size_bytes, sha256)` rather than a `"main"` URL, so a later push
to a handy-computer repo cannot silently change what a pinned entry
downloads - `catalog.rs`'s `download_url()` builds the URL from `revision`,
and a test (`download_url_uses_pinned_revision_not_main`) asserts this.

The existing CPU model (`ggml-small-q5_1.bin` from
`ggerganov/whisper.cpp`) was pinned and verified the same way for
consistency (`ModelCatalog::MODEL_REVISION`/`MODEL_SHA256` in
`src/model.rs`) - `preserving the existing model` means its behavior and
default selection are unchanged, not that it should be exempt from the same
integrity guarantee applied to every other managed download.

`src/catalog.rs` is generated from this derivation and reviewed (every
size cross-checked against `catalog.db`, licenses checked - all 13 whisper
variants are `apache-2.0`) rather than fetched at runtime: metadata is
static, reviewed data; only the actual model bytes are downloaded.

## Resumable, verified downloads

`DownloadManager::fetch_verified` (`src/model.rs`) is shared by both the
legacy CPU-model download and the new catalog installs:

- Already-installed files short-circuit without a network request.
- A partial `<dest>.download` file triggers a `Range: bytes=<n>-` request;
  a `206 Partial Content` response appends from that offset, anything else
  (including a server that ignores `Range` and returns `200 OK`) restarts
  clean. A partial file larger than the pinned size is treated as corrupt
  rather than trusted.
- The complete file (fresh or resumed) is SHA-256-verified against the
  pinned hash before being renamed into place; a mismatch deletes the
  temp file and returns an error rather than leaving a corrupt file behind
  or renaming it into place.

Covered by five unit tests in `src/model.rs` against a minimal hand-rolled
HTTP/1.1 test server (`serve_one`, understands `GET` and `Range`; no new
dependency), plus real-network verification against HuggingFace directly
(see below).

One real bug found while writing this: `verify_sha256` originally used a
1 MiB stack-allocated buffer inside an `async fn`. That buffer is embedded
inline in the generated state machine across the function's `.await` points
(not heap-allocated), and `fetch_verified` awaits it inline too - one test
(`oversized_partial_file_is_treated_as_corrupt_and_restarted`) reliably
overflowed the test thread's stack. Fixed by shrinking to 64 KiB, matching
the buffer size already used for the same job in
`examples/transcribe_benchmark.rs`'s synchronous hasher.

## Lifecycle commands

```
tonguetyped model list                                 # catalog + install status
tonguetyped model install <id> [--use]                 # resumable, verified download
tonguetyped model use <id>                             # select the active GPU model
tonguetyped model remove <id>                           # delete a non-active install
```

`use`/`install --use` write `model.gpu_model` to `config.toml`, validated in
`Config::validate` against the catalog; `remove` refuses to delete the
currently active model. `InferenceEngine` gained
`with_gpu_model(model_path, gpu_model_id)` alongside the existing `new`
(which still defaults to the same model slice 2 hardcoded, so existing
installs/configs are unaffected); `coordinator.rs` and `doctor.rs` now pass
`config.model.gpu_model` through, and the coordinator's engine cache key
includes it so switching the GPU model selection reloads the engine instead
of reusing a stale one.

Covered by seven CLI-level integration tests (`tests/model_cli.rs`) using a
`fake_install` helper (writes an empty file at the catalog's real path
without a network call) to test `list`/`use`/`remove` state transitions and
guard rails without hitting the network in CI.

## End-to-end verification

This session's machine has real Vulkan-capable hardware (confirmed in
slice 2's own benchmark), so the full path was exercised for real rather
than only through mocks:

```sh
$ tonguetyped model install whisper-tiny-q5_k_m --use
installed and verified whisper-tiny-q5_k_m at .../models/whisper-tiny-Q5_K_M.gguf
selected whisper-tiny-q5_k_m as the active GPU inference model

$ tonguetyped model list | grep whisper-tiny-q5_k_m
whisper-tiny-q5_k_m              Q5_K_M       42.2 MiB  active       apache-2.0
```

Loading and transcribing with that non-default, catalog-installed model
through the real compiled `gpu-vulkan` engine (a throwaway example binary
exercising `InferenceEngine::with_gpu_model`, not committed), against the
same `recording.wav` used in slices 1-2 (SHA-256 `2de0423a...f90c78`):

```
ggml_vulkan: 0 = NVIDIA GeForce RTX 4080 SUPER (NVIDIA)
active_model_path: Some(".../models/whisper-tiny-Q5_K_M.gguf")
active_backend_info: Some(BackendInfo { backend: "transcribe.cpp/vulkan", device: "NVIDIA GeForce RTX 4080 SUPER" })
transcript: Today I am testing local speed recognition, the microphone records my voice and the computer can receive sentence in two written text.
```

(A plausible tiny-model transcript - less accurate than slice 2's `small`
model, as expected for the smallest quantization in the catalog - confirming
the catalog-selected model is genuinely loaded and run, not silently falling
back to CPU or the old hardcoded default.)

Resumability was also verified against HuggingFace's real servers, not just
the local test server: a `whisper-base-Q5_K_M.gguf` download was cut short
with `curl -r 0-20000000` (first ~20 MB of 63.8 MB) written directly to the
`.download` temp path, then `tonguetyped model install whisper-base-q5_k_m`
resumed from byte 20,000,001, completed in ~3.8 s, and the resulting file's
SHA-256 (`8e0feb7bc357...`) matched the pinned catalog value exactly.

Guard rails were also checked directly against the compiled binary:
`model remove` on the active model, `model use`/`model remove` on an
unknown id, and `model use` on an installed-but-not-selected model all
produced the expected errors with actionable messages.

## Verification commands run

- `cargo fmt --check`
- `cargo clippy --all-targets -- -D warnings` (default, `gpu-vulkan`)
- `cargo build --features gpu-cuda` (link-only; no CUDA hardware here, and
  slice 2 already covered CUDA build/link separately)
- `cargo test` (default, `gpu-vulkan`) - 152 tests, all passing
