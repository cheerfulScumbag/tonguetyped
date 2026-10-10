# Border overlay rework - thin 2px line, tight glow - design brief and sign-off

## Sign-off outcome

Approved by the captain ("proceed") after an in-artifact review loop.

The captain iterated the look directly on a Lavish page whose images are the
real `overlay::render_frame_pixels` output composited over a dark desktop - the
same per-pixel path the shipped overlay draws through, so what was approved is
what ships. The captain's live annotations drove the final geometry:

- "more fall off" / "no" on the first before/after pair.
- "1/2 this one" on option A and "no to much" on the long-fade option B, with
  the freeform note: "i think i have confused you i want it thinner".
- A fresh thinner set: "this one with a 2px line" on the tightest glow option.
- On the final preview: "make the glow reach 23px", then "proceed".

## The captain's ask

Started as "scrap the full border and just a glowing pulsing half circle", then
corrected before dispatch to: keep the full-screen border (the captain likes it),
make the fade better, and make it smaller/thinner. Refined in the review to a
thin 2-pixel line with a tight, light glow that reaches ~23px.

The implicit question - "is it because lavish is html and rust cannot do the
glow?" - is answered by construction: Rust renders a soft glow fine. The border
is drawn pixel by pixel with alpha blending, and the whole look is a falloff
function (`border_glow`). The earlier mismatch with the HTML mockup was the
falloff shape, not the language. Every option in the review was rendered through
the real path, so no HTML/CSS-only blur was ever approved.

## What changed

Only the `border` overlay style's geometry in `src/overlay.rs` (`border_glow` +
its constants). The other styles are untouched.

- **Line:** `BORDER_LINE_FRACTION` 0.004 -> 0.002 - a thin ~2px edge at 1080p,
  about a quarter of the original 0.4%-of-short-side band.
- **Glow:** `BORDER_GLOW_FRACTION` 0.07 -> 0.023 - the light halo now tucks in
  close and reaches fully transparent by ~23px instead of ~72px. The falloff is
  still the sum of two fixed-reach `smoothstep` ramps (narrow line + soft halo),
  so it stays monotonic with no core/glow band edge.
- **Corner emphasis kept:** corners remain brighter than the straight edges (a
  squared proximity boost).
- **Phase colours unchanged** (the shared `bolden`ed palette).

## Why the earlier looks missed

The full-screen border read as a thick, bright band with a long tail. The
captain wanted a genuinely thin line with the glow held close, not a broad
wash. Halving the line and sharply shortening the halo gives a crisp thin edge
with just a hint of bloom, which is what "thinner" meant.

## Validation

- Pixel tests in `src/overlay.rs`: transparent centre + painted frame; corners
  brighter than edge midpoints; phase colours; boldened hues; breathing;
  streaming sweep; in-bounds. `border_line_is_thin_and_the_glow_fades_smoothly_inward`
  now measures the line's >= quarter-opacity width (a ~2px edge) and asserts the
  glow stays tight, fades monotonically, and reaches fully transparent well
  before the centre.
- `cargo fmt --check`, `cargo clippy --all-targets -- -D warnings`, and
  `cargo test` (177 lib + integration) all pass.
- Preview rendered from the real `render_frame_pixels` output and reviewed live
  on a Lavish page (full frames, 1:1 corner crops, every phase, the recording
  pulse cycle, and a before/after falloff profile).
