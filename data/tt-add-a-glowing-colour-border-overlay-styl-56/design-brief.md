# Border overlay style - design brief and sign-off

## Sign-off outcome

Captain approved **as-is** and asked for bolder, brighter hues along the way:

- First review: "i approve but i do want the colours to be bolder bright hues".
- Applied a hue-preserving `bolden` (saturation lift around the phase colour's
  luminance + a capped value lift, so a neutral grey only brightens).
- Final review: "approve as-is (proceed to PR)". Session ended by the captain.

Reviewed live through the offline renderer output in a Lavish review page
(`overlay.style_png` / `render_frame_pixels`), composited over a dark desktop -
not hand-drawn mockups.

## What this is

A fifth `OverlayConfig::style` value, `border`, alongside `badge`/`minimal`/
`pill`/`blob`: a phase-coloured glow that hugs every screen edge and is strongest
in the four corners. Unlike the badge styles it is a full-screen
`zwlr_layer_shell_v1` surface (anchored to all edges, no margin, zero requested
size, stretched by the compositor) with a fully transparent centre.

## The look

- A solid core band (~1.8% of the shorter side) hugging each edge, with a soft
  glow falling off inward (~9% of the shorter side).
- Corner hotspots: near a corner the glow reaches ~1.8x further and is boosted,
  so the corners read as four bright hotspots - the captain's specific ask.
- Colours are the shared phase palette, boldened (more saturated, brighter)
  rather than washed toward white. `cancelled` stays a neutral steel grey.
- Breathes on the shared elapsed-time fraction while recording/transcribing;
  `streaming_indicator` adds a recording-only highlight sweeping around the
  frame.

## Behaviour

- Ignores `overlay.position` (there is only one frame); `overlay.monitor` still
  selects the output. Click-through, no keyboard focus, same OSD/notification
  fallback chain on X11 and non-layer-shell compositors.
- Anchor/size are fixed at first overlay creation, so switching to or from
  `border` needs a daemon restart, like any other shape-changing style.

## Validation

Pixel tests in `src/overlay.rs`: transparent centre + painted frame, corners
brighter than edge midpoints, phase colours, boldened hues vs the base palette,
breathing across the cycle, the streaming sweep, and in-bounds. `cargo fmt`,
`cargo clippy --all-targets -- -D warnings`, and `cargo test` all pass.
