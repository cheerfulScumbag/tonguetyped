//! Passive wlr-layer-shell HUD overlay for dictation feedback.
//!
//! Renders directly into a persistent, click-through `zwlr_layer_shell_v1`
//! surface so recording/transcribing/success/cancellation/failure state is
//! glanceable without stealing keyboard or pointer focus. The surface is
//! created once, lazily, on the first event and kept for the daemon's
//! lifetime; between events it holds a fully transparent buffer rather than
//! being mapped and unmapped, so there is no per-dictation surface-creation
//! round trip on the latency-sensitive stop path (see AGENTS.md's stop-to-
//! idle latency note).
//!
//! `zwlr_layer_shell_v1` originated as a wlroots protocol extension (this
//! project validates against Mango, a wlroots-based compositor), but modern
//! KWin advertises it too (confirmed on KWin 6.7 via `wayland-info`), so on
//! most current KDE Plasma Wayland sessions this overlay renders directly
//! rather than falling back. The fallback chain still matters for X11
//! sessions and any compositor that doesn't advertise the global: there,
//! [`OverlayHandle::try_send`] returns `false` after one cheap failed probe
//! and `feedback.rs` falls back to the existing Plasma OSD / notification /
//! sound / no-visual chain unchanged.

use crate::config::OverlayConfig;
use crate::feedback::FeedbackEvent;
use std::sync::OnceLock;
use std::time::{Duration, Instant};

use smithay_client_toolkit::compositor::{CompositorHandler, CompositorState, Region};
use smithay_client_toolkit::output::{OutputHandler, OutputState};
use smithay_client_toolkit::reexports::calloop::{
    channel::{self, Sender},
    timer::{TimeoutAction, Timer},
    EventLoop, LoopHandle, LoopSignal,
};
use smithay_client_toolkit::reexports::calloop_wayland_source::WaylandSource;
use smithay_client_toolkit::reexports::client::{
    globals::registry_queue_init,
    protocol::{wl_output, wl_shm, wl_surface},
    Connection, QueueHandle,
};
use smithay_client_toolkit::registry::{ProvidesRegistryState, RegistryState};
use smithay_client_toolkit::shell::wlr_layer::{
    Anchor, KeyboardInteractivity, Layer, LayerShell, LayerShellHandler, LayerSurface,
    LayerSurfaceConfigure,
};
use smithay_client_toolkit::shell::WaylandSurface;
use smithay_client_toolkit::shm::slot::SlotPool;
use smithay_client_toolkit::shm::{Shm, ShmHandler};
use smithay_client_toolkit::{delegate_dispatch2, delegate_registry, registry_handlers};

const BADGE: u32 = 56;
const MARGIN: i32 = 20;
const TICK: Duration = Duration::from_millis(33);
const RECORDING_PULSE_PERIOD: Duration = Duration::from_millis(1600);
const SPIN_PERIOD: Duration = Duration::from_millis(900);
const SUCCESS_DWELL: Duration = Duration::from_millis(900);
const CANCELLED_DWELL: Duration = Duration::from_millis(900);
const ERROR_DWELL: Duration = Duration::from_millis(1800);

const RECORDING_COLOR: (u8, u8, u8) = (0xE0, 0x31, 0x31);
const TRANSCRIBING_COLOR: (u8, u8, u8) = (0x19, 0x71, 0xC2);
const SUCCESS_COLOR: (u8, u8, u8) = (0x2F, 0x9E, 0x44);
const CANCELLED_COLOR: (u8, u8, u8) = (0x86, 0x8E, 0x96);
const ERROR_COLOR: (u8, u8, u8) = (0xC9, 0x2A, 0x2A);
const GLYPH_COLOR: (u8, u8, u8) = (0xFF, 0xFF, 0xFF);

/// Lazily-started handle to the layer-shell overlay actor thread.
///
/// The probe (does this compositor speak wlr-layer-shell?) runs once, on the
/// first event, and its result is cached for the process's lifetime -- the
/// same blocking-probe-then-cache shape `inference::backend_info`/
/// `cached_backend_info` use for the GPU backend probe.
pub struct OverlayHandle {
    sender: OnceLock<Option<Sender<Command>>>,
}

impl OverlayHandle {
    pub const fn new() -> Self {
        Self {
            sender: OnceLock::new(),
        }
    }

    /// Attempts to show `event` on the layer-shell overlay. Returns `false`
    /// when no wlr-layer-shell compositor is available (or the probe hasn't
    /// finished), so the caller can fall back to the KDE OSD / notification
    /// path.
    pub fn try_send(&self, event: FeedbackEvent, config: &OverlayConfig) -> bool {
        let config = config.clone();
        let sender = self.sender.get_or_init(spawn_actor);
        match sender {
            Some(tx) => tx.send(Command { event, config }).is_ok(),
            None => false,
        }
    }
}

impl Default for OverlayHandle {
    fn default() -> Self {
        Self::new()
    }
}

struct Command {
    event: FeedbackEvent,
    config: OverlayConfig,
}

fn spawn_actor() -> Option<Sender<Command>> {
    let (ready_tx, ready_rx) = std::sync::mpsc::channel();
    std::thread::Builder::new()
        .name("tonguetyped-overlay".to_string())
        .spawn(move || run(ready_tx))
        .ok()?;
    ready_rx.recv_timeout(Duration::from_secs(2)).ok().flatten()
}

fn run(ready: std::sync::mpsc::Sender<Option<Sender<Command>>>) {
    if let Err(error) = run_actor(ready) {
        tracing::debug!("layer-shell overlay unavailable: {error:#}");
    }
}

fn run_actor(ready: std::sync::mpsc::Sender<Option<Sender<Command>>>) -> anyhow::Result<()> {
    use anyhow::Context;

    let conn = Connection::connect_to_env().context("no Wayland display")?;
    let (globals, event_queue) = registry_queue_init::<State>(&conn)
        .map_err(|error| anyhow::anyhow!("registry enumeration failed: {error}"))?;
    let qh = event_queue.handle();

    let compositor = CompositorState::bind(&globals, &qh)
        .map_err(|_| anyhow::anyhow!("wl_compositor unavailable"))?;
    let layer_shell = LayerShell::bind(&globals, &qh)
        .map_err(|_| anyhow::anyhow!("zwlr_layer_shell_v1 unavailable"))?;
    let shm = Shm::bind(&globals, &qh).map_err(|_| anyhow::anyhow!("wl_shm unavailable"))?;

    let mut event_loop: EventLoop<State> =
        EventLoop::try_new().context("failed to create the overlay event loop")?;
    let loop_handle = event_loop.handle();

    WaylandSource::new(conn, event_queue)
        .insert(loop_handle.clone())
        .map_err(|_| anyhow::anyhow!("failed to register the Wayland event source"))?;

    let (command_tx, command_rx) = channel::channel::<Command>();
    let command_loop_handle = loop_handle.clone();
    loop_handle
        .insert_source(command_rx, move |event, _, state: &mut State| match event {
            channel::Event::Msg(command) => state.handle_command(command, &command_loop_handle),
            channel::Event::Closed => state.loop_signal.stop(),
        })
        .map_err(|_| anyhow::anyhow!("failed to register the command channel"))?;

    let pool = SlotPool::new((BADGE * BADGE * 4) as usize, &shm)
        .context("failed to allocate the overlay's shared-memory pool")?;

    let mut state = State {
        registry_state: RegistryState::new(&globals),
        output_state: OutputState::new(&globals, &qh),
        compositor,
        layer_shell,
        shm,
        qh,
        pool,
        layer: None,
        width: BADGE,
        height: BADGE,
        configured: false,
        phase: None,
        deadline: None,
        anim_start: Instant::now(),
        ticking: false,
        ticking_animated: false,
        timer_generation: 0,
        loop_signal: event_loop.get_signal(),
    };

    ready
        .send(Some(command_tx))
        .map_err(|_| anyhow::anyhow!("caller stopped waiting for the overlay probe"))?;

    event_loop.run(None, &mut state, |_| {})?;
    Ok(())
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Phase {
    Recording,
    Transcribing,
    Success,
    Cancelled,
    Error,
}

impl Phase {
    fn from_event(event: FeedbackEvent) -> Self {
        match event {
            FeedbackEvent::Recording => Phase::Recording,
            FeedbackEvent::Processing => Phase::Transcribing,
            FeedbackEvent::Success => Phase::Success,
            FeedbackEvent::Cancelled => Phase::Cancelled,
            FeedbackEvent::Error => Phase::Error,
        }
    }

    fn dwell(self) -> Option<Duration> {
        match self {
            Phase::Recording | Phase::Transcribing => None,
            Phase::Success => Some(SUCCESS_DWELL),
            Phase::Cancelled => Some(CANCELLED_DWELL),
            Phase::Error => Some(ERROR_DWELL),
        }
    }

    fn is_animated(self) -> bool {
        matches!(self, Phase::Recording | Phase::Transcribing)
    }
}

struct State {
    registry_state: RegistryState,
    output_state: OutputState,
    compositor: CompositorState,
    layer_shell: LayerShell,
    shm: Shm,
    qh: QueueHandle<State>,
    pool: SlotPool,
    layer: Option<LayerSurface>,
    width: u32,
    height: u32,
    configured: bool,
    phase: Option<Phase>,
    deadline: Option<Instant>,
    anim_start: Instant,
    ticking: bool,
    ticking_animated: bool,
    timer_generation: u64,
    loop_signal: LoopSignal,
}

impl State {
    fn handle_command(&mut self, command: Command, loop_handle: &LoopHandle<'static, State>) {
        let phase = Phase::from_event(command.event);
        self.phase = Some(phase);
        self.deadline = phase.dwell().map(|dwell| Instant::now() + dwell);
        self.anim_start = Instant::now();

        if self.layer.is_none() {
            self.create_layer(&command.config);
        } else {
            // A no-op before the first `configure` arrives: `redraw` guards
            // on `self.configured` itself, and the pending `configure`
            // handler will redraw with this (already-updated) phase once it
            // lands.
            self.redraw();
        }

        self.arm_timer(loop_handle);
    }

    fn create_layer(&mut self, config: &OverlayConfig) {
        let output = resolve_output(&self.output_state, &config.monitor);
        let surface = self.compositor.create_surface(&self.qh);
        let layer = self.layer_shell.create_layer_surface(
            &self.qh,
            surface,
            Layer::Overlay,
            Some("tonguetyped-overlay"),
            output.as_ref(),
        );
        layer.set_anchor(anchor_for(&config.position));
        layer.set_margin(MARGIN, MARGIN, MARGIN, MARGIN);
        layer.set_size(BADGE, BADGE);
        layer.set_exclusive_zone(0);
        layer.set_keyboard_interactivity(KeyboardInteractivity::None);
        if let Ok(region) = Region::new(&self.compositor) {
            // No rectangles added: an empty region accepts no pointer input,
            // making the overlay click-through.
            layer.set_input_region(Some(region.wl_region()));
        }
        layer.commit();
        self.width = BADGE;
        self.height = BADGE;
        self.layer = Some(layer);
    }

    fn arm_timer(&mut self, loop_handle: &LoopHandle<'static, State>) {
        let Some(phase) = self.phase else { return };
        let needs_animated = phase.is_animated();
        if self.ticking && self.ticking_animated == needs_animated {
            return;
        }
        let initial = if needs_animated {
            TICK
        } else {
            self.deadline
                .map(|deadline| deadline.saturating_duration_since(Instant::now()))
                .unwrap_or(TICK)
        };
        self.ticking = true;
        self.ticking_animated = needs_animated;
        self.timer_generation += 1;
        let generation = self.timer_generation;
        let _ = loop_handle.insert_source(
            Timer::from_duration(initial),
            move |_, _, state: &mut State| state.on_tick(generation),
        );
    }

    fn on_tick(&mut self, generation: u64) -> TimeoutAction {
        if generation != self.timer_generation {
            return TimeoutAction::Drop;
        }
        match self.phase {
            Some(phase) if phase.is_animated() => {
                self.redraw();
                TimeoutAction::ToDuration(TICK)
            }
            Some(_) => {
                let Some(deadline) = self.deadline else {
                    self.ticking = false;
                    return TimeoutAction::Drop;
                };
                let now = Instant::now();
                if now >= deadline {
                    self.phase = None;
                    self.deadline = None;
                    self.redraw();
                    self.ticking = false;
                    TimeoutAction::Drop
                } else {
                    TimeoutAction::ToDuration(deadline - now)
                }
            }
            None => {
                self.ticking = false;
                TimeoutAction::Drop
            }
        }
    }

    fn redraw(&mut self) {
        // Attaching a buffer before the first `configure` is a protocol
        // error; `on_tick`'s animated-phase branch can otherwise race this
        // (e.g. the compositor takes longer than one `TICK` to send it).
        if !self.configured {
            return;
        }
        let Some(layer) = &self.layer else { return };
        let (width, height) = (self.width, self.height);
        let stride = width as i32 * 4;
        let Ok((buffer, pixels)) = self.pool.create_buffer(
            width as i32,
            height as i32,
            stride,
            wl_shm::Format::Argb8888,
        ) else {
            return;
        };
        let mut canvas = Canvas {
            pixels,
            width,
            height,
        };
        let t = animation_fraction(self.phase, self.anim_start);
        paint(&mut canvas, self.phase, t);

        layer
            .wl_surface()
            .damage_buffer(0, 0, width as i32, height as i32);
        let _ = buffer.attach_to(layer.wl_surface());
        layer.commit();
    }
}

fn resolve_output(output_state: &OutputState, monitor: &str) -> Option<wl_output::WlOutput> {
    if monitor == "active" {
        return None;
    }
    output_state.outputs().find(|output| {
        output_state
            .info(output)
            .and_then(|info| info.name)
            .as_deref()
            == Some(monitor)
    })
}

fn anchor_for(position: &str) -> Anchor {
    match position {
        "top-left" => Anchor::TOP | Anchor::LEFT,
        "top-right" => Anchor::TOP | Anchor::RIGHT,
        "bottom-left" => Anchor::BOTTOM | Anchor::LEFT,
        "bottom-right" => Anchor::BOTTOM | Anchor::RIGHT,
        "top" => Anchor::TOP,
        "bottom" => Anchor::BOTTOM,
        "center" => Anchor::empty(),
        _ => Anchor::TOP | Anchor::RIGHT,
    }
}

fn animation_fraction(phase: Option<Phase>, start: Instant) -> f32 {
    let period = match phase {
        Some(Phase::Recording) => RECORDING_PULSE_PERIOD,
        Some(Phase::Transcribing) => SPIN_PERIOD,
        _ => return 0.0,
    };
    let period_secs = period.as_secs_f32();
    (start.elapsed().as_secs_f32() % period_secs) / period_secs
}

impl CompositorHandler for State {
    fn scale_factor_changed(
        &mut self,
        _conn: &Connection,
        _qh: &QueueHandle<Self>,
        _surface: &wl_surface::WlSurface,
        _new_factor: i32,
    ) {
    }

    fn transform_changed(
        &mut self,
        _conn: &Connection,
        _qh: &QueueHandle<Self>,
        _surface: &wl_surface::WlSurface,
        _new_transform: wl_output::Transform,
    ) {
    }

    fn frame(
        &mut self,
        _conn: &Connection,
        _qh: &QueueHandle<Self>,
        _surface: &wl_surface::WlSurface,
        _time: u32,
    ) {
    }

    fn surface_enter(
        &mut self,
        _conn: &Connection,
        _qh: &QueueHandle<Self>,
        _surface: &wl_surface::WlSurface,
        _output: &wl_output::WlOutput,
    ) {
    }

    fn surface_leave(
        &mut self,
        _conn: &Connection,
        _qh: &QueueHandle<Self>,
        _surface: &wl_surface::WlSurface,
        _output: &wl_output::WlOutput,
    ) {
    }
}

impl OutputHandler for State {
    fn output_state(&mut self) -> &mut OutputState {
        &mut self.output_state
    }

    fn new_output(
        &mut self,
        _conn: &Connection,
        _qh: &QueueHandle<Self>,
        _output: wl_output::WlOutput,
    ) {
    }

    fn update_output(
        &mut self,
        _conn: &Connection,
        _qh: &QueueHandle<Self>,
        _output: wl_output::WlOutput,
    ) {
    }

    fn output_destroyed(
        &mut self,
        _conn: &Connection,
        _qh: &QueueHandle<Self>,
        _output: wl_output::WlOutput,
    ) {
    }
}

impl LayerShellHandler for State {
    fn closed(&mut self, _conn: &Connection, _qh: &QueueHandle<Self>, _layer: &LayerSurface) {
        self.layer = None;
        self.loop_signal.stop();
    }

    fn configure(
        &mut self,
        _conn: &Connection,
        _qh: &QueueHandle<Self>,
        _layer: &LayerSurface,
        configure: LayerSurfaceConfigure,
        _serial: u32,
    ) {
        let (width, height) = configure.new_size;
        self.width = if width == 0 { BADGE } else { width };
        self.height = if height == 0 { BADGE } else { height };
        self.configured = true;
        self.redraw();
    }
}

impl ShmHandler for State {
    fn shm_state(&mut self) -> &mut Shm {
        &mut self.shm
    }
}

delegate_registry!(State);

impl ProvidesRegistryState for State {
    fn registry(&mut self) -> &mut RegistryState {
        &mut self.registry_state
    }
    registry_handlers![OutputState];
}

delegate_dispatch2!(State);

struct Canvas<'a> {
    pixels: &'a mut [u8],
    width: u32,
    height: u32,
}

impl Canvas<'_> {
    fn blend(&mut self, x: i32, y: i32, rgb: (u8, u8, u8), coverage: f32) {
        if x < 0 || y < 0 || x as u32 >= self.width || y as u32 >= self.height {
            return;
        }
        let coverage = coverage.clamp(0.0, 1.0);
        if coverage <= 0.0 {
            return;
        }
        let index = ((y as u32 * self.width + x as u32) * 4) as usize;
        let pixel = &mut self.pixels[index..index + 4];
        let existing_alpha = pixel[3] as f32 / 255.0;
        let out_alpha = coverage + existing_alpha * (1.0 - coverage);
        if out_alpha <= 0.0 {
            pixel.copy_from_slice(&[0, 0, 0, 0]);
            return;
        }
        let mix = |src: u8, dst: u8| -> u8 {
            let src = src as f32 / 255.0;
            let dst = dst as f32 / 255.0;
            let out = (src * coverage + dst * existing_alpha * (1.0 - coverage)) / out_alpha;
            (out * 255.0).round().clamp(0.0, 255.0) as u8
        };
        pixel[0] = mix(rgb.2, pixel[0]);
        pixel[1] = mix(rgb.1, pixel[1]);
        pixel[2] = mix(rgb.0, pixel[2]);
        pixel[3] = (out_alpha * 255.0).round().clamp(0.0, 255.0) as u8;
    }
}

fn fill_circle(canvas: &mut Canvas, cx: f32, cy: f32, radius: f32, rgb: (u8, u8, u8)) {
    let span = radius + 1.0;
    let min_x = (cx - span).floor().max(0.0) as i32;
    let max_x = (cx + span).ceil().min(canvas.width as f32) as i32;
    let min_y = (cy - span).floor().max(0.0) as i32;
    let max_y = (cy + span).ceil().min(canvas.height as f32) as i32;
    for y in min_y..max_y {
        for x in min_x..max_x {
            let dx = x as f32 + 0.5 - cx;
            let dy = y as f32 + 0.5 - cy;
            let coverage = (radius + 0.5 - (dx * dx + dy * dy).sqrt()).clamp(0.0, 1.0);
            canvas.blend(x, y, rgb, coverage);
        }
    }
}

fn fill_square(canvas: &mut Canvas, cx: f32, cy: f32, half: f32, rgb: (u8, u8, u8)) {
    let min_x = (cx - half).round().max(0.0) as i32;
    let max_x = (cx + half).round().min(canvas.width as f32) as i32;
    let min_y = (cy - half).round().max(0.0) as i32;
    let max_y = (cy + half).round().min(canvas.height as f32) as i32;
    for y in min_y..max_y {
        for x in min_x..max_x {
            canvas.blend(x, y, rgb, 1.0);
        }
    }
}

fn stroke_line(
    canvas: &mut Canvas,
    x0: f32,
    y0: f32,
    x1: f32,
    y1: f32,
    thickness: f32,
    rgb: (u8, u8, u8),
) {
    let span = thickness + 1.0;
    let min_x = (x0.min(x1) - span).floor().max(0.0) as i32;
    let max_x = (x0.max(x1) + span).ceil().min(canvas.width as f32) as i32;
    let min_y = (y0.min(y1) - span).floor().max(0.0) as i32;
    let max_y = (y0.max(y1) + span).ceil().min(canvas.height as f32) as i32;
    let (dx, dy) = (x1 - x0, y1 - y0);
    let len_sq = dx * dx + dy * dy;
    for y in min_y..max_y {
        for x in min_x..max_x {
            let px = x as f32 + 0.5;
            let py = y as f32 + 0.5;
            let t = if len_sq > 0.0 {
                (((px - x0) * dx + (py - y0) * dy) / len_sq).clamp(0.0, 1.0)
            } else {
                0.0
            };
            let proj_x = x0 + t * dx;
            let proj_y = y0 + t * dy;
            let dist = ((px - proj_x).powi(2) + (py - proj_y).powi(2)).sqrt();
            let coverage = (thickness / 2.0 + 0.5 - dist).clamp(0.0, 1.0);
            canvas.blend(x, y, rgb, coverage);
        }
    }
}

fn stroke_arc(
    canvas: &mut Canvas,
    cx: f32,
    cy: f32,
    radius: f32,
    thickness: f32,
    angle_range: (f32, f32),
    rgb: (u8, u8, u8),
) {
    use std::f32::consts::TAU;
    let (start, sweep) = angle_range;
    let span = radius + thickness / 2.0 + 1.0;
    let min_x = (cx - span).floor().max(0.0) as i32;
    let max_x = (cx + span).ceil().min(canvas.width as f32) as i32;
    let min_y = (cy - span).floor().max(0.0) as i32;
    let max_y = (cy + span).ceil().min(canvas.height as f32) as i32;
    for y in min_y..max_y {
        for x in min_x..max_x {
            let dx = x as f32 + 0.5 - cx;
            let dy = y as f32 + 0.5 - cy;
            let dist = (dx * dx + dy * dy).sqrt();
            let radial_coverage = (thickness / 2.0 + 0.5 - (dist - radius).abs()).clamp(0.0, 1.0);
            if radial_coverage <= 0.0 {
                continue;
            }
            let mut relative = dy.atan2(dx) - start;
            relative = relative.rem_euclid(TAU);
            if relative <= sweep {
                canvas.blend(x, y, rgb, radial_coverage);
            }
        }
    }
}

fn paint(canvas: &mut Canvas, phase: Option<Phase>, t: f32) {
    canvas.pixels.fill(0);
    let Some(phase) = phase else { return };

    let cx = canvas.width as f32 / 2.0;
    let cy = canvas.height as f32 / 2.0;
    let disc_radius = canvas.width.min(canvas.height) as f32 * 0.42;

    match phase {
        Phase::Recording => {
            fill_circle(canvas, cx, cy, disc_radius, RECORDING_COLOR);
            let pulse = 0.5 + 0.5 * (t * std::f32::consts::TAU).sin();
            fill_circle(
                canvas,
                cx,
                cy,
                disc_radius * (0.32 + 0.10 * pulse),
                GLYPH_COLOR,
            );
        }
        Phase::Transcribing => {
            fill_circle(canvas, cx, cy, disc_radius, TRANSCRIBING_COLOR);
            let angle = t * std::f32::consts::TAU;
            stroke_arc(
                canvas,
                cx,
                cy,
                disc_radius * 0.58,
                disc_radius * 0.20,
                (angle, std::f32::consts::PI * 1.2),
                GLYPH_COLOR,
            );
        }
        Phase::Success => {
            fill_circle(canvas, cx, cy, disc_radius, SUCCESS_COLOR);
            let s = disc_radius * 0.5;
            let thickness = disc_radius * 0.18;
            stroke_line(
                canvas,
                cx - s * 0.9,
                cy + s * 0.05,
                cx - s * 0.15,
                cy + s * 0.75,
                thickness,
                GLYPH_COLOR,
            );
            stroke_line(
                canvas,
                cx - s * 0.15,
                cy + s * 0.75,
                cx + s * 1.0,
                cy - s * 0.65,
                thickness,
                GLYPH_COLOR,
            );
        }
        Phase::Cancelled => {
            fill_circle(canvas, cx, cy, disc_radius, CANCELLED_COLOR);
            fill_square(canvas, cx, cy, disc_radius * 0.38, GLYPH_COLOR);
        }
        Phase::Error => {
            fill_circle(canvas, cx, cy, disc_radius, ERROR_COLOR);
            let s = disc_radius * 0.5;
            let thickness = disc_radius * 0.18;
            stroke_line(
                canvas,
                cx - s,
                cy - s,
                cx + s,
                cy + s,
                thickness,
                GLYPH_COLOR,
            );
            stroke_line(
                canvas,
                cx - s,
                cy + s,
                cx + s,
                cy - s,
                thickness,
                GLYPH_COLOR,
            );
        }
    }
}

/// Probes whether this session's compositor speaks wlr-layer-shell, without
/// starting the persistent overlay actor. Used by `doctor` diagnostics.
pub fn probe_available() -> bool {
    (|| -> anyhow::Result<bool> {
        let conn = Connection::connect_to_env()?;
        // `State` is only used here as a type parameter to satisfy
        // `registry_queue_init`'s bound; no `State` value is created.
        let (globals, _event_queue) = registry_queue_init::<State>(&conn)
            .map_err(|error| anyhow::anyhow!("registry enumeration failed: {error}"))?;
        Ok(globals.contents().with_list(|list| {
            list.iter()
                .any(|global| global.interface == "zwlr_layer_shell_v1")
        }))
    })()
    .unwrap_or(false)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Mutex;

    static WAYLAND_DISPLAY_LOCK: Mutex<()> = Mutex::new(());

    #[test]
    fn unavailable_overlay_falls_back_without_blocking() {
        // Point WAYLAND_DISPLAY at a socket name that cannot exist, so this
        // exercises the fallback path `feedback.rs` depends on
        // deterministically -- independent of whether the machine running
        // this test happens to have a compositor (and independent of
        // whether that compositor speaks wlr-layer-shell; recent KWin does).
        let _guard = WAYLAND_DISPLAY_LOCK
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let previous = std::env::var("WAYLAND_DISPLAY").ok();
        std::env::set_var("WAYLAND_DISPLAY", "tonguetyped-test-nonexistent-socket");

        let handle = OverlayHandle::new();
        let config = OverlayConfig::default();
        let sent = handle.try_send(FeedbackEvent::Recording, &config);
        let available = probe_available();

        match previous {
            Some(value) => std::env::set_var("WAYLAND_DISPLAY", value),
            None => std::env::remove_var("WAYLAND_DISPLAY"),
        }

        assert!(!sent);
        assert!(!available);
    }

    #[test]
    fn anchor_for_maps_every_configured_position() {
        assert_eq!(anchor_for("top-left"), Anchor::TOP | Anchor::LEFT);
        assert_eq!(anchor_for("bottom-right"), Anchor::BOTTOM | Anchor::RIGHT);
        assert_eq!(anchor_for("center"), Anchor::empty());
        assert_eq!(anchor_for("nonsense"), Anchor::TOP | Anchor::RIGHT);
    }

    #[test]
    fn fill_circle_paints_only_within_transparent_bounds() {
        let mut pixels = vec![0u8; 8 * 8 * 4];
        let mut canvas = Canvas {
            pixels: &mut pixels,
            width: 8,
            height: 8,
        };
        fill_circle(&mut canvas, 4.0, 4.0, 3.0, (255, 0, 0));
        let center = ((4 * 8 + 4) * 4) as usize;
        assert_eq!(
            pixels[center + 3],
            255,
            "circle center should be fully opaque"
        );
        let corner = 0;
        assert_eq!(
            pixels[corner + 3],
            0,
            "corner outside the circle should stay transparent"
        );
    }

    #[test]
    fn every_phase_paints_distinguishable_pixels() {
        let mut seen = Vec::new();
        for phase in [
            Phase::Recording,
            Phase::Transcribing,
            Phase::Success,
            Phase::Cancelled,
            Phase::Error,
        ] {
            let mut pixels = vec![0u8; (BADGE * BADGE * 4) as usize];
            {
                let mut canvas = Canvas {
                    pixels: &mut pixels,
                    width: BADGE,
                    height: BADGE,
                };
                paint(&mut canvas, Some(phase), 0.0);
            }
            assert!(
                pixels.iter().any(|&byte| byte != 0),
                "{phase:?} painted nothing"
            );
            assert!(
                !seen.contains(&pixels),
                "{phase:?} is pixel-identical to an earlier phase"
            );
            seen.push(pixels);
        }
    }

    #[test]
    fn idle_paint_is_fully_transparent() {
        let mut pixels = vec![1u8; (BADGE * BADGE * 4) as usize];
        {
            let mut canvas = Canvas {
                pixels: &mut pixels,
                width: BADGE,
                height: BADGE,
            };
            paint(&mut canvas, None, 0.0);
        }
        assert!(pixels.iter().all(|&byte| byte == 0));
    }
}
