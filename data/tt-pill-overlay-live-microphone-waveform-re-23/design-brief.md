# Long pill overlay style - design brief

## The captain's ask

The captain captured a voice-input bar from another app as the target look: a
dark rounded bar whose middle is a live waveform - many thin vertical bars of
varying height that rise and fall with the voice, quiet moments as tiny dots,
newest audio at the right. Later rounds added:

- "the pill should have a waveform and the blob should ... react to the sound
  of your voice".
- "Here's an example of a long pill form that I've done. The waveform is like a
  squiggly line ... This will be separate from the smaller pill, so a new
  overlay."
- "more uniformed Squiggles please".
- "it needs to still have the loading and transcribe and stuff like those icons
  for it as well ... It just keeps the long pill open and then changes the
  icon."
- "maybe just putting a bit of opacity on the background ... just a tiny little
  bit." (the bar fill only; the trace and button stay fully opaque)

The captain's reference image is a private screen capture and is intentionally
**not** committed to this repository. This note is the tracked, in-repo record
of the design it describes.

## What this is

Voice-reactive recording feedback built on a real microphone level feed
(`audio::publish_level` -> `overlay::LevelHistory`), plus one new style:

- **`overlay.live_waveform`** (renamed from `overlay.streaming_indicator`,
  default now `true`): `badge`/`minimal`/`pill` draw a real multi-bar
  microphone waveform during recording instead of the synthetic animation.
- **`overlay.style = "long-pill"`**: the seventh style, separate from the small
  `pill`.
- **`blob`** became voice-reactive too - it sprouted spikes on voice peaks and
  settles into a soft, round, slowly breathing ball in silence rather than
  drawing any bars (`paint_blob`/`blob_spikes`).

## The look

- A wide 8:1 dark bar (`LONG_PILL_SURFACE` 384x48, `#323232`
  `LONG_PILL_BACKGROUND`), only faintly translucent
  (`LONG_PILL_BACKGROUND_OPACITY`, 0.85): the fill reads as a soft dark bar
  rather than a hard block, while the trace and status button stay fully opaque
  on top.
- One continuous squiggly light-grey trace (`LONG_PILL_LINE`, `#C8C8C8`) from
  near the bar's left end to just short of the status button, centred
  vertically, whose overall size follows the smoothed live level on a slow
  visual carrier - so quiet stretches are a gentle waver and loud speech swells
  the squiggle.
- A round phase status button at the right end, the phase colour made vivid
  with `bolden`: red with a white dot while recording, green with a checkmark on
  success, and the matching glyph for every other phase. The bar stays neutral
  and open in every phase - only the button's colour and glyph swap.

## Behaviour

- Geometry derives from the bar height: button 0.752h, trace 0.066h thick,
  +/-0.27h excursions, trace from 0.039w to just short of the button.
- The trace ignores `overlay.live_waveform` (the squiggle is the style's whole
  point) and, unlike `border`/`half-circle`, honours `overlay.position` like the
  badge styles.
- `long_pill_envelope` averages the live level over a short sliding window so
  the squiggle is uniform rather than jagged (the captain's round-2 review).
- Click-through, no keyboard focus, same OSD/notification fallback chain on X11
  and non-layer-shell compositors. Switching to/from `long-pill` applies on the
  next event, no daemon restart.

## Validation

- Per-phase pixel checks and shape checks in `src/overlay.rs`; `long-pill` is in
  `ALL_STYLES` and `inside_style_shape`.
- `examples/overlay_style_png.rs` renders every style/phase (and voice-reactive
  frames) offline for manual review.
