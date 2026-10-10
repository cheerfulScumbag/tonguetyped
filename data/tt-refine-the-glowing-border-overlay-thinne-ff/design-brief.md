# Border overlay rework - thinner line, smooth fade - design brief and sign-off

## Sign-off outcome

Approved by the captain on the refreshed preview (revision 2).

- First review of revision 1: "this looks good the please Make sure that the
  gradient fade looks really nice and smooth and not jagged."
- Addressed by rendering the preview natively at 1080p instead of a 640x360
  thumbnail. At the thumbnail size the hairline and its fade collapse into a
  handful of pixels, which misrepresents them; the 1:1 crops of the 1080p frame
  show the real falloff (smooth, monotonic, no banding or hard edge).
- Final: the captain approved.

The same review raised a separate ask - show the overlay on every connected
screen, not one. That is outside this task (it affects all overlay styles and the
actor's output handling); it was escalated and firstmate ruled it out of scope
for a separate task.

## The captain's ask

"the border looks really bad and should be thiner and a smooth fade."

## What changed

Only the `border` overlay style's geometry in `src/overlay.rs`
(`border_glow` + its constants). The other styles are untouched.

- **Thinner.** The old style painted a solid core band at 1.8% of the shorter
  screen side (about 19px on 1080p). It now paints a hairline at 0.4% (about 4px
  on 1080p).
- **Smooth fade.** The old profile branched between a core ramp and a quadratic
  glow, and the two did not meet: intensity jumped from 0.65 to 0.75 at the
  boundary, so the band had a hard inner edge. The new profile is the sum of two
  fixed-reach `smoothstep` ramps, a narrow hairline term plus a wide soft-halo
  term. Both start flat at the screen edge and reach exactly zero with zero slope
  by their own reach, so the falloff is smooth and monotonic, with no band edge.
- **Corner emphasis kept.** Corners are still brighter than the straight edges
  (a squared proximity boost), now the only corner effect since the reach is
  uniform. Phase colours are unchanged (the shared boldened palette).
- **Faithful preview.** `BORDER_PREVIEW` (the offline preview and pixel-test
  surface size) moved from 640x360 to native 1920x1080, so the hairline and its
  gradient are shown at the resolution a real screen paints them.

## Why the old one looked bad

Two things: the band was thick, and the core/glow boundary was a step
discontinuity (0.65 to 0.75) that read as a hard-edged band rather than a glow.
The rework removes both - a thin line, then a smooth ramp inward.

## Validation

- Pixel tests in `src/overlay.rs`: transparent centre + painted frame; corners
  brighter than edge midpoints; phase colours; boldened hues; breathing;
  streaming sweep; in-bounds. A new test
  (`border_line_is_thin_and_the_glow_fades_smoothly_inward`) measures the bright
  line's half-opacity width (a hairline) and asserts the falloff is monotonic and
  reaches fully transparent before the centre.
- `cargo fmt --check`, `cargo clippy --all-targets -- -D warnings`, `cargo test`
  all pass.
- Refreshed preview rendered from the real `render_frame_pixels` output and
  reviewed on a Lavish page (before/after, all phases, animation cycles).
