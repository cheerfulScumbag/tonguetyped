# Half Circle overlay style - design brief and sign-off

## Sign-off outcome

Approved **as-is** by the captain on the first review (the sign-off form on the
Lavish page: "Half Circle overlay style sign-off: Approve as-is"). The captain
ended the review session.

The preview was the real pixel output - every frame rendered through
`overlay::render_frame_pixels` / `paint_half_circle`, the same per-pixel
alpha-blending path the live overlay actor runs - composited over a dark
1920x1080 desktop. No HTML/CSS blur and no mockup, so the approved look is what
ships.

## The captain's ask

"Still keep the half circle at the top - we're going to do that as well."

## What this is

A new `OverlayConfig::style` value, `half-circle`, alongside `badge`/`minimal`/
`pill`/`blob`/`border`: a glowing, pulsing phase-coloured semicircle at
top-centre, resting flat on the top edge of the screen. The `border` style is
unchanged (it is being improved separately); this is purely an additional style.

## The look

- A half-disc whose flat side is the screen's top edge and whose centre is that
  edge's midpoint, bulging downward - a half-sun rising from the top bezel.
- Phase-coloured: a solid fill (phase hue lightened a little toward white as
  intensity rises), brightest at the core and dimming gently toward the curved
  edge, dissolving into a smooth radial glow that reaches zero with a flat
  (smoothstep) start, so it has no band edge. All of it is mirror-symmetric
  about the vertical centre line (the profile depends only on the radius).
- The whole dome breathes in brightness while recording/transcribing.
- `streaming_indicator` adds a recording-only bright bead that travels along the
  arc - the same "actively capturing" cue the other styles show as a waveform.
- The dome plus its glow fade out with a transparent margin before the surface
  edge, so nothing clips.

## Behaviour

- Geometry as fractions of the surface height: dome radius `0.5`, glow reach
  `0.30` (their sum is under the surface half-width and height, leaving the
  margin). Surface is `HALF_CIRCLE_SURFACE` = `BADGE * 2` wide by `BADGE` tall.
- Top-docked like `border` is full-screen: it ignores `overlay.position` and is
  always anchored to `Anchor::TOP` with no margin (the compositor centres it
  horizontally). `overlay.monitor` still selects the output. `is_top_docked_style`
  owns that rule; `create_layer` uses it alongside `is_fullscreen_style`.
- Click-through, no keyboard focus, same OSD/notification fallback chain on X11
  and non-layer-shell compositors.
- Anchor/size are fixed at surface creation, so the actor recreates the surface
  when the style (or `position`/`monitor`) changes; switching to/from
  `half-circle` applies on the next event, with no daemon restart.

## Validation

New pixel tests in `src/overlay.rs` (plus `half-circle` added to `ALL_STYLES`,
`style_for`, and `inside_style_shape`):

- `half_circle_paints_a_dome_resting_on_the_top_edge_with_a_transparent_margin`:
  top-centre painted; four corners and a bottom band transparent.
- `half_circle_glow_fades_smoothly_and_symmetrically`: `half_circle_profile` is
  monotonic outward and reaches exactly zero at `radius + glow`; the painted
  frame is mirror-symmetric about the vertical centre line.
- `half_circle_uses_the_phase_colour`, `half_circle_pulses_across_its_animation_cycle`,
  `half_circle_streaming_indicator_sweeps_a_travelling_highlight`.
- The shared per-style tests (`every_phase_paints_distinguishable_pixels...`,
  `streaming_waveform_bars_stay_inside_every_style_shape`, etc.) now cover it too.

`cargo fmt --check`, `cargo clippy --all-targets -- -D warnings`, and `cargo
test` pass.
