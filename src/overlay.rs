//! Passive wlr-layer-shell HUD overlay for dictation feedback.
//!
//! Renders directly into a persistent, click-through `zwlr_layer_shell_v1`
//! surface so recording/transcribing/success/no-speech/cancellation/failure
//! state is glanceable without stealing keyboard or pointer focus. The
//! surface is created lazily, on the first event; between events it holds a
//! fully transparent buffer rather than being mapped and unmapped, and it is
//! only recreated when the config-derived geometry or placement changes (an
//! `overlay.style`/`position`/`monitor` change applied via `tonguetyped
//! reload`), never per dictation, so there is no per-dictation
//! surface-creation round trip on the latency-sensitive stop path (see
//! AGENTS.md's stop-to-idle latency note).
//!
//! `zwlr_layer_shell_v1` originated as a wlroots protocol extension (this
//! project is code-reviewed, but not live-validated, against Mango, a
//! wlroots-based compositor), but modern KWin advertises it too (confirmed
//! and live-validated on KDE Plasma 6.7 via `wayland-info`), so on most
//! current KDE Plasma Wayland sessions this overlay renders directly rather
//! than falling back. The fallback chain still matters for X11
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
/// The `Blob` style's square surface, larger than `BADGE` so its glow has room
/// to fall off to nothing before the surface edge (see `paint_blob`).
const BLOB_SURFACE: u32 = BADGE * 3 / 2;
/// The `Border` style's stand-in surface size. On a live compositor it is a
/// full-screen all-edge-anchored layer (the compositor stretches a zero-sized
/// surface to the whole output and reports the real dimensions in `configure`),
/// so this is only the representative size the offline preview and the pixel
/// tests render into. It is a common 16:9 frame at native 1080p rather than a
/// thumbnail: `paint_border` scales its geometry to the shorter side, but the
/// hairline and its inward fade only look right - thin line, smooth gradient -
/// when rendered at a resolution a real screen actually uses, so a scaled-down
/// preview does not misrepresent them.
const BORDER_PREVIEW: (u32, u32) = (1920, 1080);
/// `paint_border` geometry, all as fractions of the surface's shorter side so
/// the frame looks the same on any output: the thin bright line's thickness,
/// the reach of the soft glow it dissolves into inward, and how far along the
/// edges a corner's hotspot reaches. The captain iterated this look through a
/// faithful preview: the line is now a thin ~2-pixel edge at 1080p (a quarter
/// of what it was) with a *tight*, light glow that tucks in close (~23px) and
/// dies out quickly, rather than a thick band with a long tail.
const BORDER_LINE_FRACTION: f32 = 0.002;
const BORDER_GLOW_FRACTION: f32 = 0.023;
const BORDER_CORNER_REACH_FRACTION: f32 = 0.30;
/// Peak opacity of the thin edge line and of the tight glow it fades into. The
/// line reads as a crisp edge hugging the screen; the weaker halo is the light,
/// smooth inward fade around it.
const BORDER_LINE_STRENGTH: f32 = 0.85;
const BORDER_HALO_STRENGTH: f32 = 0.20;
/// How much extra brightness a corner hotspot adds over the straight edges.
const BORDER_CORNER_BOOST: f32 = 1.6;
const MARGIN: i32 = 20;
const TICK: Duration = Duration::from_millis(33);
const RECORDING_PULSE_PERIOD: Duration = Duration::from_millis(1600);
/// The `Blob` style breathes more slowly than the other styles' plain pulse -
/// a calm, slow breath rather than a quick heartbeat.
const BLOB_PULSE_PERIOD: Duration = Duration::from_millis(2800);
const SPIN_PERIOD: Duration = Duration::from_millis(900);
const SUCCESS_DWELL: Duration = Duration::from_millis(900);
// A touch longer than `Success`: a "no speech" cue is information the user
// needs to notice, but it is not an error and must not feel alarming.
const NO_SPEECH_DWELL: Duration = Duration::from_millis(1400);
const CANCELLED_DWELL: Duration = Duration::from_millis(900);
const ERROR_DWELL: Duration = Duration::from_millis(1800);

const RECORDING_COLOR: (u8, u8, u8) = (0xE0, 0x31, 0x31);
const TRANSCRIBING_COLOR: (u8, u8, u8) = (0x19, 0x71, 0xC2);
const SUCCESS_COLOR: (u8, u8, u8) = (0x2F, 0x9E, 0x44);
// Amber: caution, not failure - distinct from every other phase hue.
const NO_SPEECH_COLOR: (u8, u8, u8) = (0xE8, 0x9A, 0x2B);
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

/// The config-derived inputs that fix a layer surface's geometry and placement:
/// whether it spans the whole output, its requested size, and the anchor
/// position and output it is bound to.
#[derive(Debug, PartialEq, Eq)]
struct LayerSpec {
    fullscreen: bool,
    size: (u32, u32),
    position: String,
    monitor: String,
}

fn layer_spec(config: &OverlayConfig, style: Style) -> LayerSpec {
    LayerSpec {
        fullscreen: is_fullscreen_style(style),
        size: surface_size_for(style),
        position: config.position.clone(),
        monitor: config.monitor.clone(),
    }
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
        streaming_indicator: false,
        style: Style::Badge,
        layer_spec: None,
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
    NoSpeech,
    Cancelled,
    Error,
}

impl Phase {
    fn from_event(event: FeedbackEvent) -> Self {
        match event {
            FeedbackEvent::Recording => Phase::Recording,
            FeedbackEvent::Processing => Phase::Transcribing,
            FeedbackEvent::Success => Phase::Success,
            FeedbackEvent::NoSpeech => Phase::NoSpeech,
            FeedbackEvent::Cancelled => Phase::Cancelled,
            FeedbackEvent::Error => Phase::Error,
        }
    }

    fn dwell(self) -> Option<Duration> {
        match self {
            Phase::Recording | Phase::Transcribing => None,
            Phase::Success => Some(SUCCESS_DWELL),
            Phase::NoSpeech => Some(NO_SPEECH_DWELL),
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
    streaming_indicator: bool,
    style: Style,
    layer_spec: Option<LayerSpec>,
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
        self.streaming_indicator = command.config.streaming_indicator;
        self.style = style_for(&command.config.style);

        let spec = layer_spec(&command.config, self.style);
        if self.layer.is_none() || self.layer_spec.as_ref() != Some(&spec) {
            self.configured = false;
            self.create_layer(&command.config);
            self.layer_spec = Some(spec);
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
        let (width, height) = surface_size_for(self.style);
        let fullscreen = is_fullscreen_style(self.style);
        // A full-screen style spans every edge with no margin and a zero
        // requested size, letting the compositor stretch it to the output; a
        // badge style sits at the configured corner/edge with a fixed size.
        layer.set_anchor(if fullscreen {
            Anchor::TOP | Anchor::BOTTOM | Anchor::LEFT | Anchor::RIGHT
        } else {
            anchor_for(&config.position)
        });
        let margin = if fullscreen { 0 } else { MARGIN };
        layer.set_margin(margin, margin, margin, margin);
        if fullscreen {
            layer.set_size(0, 0);
        } else {
            layer.set_size(width, height);
        }
        layer.set_exclusive_zone(0);
        layer.set_keyboard_interactivity(KeyboardInteractivity::None);
        if let Ok(region) = Region::new(&self.compositor) {
            // No rectangles added: an empty region accepts no pointer input,
            // making the overlay click-through.
            layer.set_input_region(Some(region.wl_region()));
        }
        layer.commit();
        self.width = width;
        self.height = height;
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
        let t = animation_fraction(self.phase, self.style, self.anim_start);
        let breath = blob_breath_fraction(self.phase, self.anim_start);
        paint(
            &mut canvas,
            self.phase,
            t,
            breath,
            self.streaming_indicator,
            self.style,
        );

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

/// The `OverlayConfig::position` values both configuration UIs (the setup
/// console and the dashboard's Overlay screen) cycle through, in the order
/// `anchor_for` recognizes them.
pub(crate) const POSITION_VALUES: [&str; 7] = [
    "top-left",
    "top",
    "top-right",
    "center",
    "bottom-left",
    "bottom",
    "bottom-right",
];

/// The `OverlayConfig::style` values both configuration UIs offer, matching
/// `style_for`'s accepted values 1:1. These were reviewed as Superdesign
/// mockups and approved by the captain; `Blob` was added later as a captain-
/// requested "pulsating glowy blob, plasma-like" look, and `Border` as a
/// captain-requested phase-coloured screen-edge glow that is strongest in the
/// corners.
pub(crate) const STYLE_VALUES: [&str; 5] = ["badge", "minimal", "pill", "blob", "border"];

/// Display labels matching `STYLE_VALUES` position for position.
pub(crate) const STYLE_LABELS: [&str; 5] = ["Badge", "Minimal", "Pill", "Blob", "Border"];

/// Display labels for `OverlayConfig::streaming_indicator`: the plain pulsing
/// dot first, the busier live-capture treatment second. Mirrors Handy's
/// (github.com/cjpais/Handy) distinction between a minimal recording pill and
/// a busier "Live" panel with a reactive waveform once streaming
/// transcription is active - see `OverlayConfig::streaming_indicator` for why
/// this is a synthetic animation rather than a true audio-reactive one.
pub(crate) const STREAMING_LABELS: [&str; 2] = [
    "Simple pulse",
    "Streaming waveform (live-capture indicator)",
];

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

/// The `overlay.streaming_indicator` look for `Phase::Recording`: a small
/// multi-bar waveform in place of the single pulsing dot, standing in for
/// Handy's reactive live-capture waveform (see `OverlayConfig::
/// streaming_indicator`'s doc comment for why this is synthetic animation
/// rather than real microphone-reactive bars - there is no live audio-level
/// feed wired to this actor, only the elapsed-time fraction every other phase
/// already animates from). Shared by every `Style` - each passes its own
/// available width/height/color so the same five-bar motion reads correctly
/// whether it sits inside a round badge or a wide pill.
fn paint_waveform_bars(
    canvas: &mut Canvas,
    cx: f32,
    baseline_y: f32,
    available_width: f32,
    max_bar_height: f32,
    rgb: (u8, u8, u8),
    t: f32,
) {
    use std::f32::consts::TAU;
    const BAR_COUNT: usize = 5;
    const FREQUENCIES: [f32; BAR_COUNT] = [1.0, 1.6, 2.3, 1.4, 1.9];
    const PHASE_OFFSETS: [f32; BAR_COUNT] = [0.0, 0.5, 0.15, 0.8, 0.35];

    let gap = available_width * 0.08;
    let bar_width = (available_width - (BAR_COUNT - 1) as f32 * gap) / BAR_COUNT as f32;
    let total_width = BAR_COUNT as f32 * bar_width + (BAR_COUNT - 1) as f32 * gap;
    let mut x = cx - total_width / 2.0 + bar_width / 2.0;
    for i in 0..BAR_COUNT {
        let wave = 0.5 + 0.5 * ((t + PHASE_OFFSETS[i]) * TAU * FREQUENCIES[i]).sin();
        let height = max_bar_height * (0.22 + 0.78 * wave);
        fill_bar(canvas, x, baseline_y, bar_width, height, rgb);
        x += bar_width + gap;
    }
}

fn cycle_fraction(period: Duration, start: Instant) -> f32 {
    let period_secs = period.as_secs_f32();
    (start.elapsed().as_secs_f32() % period_secs) / period_secs
}

fn animation_fraction(phase: Option<Phase>, style: Style, start: Instant) -> f32 {
    let period = match phase {
        Some(Phase::Recording) => match style {
            Style::Blob => BLOB_PULSE_PERIOD,
            _ => RECORDING_PULSE_PERIOD,
        },
        Some(Phase::Transcribing) => SPIN_PERIOD,
        _ => return 0.0,
    };
    cycle_fraction(period, start)
}

/// The `Blob` silhouette's breathing fraction: always the slow
/// `BLOB_PULSE_PERIOD` in every animated phase (`Recording` and
/// `Transcribing`), independent of the faster `SPIN_PERIOD` fraction
/// `animation_fraction` returns while transcribing. Keeps the blob's calm
/// breath the same whether recording or processing.
fn blob_breath_fraction(phase: Option<Phase>, start: Instant) -> f32 {
    match phase {
        Some(Phase::Recording) | Some(Phase::Transcribing) => {
            cycle_fraction(BLOB_PULSE_PERIOD, start)
        }
        _ => 0.0,
    }
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
        // A full-screen style's real size arrives here (the compositor fills in
        // the stretched dimensions); fall back to the style's representative
        // size only if a compositor reports a zero dimension.
        let fallback = surface_size_for(self.style);
        self.width = if width == 0 { fallback.0 } else { width };
        self.height = if height == 0 { fallback.1 } else { height };
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

/// Fills a capsule ("stadium") shape spanning `2*half_width` by
/// `2*half_height`, with semicircular ends of radius `min(half_width,
/// half_height)` on the longer axis - the horizontal `Style::Pill` badge and
/// the upright muted-mic body. Degenerates to a circle when the two are equal,
/// via the same "distance from the nearest point on a line segment" trick
/// `stroke_line` uses, but filled solid within that radius of the segment
/// rather than stroked.
fn fill_capsule(
    canvas: &mut Canvas,
    cx: f32,
    cy: f32,
    half_width: f32,
    half_height: f32,
    rgb: (u8, u8, u8),
) {
    let radius = half_width.min(half_height);
    let half_segment_x = (half_width - radius).max(0.0);
    let half_segment_y = (half_height - radius).max(0.0);
    let (x0, x1) = (cx - half_segment_x, cx + half_segment_x);
    let (y0, y1) = (cy - half_segment_y, cy + half_segment_y);
    let min_x = (cx - half_width - 1.0).floor().max(0.0) as i32;
    let max_x = (cx + half_width + 1.0).ceil().min(canvas.width as f32) as i32;
    let min_y = (cy - half_height - 1.0).floor().max(0.0) as i32;
    let max_y = (cy + half_height + 1.0).ceil().min(canvas.height as f32) as i32;
    for y in min_y..max_y {
        for x in min_x..max_x {
            let px = x as f32 + 0.5;
            let py = y as f32 + 0.5;
            let proj_x = px.clamp(x0, x1);
            let proj_y = py.clamp(y0, y1);
            let dist = ((px - proj_x).powi(2) + (py - proj_y).powi(2)).sqrt();
            let coverage = (radius + 0.5 - dist).clamp(0.0, 1.0);
            canvas.blend(x, y, rgb, coverage);
        }
    }
}

/// The outline of the `Style::Blob` silhouette at `angle`, time `t`: a slow,
/// calm breathing scale with only a faint low-frequency sway, so it reads as a
/// gently living blob rather than a churning one. The amplitudes sum to 0.12,
/// bounding the outline at `radius * 1.12` - what `paint_blob` and
/// `surface_size_for` account for.
fn blob_edge(radius: f32, angle: f32, t: f32) -> f32 {
    use std::f32::consts::TAU;
    let breathe = 0.07 * (t * TAU).sin();
    let sway = 0.03 * (2.0 * angle - t * TAU).sin() + 0.02 * (3.0 * angle + t * TAU).sin();
    radius * (1.0 + breathe + sway)
}

/// Blends `color` toward white by `amount` (0..1) - used for the blob's bright
/// inner glow and near-white sheen.
fn lighten(color: (u8, u8, u8), amount: f32) -> (u8, u8, u8) {
    let f = |channel: u8| -> u8 {
        (channel as f32 + (255.0 - channel as f32) * amount)
            .round()
            .clamp(0.0, 255.0) as u8
    };
    (f(color.0), f(color.1), f(color.2))
}

/// Multiplies `color` by `factor` - used to keep a slightly darker base under
/// the blob's bright inner glow.
fn scale_color(color: (u8, u8, u8), factor: f32) -> (u8, u8, u8) {
    let f = |channel: u8| -> u8 { (channel as f32 * factor).round().clamp(0.0, 255.0) as u8 };
    (f(color.0), f(color.1), f(color.2))
}

/// Pushes a phase colour toward the bolder, brighter hue the captain asked the
/// `Border` style to use: saturation is boosted around the colour's own
/// luminance and the value is scaled toward full. It only *derives* from the
/// shared palette (no new hue is introduced), so the border still speaks the
/// same colour language as the badge styles - a saturated grey just brightens.
fn bolden(color: (u8, u8, u8)) -> (u8, u8, u8) {
    let (r, g, b) = (color.0 as f32, color.1 as f32, color.2 as f32);
    let lum = 0.299 * r + 0.587 * g + 0.114 * b;
    let saturated = |c: f32| (lum + (c - lum) * 1.6).clamp(0.0, 255.0);
    let (r, g, b) = (saturated(r), saturated(g), saturated(b));
    let max = r.max(g).max(b).max(1.0);
    let scale = (255.0 / max).min(1.05);
    let f = |c: f32| (c * scale).round().clamp(0.0, 255.0) as u8;
    (f(r), f(g), f(b))
}

/// Hermite smoothstep: 0 at `x <= 0`, 1 at `x >= 1`, with zero slope at both
/// ends - the C1 ramp the `border` glow fades along, so its falloff has no hard
/// band edge.
fn smoothstep(x: f32) -> f32 {
    let x = x.clamp(0.0, 1.0);
    x * x * (3.0 - 2.0 * x)
}

/// Linear blend of two colors, `t = 0` giving `a` and `t = 1` giving `b`.
fn mix_color(a: (u8, u8, u8), b: (u8, u8, u8), t: f32) -> (u8, u8, u8) {
    let t = t.clamp(0.0, 1.0);
    let f = |x: u8, y: u8| -> u8 {
        (x as f32 + (y as f32 - x as f32) * t)
            .round()
            .clamp(0.0, 255.0) as u8
    };
    (f(a.0, b.0), f(a.1, b.1), f(a.2, b.2))
}

fn normalize3(x: f32, y: f32, z: f32) -> (f32, f32, f32) {
    let len = (x * x + y * y + z * z).sqrt();
    (x / len, y / len, z / len)
}

/// Applies a soft metallic sheen to one blob pixel: an `env` base color lit by
/// `lit`, plus a white specular `spec` highlight and a bright fresnel `rim`
/// tinted by `sheen`. All math is in 0..255 channel space.
fn metallic_shade(
    env: (u8, u8, u8),
    lit: f32,
    spec: f32,
    rim: f32,
    sheen: (u8, u8, u8),
) -> (u8, u8, u8) {
    let channel = |env_channel: u8, sheen_channel: u8| -> u8 {
        (env_channel as f32 * lit + 235.0 * spec + sheen_channel as f32 * 0.5 * rim)
            .round()
            .clamp(0.0, 255.0) as u8
    };
    (
        channel(env.0, sheen.0),
        channel(env.1, sheen.1),
        channel(env.2, sheen.2),
    )
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

/// Fills a vertical bar of `width` centered on `cx`, growing upward from
/// `baseline_y` by `height` - the building block for the streaming-indicator
/// waveform (see `paint`'s `Phase::Recording` branch).
fn fill_bar(
    canvas: &mut Canvas,
    cx: f32,
    baseline_y: f32,
    width: f32,
    height: f32,
    rgb: (u8, u8, u8),
) {
    let min_x = (cx - width / 2.0).round().max(0.0) as i32;
    let max_x = (cx + width / 2.0).round().min(canvas.width as f32) as i32;
    let min_y = (baseline_y - height).round().max(0.0) as i32;
    let max_y = baseline_y.round().min(canvas.height as f32) as i32;
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

/// The selectable looks (see `OverlayConfig::style`): `Badge` is the original
/// solid-disc-and-glyph treatment, `Minimal` strips it to a thin outline ring
/// with a small glyph, `Pill` reshapes the badge into a capsule with room for
/// a wider waveform, `Blob` is a bright, slowly-breathing glowing blob with
/// a soft liquid-metal sheen, and `Border` is a thin phase-coloured glowing
/// line hugging the whole screen edge that fades smoothly inward, brightest in
/// the corners. Every style reskins
/// all five phases consistently, per the design review.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Style {
    Badge,
    Minimal,
    Pill,
    Blob,
    Border,
}

/// Parses `OverlayConfig::style`, falling back to `Badge` for an unrecognized
/// value - the same tolerant-fallback shape `anchor_for` uses for `position`.
fn style_for(name: &str) -> Style {
    match name {
        "minimal" => Style::Minimal,
        "pill" => Style::Pill,
        "blob" => Style::Blob,
        "border" => Style::Border,
        _ => Style::Badge,
    }
}

/// Whether a style owns the whole output rather than a small anchored badge.
/// Only `Border` does: it is a full-screen `zwlr_layer_shell_v1` surface whose
/// glow traces the screen edges, so `create_layer` anchors it to every edge
/// with no margin and a zero requested size (the compositor stretches it to the
/// output), and `configure` supplies its real dimensions.
fn is_fullscreen_style(style: Style) -> bool {
    matches!(style, Style::Border)
}

/// The layer-shell surface size to request for a style - `Pill` widens to hold
/// its capsule shape and waveform, and `Blob` enlarges to give its glow room
/// to fade out before the surface edge. For the full-screen `Border` style this
/// is only the representative size used by the offline preview and the tests;
/// `create_layer` requests a stretched zero-sized surface instead.
fn surface_size_for(style: Style) -> (u32, u32) {
    match style {
        Style::Badge | Style::Minimal => (BADGE, BADGE),
        Style::Pill => (BADGE * 5 / 4, BADGE * 5 / 8),
        Style::Blob => (BLOB_SURFACE, BLOB_SURFACE),
        Style::Border => BORDER_PREVIEW,
    }
}

fn glyph_checkmark(
    canvas: &mut Canvas,
    cx: f32,
    cy: f32,
    s: f32,
    thickness: f32,
    rgb: (u8, u8, u8),
) {
    stroke_line(
        canvas,
        cx - s * 0.9,
        cy + s * 0.05,
        cx - s * 0.15,
        cy + s * 0.75,
        thickness,
        rgb,
    );
    stroke_line(
        canvas,
        cx - s * 0.15,
        cy + s * 0.75,
        cx + s * 1.0,
        cy - s * 0.65,
        thickness,
        rgb,
    );
}

fn glyph_cross(canvas: &mut Canvas, cx: f32, cy: f32, s: f32, thickness: f32, rgb: (u8, u8, u8)) {
    stroke_line(canvas, cx - s, cy - s, cx + s, cy + s, thickness, rgb);
    stroke_line(canvas, cx - s, cy + s, cx + s, cy - s, thickness, rgb);
}

/// A microphone with a slash through it - the `NoSpeech` glyph, so a silent or
/// muted capture reads as "no speech detected" rather than as a generic error
/// cross or a plain success checkmark. `s` is the glyph's half-height, matching
/// the scale convention of `glyph_checkmark`/`glyph_cross`.
fn glyph_muted_mic(
    canvas: &mut Canvas,
    cx: f32,
    cy: f32,
    s: f32,
    thickness: f32,
    rgb: (u8, u8, u8),
) {
    // Mic body: an upright capsule, sitting slightly above center to leave room
    // for its cradle and stem.
    fill_capsule(canvas, cx, cy - s * 0.35, s * 0.34, s * 0.5, rgb);
    // Cradle: an upward-opening arc (a U) hugging the lower half of the body.
    stroke_arc(
        canvas,
        cx,
        cy - s * 0.25,
        s * 0.62,
        thickness,
        (0.0, std::f32::consts::PI),
        rgb,
    );
    // Stem below the cradle.
    stroke_line(canvas, cx, cy + s * 0.37, cx, cy + s * 0.78, thickness, rgb);
    // The slash: the "muted" signal, drawn last so it sits on top.
    stroke_line(
        canvas,
        cx - s * 0.95,
        cy + s * 0.95,
        cx + s * 0.95,
        cy - s * 0.95,
        thickness * 1.1,
        rgb,
    );
}

fn phase_color(phase: Phase) -> (u8, u8, u8) {
    match phase {
        Phase::Recording => RECORDING_COLOR,
        Phase::Transcribing => TRANSCRIBING_COLOR,
        Phase::Success => SUCCESS_COLOR,
        Phase::NoSpeech => NO_SPEECH_COLOR,
        Phase::Cancelled => CANCELLED_COLOR,
        Phase::Error => ERROR_COLOR,
    }
}

fn paint(
    canvas: &mut Canvas,
    phase: Option<Phase>,
    t: f32,
    breath: f32,
    streaming_indicator: bool,
    style: Style,
) {
    canvas.pixels.fill(0);
    let Some(phase) = phase else { return };
    match style {
        Style::Badge => paint_badge(canvas, phase, t, streaming_indicator),
        Style::Minimal => paint_minimal(canvas, phase, t, streaming_indicator),
        Style::Pill => paint_pill(canvas, phase, t, streaming_indicator),
        Style::Blob => paint_blob(canvas, phase, t, breath, streaming_indicator),
        Style::Border => paint_border(canvas, phase, t, streaming_indicator),
    }
}

fn paint_badge(canvas: &mut Canvas, phase: Phase, t: f32, streaming_indicator: bool) {
    let cx = canvas.width as f32 / 2.0;
    let cy = canvas.height as f32 / 2.0;
    let disc_radius = canvas.width.min(canvas.height) as f32 * 0.42;
    let color = phase_color(phase);

    match phase {
        Phase::Recording if streaming_indicator => {
            fill_circle(canvas, cx, cy, disc_radius, color);
            // Keep the waveform's bounding box well inside the disc: the
            // tallest bar reaches `max_bar_height` above the baseline and the
            // bars span `available_width` across, so centering that box on the
            // disc center leaves it within the circle's radius (its half
            // diagonal is ~0.67*disc_radius).
            let max_bar_height = disc_radius * 0.62;
            paint_waveform_bars(
                canvas,
                cx,
                cy + max_bar_height * 0.5,
                disc_radius * 1.2,
                max_bar_height,
                GLYPH_COLOR,
                t,
            );
        }
        Phase::Recording => {
            fill_circle(canvas, cx, cy, disc_radius, color);
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
            fill_circle(canvas, cx, cy, disc_radius, color);
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
            fill_circle(canvas, cx, cy, disc_radius, color);
            glyph_checkmark(
                canvas,
                cx,
                cy,
                disc_radius * 0.5,
                disc_radius * 0.18,
                GLYPH_COLOR,
            );
        }
        Phase::NoSpeech => {
            fill_circle(canvas, cx, cy, disc_radius, color);
            glyph_muted_mic(
                canvas,
                cx,
                cy,
                disc_radius * 0.42,
                disc_radius * 0.15,
                GLYPH_COLOR,
            );
        }
        Phase::Cancelled => {
            fill_circle(canvas, cx, cy, disc_radius, color);
            fill_square(canvas, cx, cy, disc_radius * 0.38, GLYPH_COLOR);
        }
        Phase::Error => {
            fill_circle(canvas, cx, cy, disc_radius, color);
            glyph_cross(
                canvas,
                cx,
                cy,
                disc_radius * 0.5,
                disc_radius * 0.18,
                GLYPH_COLOR,
            );
        }
    }
}

/// A thin outline ring with a small centered, phase-colored glyph - the
/// lowest-visual-weight style, with no filled background to rest a
/// contrasting white glyph against.
fn paint_minimal(canvas: &mut Canvas, phase: Phase, t: f32, streaming_indicator: bool) {
    let cx = canvas.width as f32 / 2.0;
    let cy = canvas.height as f32 / 2.0;
    let ring_radius = canvas.width.min(canvas.height) as f32 * 0.34;
    let ring_thickness = ring_radius * 0.14;
    let color = phase_color(phase);
    stroke_arc(
        canvas,
        cx,
        cy,
        ring_radius,
        ring_thickness,
        (0.0, std::f32::consts::TAU),
        color,
    );

    match phase {
        Phase::Recording if streaming_indicator => {
            paint_waveform_bars(
                canvas,
                cx,
                cy + ring_radius * 0.5,
                ring_radius * 1.5,
                ring_radius * 0.9,
                color,
                t,
            );
        }
        Phase::Recording => {
            let pulse = 0.5 + 0.5 * (t * std::f32::consts::TAU).sin();
            fill_circle(canvas, cx, cy, ring_radius * (0.28 + 0.12 * pulse), color);
        }
        Phase::Transcribing => {
            let angle = t * std::f32::consts::TAU;
            stroke_arc(
                canvas,
                cx,
                cy,
                ring_radius * 0.5,
                ring_radius * 0.18,
                (angle, std::f32::consts::PI * 1.2),
                color,
            );
        }
        Phase::Success => {
            glyph_checkmark(
                canvas,
                cx,
                cy,
                ring_radius * 0.42,
                ring_radius * 0.16,
                color,
            );
        }
        Phase::NoSpeech => {
            glyph_muted_mic(
                canvas,
                cx,
                cy,
                ring_radius * 0.36,
                ring_radius * 0.13,
                color,
            );
        }
        Phase::Cancelled => {
            fill_square(canvas, cx, cy, ring_radius * 0.3, color);
        }
        Phase::Error => {
            glyph_cross(
                canvas,
                cx,
                cy,
                ring_radius * 0.42,
                ring_radius * 0.16,
                color,
            );
        }
    }
}

/// A capsule ("pill") badge, solid-filled like `Badge` but wider than tall -
/// the extra width gives the streaming waveform more room to read.
fn paint_pill(canvas: &mut Canvas, phase: Phase, t: f32, streaming_indicator: bool) {
    let cx = canvas.width as f32 / 2.0;
    let cy = canvas.height as f32 / 2.0;
    let half_width = canvas.width as f32 * 0.46;
    let half_height = canvas.height as f32 * 0.42;
    let color = phase_color(phase);
    fill_capsule(canvas, cx, cy, half_width, half_height, color);

    match phase {
        Phase::Recording if streaming_indicator => {
            paint_waveform_bars(
                canvas,
                cx,
                cy + half_height * 0.6,
                half_width * 1.5,
                half_height * 1.3,
                GLYPH_COLOR,
                t,
            );
        }
        Phase::Recording => {
            let pulse = 0.5 + 0.5 * (t * std::f32::consts::TAU).sin();
            fill_circle(
                canvas,
                cx,
                cy,
                half_height * (0.4 + 0.14 * pulse),
                GLYPH_COLOR,
            );
        }
        Phase::Transcribing => {
            let angle = t * std::f32::consts::TAU;
            stroke_arc(
                canvas,
                cx,
                cy,
                half_height * 0.6,
                half_height * 0.22,
                (angle, std::f32::consts::PI * 1.2),
                GLYPH_COLOR,
            );
        }
        Phase::Success => {
            glyph_checkmark(
                canvas,
                cx,
                cy,
                half_height * 0.55,
                half_height * 0.2,
                GLYPH_COLOR,
            );
        }
        Phase::NoSpeech => {
            glyph_muted_mic(
                canvas,
                cx,
                cy,
                half_height * 0.46,
                half_height * 0.17,
                GLYPH_COLOR,
            );
        }
        Phase::Cancelled => {
            fill_square(canvas, cx, cy, half_height * 0.4, GLYPH_COLOR);
        }
        Phase::Error => {
            glyph_cross(
                canvas,
                cx,
                cy,
                half_height * 0.55,
                half_height * 0.2,
                GLYPH_COLOR,
            );
        }
    }
}

/// A bright, glowing blob in the phase color that slowly breathes - the `Blob`
/// style. The silhouette gently inhales and exhales on its own slower cycle
/// (`breath`, derived from `BLOB_PULSE_PERIOD`) with only a faint organic sway,
/// while a soft metallic sheen (a gentle dome highlight and a bright rim) keeps
/// it feeling like liquid metal rather than a flat disc. The `Transcribing`
/// spinner glyph advances on the faster fraction `t` independently, so the
/// silhouette keeps breathing slowly while the spinner turns. A white glyph
/// carries the state for the terminal phases and the streaming waveform,
/// exactly as every other style does; those phases are simply static (`breath`
/// is 0 once the animation stops), which is why the blob sits still for them.
fn paint_blob(canvas: &mut Canvas, phase: Phase, t: f32, breath: f32, streaming_indicator: bool) {
    let cx = canvas.width as f32 / 2.0;
    let cy = canvas.height as f32 / 2.0;
    let radius = canvas.width.min(canvas.height) as f32 * 0.30;
    let color = phase_color(phase);
    // A bright body with an even brighter inner glow and a near-white sheen,
    // so the blob stays luminous rather than turning dark and silvered.
    let body = scale_color(lighten(color, 0.1), 0.88);
    let core = lighten(color, 0.62);
    let sheen = lighten(color, 0.98);

    // Light and its half-vector (view direction is +z), for the specular dot.
    let (lx, ly, lz) = normalize3(-0.45, -0.68, 0.58);
    let (hx, hy, hz) = normalize3(lx, ly, lz + 1.0);
    let halo_width = radius * 0.22;

    let outer = radius * 1.2 + halo_width + 2.0;
    let min_x = (cx - outer).floor().max(0.0) as i32;
    let max_x = (cx + outer).ceil().min(canvas.width as f32) as i32;
    let min_y = (cy - outer).floor().max(0.0) as i32;
    let max_y = (cy + outer).ceil().min(canvas.height as f32) as i32;

    for y in min_y..max_y {
        for x in min_x..max_x {
            let dx = x as f32 + 0.5 - cx;
            let dy = y as f32 + 0.5 - cy;
            let dist = (dx * dx + dy * dy).sqrt();
            let angle = dy.atan2(dx);
            let edge = blob_edge(radius, angle, breath);

            if dist >= edge {
                // A soft glowing halo hugging the silhouette, so the blob
                // still reads as a luminous plasma on a dark desktop.
                let into_halo = dist - edge;
                if into_halo < halo_width {
                    let strength = 1.0 - into_halo / halo_width;
                    canvas.blend(x, y, sheen, 0.22 * strength * strength);
                }
                continue;
            }

            // Treat the blob as a dome: the sphere-like normal of the
            // normalized radius rolls a gentle metal sheen across the surface.
            let r = dist / edge;
            let nz = (1.0 - r * r).max(0.0).sqrt();
            let nx = dx / edge;
            let ny = dy / edge;
            let diffuse = (nx * lx + ny * ly + nz * lz).max(0.0);
            let specular = (nx * hx + ny * hy + nz * hz).max(0.0).powi(18);
            let rim = (1.0 - nz).powi(2);
            // Bright inner glow in the middle, the brighter "sky" reflection
            // up top, and the base body color near the lower edge.
            let sky = (0.5 - ny * 1.1).clamp(0.0, 1.0);
            let glow = 0.55 * (1.0 - r).powi(2) + 0.25 * sky;
            let env = mix_color(body, core, glow);
            let rgb = metallic_shade(env, 0.85 + 0.25 * diffuse, 0.9 * specular, rim, sheen);

            // Feathered edge for anti-aliasing.
            let coverage = ((edge - dist) / 1.5).clamp(0.0, 1.0);
            canvas.blend(x, y, rgb, coverage);
        }
    }

    match phase {
        Phase::Recording if streaming_indicator => {
            paint_waveform_bars(
                canvas,
                cx,
                cy + radius * 0.55,
                radius * 1.4,
                radius * 1.1,
                GLYPH_COLOR,
                t,
            );
        }
        Phase::Recording => {}
        Phase::Transcribing => {
            let angle = t * std::f32::consts::TAU;
            stroke_arc(
                canvas,
                cx,
                cy,
                radius * 0.5,
                radius * 0.18,
                (angle, std::f32::consts::PI * 1.2),
                GLYPH_COLOR,
            );
        }
        Phase::Success => {
            glyph_checkmark(canvas, cx, cy, radius * 0.42, radius * 0.16, GLYPH_COLOR);
        }
        Phase::NoSpeech => {
            glyph_muted_mic(canvas, cx, cy, radius * 0.36, radius * 0.13, GLYPH_COLOR);
        }
        Phase::Cancelled => {
            fill_square(canvas, cx, cy, radius * 0.3, GLYPH_COLOR);
        }
        Phase::Error => {
            glyph_cross(canvas, cx, cy, radius * 0.42, radius * 0.16, GLYPH_COLOR);
        }
    }
}

/// The `Border` style's per-pixel glow intensity in `0.0..=1.0` at pixel center
/// `(x, y)` of a `width` x `height` frame. It is a thin bright line hugging
/// whichever edge is nearest, dissolving smoothly into a softer glow that fades
/// inward with no visible band edge, and it is pushed higher near a corner, so
/// the four corners glow more strongly than the straight edge midpoints. `pulse`
/// (0..1) breathes the whole frame.
fn border_glow(x: f32, y: f32, width: f32, height: f32, pulse: f32) -> f32 {
    let dx_edge = x.min(width - x);
    let dy_edge = y.min(height - y);
    let d = dx_edge.min(dy_edge);
    let short = width.min(height);

    let line = (short * BORDER_LINE_FRACTION).max(1.0);
    let halo = (short * BORDER_GLOW_FRACTION).max(line * 3.0);
    if d >= halo {
        return 0.0;
    }
    let corner_reach = (short * BORDER_CORNER_REACH_FRACTION).max(halo);

    // Corner proximity: 1 exactly at a corner, falling to 0 beyond
    // `corner_reach` along either edge.
    let corner_dist = (dx_edge * dx_edge + dy_edge * dy_edge).sqrt();
    let near_corner = (1.0 - corner_dist / corner_reach).clamp(0.0, 1.0);

    // Two smoothstep ramps, both starting flat at the screen edge (`d = 0`) and
    // reaching exactly zero, with zero slope, by their own reach. Their sum is a
    // smooth, monotonic fade with no hard band edge: the narrow `line` term is
    // the crisp edge line, and the tight `halo` term is the light glow it
    // dissolves into. Because both reaches are fixed, the falloff is uniform
    // along an edge rather than stepping at a core/glow boundary.
    let profile = BORDER_LINE_STRENGTH * smoothstep(1.0 - d / line)
        + BORDER_HALO_STRENGTH * smoothstep(1.0 - d / halo);
    let boost = 1.0 + BORDER_CORNER_BOOST * near_corner * near_corner;
    (profile * boost * pulse).min(1.0)
}

/// A phase-coloured glowing frame around the whole screen edge, brightest in the
/// corners - the `Border` style. Unlike every other style its surface fills the
/// output, so it paints a border/glow band rather than a centered shape and
/// leaves the centre transparent. The whole frame breathes on the elapsed-time
/// fraction; with `streaming_indicator` a bright highlight also sweeps once
/// around the frame per cycle, standing in for the "actively capturing" signal
/// the other styles show as a waveform (see `OverlayConfig::streaming_indicator`).
fn paint_border(canvas: &mut Canvas, phase: Phase, t: f32, streaming_indicator: bool) {
    use std::f32::consts::{PI, TAU};
    let width = canvas.width as f32;
    let height = canvas.height as f32;
    let (cx, cy) = (width / 2.0, height / 2.0);
    // Bolder, brighter hues than the base phase colour (the captain's ask),
    // with only a light lift on the core so the hue never washes out to white.
    let color = bolden(phase_color(phase));
    let hot = lighten(color, 0.28);
    let pulse = 0.7 + 0.3 * (0.5 + 0.5 * (t * TAU).sin());
    let head = t * TAU - PI;
    // The sweep is a recording-only "actively capturing" cue, matching every
    // other style's `Phase::Recording if streaming_indicator` branch.
    let sweeping = streaming_indicator && matches!(phase, Phase::Recording);
    // Angular half-width of the streaming highlight's bright arc.
    const SWEEP: f32 = 0.55;

    for y in 0..canvas.height {
        for x in 0..canvas.width {
            let px = x as f32 + 0.5;
            let py = y as f32 + 0.5;
            let mut intensity = border_glow(px, py, width, height, pulse);
            if sweeping && intensity > 0.0 {
                let angle = (py - cy).atan2(px - cx);
                let delta = (angle - head + PI).rem_euclid(TAU) - PI;
                let sweep = (1.0 - delta.abs() / SWEEP).clamp(0.0, 1.0);
                intensity = (intensity + sweep * sweep * 0.85).min(1.0);
            }
            if intensity <= 0.0 {
                continue;
            }
            let rgb = mix_color(color, hot, intensity);
            canvas.blend(x as i32, y as i32, rgb, intensity);
        }
    }
}

/// Renders one overlay frame into a straight-alpha BGRA8 buffer (wl_shm
/// `Argb8888`, little-endian), for the offline design-preview example
/// (`examples/overlay_style_png.rs`, which reorders it to PNG's RGBA). `style`
/// and `phase` use the same string values the config and feedback events use
/// (`badge`/`minimal`/`pill`/`blob`/`border`; `recording`/`transcribing`/
/// `success`/`no-speech`/`cancelled`/`error`), and `None` clears to fully
/// transparent.
/// Returns `(width, height, pixels)` with `width * height * 4` bytes. For the
/// full-screen `border` style this is the representative `BORDER_PREVIEW` size,
/// not a compositor-provided output size.
#[doc(hidden)]
pub fn render_frame_pixels(
    style: &str,
    phase: Option<&str>,
    t: f32,
    streaming_indicator: bool,
) -> (u32, u32, Vec<u8>) {
    let style = style_for(style);
    let phase = phase.map(|name| match name {
        "transcribing" => Phase::Transcribing,
        "success" => Phase::Success,
        "no-speech" => Phase::NoSpeech,
        "cancelled" => Phase::Cancelled,
        "error" => Phase::Error,
        _ => Phase::Recording,
    });
    let (width, height) = surface_size_for(style);
    let mut pixels = vec![0u8; (width * height * 4) as usize];
    {
        let mut canvas = Canvas {
            pixels: &mut pixels,
            width,
            height,
        };
        paint(&mut canvas, phase, t, t, streaming_indicator, style);
    }
    (width, height, pixels)
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
    fn layer_spec_tracks_every_surface_geometry_and_placement_input() {
        let base = OverlayConfig::default();
        let base_spec = layer_spec(&base, Style::Badge);

        // A style whose surface geometry differs must yield a different spec,
        // so a reload recreates the layer instead of painting the new style
        // into the previous surface's size/anchor.
        assert_ne!(base_spec, layer_spec(&base, Style::Border));
        assert_ne!(base_spec, layer_spec(&base, Style::Pill));

        // The anchor (position) and the output (monitor) are baked into the
        // surface at creation too, so each must move the spec.
        let mut other_position = base.clone();
        other_position.position = "bottom-left".into();
        assert_ne!(base_spec, layer_spec(&other_position, Style::Badge));

        let mut other_monitor = base.clone();
        other_monitor.monitor = "HDMI-A-1".into();
        assert_ne!(base_spec, layer_spec(&other_monitor, Style::Badge));

        // An identical config must not force a recreation.
        assert_eq!(base_spec, layer_spec(&base.clone(), Style::Badge));
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

    const ALL_STYLES: [Style; 5] = [
        Style::Badge,
        Style::Minimal,
        Style::Pill,
        Style::Blob,
        Style::Border,
    ];

    fn paint_to_pixels(
        phase: Option<Phase>,
        t: f32,
        streaming_indicator: bool,
        style: Style,
    ) -> Vec<u8> {
        paint_to_pixels_with_breath(phase, t, t, streaming_indicator, style)
    }

    fn paint_to_pixels_with_breath(
        phase: Option<Phase>,
        t: f32,
        breath: f32,
        streaming_indicator: bool,
        style: Style,
    ) -> Vec<u8> {
        let (width, height) = surface_size_for(style);
        let mut pixels = vec![0u8; (width * height * 4) as usize];
        {
            let mut canvas = Canvas {
                pixels: &mut pixels,
                width,
                height,
            };
            paint(&mut canvas, phase, t, breath, streaming_indicator, style);
        }
        pixels
    }

    #[test]
    fn no_speech_event_maps_to_a_dwelling_static_phase() {
        assert_eq!(Phase::from_event(FeedbackEvent::NoSpeech), Phase::NoSpeech);
        assert_eq!(Phase::NoSpeech.dwell(), Some(NO_SPEECH_DWELL));
        assert!(!Phase::NoSpeech.is_animated());
    }

    #[test]
    fn every_phase_paints_distinguishable_pixels_in_every_style() {
        for style in ALL_STYLES {
            let mut seen = Vec::new();
            for phase in [
                Phase::Recording,
                Phase::Transcribing,
                Phase::Success,
                Phase::NoSpeech,
                Phase::Cancelled,
                Phase::Error,
            ] {
                let pixels = paint_to_pixels(Some(phase), 0.0, false, style);
                assert!(
                    pixels.iter().any(|&byte| byte != 0),
                    "{style:?}/{phase:?} painted nothing"
                );
                assert!(
                    !seen.contains(&pixels),
                    "{style:?}/{phase:?} is pixel-identical to an earlier phase in the same style"
                );
                seen.push(pixels);
            }
        }
    }

    #[test]
    fn idle_paint_is_fully_transparent_in_every_style() {
        for style in ALL_STYLES {
            let pixels = paint_to_pixels(None, 0.0, false, style);
            assert!(pixels.iter().all(|&byte| byte == 0), "{style:?}");
        }
    }

    #[test]
    fn streaming_indicator_paints_a_different_recording_glyph_than_the_default_pulse_in_every_style(
    ) {
        for style in ALL_STYLES {
            let default_pixels = paint_to_pixels(Some(Phase::Recording), 0.25, false, style);
            let streaming_pixels = paint_to_pixels(Some(Phase::Recording), 0.25, true, style);
            assert_ne!(
                default_pixels, streaming_pixels,
                "{style:?}: streaming_indicator should visually differ from the default pulse"
            );
        }
    }

    #[test]
    fn every_style_paints_visually_distinct_recording_pixels() {
        // Each style has its own surface dimensions (`surface_size_for`), so
        // compare via pixel content rather than raw byte-equality across
        // differently-sized buffers.
        let badge = paint_to_pixels(Some(Phase::Recording), 0.0, false, Style::Badge);
        let minimal = paint_to_pixels(Some(Phase::Recording), 0.0, false, Style::Minimal);
        let pill = paint_to_pixels(Some(Phase::Recording), 0.0, false, Style::Pill);
        let blob = paint_to_pixels(Some(Phase::Recording), 0.0, false, Style::Blob);
        let border = paint_to_pixels(Some(Phase::Recording), 0.0, false, Style::Border);
        assert_ne!(
            badge.len(),
            pill.len(),
            "Pill should use a differently-sized surface"
        );
        assert_ne!(
            badge.len(),
            blob.len(),
            "Blob should use a differently-sized surface"
        );
        assert_ne!(
            blob.len(),
            border.len(),
            "Border should use a differently-sized (full-screen) surface"
        );
        assert_ne!(
            badge, minimal,
            "Badge and Minimal should look different at the same size"
        );
    }

    #[test]
    fn blob_paints_inside_its_bounds_with_transparent_margins() {
        // The blob's glow has to fade to nothing before the surface edge, or
        // it would clip; check a transparent band on every side across a
        // full breathing cycle, and that it paints something at all.
        const SAMPLES: [f32; 5] = [0.0, 0.2, 0.4, 0.6, 0.8];
        let (width, height) = surface_size_for(Style::Blob);
        for t in SAMPLES {
            let pixels = paint_to_pixels(Some(Phase::Recording), t, false, Style::Blob);
            let mut painted = 0u32;
            for y in 0..height {
                for x in 0..width {
                    if pixels[((y * width + x) * 4 + 3) as usize] == 0 {
                        continue;
                    }
                    painted += 1;
                    assert!(
                        x >= 2 && y >= 2 && x + 2 < width && y + 2 < height,
                        "blob painted edge pixel ({x},{y}) at t={t}; its glow needs a transparent margin"
                    );
                }
            }
            assert!(painted > 0, "blob painted nothing at t={t}");
        }
    }

    #[test]
    fn blob_style_paints_a_distinct_pixel_treatment_per_phase() {
        let mut seen: Vec<Vec<u8>> = Vec::new();
        for phase in [
            Phase::Recording,
            Phase::Transcribing,
            Phase::Success,
            Phase::NoSpeech,
            Phase::Cancelled,
            Phase::Error,
        ] {
            let pixels = paint_to_pixels(Some(phase), 0.0, false, Style::Blob);
            assert!(
                pixels.iter().any(|&byte| byte != 0),
                "blob/{phase:?} painted nothing"
            );
            assert!(
                !seen.contains(&pixels),
                "blob/{phase:?} is pixel-identical to an earlier blob phase"
            );
            seen.push(pixels);
        }
    }

    #[test]
    fn blob_slowly_breathes_across_its_animation_cycle() {
        // The recording blob is animated: successive frames of the cycle must
        // differ, or the "slowly breathing" look has regressed to a static
        // shape.
        let frames: Vec<Vec<u8>> = [0.0, 0.25, 0.5, 0.75]
            .iter()
            .map(|&t| paint_to_pixels(Some(Phase::Recording), t, false, Style::Blob))
            .collect();
        for pair in frames.windows(2) {
            assert_ne!(
                pair[0], pair[1],
                "consecutive blob frames should differ (slow breathing)"
            );
        }
    }

    #[test]
    fn blob_silhouette_breathes_on_the_slow_breath_fraction_not_the_spinner() {
        // While transcribing, the spinner advances on its own faster fraction
        // while the silhouette must keep breathing on the slow BLOB_PULSE_PERIOD
        // fraction. Change the spinner with the breath held fixed: the glyph
        // moves but the silhouette's alpha boundary is identical. Change the
        // breath with the spinner held fixed: the silhouette boundary moves.
        let alpha = |pixels: &[u8]| -> Vec<u8> {
            pixels
                .as_chunks::<4>()
                .0
                .iter()
                .map(|pixel| pixel[3])
                .collect()
        };

        let spinner_a =
            paint_to_pixels_with_breath(Some(Phase::Transcribing), 0.0, 0.3, false, Style::Blob);
        let spinner_b =
            paint_to_pixels_with_breath(Some(Phase::Transcribing), 0.6, 0.3, false, Style::Blob);
        assert_ne!(
            spinner_a, spinner_b,
            "the transcribing spinner should still move on its own fraction"
        );
        assert_eq!(
            alpha(&spinner_a),
            alpha(&spinner_b),
            "the blob silhouette must not follow the spinner fraction"
        );

        let breath_b =
            paint_to_pixels_with_breath(Some(Phase::Transcribing), 0.0, 0.8, false, Style::Blob);
        assert_ne!(
            alpha(&spinner_a),
            alpha(&breath_b),
            "the blob silhouette must breathe on the breath fraction"
        );
    }

    #[test]
    fn blob_edge_is_continuous_across_the_breath_cycle_wrap() {
        // The breath fraction is a period-1 sawtooth, so the silhouette edge
        // at the end of a cycle (t = 1) must match the start (t = 0); a term
        // that is not periodic in t would flip sign there and pop the edge.
        let radius = 30.0;
        for i in 0..64 {
            let angle = i as f32 / 64.0 * std::f32::consts::TAU - std::f32::consts::PI;
            let start = blob_edge(radius, angle, 0.0);
            let wrap = blob_edge(radius, angle, 1.0);
            assert!(
                (start - wrap).abs() < 0.01,
                "blob_edge jumps at the breath wrap for angle {angle}: {start} vs {wrap}"
            );
        }
    }

    fn alpha_at(pixels: &[u8], width: u32, x: u32, y: u32) -> u8 {
        pixels[((y * width + x) * 4 + 3) as usize]
    }

    fn alpha_sum(pixels: &[u8], width: u32, x0: u32, y0: u32, side: u32) -> u32 {
        let mut sum = 0u32;
        for y in y0..y0 + side {
            for x in x0..x0 + side {
                sum += alpha_at(pixels, width, x, y) as u32;
            }
        }
        sum
    }

    #[test]
    fn border_paints_a_frame_around_the_edges_with_a_transparent_centre() {
        let (width, height) = surface_size_for(Style::Border);
        let pixels = paint_to_pixels(Some(Phase::Recording), 0.0, false, Style::Border);
        // The middle of the screen must stay clear: this style only paints a
        // border/glow band, never anything in the centre.
        for y in height / 2 - 20..height / 2 + 20 {
            for x in width / 2 - 20..width / 2 + 20 {
                assert_eq!(
                    alpha_at(&pixels, width, x, y),
                    0,
                    "border centre pixel ({x},{y}) should be transparent"
                );
            }
        }
        // Every edge and every corner paints.
        assert!(alpha_at(&pixels, width, width / 2, 0) > 0, "top edge");
        assert!(
            alpha_at(&pixels, width, width / 2, height - 1) > 0,
            "bottom edge"
        );
        assert!(alpha_at(&pixels, width, 0, height / 2) > 0, "left edge");
        assert!(
            alpha_at(&pixels, width, width - 1, height / 2) > 0,
            "right edge"
        );
        assert!(alpha_at(&pixels, width, 0, 0) > 0, "top-left corner");
        assert!(
            alpha_at(&pixels, width, width - 1, height - 1) > 0,
            "bottom-right corner"
        );
    }

    #[test]
    fn border_corners_glow_brighter_than_edge_midpoints() {
        // The captain's ask: the glow is strongest in the corners. Compare an
        // equal-area window at a corner against the same-sized window centred
        // on an edge, at the same animation instant so the breathing cancels.
        let (width, height) = surface_size_for(Style::Border);
        let pixels = paint_to_pixels(Some(Phase::Recording), 0.0, false, Style::Border);
        let side = 40;
        let corner = alpha_sum(&pixels, width, 0, 0, side);
        let top_mid = alpha_sum(&pixels, width, width / 2 - side / 2, 0, side);
        let left_mid = alpha_sum(&pixels, width, 0, height / 2 - side / 2, side);
        assert!(
            corner > top_mid,
            "corner glow {corner} should exceed top-edge midpoint {top_mid}"
        );
        assert!(
            corner > left_mid,
            "corner glow {corner} should exceed left-edge midpoint {left_mid}"
        );
    }

    #[test]
    fn border_line_is_thin_and_the_glow_fades_smoothly_inward() {
        // The captain's ask: a real thin border - about a quarter of the old
        // thickness - with a light glow that tucks in tight. Walk a column in
        // from the top edge at the horizontal middle, so no corner hotspot
        // colours the profile.
        let (width, height) = surface_size_for(Style::Border);
        let pixels = paint_to_pixels(Some(Phase::Recording), 0.0, false, Style::Border);
        let x = width / 2;
        let alphas: Vec<u8> = (0..height / 2)
            .map(|y| alpha_at(&pixels, width, x, y))
            .collect();

        // The line is a thin ~2-pixel edge (BORDER_LINE_FRACTION of the shorter
        // side) - about a quarter of the original 0.4%-of-short-side band.
        let short = width.min(height) as f32;
        let line = alphas.iter().take_while(|&&a| a >= 64).count() as f32;
        assert!(
            line >= 1.0,
            "the border should still paint a visible line at the edge"
        );
        assert!(
            line <= short * 0.004,
            "the border line should be a thin ~2px edge, but stayed >= quarter opacity for {line}px of a {short}px side"
        );

        // The glow is tight: past the line it dies out within a small fraction
        // of the shorter side (BORDER_GLOW_FRACTION), and only ever weakens - a
        // monotonic fade with no band or ripple. It reaches fully transparent
        // long before the centre.
        let nonzero = alphas.iter().take_while(|&&a| a > 0).count() as f32;
        assert!(
            nonzero > line,
            "the border should fade inward past the line, not stop at it"
        );
        assert!(
            nonzero <= short * 0.04,
            "the border's glow should tuck in tight, but reached {nonzero}px of a {short}px side"
        );
        for pair in alphas.windows(2) {
            assert!(
                pair[1] <= pair[0],
                "border glow should fade monotonically inward, got {} then {}",
                pair[0],
                pair[1]
            );
        }
        assert_eq!(
            *alphas.last().unwrap(),
            0,
            "the fade should reach fully transparent before the frame centre"
        );
    }

    #[test]
    fn border_uses_the_phase_colour() {
        // Buffer is BGRA, so channel 2 is red and channel 0 is blue.
        let (width, _) = surface_size_for(Style::Border);
        let index = ((width / 2) * 4) as usize;
        let recording = paint_to_pixels(Some(Phase::Recording), 0.0, false, Style::Border);
        let transcribing = paint_to_pixels(Some(Phase::Transcribing), 0.0, false, Style::Border);
        assert!(
            recording[index + 2] > recording[index],
            "recording border should be red-dominant"
        );
        assert!(
            transcribing[index] > transcribing[index + 2],
            "transcribing border should be blue-dominant"
        );
    }

    #[test]
    fn border_colours_are_bolder_and_brighter_than_the_base_phase_colour() {
        // The captain's follow-up: the border should use bolder, brighter hues
        // than the base phase colour, without dropping the shared hue.
        let saturation = |c: (u8, u8, u8)| -> f32 {
            let max = c.0.max(c.1).max(c.2) as f32;
            let min = c.0.min(c.1).min(c.2) as f32;
            if max <= 0.0 {
                0.0
            } else {
                (max - min) / max
            }
        };
        for phase in [
            Phase::Recording,
            Phase::Transcribing,
            Phase::Success,
            Phase::NoSpeech,
            Phase::Error,
        ] {
            let base = phase_color(phase);
            let bold = bolden(base);
            assert!(
                bold.0.max(bold.1).max(bold.2) >= base.0.max(base.1).max(base.2),
                "{phase:?}: boldened colour should not get darker ({bold:?} vs {base:?})"
            );
            assert!(
                saturation(bold) >= saturation(base),
                "{phase:?}: boldened colour should be at least as saturated ({bold:?} vs {base:?})"
            );
        }
    }

    #[test]
    fn border_breathes_across_its_animation_cycle() {
        let frames: Vec<Vec<u8>> = [0.0, 0.25, 0.5, 0.75]
            .iter()
            .map(|&t| paint_to_pixels(Some(Phase::Recording), t, false, Style::Border))
            .collect();
        for pair in frames.windows(2) {
            assert_ne!(
                pair[0], pair[1],
                "consecutive border frames should differ (breathing glow)"
            );
        }
    }

    #[test]
    fn border_streaming_indicator_sweeps_a_travelling_highlight() {
        let sum =
            |pixels: &[u8]| -> u64 { pixels.as_chunks::<4>().0.iter().map(|p| p[3] as u64).sum() };
        let default = paint_to_pixels(Some(Phase::Recording), 0.15, false, Style::Border);
        let streaming = paint_to_pixels(Some(Phase::Recording), 0.15, true, Style::Border);
        assert!(
            sum(&streaming) > sum(&default),
            "the streaming sweep should add extra glow over the default border"
        );
        // The highlight's position advances with the cycle. At t = 0.0 and
        // t = 0.5 the base pulse is identical (sin 0 and sin pi are both 0), so
        // any difference is the highlight having swept to the opposite side.
        let a = paint_to_pixels(Some(Phase::Recording), 0.0, true, Style::Border);
        let b = paint_to_pixels(Some(Phase::Recording), 0.5, true, Style::Border);
        assert_ne!(
            a, b,
            "the streaming highlight should travel around the frame, not sit still"
        );
    }

    /// Is `(x, y)` (a pixel center in surface coordinates) inside the filled
    /// shape a style paints its streaming waveform into? `Badge` is the disc,
    /// `Minimal` the ring's inner edge (no filled background, just the
    /// outline), `Pill` the capsule, `Blob` the blob's glow extent, `Border`
    /// the glow band hugging the screen edge.
    fn inside_style_shape(style: Style, x: f32, y: f32) -> bool {
        let (width, height) = surface_size_for(style);
        let cx = width as f32 / 2.0;
        let cy = height as f32 / 2.0;
        match style {
            Style::Badge => {
                let radius = width.min(height) as f32 * 0.42;
                (x - cx).powi(2) + (y - cy).powi(2) <= radius * radius
            }
            Style::Minimal => {
                let ring_radius = width.min(height) as f32 * 0.34;
                let inner = ring_radius - ring_radius * 0.14 / 2.0;
                (x - cx).powi(2) + (y - cy).powi(2) <= inner * inner
            }
            Style::Pill => {
                let half_width = width as f32 * 0.46;
                let half_height = height as f32 * 0.42;
                let half_segment = (half_width - half_height).max(0.0);
                let proj_x = x.clamp(cx - half_segment, cx + half_segment);
                (x - proj_x).powi(2) + (y - cy).powi(2) <= half_height * half_height
            }
            Style::Blob => {
                // The blob's breath and sway can push its edge out to 1.12x the
                // nominal radius, and the glow halo adds a further 0.22x, so
                // treat ~1.45x as the shape's outer bound.
                let outer = width.min(height) as f32 * 0.30 * 1.45;
                (x - cx).powi(2) + (y - cy).powi(2) <= outer * outer
            }
            Style::Border => {
                // Every painted pixel lies within the soft glow's reach of the
                // nearest edge (see `border_glow`). Add a pixel of slack for the
                // anti-aliased boundary.
                let reach = width.min(height) as f32 * BORDER_GLOW_FRACTION + 1.0;
                x.min(width as f32 - x).min(y.min(height as f32 - y)) < reach
            }
        }
    }

    #[test]
    fn streaming_waveform_bars_stay_inside_every_style_shape() {
        // Identify the waveform pixels and check every one sits inside the
        // style's shape. `Badge`/`Pill` draw the bars in opaque white over a
        // solid phase-colored fill, so those are exactly the pure-white
        // pixels. `Minimal` draws the bars in the same color as its ring, so
        // the bars are the phase-colored pixels that move as the animation
        // advances (the ring itself is static). Every pixel found must be
        // inside the shape; the pre-fix badge sprawled ~2px past the disc on
        // each side.
        const SAMPLES: [f32; 5] = [0.0, 0.13, 0.27, 0.41, 0.55];
        for style in ALL_STYLES {
            let (width, height) = surface_size_for(style);
            let frames: Vec<Vec<u8>> = SAMPLES
                .iter()
                .map(|&t| paint_to_pixels(Some(Phase::Recording), t, true, style))
                .collect();
            let white_glyph = matches!(style, Style::Badge | Style::Pill);
            let base = &frames[0];
            let mut waveform_pixels = 0u32;
            for y in 0..height {
                for x in 0..width {
                    let index = ((y * width + x) * 4) as usize;
                    let varies = frames
                        .iter()
                        .any(|frame| frame[index..index + 4] != base[index..index + 4]);
                    let glyph = white_glyph && base[index..index + 4] == [255, 255, 255, 255];
                    if !varies && !glyph {
                        continue;
                    }
                    waveform_pixels += 1;
                    let (px, py) = (x as f32 + 0.5, y as f32 + 0.5);
                    assert!(
                        inside_style_shape(style, px, py),
                        "{style:?}: waveform pixel ({x},{y}) painted outside the shape"
                    );
                }
            }
            assert!(
                waveform_pixels >= 30,
                "{style:?}: expected a visible waveform inside the shape, only {waveform_pixels} waveform pixels"
            );
        }
    }

    #[test]
    fn style_for_recognizes_every_configured_value_and_falls_back_to_badge() {
        assert_eq!(style_for("badge"), Style::Badge);
        assert_eq!(style_for("minimal"), Style::Minimal);
        assert_eq!(style_for("pill"), Style::Pill);
        assert_eq!(style_for("blob"), Style::Blob);
        assert_eq!(style_for("border"), Style::Border);
        assert_eq!(style_for("nonsense"), Style::Badge);
    }
}
