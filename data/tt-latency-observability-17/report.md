# Latency observability implementation report

Date: 2026-09-28

## End-user path reproduction

Before changing source, the coordinator IPC test drove start, stop, processing,
output, and return to idle:

```sh
nix develop -c cargo test --test coordinator_ipc feedback_tracks_successful_state_transitions_once -- --exact --nocapture
```

```text
running 1 test
test feedback_tracks_successful_state_transitions_once ... ok

test result: ok. 1 passed; 0 failed; 0 ignored; 0 measured; 29 filtered out; finished in 0.05s
```

The original benchmark command could not run because this disposable worktree
did not contain `recording.wav`. I generated the same synthetic 8.597-second
fixture documented in `README.md` before collecting benchmark evidence.

## Implemented measurements

The latency event and its timing boundaries are documented in `README.md` under
"Latency diagnostics." Terminal outcomes distinguish `success`, `empty`,
`cancelled`, `recording_error`, `transcription_error`, `output_error`, and
`history_error`.

Startup diagnostics and `tonguetyped doctor` report the selected model ID,
actual inference backend, and device. This build reports `whisper.cpp/cpu` and
`CPU`; whisper.cpp also printed `no GPU found` while loading the benchmark model.

## Five-run CPU evidence

Command:

```sh
nix develop -c cargo run --release --example transcribe_benchmark -- /home/kenn/.local/share/tonguetyped/models/ggml-small-q5_1.bin recording.wav --runs 5
```

Machine-readable stdout:

```json
{"model_file":"ggml-small-q5_1.bin","model_sha256":"ae85e4a935d7a567bd102fe55afc16bb595bdb618e11b2fc7591bc08120411bb","audio_sha256":"2de0423a4b272c60df025c0e77d8b9c974d079ae0814294fd4fa796bb7f90c78","audio_seconds":8.597,"model_load_seconds":0.191366312,"backend":"whisper.cpp/cpu","device":"CPU","host_cpu":"AMD Ryzen 7 9700X 8-Core Processor","thread_count":8,"runs":[{"run":1,"vad_seconds":0.0,"inference_seconds":19.213197733,"realtime_factor":2.234872366290567,"retained_audio_seconds":8.597},{"run":2,"vad_seconds":0.0,"inference_seconds":19.91037775,"realtime_factor":2.315968099336978,"retained_audio_seconds":8.597},{"run":3,"vad_seconds":0.0,"inference_seconds":19.296364652,"realtime_factor":2.244546312899849,"retained_audio_seconds":8.597},{"run":4,"vad_seconds":0.0,"inference_seconds":19.593175649,"realtime_factor":2.27907126311504,"retained_audio_seconds":8.597},{"run":5,"vad_seconds":0.0,"inference_seconds":19.179453257,"realtime_factor":2.2309472207746888,"retained_audio_seconds":8.597}],"median_inference_seconds":19.296364652,"p95_inference_seconds":19.91037775,"competing_load_warning":"pre-run 1-minute load average 1.69 exceeded 1.00; do not use this result as a release threshold"}
```

Relevant whisper.cpp stderr:

```text
whisper_backend_init_gpu: device 0: CPU (type: 0)
whisper_backend_init_gpu: no GPU found
```

This is not a controlled baseline. Other OpenCode workers were consuming about
one CPU core and unrelated services, including `llama-server`, were active. I
did not stop shared or unrelated work. The benchmark warning therefore marks
the result unsuitable as a release threshold.

## Optional VAD check

Command:

```sh
nix develop -c cargo run --release --example transcribe_benchmark -- /home/kenn/.local/share/tonguetyped/models/ggml-small-q5_1.bin recording.wav --runs 1 --vad-model /home/kenn/.local/share/tonguetyped/models/silero_vad_v4.onnx
```

Machine-readable stdout:

```json
{"model_file":"ggml-small-q5_1.bin","model_sha256":"ae85e4a935d7a567bd102fe55afc16bb595bdb618e11b2fc7591bc08120411bb","audio_sha256":"2de0423a4b272c60df025c0e77d8b9c974d079ae0814294fd4fa796bb7f90c78","audio_seconds":8.597,"model_load_seconds":0.061226121,"backend":"whisper.cpp/cpu","device":"CPU","host_cpu":"AMD Ryzen 7 9700X 8-Core Processor","thread_count":8,"runs":[{"run":1,"vad_seconds":0.03504017,"inference_seconds":19.235199028,"realtime_factor":2.2374315491450507,"retained_audio_seconds":8.597}],"median_inference_seconds":19.235199028,"p95_inference_seconds":19.235199028,"competing_load_warning":"pre-run 1-minute load average 7.70 exceeded 1.00; do not use this result as a release threshold"}
```
