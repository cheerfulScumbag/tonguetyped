## Adapted Code Notices

This project adapts logic from the following open-source projects under the MIT license.

### Handy (github.com/cjpais/Handy)

Commit: 8f9cf53
License: MIT

Adapted modules and their origins in Handy:

- `transcription_coordinator.rs` -> state machine logic in `src/coordinator.rs`
- `model/download.rs` -> download manager in `src/model.rs`
- `audio_toolkit/vad/` -> Silero VAD wrapper in `src/vad.rs`
- `audio_toolkit/audio/resampler.rs` -> audio resampling in `src/audio.rs`

MIT License (Handy):

```
Copyright (c) 2025 CJ Pais

Permission is hereby granted, free of charge, to any person obtaining a copy
of this software and associated documentation files (the "Software"), to deal
in the Software without restriction, including without limitation the rights
to use, copy, modify, merge, publish, distribute, sublicense, and/or sell
copies of the Software, and to permit persons to whom the Software is
furnished to do so, subject to the following conditions:

The above copyright notice and this permission notice shall be included in all
copies or substantial portions of the Software.

THE SOFTWARE IS PROVIDED "AS IS", WITHOUT WARRANTY OF ANY KIND, EXPRESS OR
IMPLIED, INCLUDING BUT NOT LIMITED TO THE WARRANTIES OF MERCHANTABILITY,
FITNESS FOR A PARTICULAR PURPOSE AND NONINFRINGEMENT. IN NO EVENT SHALL THE
AUTHORS OR COPYRIGHT HOLDERS BE LIABLE FOR ANY CLAIM, DAMAGES OR OTHER
LIABILITY, WHETHER IN AN ACTION OF CONTRACT, TORT OR OTHERWISE, ARISING FROM,
OUT OF OR IN CONNECTION WITH THE SOFTWARE OR THE USE OR OTHER DEALINGS IN THE
SOFTWARE.
```

### vad-rs (github.com/cjpais/vad-rs)

Commit: 2a412ed858695b9251f3f5a1a20d95b59fa7c498
License: MIT

### transcribe-rs (github.com/thewh1teagle/transcribe-rs)

License: MIT

### whisper.cpp (github.com/ggerganov/whisper.cpp)

License: MIT
