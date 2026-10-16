//! Passive wlr-layer-shell HUD overlay for dictation feedback.
//!
//! Renders directly into persistent, click-through `zwlr_layer_shell_v1`
//! surfaces - one per target output - so recording/transcribing/success/
//! no-speech/copied/cancellation/failure state is glanceable on every connected
//! screen at once, without stealing keyboard or pointer focus. By default
//! (`overlay.monitor = "all"`) the overlay is shown on *every* connected
//! output; a named monitor narrows it to that one output (see [`target_for`]).
//! Each surface is created lazily, on the first event; between events it holds
//! a fully transparent buffer rather than being mapped and unmapped, and the
//! set is only recreated when the config-derived geometry or placement changes
//! (an `overlay.style`/`position`/`monitor` change applied via `tonguetyped
//! reload`), never per dictation, so there is no per-dictation
//! surface-creation round trip on the latency-sensitive stop path (see
//! AGENTS.md's stop-to-idle latency note). Output add/remove is handled live:
//! a hotplugged monitor gains its own surface on the next reconcile and a
//! removed one's surface is dropped.
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
//!
//! Besides the state cues above, the overlay can paint **live text**: when a
//! streaming model is active, the coordinator forwards its partial transcript
//! through [`OverlayHandle::try_send_live_text`] and, when a `live-text` style
//! is configured (see [`Style::LiveText`]), the overlay draws it as a dark
//! glass card with a leading phase chip, the frozen prefix bright, the
//! still-revisable tail dimmed, and a caret. The configured style is
//! authoritative: a non-caption style (for example `blob`) never switches to
//! the caption, so with a streaming model it shows itself and paints no live
//! text at all. That text is preview only: it is never typed, persisted, or
//! mirrored to the fallback chain.

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

mod text;

const BADGE: u32 = 56;
/// The `Blob` style's square surface, larger than `BADGE` so its glow - and
/// the spikes its voice response grows - has room to fade out before the
/// surface edge (see `paint_blob`/`blob_edge`).
const BLOB_SURFACE: u32 = 96;
/// The `LongPill` style's surface: a wide bar whose 8:1 width-to-height ratio
/// matches the captain's reference (a 944x121 bar; see the design record at
/// `data/tt-pill-overlay-live-microphone-waveform-re-23/design-brief.md`).
/// Unlike the small badge styles it has no glow to pad for, so the surface is
/// exactly the bar itself.
const LONG_PILL_SURFACE: (u32, u32) = (384, 48);
/// The long pill's squiggle trace colour: the reference's light neutral grey.
const LONG_PILL_LINE: (u8, u8, u8) = (0xC8, 0xC8, 0xC8);
/// The `HalfCircle` style's surface: twice the badge width by the badge height,
/// so a dome resting on the top edge and the soft glow around it both fade to
/// nothing before the surface edge (see `paint_half_circle`). It is anchored to
/// the top edge only, so it is horizontally centred by the compositor.
const HALF_CIRCLE_SURFACE: (u32, u32) = (BADGE * 2, BADGE);
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
/// `paint_half_circle` geometry, all as fractions of the surface height: the
/// dome's radius and the reach of the soft glow that dissolves outward from its
/// curved edge. Their sum (`0.5 + 0.30 = 0.80`) is under the surface half-width
/// and under the height, leaving an equal transparent margin to the sides and
/// below the flat edge, so the glow never clips.
const HALF_CIRCLE_RADIUS_FRACTION: f32 = 0.5;
const HALF_CIRCLE_GLOW_FRACTION: f32 = 0.30;
/// How far the dome's solid fill dims from its core to its curved edge - small
/// enough that the dome still reads solid, not as a ring.
const HALF_CIRCLE_FILL_FALLOFF: f32 = 0.12;
/// Angular half-width of the recording-only streaming highlight sweeping along
/// the arc (radians).
const HALF_CIRCLE_SWEEP: f32 = 0.9;
const MARGIN: i32 = 20;
const TICK: Duration = Duration::from_millis(33);
/// How many live microphone level samples the actor keeps: one per `TICK`
/// while recording, so the waveform's window is about two seconds long.
const LEVEL_HISTORY: usize = 64;
const RECORDING_PULSE_PERIOD: Duration = Duration::from_millis(1600);
/// The `Blob` style breathes more slowly than the other styles' plain pulse -
/// a calm, slow breath rather than a quick heartbeat.
const BLOB_PULSE_PERIOD: Duration = Duration::from_millis(2800);
const SPIN_PERIOD: Duration = Duration::from_millis(900);
const SUCCESS_DWELL: Duration = Duration::from_millis(900);
// A touch longer than `Success`: a "no speech" cue is information the user
// needs to notice, but it is not an error and must not feel alarming.
const NO_SPEECH_DWELL: Duration = Duration::from_millis(1400);
// Longer than `Success`: the user has to notice the text is waiting on the
// clipboard rather than typed.
const COPIED_DWELL: Duration = Duration::from_millis(1200);
// As long as an error: typing is broken and the user has to paste by hand.
const CLIPBOARD_FALLBACK_DWELL: Duration = Duration::from_millis(1800);
// Longer than `Success`: the text is in, but Enter was not pressed.
const SUBMIT_FAILED_DWELL: Duration = Duration::from_millis(1800);
const CANCELLED_DWELL: Duration = Duration::from_millis(900);
const ERROR_DWELL: Duration = Duration::from_millis(1800);

const RECORDING_COLOR: (u8, u8, u8) = (0xE0, 0x31, 0x31);
const TRANSCRIBING_COLOR: (u8, u8, u8) = (0x19, 0x71, 0xC2);
const SUCCESS_COLOR: (u8, u8, u8) = (0x2F, 0x9E, 0x44);
// Amber: caution, not failure - distinct from every other phase hue.
const NO_SPEECH_COLOR: (u8, u8, u8) = (0xE8, 0x9A, 0x2B);
// Teal: done, like `Success`, but the text went to the clipboard.
const COPIED_COLOR: (u8, u8, u8) = (0x0C, 0x96, 0x88);
// Deep orange: a warning stronger than `NoSpeech`'s amber, short of an error.
const CLIPBOARD_FALLBACK_COLOR: (u8, u8, u8) = (0xE8, 0x70, 0x1E);
// Violet: the text landed (like `Success`) but the submit did not, so it is
// neither a plain success nor an error.
const SUBMIT_FAILED_COLOR: (u8, u8, u8) = (0x7E, 0x57, 0xC2);
const CANCELLED_COLOR: (u8, u8, u8) = (0x86, 0x8E, 0x96);
const ERROR_COLOR: (u8, u8, u8) = (0xC9, 0x2A, 0x2A);
const GLYPH_COLOR: (u8, u8, u8) = (0xFF, 0xFF, 0xFF);

/// The single-line caption's surface ceiling: a pill sized for a long caption
/// line plus padding (the reviewed mockup's 720x96). The single-line caption is
/// content-sized in width between [`LIVE_TEXT_MIN_WIDTH`] and this ceiling and
/// in height from its chip slot and [`LIVE_TEXT_VPADDING`], so an empty or
/// short caption renders as a small pill that hugs its content instead of a
/// wide, mostly-empty bar; it is anchored at the configured `overlay.position`
/// like the badge styles. The box style shares the content-sizing but keeps its
/// own wrapping surface; see [`LIVE_TEXT_BOX_SURFACE`].
const LIVE_TEXT_SURFACE: (u32, u32) = (720, 96);
/// The narrowest the single-line caption pill shrinks to: wide enough for the
/// empty state's cue and listening pad without collapsing to a stub.
const LIVE_TEXT_MIN_WIDTH: u32 = 150;
/// The `LiveTextBox` style's surface: a wrapping card that grows to fit its
/// lines and scrolls internally past [`text::CAPTION_MAX_LINES`], for long
/// passages. The width is fixed (it sets the wrap measure); the height is
/// content-sized from the card's own top/bottom padding up to this ceiling.
const LIVE_TEXT_BOX_SURFACE: (u32, u32) = (400, 320);
/// Inset of the card's dark background from the surface edge, leaving room for
/// the capsule's anti-aliased ends to fade out before the surface boundary.
const LIVE_TEXT_INSET: f32 = 4.0;
/// Left/right padding between the card edge and the contents.
const LIVE_TEXT_PADDING: f32 = 26.0;
/// The single-line capsule's vertical padding above and below the phase chip:
/// the capsule is exactly the chip's diameter plus this padding on each side,
/// matching the reviewed 88px pill mock.
const LIVE_TEXT_VPADDING: f32 = 23.0;
/// The leading phase chip's radius and the gap between it and the text.
const LIVE_TEXT_CHIP_RADIUS: f32 = 21.0;
const LIVE_TEXT_CHIP_GAP: f32 = 15.0;
/// Gap between the scrolling box's leading chip row and its first text row.
const LIVE_TEXT_BOX_CHIP_GAP: f32 = 12.0;
/// The smallest the scrolling box shrinks to: the chip row plus its padding,
/// the empty listening state before the first word.
const LIVE_TEXT_BOX_MIN_HEIGHT: u32 = 96;
/// The caption surface is quantised up to this many pixels: a word-by-word
/// change that does not move the measured size by a whole step never resizes
/// the layer. Only a quantised change commits a new `set_size` (the surface/
/// `layer_spec` note in the PR 74 report), so the layer is not resized per
/// frame while words stream in.
const LIVE_TEXT_SIZE_QUANTUM: u32 = 8;
/// The card's dark glass fill: a vertical gradient from a lifted top to a
/// near-black bottom, under a just-under-solid opacity so it reads as glass.
const LIVE_TEXT_BACKGROUND_TOP: (u8, u8, u8) = (0x26, 0x26, 0x2B);
const LIVE_TEXT_BACKGROUND_BOTTOM: (u8, u8, u8) = (0x0F, 0x0F, 0x12);
const LIVE_TEXT_BACKGROUND_OPACITY: f32 = 0.92;
/// Frozen prefix colour (near-white) and the dimmer, still-revisable tail.
const LIVE_TEXT_COMMITTED: (u8, u8, u8) = (0xF4, 0xF4, 0xF5);
const LIVE_TEXT_TENTATIVE: (u8, u8, u8) = (0x74, 0x74, 0x78);
/// The soft caret after the newest word.
const LIVE_TEXT_CARET: (u8, u8, u8) = (0xFF, 0xFF, 0xFF);
/// The caret's width and height (a rounded bar sitting on the text baseline).
const LIVE_TEXT_CARET_WIDTH: f32 = 3.0;
const LIVE_TEXT_CARET_HEIGHT: f32 = 26.0;
/// The listening pad's three breathing dots: radius and the gap between them.
/// [`LIVE_TEXT_MIN_WIDTH`] keeps the empty pill from collapsing to a stub.
const LIVE_TEXT_LISTEN_RADIUS: f32 = 6.0;
const LIVE_TEXT_LISTEN_GAP: f32 = 11.0;

/// One live-text snapshot: the frozen `committed` prefix and the still-revisable
/// `tentative` tail a streaming model reports while the user speaks. Produced by
/// the coordinator from `transcribe_cpp::StreamText` and painted by the
/// `live-text` presentation; it never reaches the focused application, history,
/// or the transcript, so partial words can never be typed.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct LiveText {
    pub committed: String,
    pub tentative: String,
}

impl LiveText {
    pub fn is_empty(&self) -> bool {
        self.committed.is_empty() && self.tentative.is_empty()
    }
}

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
        self.try_send_command(Command {
            event,
            live_text: None,
            config: config.clone(),
        })
    }

    fn try_send_command(&self, command: Command) -> bool {
        let sender = self.sender.get_or_init(spawn_actor);
        match sender {
            Some(tx) => tx.send(command).is_ok(),
            None => false,
        }
    }

    /// Shows a live-text snapshot (a streaming model's partial transcript) on
    /// the layer-shell overlay. Like [`try_send`](Self::try_send), returns
    /// `false` when no wlr-layer-shell compositor is available, so the caller
    /// simply drops the partial rather than falling back to a notification
    /// (partial text must never be mirrored into a desktop OSD). A snapshot is
    /// painted only by a configured `live-text` style: with any other style the
    /// snapshot is ignored and the configured style stays on screen, so no live
    /// text appears.
    pub fn try_send_live_text(&self, live_text: LiveText, config: &OverlayConfig) -> bool {
        self.try_send_command(Command {
            event: FeedbackEvent::Recording,
            live_text: Some(live_text),
            config: config.clone(),
        })
    }

    /// Whether the actor has already been probed (whether or not it turned out
    /// to be available). Once true, a send is a non-blocking channel write, so
    /// a caller can send inline instead of paying for an OS thread per call.
    pub(crate) fn is_started(&self) -> bool {
        self.sender.get().is_some()
    }
}

impl Default for OverlayHandle {
    fn default() -> Self {
        Self::new()
    }
}

struct Command {
    event: FeedbackEvent,
    /// A live-text snapshot to paint, or `None` for a plain state event. A
    /// `Some` command is a paint-only update; the carried event (always
    /// `FeedbackEvent::Recording`) is only a placeholder so the actor's
    /// phase/style/config handling is shared.
    live_text: Option<LiveText>,
    config: OverlayConfig,
}

/// Which output(s) the overlay renders on, resolved from `overlay.monitor`.
#[derive(Debug, Clone, PartialEq, Eq)]
enum Target {
    /// Every connected output - the default.
    All,
    /// Only the output whose name matches, when it is connected.
    Named(String),
}

/// Resolves `overlay.monitor` into a [`Target`]. `"all"` (the default), the
/// legacy `"active"`, and an empty value all mean every connected output; any
/// other value names a single output to pin the overlay to. Folding the old
/// `"active"` in means existing installs (whose config still says `"active"`)
/// gain the all-screens behavior instead of silently staying on one screen;
/// a named monitor remains the "just the one" escape hatch.
fn target_for(monitor: &str) -> Target {
    if monitor.is_empty() || monitor == "all" || monitor == "active" {
        Target::All
    } else {
        Target::Named(monitor.to_string())
    }
}

/// Whether `target` wants the overlay on the output with this name (`None` when
/// the compositor has not reported a name yet). `All` wants every output, even
/// before its name is known, so surfaces appear as soon as outputs do; `Named`
/// wants exactly the matching name, so it waits for the name to resolve.
fn target_includes_output(target: &Target, name: Option<&str>) -> bool {
    match target {
        Target::All => true,
        Target::Named(wanted) => name == Some(wanted.as_str()),
    }
}

/// Which connected outputs (by index into `names`, in compositor order) the
/// overlay should render on: every one for [`Target::All`], only the name
/// matches for [`Target::Named`]. An empty result means "no outputs", which the
/// caller turns into a single compositor-chosen fallback surface only for
/// [`Target::All`] - a named target that is disconnected stays dark rather than
/// leaking onto another screen.
fn selected_output_indices(target: &Target, names: &[Option<&str>]) -> Vec<usize> {
    match target {
        Target::All => (0..names.len()).collect(),
        Target::Named(_) => names
            .iter()
            .enumerate()
            .filter(|(_, name)| target_includes_output(target, **name))
            .map(|(index, _)| index)
            .collect(),
    }
}

/// The config-derived inputs that fix a layer surface's geometry and placement:
/// whether it spans the whole output, its requested size, and the anchor
/// position and target outputs.
#[derive(Debug, PartialEq, Eq)]
struct LayerSpec {
    fullscreen: bool,
    size: (u32, u32),
    position: String,
    target: Target,
}

fn layer_spec(config: &OverlayConfig, style: Style) -> LayerSpec {
    LayerSpec {
        fullscreen: is_fullscreen_style(style),
        size: surface_size_for(style),
        position: config.position.clone(),
        target: target_for(&config.monitor),
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
        surfaces: Vec::new(),
        phase: None,
        deadline: None,
        anim_start: Instant::now(),
        live_waveform: false,
        style: Style::Badge,
        // Overwritten by the first `handle_command`; surfaces are not created
        // until then, so these are never used to build a surface beforehand.
        position: "top-right".to_string(),
        target: Target::All,
        layer_spec: None,
        ticking: false,
        ticking_animated: false,
        timer_generation: 0,
        levels: LevelHistory::new(),
        live_text: None,
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
    Copied,
    ClipboardFallback,
    SubmitFailed,
    Cancelled,
    Error,
    ConfigError,
}

impl Phase {
    fn from_event(event: FeedbackEvent) -> Self {
        match event {
            FeedbackEvent::Recording => Phase::Recording,
            FeedbackEvent::Processing => Phase::Transcribing,
            FeedbackEvent::Success => Phase::Success,
            FeedbackEvent::NoSpeech => Phase::NoSpeech,
            FeedbackEvent::Copied => Phase::Copied,
            FeedbackEvent::ClipboardFallback => Phase::ClipboardFallback,
            FeedbackEvent::SubmitFailed => Phase::SubmitFailed,
            FeedbackEvent::Truncated => Phase::Error,
            FeedbackEvent::Cancelled => Phase::Cancelled,
            FeedbackEvent::Error => Phase::Error,
            FeedbackEvent::ConfigError => Phase::ConfigError,
        }
    }

    fn dwell(self) -> Option<Duration> {
        match self {
            Phase::Recording | Phase::Transcribing => None,
            Phase::Success => Some(SUCCESS_DWELL),
            Phase::NoSpeech => Some(NO_SPEECH_DWELL),
            Phase::Copied => Some(COPIED_DWELL),
            Phase::ClipboardFallback => Some(CLIPBOARD_FALLBACK_DWELL),
            Phase::SubmitFailed => Some(SUBMIT_FAILED_DWELL),
            Phase::Cancelled => Some(CANCELLED_DWELL),
            Phase::Error | Phase::ConfigError => Some(ERROR_DWELL),
        }
    }

    fn is_animated(self) -> bool {
        matches!(self, Phase::Recording | Phase::Transcribing)
    }
}

/// One created layer surface and the output it belongs to. `output` is the
/// output the surface is bound to, or `None` for the compositor-chosen surface
/// used as a fallback when no outputs are enumerated yet.
struct Surface {
    output: Option<wl_output::WlOutput>,
    layer: LayerSurface,
    width: u32,
    height: u32,
    /// The size most recently requested from the compositor (the `set_size`
    /// value), so a content-driven caption resize can tell when the request
    /// already matches and skip a redundant reconfigure. `(0, 0)` for a
    /// full-screen stretched style.
    requested: (u32, u32),
    configured: bool,
}

struct State {
    registry_state: RegistryState,
    output_state: OutputState,
    compositor: CompositorState,
    layer_shell: LayerShell,
    shm: Shm,
    qh: QueueHandle<State>,
    pool: SlotPool,
    surfaces: Vec<Surface>,
    phase: Option<Phase>,
    deadline: Option<Instant>,
    anim_start: Instant,
    live_waveform: bool,
    style: Style,
    position: String,
    target: Target,
    layer_spec: Option<LayerSpec>,
    ticking: bool,
    ticking_animated: bool,
    timer_generation: u64,
    /// Live microphone levels sampled once per tick while recording (newest
    /// last) - what the recording animations react to.
    levels: LevelHistory,
    /// The most recent live-text snapshot from a streaming model. It is read
    /// only by a configured `live-text` style; it is cleared by the start of a
    /// fresh recording and kept through the terminal phases so the caption can
    /// hold the last words as a status cue, so a finished dictation never
    /// leaves stale partial words on screen for the next one.
    live_text: Option<LiveText>,
    loop_signal: LoopSignal,
}

impl State {
    fn handle_command(&mut self, command: Command, loop_handle: &LoopHandle<'static, State>) {
        let paint_only = command.live_text.is_some();
        // A live-text snapshot is preview paint, not a state transition: it
        // must not advance the phase, restart the animation clock, or reset the
        // dwell deadline. It only paints while a recording is already on
        // screen, so a partial that lands after a terminal event (a cancel, a
        // stop) is dropped instead of resurrecting Recording - with a `None`
        // deadline - and stranding the overlay on stale words.
        if paint_only {
            if self.phase != Some(Phase::Recording) {
                return;
            }
            self.live_text = command.live_text;
        } else {
            let phase = Phase::from_event(command.event);
            let previous_phase = self.phase;
            self.phase = Some(phase);
            self.deadline = phase.dwell().map(|dwell| Instant::now() + dwell);
            self.anim_start = Instant::now();
            // The live-text snapshot is kept through the terminal phases so the
            // caption can hold the last words as a status cue; it is cleared
            // only when a fresh recording starts, so stale words never leak
            // into the next dictation.
            if phase == Phase::Recording {
                self.live_text = None;
            }
            if phase == Phase::Recording && previous_phase != Some(Phase::Recording) {
                // A new dictation starts a fresh waveform; the previous
                // recording's voice must never scroll into the new one.
                self.levels.clear();
            }
        }
        self.live_waveform = command.config.live_waveform;
        self.style = resolved_style(&command.config.style);
        self.position = command.config.position.clone();
        self.target = target_for(&command.config.monitor);

        let spec = layer_spec(&command.config, self.style);
        if self.layer_spec.as_ref() != Some(&spec) {
            // Geometry/placement/style/target changed: every existing surface
            // was built with the old spec, so tear them down and rebuild for
            // the newly resolved target set.
            self.surfaces.clear();
            self.layer_spec = Some(spec);
        }
        // Create surfaces for any desired output that lacks one (first event,
        // or a hotplug) and drop any no longer desired.
        self.reconcile_surfaces();
        // Grow/shrink the content-sized caption pill in place when the words
        // changed its measured width, without recreating the layer.
        self.sync_layer_size();
        // A no-op for not-yet-`configure`d surfaces; the pending `configure`
        // handler redraws each with this (already-updated) phase once it lands.
        self.redraw_all();

        self.arm_timer(loop_handle);
    }

    /// Brings the live surface set in line with the resolved [`Target`] and the
    /// outputs the compositor currently reports: creates a surface for every
    /// desired output that lacks one, and drops surfaces whose output is no
    /// longer desired. Called on every command and whenever an output is
    /// added or updated.
    fn reconcile_surfaces(&mut self) {
        let target = self.target.clone();
        let outputs: Vec<wl_output::WlOutput> = self.output_state.outputs().collect();
        let names: Vec<Option<String>> = outputs
            .iter()
            .map(|output| self.output_state.info(output).and_then(|info| info.name))
            .collect();
        let name_refs: Vec<Option<&str>> = names.iter().map(|name| name.as_deref()).collect();
        let desired: Vec<Option<wl_output::WlOutput>> =
            if target == Target::All && outputs.is_empty() {
                // No outputs enumerated (yet): fall back to a single
                // compositor-chosen surface so feedback still shows.
                vec![None]
            } else {
                selected_output_indices(&target, &name_refs)
                    .into_iter()
                    .map(|index| Some(outputs[index].clone()))
                    .collect()
            };

        // Preserve surfaces whose output is still desired (so an unchanged set
        // is never torn down and recreated), then create the missing ones.
        self.surfaces
            .retain(|surface| desired.iter().any(|output| output == &surface.output));
        for output in desired {
            if self.surfaces.iter().any(|surface| surface.output == output) {
                continue;
            }
            self.create_surface(output);
        }
    }

    /// Builds one layer surface bound to `output` (`None` for the
    /// compositor-chosen fallback output) using the current style/position.
    fn create_surface(&mut self, output: Option<wl_output::WlOutput>) {
        let surface = self.compositor.create_surface(&self.qh);
        let layer = self.layer_shell.create_layer_surface(
            &self.qh,
            surface,
            Layer::Overlay,
            Some("tonguetyped-overlay"),
            output.as_ref(),
        );
        let (width, height) = self.desired_layer_size(self.style);
        let fullscreen = is_fullscreen_style(self.style);
        let top_docked = is_top_docked_style(self.style);
        // A full-screen style spans every edge with no margin and a zero
        // requested size, letting the compositor stretch it to the output; a
        // top-docked style (the semicircle) sits flush against the top edge at
        // top-centre; a badge style sits at the configured corner/edge with a
        // fixed size.
        layer.set_anchor(if fullscreen {
            Anchor::TOP | Anchor::BOTTOM | Anchor::LEFT | Anchor::RIGHT
        } else if top_docked {
            Anchor::TOP
        } else {
            anchor_for(&self.position)
        });
        let margin = if fullscreen || top_docked { 0 } else { MARGIN };
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
        self.surfaces.push(Surface {
            output,
            layer,
            width,
            height,
            requested: if fullscreen { (0, 0) } else { (width, height) },
            configured: false,
        });
    }

    /// The size to request for `style` given the current content. Every style
    /// uses its fixed representative size except the caption styles, which are
    /// content-measured (see [`caption_surface_size`]): the single-line pill and
    /// the scrolling box both hug their text, and every caption size is
    /// quantised so the layer is not resized for each frame of a streamed
    /// partial. A full-screen style requests `(0, 0)` so the compositor
    /// stretches it to the output.
    fn desired_layer_size(&self, style: Style) -> (u32, u32) {
        if is_fullscreen_style(style) {
            return (0, 0);
        }
        if is_caption_style(style) {
            return caption_surface_size(style, self.phase, self.live_text.as_ref());
        }
        surface_size_for(style)
    }

    /// Resizes any caption surface whose requested size no longer matches the
    /// current content, in place: `set_size` + `commit` asks the compositor for
    /// a fresh configure without tearing the layer down, so the pill grows and
    /// shrinks as words arrive for the cost of a reconfigure, never a surface
    /// recreate. A no-op for the fixed-size styles and when the size is
    /// unchanged.
    fn sync_layer_size(&mut self) {
        let desired = self.desired_layer_size(self.style);
        for surface in &mut self.surfaces {
            if surface.requested == desired {
                continue;
            }
            surface.layer.set_size(desired.0, desired.1);
            surface.requested = desired;
            surface.layer.commit();
        }
    }

    /// Reconciles after a compositor-reported output change (hotplug). A no-op
    /// before the first event: surfaces are created lazily, so an idle actor
    /// with nothing to draw stays at zero weight until the first command
    /// reconciles it.
    fn on_outputs_changed(&mut self) {
        if self.phase.is_none() && self.surfaces.is_empty() {
            return;
        }
        self.reconcile_surfaces();
        self.redraw_all();
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
                if phase == Phase::Recording {
                    // Sample the live microphone once per tick: the waveform
                    // scrolls with the tick cadence, and a slow compositor
                    // drops ticks rather than queueing audio data.
                    self.levels.push(crate::audio::latest_level());
                }
                self.redraw_all();
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
                    self.redraw_all();
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

    fn redraw_all(&mut self) {
        for index in 0..self.surfaces.len() {
            self.redraw_surface(index);
        }
    }

    fn redraw_surface(&mut self, index: usize) {
        let Some((layer, width, height, configured)) = self.surfaces.get(index).map(|surface| {
            (
                surface.layer.clone(),
                surface.width,
                surface.height,
                surface.configured,
            )
        }) else {
            return;
        };
        // Attaching a buffer before the first `configure` is a protocol
        // error; `on_tick`'s animated-phase branch can otherwise race this
        // (e.g. the compositor takes longer than one `TICK` to send it).
        if !configured {
            return;
        }
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
            self.live_waveform,
            self.style,
            OverlayFrame {
                levels: &self.levels,
                live_text: self.live_text.as_ref(),
            },
        );

        layer
            .wl_surface()
            .damage_buffer(0, 0, width as i32, height as i32);
        let _ = buffer.attach_to(layer.wl_surface());
        layer.commit();
    }
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
/// requested "pulsating glowy blob, plasma-like" look, `Border` as a
/// captain-requested phase-coloured screen-edge glow that is strongest in the
/// corners, `HalfCircle` as a captain-requested glowing, pulsing
/// semicircle resting on the top edge, and `LongPill` as a captain-requested
/// wide dark bar with a squiggly live waveform and a round status button,
/// separately from the small `Pill`. The `live-text`, `live-text-minimal`, and
/// `live-text-box` values are the streaming caption family: a dark glass card
/// showing the live partial words from a streaming model (see [`LiveText`]),
/// with the listening pad before the first word. They are the only styles that
/// paint live text, and they are chosen explicitly rather than adopted when a
/// streaming model is active.
pub(crate) const STYLE_VALUES: [&str; 10] = [
    "badge",
    "minimal",
    "pill",
    "blob",
    "border",
    "half-circle",
    "long-pill",
    "live-text",
    "live-text-minimal",
    "live-text-box",
];

/// Display labels matching `STYLE_VALUES` position for position.
pub(crate) const STYLE_LABELS: [&str; 10] = [
    "Badge",
    "Minimal",
    "Pill",
    "Blob",
    "Border",
    "Half Circle",
    "Long Pill",
    "Live Text (glass)",
    "Live Text (minimal)",
    "Live Text (box)",
];

/// Display labels for `OverlayConfig::live_waveform`: the plain pulsing dot
/// first, the live multi-bar waveform second. Every non-caption style's
/// recording animation reacts to the microphone either way - the waveform
/// styles follow the live level sample by sample, the simple-pulse styles
/// follow its envelope - so this only picks the busier glyph, not whether the
/// overlay is voice-reactive. The live-text caption styles never draw a
/// waveform and ignore this toggle.
pub(crate) const WAVEFORM_LABELS: [&str; 2] = ["Simple pulse", "Live microphone waveform"];

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

/// The `overlay.live_waveform` look for `Phase::Recording`: a small
/// multi-bar waveform in place of the single pulsing dot. The bars come from
/// [`LevelHistory`], which the actor fills from the recording path's published
/// microphone level (see [`crate::audio::publish_level`]), so the shape follows
/// the voice: loud speech reaches up the available height, quiet stretches sit
/// as tiny dots, and the newest sample is the right-most bar. Shared by every
/// `Style` - each passes its own available width/height/color so the same
/// motion reads correctly whether it sits inside a round badge or a wide pill.
fn paint_live_waveform(
    canvas: &mut Canvas,
    cx: f32,
    cy: f32,
    available_width: f32,
    max_bar_height: f32,
    rgb: (u8, u8, u8),
    levels: &LevelHistory,
) {
    /// Horizontal distance between bar centres; determines how many thin bars
    /// fit the available width at a given style size.
    const BAR_PITCH: f32 = 3.0;
    /// Fraction of the pitch the bar itself fills, leaving a gap between bars.
    const BAR_WIDTH_FRACTION: f32 = 0.55;

    let count = ((available_width / BAR_PITCH).round() as usize).clamp(6, LEVEL_HISTORY);
    let pitch = available_width / count as f32;
    let bar_width = (pitch * BAR_WIDTH_FRACTION).max(1.0);
    let mut x = cx - available_width / 2.0 + pitch / 2.0;
    for i in 0..count {
        let level = levels.waveform_sample(count, i);
        let height = (max_bar_height * bar_height_fraction(level)).max(bar_width);
        fill_centered_bar(canvas, x, cy, bar_width, height, rgb);
        x += pitch;
    }
}

/// Maps a raw microphone RMS level to the 0..1 reactivity fraction the
/// recording animations use: 0 at silence (about -50 dB and below, where
/// room noise lives), 1 at full scale. The slight gamma lift keeps quiet
/// speech visible without letting idle noise animate the overlay.
fn level_fraction(level: f32) -> f32 {
    let ratio = crate::audio::level_to_ratio(level) as f32;
    ((ratio - 0.15) / 0.85).clamp(0.0, 1.0).powf(0.7)
}

/// Maps a raw microphone RMS level to a waveform bar's height fraction in
/// `MIN_BAR_FRACTION..=1.0`: silence still leaves a small dot (the reference's
/// quiet moments), and louder speech grows toward the full bar height.
fn bar_height_fraction(level: f32) -> f32 {
    const MIN_BAR_FRACTION: f32 = 0.10;
    MIN_BAR_FRACTION + (1.0 - MIN_BAR_FRACTION) * level_fraction(level)
}

/// A short rolling history of microphone level samples (raw block RMS), newest
/// at the end. The overlay actor pushes one sample per animation tick while
/// recording, and paints a scrolling window of it with the newest bar at the
/// right. The capacity bounds memory and the visible window length.
struct LevelHistory {
    samples: [f32; LEVEL_HISTORY],
    len: usize,
}

impl LevelHistory {
    const fn new() -> Self {
        Self {
            samples: [0.0; LEVEL_HISTORY],
            len: 0,
        }
    }

    /// Forgets all samples - used when a new dictation starts, so the waveform
    /// never shows the previous recording's voice.
    fn clear(&mut self) {
        self.len = 0;
    }

    fn push(&mut self, level: f32) {
        if self.len == LEVEL_HISTORY {
            self.samples.copy_within(1.., 0);
            self.len -= 1;
        }
        self.samples[self.len] = level;
        self.len += 1;
    }

    /// The level for waveform bar `i` of `count`, ordering bars oldest
    /// (`i = 0`, left) to newest (`i = count - 1`, right). Missing older
    /// samples read as silence, so a fresh history starts with flat dots on
    /// the left while the live audio fills in from the right.
    fn waveform_sample(&self, count: usize, i: usize) -> f32 {
        let pad = count.saturating_sub(self.len);
        if i < pad {
            return 0.0;
        }
        let start = self.len.saturating_sub(count);
        self.samples[start + (i - pad)]
    }

    /// Peak of the newest `window` samples: the envelope the non-waveform
    /// recording pulses follow, so a brief quiet moment inside speech does not
    /// collapse the animation.
    fn recent_peak(&self, window: usize) -> f32 {
        let start = self.len.saturating_sub(window);
        self.samples[start..self.len]
            .iter()
            .copied()
            .fold(0.0, f32::max)
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
        // A hotplugged monitor gains its own surface (when the target is all).
        self.on_outputs_changed();
    }

    fn update_output(
        &mut self,
        _conn: &Connection,
        _qh: &QueueHandle<Self>,
        _output: wl_output::WlOutput,
    ) {
        // An output's name becomes known here; a `Named` target may now match.
        self.on_outputs_changed();
    }

    fn output_destroyed(
        &mut self,
        _conn: &Connection,
        _qh: &QueueHandle<Self>,
        output: wl_output::WlOutput,
    ) {
        // smithay-client-toolkit calls this before removing the output from
        // `OutputState`, so `output_state.outputs()` still lists it here;
        // reconciling would bind a fresh surface to a wl_output the compositor
        // is destroying. Removal only drops surfaces - creation on an output
        // change is handled by `new_output`/`update_output`, which see a
        // consistent output list.
        self.surfaces
            .retain(|surface| surface.output.as_ref() != Some(&output));
    }
}

impl LayerShellHandler for State {
    fn closed(&mut self, _conn: &Connection, _qh: &QueueHandle<Self>, layer: &LayerSurface) {
        // One surface was closed (e.g. its output went away). Drop just that
        // one and keep the actor alive for the remaining outputs; stopping the
        // event loop here, as the single-surface version did, would kill
        // feedback on every other screen. A replacement is created on the next
        // reconcile when the output reappears.
        self.surfaces.retain(|surface| &surface.layer != layer);
    }

    fn configure(
        &mut self,
        _conn: &Connection,
        _qh: &QueueHandle<Self>,
        layer: &LayerSurface,
        configure: LayerSurfaceConfigure,
        _serial: u32,
    ) {
        let Some(index) = self
            .surfaces
            .iter()
            .position(|surface| &surface.layer == layer)
        else {
            return;
        };
        let (width, height) = configure.new_size;
        // A full-screen style's real size arrives here (the compositor fills in
        // the stretched dimensions); fall back only if a compositor reports a
        // zero dimension. The caption styles fall back to their content-measured
        // size, the rest to their fixed representative size.
        let fallback = if is_fullscreen_style(self.style) {
            surface_size_for(self.style)
        } else {
            self.desired_layer_size(self.style)
        };
        let (width, height) = (
            if width == 0 { fallback.0 } else { width },
            if height == 0 { fallback.1 } else { height },
        );
        {
            let surface = &mut self.surfaces[index];
            surface.width = width;
            surface.height = height;
            surface.configured = true;
        }
        self.redraw_surface(index);
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
    /// Source-over blends `rgb` at `coverage` opacity into the canvas.
    ///
    /// The buffer is the layer surface's wl_shm `Argb8888`, whose alpha is
    /// **premultiplied** (the Wayland contract), so the stored colour is
    /// `rgb * coverage` plus whatever was already there scaled by its own
    /// alpha. Getting this wrong is very visible: a straight-alpha fade is
    /// read by the compositor as an opaque, full-brightness colour wherever
    /// alpha > 0, so glows stop fading and pale tints read as solid white.
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
        // Both terms are already premultiplied: the source contributes its
        // colour scaled by its coverage, the destination what it already holds
        // scaled by how much of it survives.
        let mix = |src: u8, dst: u8| -> u8 {
            let src = src as f32 / 255.0 * coverage;
            let dst = dst as f32 / 255.0 * (1.0 - coverage);
            ((src + dst) * 255.0).round().clamp(0.0, 255.0) as u8
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
/// rather than stroked. `opacity` scales the fill's overall coverage (1.0 is a
/// solid shape, less makes it faintly translucent) while leaving the edge
/// anti-aliasing shape unchanged.
fn fill_capsule(
    canvas: &mut Canvas,
    cx: f32,
    cy: f32,
    half_width: f32,
    half_height: f32,
    rgb: (u8, u8, u8),
    opacity: f32,
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
            let coverage = (radius + 0.5 - dist).clamp(0.0, 1.0) * opacity;
            canvas.blend(x, y, rgb, coverage);
        }
    }
}

/// The outline of the `Style::Blob` silhouette at `angle`, time `t`, moved by
/// the live voice level `lf`. The calm slow breath and a faint organic sway are
/// always present, so silence is a soft, round, gently breathing orb; the voice
/// makes the body swell slightly and sprouts sharp radial spikes - the louder
/// the level, the longer the spikes (see `blob_spikes`). The amplitudes are
/// bounded so the silhouette plus its halo always fades out before the surface
/// edge (see `paint_blob` and `surface_size_for`): at full level the edge
/// reaches `radius * 1.61`.
fn blob_edge(radius: f32, angle: f32, t: f32, lf: f32) -> f32 {
    use std::f32::consts::TAU;
    let breathe = 0.05 * (t * TAU).sin();
    let sway = 0.02 * (2.0 * angle - t * TAU).sin() + 0.01 * (3.0 * angle + t * TAU).sin();
    radius * (1.0 + 0.10 * lf) * (1.0 + breathe + sway) + radius * lf * blob_spikes(angle, t)
}

/// The blob's spiky voice response at `angle` and time `t`, in units of the
/// blob's radius: two counter-rotating sets of narrow, high-power lobes, so
/// voice peaks become sharp spikes radiating from the body rather than a
/// smooth lumpy wobble. Both sets rotate on integer multiples of the breath
/// cycle (keeping the silhouette periodic across the breath wrap), and the
/// caller scales the whole response by the live level so silence leaves a
/// round ball.
fn blob_spikes(angle: f32, t: f32) -> f32 {
    use std::f32::consts::TAU;
    let lobes = |count: f32, phase: f32, spin: f32, power: i32| -> f32 {
        let wave = 0.5 + 0.5 * (count * angle + phase + t * TAU * spin).cos();
        wave.powi(power)
    };
    // 5 slow lobes plus 8 faster counter-rotating ones: the two sets rarely
    // peak together, so the corona breathes unevenly like a rough flame.
    0.26 * lobes(5.0, 0.0, 1.0, 20) + 0.16 * lobes(8.0, 1.7, -2.0, 24)
}

/// Blends `color` toward white by `amount` (0..1) - used for the blob's bright
/// inner glow and its lighter sheen.
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

/// Fills a vertical bar of `width` centered on `cx` and `cy`, extending
/// `height` in both directions - the building block for the live waveform's
/// equalizer-style bars (see `paint_live_waveform`).
fn fill_centered_bar(
    canvas: &mut Canvas,
    cx: f32,
    cy: f32,
    width: f32,
    height: f32,
    rgb: (u8, u8, u8),
) {
    let min_x = (cx - width / 2.0).round().max(0.0) as i32;
    let max_x = (cx + width / 2.0).round().min(canvas.width as f32) as i32;
    let min_y = (cy - height / 2.0).round().max(0.0) as i32;
    let max_y = (cy + height / 2.0).round().min(canvas.height as f32) as i32;
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
/// a soft liquid-metal sheen, `Border` is a thin phase-coloured glowing
/// line hugging the whole screen edge that fades smoothly inward, brightest in
/// the corners, `HalfCircle` is a glowing, pulsing phase-coloured
/// semicircle resting flat on the top edge of the screen at top-centre, and
/// `LongPill` is a wide dark bar holding one continuous squiggly waveform line
/// plus a round phase status button at its right end (the captain's reference
/// look). Every style reskins all six phases consistently, per the design
/// review.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Style {
    Badge,
    Minimal,
    Pill,
    Blob,
    Border,
    HalfCircle,
    LongPill,
    /// The streaming caption, glass variant (the default): a content-sized dark
    /// glass card holding a leading phase chip and one line of the live partial
    /// transcript, or the listening pad until words arrive. It is shown only
    /// when the user picks it, and it is the only family that paints live text.
    LiveText,
    /// The streaming caption, minimal variant: the same single-line card with a
    /// single leading phase dot instead of the full chip.
    LiveTextMinimal,
    /// The streaming caption, box variant: the same content sizing but a
    /// wrapping card that grows a line per wrapped row and scrolls internally
    /// past its cap, for long passages.
    LiveTextBox,
}

/// The style the overlay actually paints: exactly the configured style, always.
/// The active model never switches it, and neither does an incoming live-text
/// snapshot - live text is painted only when the user has chosen one of the
/// `live-text` caption styles, and any other style (for example `blob`) stays
/// on screen and paints no live text even for a streaming model.
fn resolved_style(configured: &str) -> Style {
    style_for(configured)
}

/// The caption variant used by the offline preview renderer: the named caption
/// variant if the caller picked one, else the glass default.
fn caption_variant(configured: Style) -> Style {
    match configured {
        Style::LiveTextMinimal => Style::LiveTextMinimal,
        Style::LiveTextBox => Style::LiveTextBox,
        _ => Style::LiveText,
    }
}

/// Whether `style` is one of the `live-text` caption variants.
fn is_caption_style(style: Style) -> bool {
    matches!(
        style,
        Style::LiveText | Style::LiveTextMinimal | Style::LiveTextBox
    )
}

/// The vertical centre of the scrolling box's leading chip row (and so the row
/// its empty listening pad shares). Both the chip and the pad are measured from
/// the box's top padding, independent of the box's content-sized height.
fn boxed_chip_center() -> f32 {
    LIVE_TEXT_PADDING + LIVE_TEXT_CHIP_RADIUS
}

/// The top of the box's first text row: below the leading chip row plus the
/// gap between them. `paint_live_text` and [`boxed_surface_height`] share this
/// so the drawn lines and the measured height can never drift.
fn boxed_text_top() -> f32 {
    LIVE_TEXT_PADDING + LIVE_TEXT_CHIP_RADIUS * 2.0 + LIVE_TEXT_BOX_CHIP_GAP
}

/// The wrap measure for the box's text, matching `paint_live_text`'s
/// `right - LIVE_TEXT_PADDING`.
fn boxed_text_max_width() -> f32 {
    (LIVE_TEXT_BOX_SURFACE.0 as f32 - 2.0 * LIVE_TEXT_PADDING).max(40.0)
}

/// Rounds a measured pixel size up to the next [`LIVE_TEXT_SIZE_QUANTUM`]
/// boundary, so a change too small to cross a step does not resize the layer.
fn quantize_size(value: f32) -> u32 {
    let step = LIVE_TEXT_SIZE_QUANTUM.max(1) as f32;
    ((value / step).ceil() * step) as u32
}

/// The single-line caption's content height: the capsule hugging its phase
/// chip (the tallest element on the line) plus [`LIVE_TEXT_VPADDING`] and the
/// background inset on each side, quantised like the width.
fn single_line_surface_height() -> u32 {
    let capsule = LIVE_TEXT_CHIP_RADIUS * 2.0 + LIVE_TEXT_VPADDING * 2.0;
    quantize_size(capsule + LIVE_TEXT_INSET * 2.0)
}

/// The scrolling box's content height: the leading chip row plus one row per
/// wrapped line and the bottom padding, so the card hugs its text with no dead
/// band below the last line. The empty (listening) state is just the chip row;
/// a terminal status message counts as its single drawn line; the wrap is
/// capped at [`text::CAPTION_MAX_LINES`], so the box stops growing and scrolls
/// past that cap. Clamped to [`LIVE_TEXT_BOX_MIN_HEIGHT`] and the surface
/// ceiling.
fn boxed_surface_height(phase: Option<Phase>, live_text: Option<&LiveText>) -> u32 {
    let captured = live_text.filter(|text| !text.is_empty());
    let lines = match captured {
        Some(text) => {
            text::caption_lines(&text.committed, &text.tentative, boxed_text_max_width()).len()
        }
        // No words yet: only a terminal phase draws a short status line below
        // the chip row; every other phase (the listening pad, the transcribing
        // spinner, idle) shares the chip row and draws nothing more.
        None => usize::from(phase.and_then(phase_message).is_some()),
    };
    let content = if lines == 0 {
        LIVE_TEXT_PADDING + LIVE_TEXT_CHIP_RADIUS * 2.0 + LIVE_TEXT_PADDING
    } else {
        boxed_text_top() + lines as f32 * text::CAPTION_LINE_HEIGHT + LIVE_TEXT_PADDING
    };
    quantize_size(content).clamp(LIVE_TEXT_BOX_MIN_HEIGHT, LIVE_TEXT_BOX_SURFACE.1)
}

/// The surface size for a caption style. Both families are content-measured so
/// the card hugs its text: the box grows one line-height at a time up to its
/// ceiling, then scrolls; the single-line variants hug their chip and line. The
/// single-line width mirrors the layout `paint_live_text` draws: the leading
/// cue, then the current line (frozen plus tentative words, a caret while words
/// are arriving) or the listening pad or the terminal status message, then the
/// right padding. With no phase (idle) it falls back to the empty-state width.
fn caption_surface_size(
    style: Style,
    phase: Option<Phase>,
    live_text: Option<&LiveText>,
) -> (u32, u32) {
    if style == Style::LiveTextBox {
        return (
            LIVE_TEXT_BOX_SURFACE.0,
            boxed_surface_height(phase, live_text),
        );
    }
    let minimal = style == Style::LiveTextMinimal;
    let captured = live_text.filter(|text| !text.is_empty());
    let recording = phase == Some(Phase::Recording);
    // The text/pad origin, matching paint_live_text's single-line layout.
    let origin = if minimal {
        if captured.is_some() || !recording {
            LIVE_TEXT_PADDING + 7.0 + 16.0
        } else {
            LIVE_TEXT_PADDING + 6.0
        }
    } else {
        LIVE_TEXT_PADDING + LIVE_TEXT_CHIP_RADIUS * 2.0 + LIVE_TEXT_CHIP_GAP
    };
    let content = match captured {
        Some(text) => {
            let line = text::caption_line(&text.committed, &text.tentative);
            let caret = if matches!(phase, Some(Phase::Recording) | Some(Phase::Transcribing)) {
                4.0 + LIVE_TEXT_CARET_WIDTH
            } else {
                0.0
            };
            text::line_width(&line) + caret
        }
        None if recording => {
            let pitch = LIVE_TEXT_LISTEN_RADIUS * 2.0 + LIVE_TEXT_LISTEN_GAP;
            6.0 + LIVE_TEXT_LISTEN_RADIUS * 2.0 + pitch * 2.0
        }
        None => phase
            .and_then(phase_message)
            .map(|message| text::line_width(&text::caption_line(message, "")))
            .unwrap_or(0.0),
    };
    let width = quantize_size(origin + content + LIVE_TEXT_PADDING)
        .clamp(LIVE_TEXT_MIN_WIDTH, LIVE_TEXT_SURFACE.0);
    (width, single_line_surface_height())
}

/// Parses `OverlayConfig::style`, falling back to `Badge` for an unrecognized
/// value - the same tolerant-fallback shape `anchor_for` uses for `position`.
fn style_for(name: &str) -> Style {
    match name {
        "minimal" => Style::Minimal,
        "pill" => Style::Pill,
        "blob" => Style::Blob,
        "border" => Style::Border,
        "half-circle" => Style::HalfCircle,
        "long-pill" => Style::LongPill,
        "live-text" => Style::LiveText,
        "live-text-minimal" => Style::LiveTextMinimal,
        "live-text-box" => Style::LiveTextBox,
        _ => Style::Badge,
    }
}

/// Whether a style owns the whole output rather than a small anchored badge.
/// Only `Border` does: it is a full-screen `zwlr_layer_shell_v1` surface whose
/// glow traces the screen edges, so `create_surface` anchors it to every edge
/// with no margin and a zero requested size (the compositor stretches it to the
/// output), and `configure` supplies its real dimensions.
fn is_fullscreen_style(style: Style) -> bool {
    matches!(style, Style::Border)
}

/// Whether a style is docked to a single screen edge rather than placed at the
/// configured `overlay.position`. Only `HalfCircle` is: a semicircle resting on
/// the top edge only reads correctly at top-centre, so `create_surface` anchors it
/// to the top edge (which centres it horizontally) with no margin, and it
/// ignores `overlay.position` the same way `Border` does.
fn is_top_docked_style(style: Style) -> bool {
    matches!(style, Style::HalfCircle)
}

/// The layer-shell surface size to request for a style - `Pill` widens to hold
/// its capsule shape and waveform, `LongPill` is the reference's wide dark bar,
/// `Blob` enlarges to give its glow room to fade out before the surface edge,
/// and `HalfCircle` is wider than tall so its dome and glow have room to fade.
/// For the full-screen `Border` style this is only the representative size used
/// by the offline preview and the tests; `create_surface` requests a stretched
/// zero-sized surface instead.
fn surface_size_for(style: Style) -> (u32, u32) {
    match style {
        Style::Badge | Style::Minimal => (BADGE, BADGE),
        Style::Pill => (BADGE * 5 / 4, BADGE * 5 / 8),
        Style::Blob => (BLOB_SURFACE, BLOB_SURFACE),
        Style::Border => BORDER_PREVIEW,
        Style::HalfCircle => HALF_CIRCLE_SURFACE,
        Style::LongPill => LONG_PILL_SURFACE,
        Style::LiveText | Style::LiveTextMinimal => LIVE_TEXT_SURFACE,
        Style::LiveTextBox => LIVE_TEXT_BOX_SURFACE,
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
    fill_capsule(canvas, cx, cy - s * 0.35, s * 0.34, s * 0.5, rgb, 1.0);
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

/// A clipboard - the glyph for a transcript left on the clipboard, by choice
/// (`Copied`) or because typing failed (`ClipboardFallback`; the phase color
/// tells the two apart). `s` is the glyph's half-height, matching
/// `glyph_muted_mic`.
fn glyph_clipboard(
    canvas: &mut Canvas,
    cx: f32,
    cy: f32,
    s: f32,
    thickness: f32,
    rgb: (u8, u8, u8),
) {
    // The board: an outlined rectangle whose round line caps soften the
    // corners.
    let (left, right) = (cx - s * 0.7, cx + s * 0.7);
    let (top, bottom) = (cy - s * 0.6, cy + s * 0.95);
    stroke_line(canvas, left, top, left, bottom, thickness, rgb);
    stroke_line(canvas, right, top, right, bottom, thickness, rgb);
    stroke_line(canvas, left, bottom, right, bottom, thickness, rgb);
    stroke_line(canvas, left, top, right, top, thickness, rgb);
    // The clip: a solid tab standing up out of the board's top edge, the
    // part that makes the shape read as a clipboard rather than a page.
    fill_capsule(canvas, cx, top - s * 0.12, s * 0.36, s * 0.26, rgb, 1.0);
    // Two lines of "text" on the board.
    let line = thickness * 0.8;
    stroke_line(
        canvas,
        cx - s * 0.32,
        cy + s * 0.05,
        cx + s * 0.32,
        cy + s * 0.05,
        line,
        rgb,
    );
    stroke_line(
        canvas,
        cx - s * 0.32,
        cy + s * 0.5,
        cx + s * 0.32,
        cy + s * 0.5,
        line,
        rgb,
    );
}

fn phase_color(phase: Phase) -> (u8, u8, u8) {
    match phase {
        Phase::Recording => RECORDING_COLOR,
        Phase::Transcribing => TRANSCRIBING_COLOR,
        Phase::Success => SUCCESS_COLOR,
        Phase::NoSpeech => NO_SPEECH_COLOR,
        Phase::Copied => COPIED_COLOR,
        Phase::ClipboardFallback => CLIPBOARD_FALLBACK_COLOR,
        Phase::SubmitFailed => SUBMIT_FAILED_COLOR,
        Phase::Cancelled => CANCELLED_COLOR,
        Phase::Error | Phase::ConfigError => ERROR_COLOR,
    }
}

/// The per-frame inputs a style draws from, grouped so `paint` and the style
/// functions keep a readable argument list: the live microphone level history
/// and the most recent live-text snapshot (only the `live-text` presentation
/// reads the latter).
struct OverlayFrame<'a> {
    levels: &'a LevelHistory,
    live_text: Option<&'a LiveText>,
}

fn paint(
    canvas: &mut Canvas,
    phase: Option<Phase>,
    t: f32,
    breath: f32,
    live_waveform: bool,
    style: Style,
    frame: OverlayFrame<'_>,
) {
    let OverlayFrame { levels, live_text } = frame;
    canvas.pixels.fill(0);
    let Some(phase) = phase else { return };
    match style {
        Style::Badge => paint_badge(canvas, phase, t, live_waveform, levels),
        Style::Minimal => paint_minimal(canvas, phase, t, live_waveform, levels),
        Style::Pill => paint_pill(canvas, phase, t, live_waveform, levels),
        // The blob carries its reaction in its own body, never a bar
        // waveform, so it ignores `live_waveform`.
        Style::Blob => paint_blob(canvas, phase, t, breath, levels),
        Style::Border => paint_border(canvas, phase, t, live_waveform, levels),
        Style::HalfCircle => paint_half_circle(canvas, phase, t, live_waveform, levels),
        Style::LongPill => paint_long_pill(canvas, phase, t, levels),
        Style::LiveText | Style::LiveTextMinimal | Style::LiveTextBox => {
            paint_live_text(canvas, phase, t, live_text, style)
        }
    }
}

/// The 0..1 level envelope the non-waveform recording animations follow: the
/// newest samples' peak, so the pulse/breath tracks the voice but does not
/// flicker on every quiet intra-word gap. `LevelHistory` is only cleared when a
/// new recording starts, so after recording ends the last envelope keeps
/// driving the blob's spikes and the border/half-circle glow through the
/// terminal phases. That is deliberate (the captain's correction on review):
/// the voice-driven animation must stay active and visible while the overlay is
/// up, never collapse to a flat, blank-looking shape.
fn recording_level(levels: &LevelHistory) -> f32 {
    level_fraction(levels.recent_peak(5))
}

fn paint_badge(
    canvas: &mut Canvas,
    phase: Phase,
    t: f32,
    live_waveform: bool,
    levels: &LevelHistory,
) {
    let cx = canvas.width as f32 / 2.0;
    let cy = canvas.height as f32 / 2.0;
    let disc_radius = canvas.width.min(canvas.height) as f32 * 0.42;
    let color = phase_color(phase);
    let lf = recording_level(levels);

    match phase {
        Phase::Recording if live_waveform => {
            fill_circle(canvas, cx, cy, disc_radius, color);
            // The bars are centred in the disc; the bounding box's half
            // diagonal (~0.81 * disc_radius) stays inside the circle.
            paint_live_waveform(
                canvas,
                cx,
                cy,
                disc_radius * 1.2,
                disc_radius * 1.1,
                GLYPH_COLOR,
                levels,
            );
        }
        Phase::Recording => {
            fill_circle(canvas, cx, cy, disc_radius, color);
            let pulse = 0.5 + 0.5 * (t * std::f32::consts::TAU).sin();
            // The inner dot's size follows the voice: a small calm marker at
            // silence, the original pulsing dot at full speech level.
            fill_circle(
                canvas,
                cx,
                cy,
                disc_radius * (0.22 + 0.20 * lf * (0.8 + 0.2 * pulse)),
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
        Phase::Success | Phase::SubmitFailed => {
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
        Phase::Copied | Phase::ClipboardFallback => {
            fill_circle(canvas, cx, cy, disc_radius, color);
            glyph_clipboard(
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
        Phase::Error | Phase::ConfigError => {
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
fn paint_minimal(
    canvas: &mut Canvas,
    phase: Phase,
    t: f32,
    live_waveform: bool,
    levels: &LevelHistory,
) {
    let cx = canvas.width as f32 / 2.0;
    let cy = canvas.height as f32 / 2.0;
    let ring_radius = canvas.width.min(canvas.height) as f32 * 0.34;
    let ring_thickness = ring_radius * 0.14;
    let color = phase_color(phase);
    let lf = recording_level(levels);
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
        Phase::Recording if live_waveform => {
            paint_live_waveform(
                canvas,
                cx,
                cy,
                ring_radius * 1.5,
                ring_radius * 0.9,
                color,
                levels,
            );
        }
        Phase::Recording => {
            let pulse = 0.5 + 0.5 * (t * std::f32::consts::TAU).sin();
            fill_circle(
                canvas,
                cx,
                cy,
                ring_radius * (0.20 + 0.20 * lf * (0.8 + 0.2 * pulse)),
                color,
            );
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
        Phase::Success | Phase::SubmitFailed => {
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
        Phase::Copied | Phase::ClipboardFallback => {
            glyph_clipboard(
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
        Phase::Error | Phase::ConfigError => {
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
/// the extra width gives the live waveform more room to read.
fn paint_pill(
    canvas: &mut Canvas,
    phase: Phase,
    t: f32,
    live_waveform: bool,
    levels: &LevelHistory,
) {
    let cx = canvas.width as f32 / 2.0;
    let cy = canvas.height as f32 / 2.0;
    let half_width = canvas.width as f32 * 0.46;
    let half_height = canvas.height as f32 * 0.42;
    let color = phase_color(phase);
    let lf = recording_level(levels);
    fill_capsule(canvas, cx, cy, half_width, half_height, color, 1.0);

    match phase {
        Phase::Recording if live_waveform => {
            paint_live_waveform(
                canvas,
                cx,
                cy,
                half_width * 1.5,
                half_height * 1.5,
                GLYPH_COLOR,
                levels,
            );
        }
        Phase::Recording => {
            let pulse = 0.5 + 0.5 * (t * std::f32::consts::TAU).sin();
            fill_circle(
                canvas,
                cx,
                cy,
                half_height * (0.28 + 0.26 * lf * (0.8 + 0.2 * pulse)),
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
        Phase::Success | Phase::SubmitFailed => {
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
        Phase::Copied | Phase::ClipboardFallback => {
            glyph_clipboard(
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
        Phase::Error | Phase::ConfigError => {
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

/// A wide dark bar holding one continuous squiggly waveform line plus a round
/// phase status button at its right end - the `LongPill` style, built to the
/// captain's reference (see the design record at
/// `data/tt-pill-overlay-live-microphone-waveform-re-23/design-brief.md`): the
/// bar is a dark glass capsule sharing the caption card's vertical gradient and
/// faint translucency; the phase colour lives in the button, not the bar. The
/// trace is a smooth light-grey line that rolls with the live `LevelHistory`,
/// and the button is the phase colour made vivid with `bolden` (red with a
/// white dot while recording, green with a checkmark after success, and the
/// matching glyph for every other phase). The trace ignores `live_waveform`: it
/// is this style's whole point.
fn paint_long_pill(canvas: &mut Canvas, phase: Phase, t: f32, levels: &LevelHistory) {
    let width = canvas.width as f32;
    let height = canvas.height as f32;
    let cy = height / 2.0;
    // The dark glass bar, refreshed to match the caption card: a vertical
    // gradient capsule (the captain asked the long pill to share the caption's
    // finish while keeping its waveform).
    fill_rounded_rect(
        canvas,
        width / 2.0,
        cy,
        width / 2.0,
        height / 2.0,
        height / 2.0,
        LIVE_TEXT_BACKGROUND_TOP,
        LIVE_TEXT_BACKGROUND_BOTTOM,
        LIVE_TEXT_BACKGROUND_OPACITY,
    );

    // Button geometry from the reference: a circle 0.752 of the bar height,
    // its right edge 0.14 of the height in from the bar's right edge.
    let button_radius = height * 0.376;
    let button_cx = width - height * 0.516;
    let button_color = bolden(phase_color(phase));
    fill_circle(canvas, button_cx, cy, button_radius, button_color);
    match phase {
        Phase::Recording => {
            // The reference's solid white dot, half the button's diameter.
            fill_circle(canvas, button_cx, cy, button_radius * 0.50, GLYPH_COLOR);
        }
        Phase::Transcribing => {
            let angle = t * std::f32::consts::TAU;
            stroke_arc(
                canvas,
                button_cx,
                cy,
                button_radius * 0.58,
                button_radius * 0.22,
                (angle, std::f32::consts::PI * 1.2),
                GLYPH_COLOR,
            );
        }
        Phase::Success | Phase::SubmitFailed => {
            glyph_checkmark(
                canvas,
                button_cx,
                cy,
                button_radius * 0.62,
                button_radius * 0.24,
                GLYPH_COLOR,
            );
        }
        Phase::NoSpeech => {
            glyph_muted_mic(
                canvas,
                button_cx,
                cy,
                button_radius * 0.52,
                button_radius * 0.18,
                GLYPH_COLOR,
            );
        }
        Phase::Copied | Phase::ClipboardFallback => {
            glyph_clipboard(
                canvas,
                button_cx,
                cy,
                button_radius * 0.52,
                button_radius * 0.18,
                GLYPH_COLOR,
            );
        }
        Phase::Cancelled => {
            fill_square(canvas, button_cx, cy, button_radius * 0.34, GLYPH_COLOR);
        }
        Phase::Error | Phase::ConfigError => {
            glyph_cross(
                canvas,
                button_cx,
                cy,
                button_radius * 0.62,
                button_radius * 0.24,
                GLYPH_COLOR,
            );
        }
    }

    // The trace: a smooth light-grey line from near the bar's left end to just
    // short of the button, centred on the bar, whose excursions follow the
    // live level (see `long_pill_trace_sample`).
    let line_start = width * 0.039;
    let line_end = button_cx - button_radius - height * 0.21;
    let thickness = height * 0.066;
    let max_excursion = height * 0.27;
    let envelope = long_pill_envelope(levels);
    let span = (line_end - line_start).max(1.0);
    let mut previous: Option<(f32, f32)> = None;
    let mut x = line_start;
    while x <= line_end {
        let position = (x - line_start) / span * (LEVEL_HISTORY - 1) as f32;
        let i0 = position.floor() as usize;
        let i1 = (i0 + 1).min(LEVEL_HISTORY - 1);
        let frac = position - i0 as f32;
        let a = long_pill_trace_sample(envelope[i0], i0);
        let b = long_pill_trace_sample(envelope[i1], i1);
        let value = a + (b - a) * frac;
        let point = (x, cy - value * max_excursion);
        if let Some(previous) = previous {
            stroke_line(
                canvas,
                previous.0,
                previous.1,
                point.0,
                point.1,
                thickness,
                LONG_PILL_LINE,
            );
        }
        previous = Some(point);
        x += 2.0;
    }
}

/// The `live-text` streaming caption, painted only when one of the `live-text`
/// styles is configured. Partial text is painted here and nowhere else - it is
/// never typed, persisted, or shown in the fallback notification chain. The
/// card is a dark glass panel with a leading phase chip (or a bare dot in the
/// minimal variant), one line of the frozen `committed` prefix in near-white
/// and the still-revisable `tentative` tail dimmed, and a soft caret after the
/// newest word. `style` selects the variant (glass, minimal, or the scrolling
/// box). Its empty recording state is always the listening pad - the caption
/// never draws a waveform, so the two never appear together.
fn paint_live_text(
    canvas: &mut Canvas,
    phase: Phase,
    t: f32,
    live_text: Option<&LiveText>,
    style: Style,
) {
    let width = canvas.width as f32;
    let height = canvas.height as f32;
    let cx = width / 2.0;
    let cy = height / 2.0;
    let boxed = style == Style::LiveTextBox;
    let minimal = style == Style::LiveTextMinimal;
    let half_width = width / 2.0 - LIVE_TEXT_INSET;
    let half_height = height / 2.0 - LIVE_TEXT_INSET;
    let radius = if boxed { 26.0 } else { half_height };
    fill_rounded_rect(
        canvas,
        cx,
        cy,
        half_width,
        half_height,
        radius,
        LIVE_TEXT_BACKGROUND_TOP,
        LIVE_TEXT_BACKGROUND_BOTTOM,
        LIVE_TEXT_BACKGROUND_OPACITY,
    );

    // The single-line cards centre on the surface; the box stacks from the top
    // (see `boxed_text_top`) and returns before the caret, so it never reads
    // `line_center`/`y_top`.
    let line_center = cy;
    let y_top = line_center - text::CAPTION_FONT_SIZE * 0.62;

    let right = width - LIVE_TEXT_PADDING;
    let captured = live_text.filter(|text| !text.is_empty());

    // The phase cue: a round chip (glass/box) or a bare dot (minimal). The chip
    // is always painted so the card reads its phase even before the first word;
    // the minimal variant's bare dot is skipped in the recording-empty state,
    // where the listening pad is already the cue and a second dot would just
    // read as a stray extra dot.
    let text_x = if boxed {
        let chip_cx = LIVE_TEXT_PADDING + LIVE_TEXT_CHIP_RADIUS;
        draw_caption_chip(
            canvas,
            chip_cx,
            boxed_chip_center(),
            LIVE_TEXT_CHIP_RADIUS,
            phase,
            t,
        );
        LIVE_TEXT_PADDING
    } else if minimal {
        if captured.is_some() || !matches!(phase, Phase::Recording) {
            let dot_cx = LIVE_TEXT_PADDING + 7.0;
            fill_circle(canvas, dot_cx, cy, 7.0, bolden(phase_color(phase)));
            dot_cx + 16.0
        } else {
            LIVE_TEXT_PADDING + 6.0
        }
    } else {
        let chip_cx = LIVE_TEXT_PADDING + LIVE_TEXT_CHIP_RADIUS;
        draw_caption_chip(canvas, chip_cx, cy, LIVE_TEXT_CHIP_RADIUS, phase, t);
        chip_cx + LIVE_TEXT_CHIP_RADIUS + LIVE_TEXT_CHIP_GAP
    };

    if let Some(live_text) = captured {
        let colors = text::CaptionColors {
            committed: LIVE_TEXT_COMMITTED,
            tentative: LIVE_TEXT_TENTATIVE,
            alpha: 1.0,
        };
        if boxed {
            text::draw_caption(
                canvas,
                LIVE_TEXT_PADDING,
                boxed_text_top(),
                boxed_text_max_width(),
                live_text,
                colors,
            );
            return;
        }
        let line = text::caption_line(&live_text.committed, &live_text.tentative);
        let line_w = text::line_width(&line);
        let available = (right - text_x).max(40.0);
        // Single line: when the words overrun, the newest stay in frame and the
        // old dissolve off the left as the line slides left (the marquee).
        let (x, clip) = if line_w <= available {
            (text_x, None)
        } else {
            (right - line_w, Some(text_x))
        };
        text::draw_line(canvas, x, y_top, &line, &colors, clip);
        // A soft caret after the newest word, only while words are arriving.
        if matches!(phase, Phase::Recording | Phase::Transcribing) {
            let caret_x = (x + line_w + 4.0).min(right - LIVE_TEXT_CARET_WIDTH);
            if caret_x >= text_x {
                let pulse = 0.5 + 0.5 * (0.5 + 0.5 * (t * std::f32::consts::TAU).sin());
                let color = mix_color(LIVE_TEXT_BACKGROUND_BOTTOM, LIVE_TEXT_CARET, pulse);
                fill_centered_bar(
                    canvas,
                    caret_x,
                    line_center + 2.0,
                    LIVE_TEXT_CARET_WIDTH,
                    LIVE_TEXT_CARET_HEIGHT,
                    color,
                );
            }
        }
        return;
    }

    // No words yet.
    match phase {
        Phase::Recording => {
            // The listening pad: the caption is live, waiting for the first
            // word. It never falls back to the waveform.
            let (pad_x, pad_y) = if boxed {
                (
                    LIVE_TEXT_PADDING + LIVE_TEXT_CHIP_RADIUS * 2.0 + 22.0,
                    boxed_chip_center(),
                )
            } else {
                (text_x + 6.0, cy)
            };
            paint_listening_pad(canvas, pad_x, pad_y, t, bolden(phase_color(phase)));
        }
        _ => {
            // A terminal phase with no captured words: a short status message.
            if let Some(message) = phase_message(phase) {
                let colors = text::CaptionColors {
                    committed: LIVE_TEXT_COMMITTED,
                    tentative: LIVE_TEXT_COMMITTED,
                    alpha: 0.9,
                };
                let line = text::caption_line(message, "");
                let message_y = if boxed { boxed_text_top() } else { y_top };
                text::draw_line(canvas, text_x, message_y, &line, &colors, None);
            }
        }
    }
}

/// The round phase chip at the caption's leading edge: a saturated phase disc
/// with the same white state glyph the other styles use, so recording,
/// transcribing, success, no-speech, clipboard, cancellation, and error all read
/// from the chip.
fn draw_caption_chip(canvas: &mut Canvas, cx: f32, cy: f32, radius: f32, phase: Phase, t: f32) {
    fill_circle(canvas, cx, cy, radius, bolden(phase_color(phase)));
    let glyph = GLYPH_COLOR;
    match phase {
        Phase::Recording => fill_circle(canvas, cx, cy, radius * 0.36, glyph),
        Phase::Transcribing => {
            let angle = t * std::f32::consts::TAU;
            stroke_arc(
                canvas,
                cx,
                cy,
                radius * 0.55,
                radius * 0.22,
                (angle, std::f32::consts::PI * 1.2),
                glyph,
            );
        }
        Phase::Success | Phase::SubmitFailed => {
            glyph_checkmark(canvas, cx, cy, radius * 0.5, radius * 0.2, glyph)
        }
        Phase::NoSpeech => glyph_muted_mic(canvas, cx, cy, radius * 0.5, radius * 0.16, glyph),
        Phase::Copied | Phase::ClipboardFallback => {
            glyph_clipboard(canvas, cx, cy, radius * 0.48, radius * 0.16, glyph)
        }
        Phase::Cancelled => fill_square(canvas, cx, cy, radius * 0.3, glyph),
        Phase::Error | Phase::ConfigError => {
            glyph_cross(canvas, cx, cy, radius * 0.5, radius * 0.2, glyph)
        }
    }
}

/// Three phase-coloured dots breathing in sequence - the caption's empty-state
/// "listening" cue before the first word, in place of the waveform.
fn paint_listening_pad(canvas: &mut Canvas, x: f32, cy: f32, t: f32, color: (u8, u8, u8)) {
    let pitch = LIVE_TEXT_LISTEN_RADIUS * 2.0 + LIVE_TEXT_LISTEN_GAP;
    for index in 0..3 {
        let wave = 0.5 + 0.5 * (t * std::f32::consts::TAU - index as f32 * 0.9).sin();
        let radius = LIVE_TEXT_LISTEN_RADIUS * (0.8 + 0.3 * wave);
        let cx = x + LIVE_TEXT_LISTEN_RADIUS + index as f32 * pitch;
        fill_circle(canvas, cx, cy, radius, color);
    }
}

/// A short status label for a terminal phase shown by a caption style that has
/// no captured words.
fn phase_message(phase: Phase) -> Option<&'static str> {
    match phase {
        Phase::Success => Some("Ready"),
        Phase::NoSpeech => Some("No speech detected"),
        Phase::Copied => Some("Copied to clipboard"),
        Phase::ClipboardFallback => Some("Typing failed - on clipboard"),
        Phase::SubmitFailed => Some("Text entered, but Enter failed"),
        Phase::Cancelled => Some("Cancelled"),
        Phase::Error => Some("Transcription failed"),
        Phase::ConfigError => Some("Config edit rejected - run tonguetyped status"),
        _ => None,
    }
}

/// Fills a rounded rectangle (a stadium when `radius` is its half-height) with
/// a vertical gradient from `top` to `bottom`, anti-aliased at the edge and
/// scaled by `opacity`. Used for the caption's dark glass cards and the
/// refreshed long pill.
#[allow(clippy::too_many_arguments)]
fn fill_rounded_rect(
    canvas: &mut Canvas,
    cx: f32,
    cy: f32,
    half_width: f32,
    half_height: f32,
    radius: f32,
    top: (u8, u8, u8),
    bottom: (u8, u8, u8),
    opacity: f32,
) {
    let radius = radius.min(half_width).min(half_height);
    let min_x = (cx - half_width - 1.0).floor().max(0.0) as i32;
    let max_x = (cx + half_width + 1.0).ceil().min(canvas.width as f32) as i32;
    let min_y = (cy - half_height - 1.0).floor().max(0.0) as i32;
    let max_y = (cy + half_height + 1.0).ceil().min(canvas.height as f32) as i32;
    for y in min_y..max_y {
        for x in min_x..max_x {
            let px = x as f32 + 0.5 - cx;
            let py = y as f32 + 0.5 - cy;
            let qx = px.abs() - (half_width - radius);
            let qy = py.abs() - (half_height - radius);
            let outside =
                (qx.max(0.0).powi(2) + qy.max(0.0).powi(2)).sqrt() + qx.max(qy).min(0.0) - radius;
            let coverage = (0.5 - outside).clamp(0.0, 1.0) * opacity;
            if coverage <= 0.0 {
                continue;
            }
            let gradient = ((py + half_height) / (2.0 * half_height)).clamp(0.0, 1.0);
            canvas.blend(x, y, mix_color(top, bottom, gradient), coverage);
        }
    }
}

/// The long pill's trace envelope: the live level's size for each of the
/// `LEVEL_HISTORY` positions, averaged over a short sliding window. The
/// smoothing is what makes the wave *uniform* - a raw per-sample size jitters
/// with every audio block and leaves a jagged line, while the reference's
/// squiggle swells and falls evenly with the voice.
fn long_pill_envelope(levels: &LevelHistory) -> [f32; LEVEL_HISTORY] {
    /// Positions either side of the centre that the average reaches.
    const SMOOTHING: usize = 4;
    let mut envelope = [0.0f32; LEVEL_HISTORY];
    for (i, value) in envelope.iter_mut().enumerate() {
        let start = i.saturating_sub(SMOOTHING);
        let end = (i + SMOOTHING).min(LEVEL_HISTORY - 1);
        let sum: f32 = (start..=end)
            .map(|j| level_fraction(levels.waveform_sample(LEVEL_HISTORY, j)))
            .sum();
        *value = sum / (end - start + 1) as f32;
    }
    envelope
}

/// The long pill's signed trace excursion in `-1.0..=1.0` for position `i` at
/// size `envelope` (see `long_pill_envelope`, oldest left, newest right): the
/// level's size on a slow visual carrier, so the trace rises and falls around
/// the centre line like a real waveform while its overall size follows the
/// voice. The small floor keeps silence a gently wavering line rather than a
/// dead straight edge.
fn long_pill_trace_sample(envelope: f32, i: usize) -> f32 {
    use std::f32::consts::TAU;
    /// Samples per visual wave: the reference's squiggle has a handful of
    /// tight, smooth bumps across the bar.
    const WAVE_PERIOD_SAMPLES: f32 = 12.0;
    const FLOOR: f32 = 0.05;
    let size = FLOOR + (1.0 - FLOOR) * envelope;
    size * (i as f32 / WAVE_PERIOD_SAMPLES * TAU).sin()
}

/// A bright, glowing blob in the phase color that slowly breathes - the `Blob`
/// style. The silhouette gently inhales and exhales on its own slower cycle
/// (`breath`, derived from `BLOB_PULSE_PERIOD`), with a soft metallic sheen (a
/// gentle dome highlight and a bright rim) keeping it feeling like liquid metal
/// rather than a flat disc. The live voice level (`levels`) moves the body
/// itself - voice peaks sprout sharp spikes from the body and swell it
/// slightly, while silence settles back into a soft, round, slowly breathing
/// ball (no bar waveform is drawn inside it; see `blob_edge`/`blob_spikes` and
/// the captain's review in
/// `data/tt-pill-overlay-live-microphone-waveform-re-23/design-brief.md`).
/// The halo brightens with the voice too. The `Transcribing` spinner glyph
/// advances on the faster fraction `t` independently, so the silhouette keeps
/// breathing slowly while the spinner turns. A white glyph carries the state
/// for the terminal phases; `t` and `breath` are both 0 there, so the
/// silhouette stops animating, but it keeps the last recording's level, so a
/// loud dictation freezes as a spiky blob rather than settling back to a calm,
/// round ball.
fn paint_blob(canvas: &mut Canvas, phase: Phase, t: f32, breath: f32, levels: &LevelHistory) {
    let cx = canvas.width as f32 / 2.0;
    let cy = canvas.height as f32 / 2.0;
    let radius = canvas.width.min(canvas.height) as f32 * 0.26;
    let color = phase_color(phase);
    let lf = recording_level(levels);
    // A bold, saturated body in the phase hue - the same `bolden` finish the
    // other overlay styles use - with a light inner glow and a pulled-in
    // sheen, so the blob reads as a big bright sphere rather than a pale,
    // silvered one. The dome shading and the rim are unchanged.
    let body = scale_color(bolden(color), 0.85);
    let core = lighten(bolden(color), 0.4);
    let sheen = lighten(bolden(color), 0.6);

    // Light and its half-vector (view direction is +z), for the specular dot.
    let (lx, ly, lz) = normalize3(-0.45, -0.68, 0.58);
    let (hx, hy, hz) = normalize3(lx, ly, lz + 1.0);
    let halo_width = radius * 0.18;
    // The halo is dim and steady at silence and flares with the voice.
    let halo_strength = 0.08 + 0.14 * lf;

    let outer = radius * 1.7 + halo_width + 2.0;
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
            let edge = blob_edge(radius, angle, breath, lf);

            if dist >= edge {
                // A soft glowing halo hugging the silhouette, so the blob
                // still reads as a luminous plasma on a dark desktop. It is
                // the phase hue fading by opacity only - never a pale,
                // whitened tint (the captain's review: the red should just
                // fade opacity).
                let into_halo = dist - edge;
                if into_halo < halo_width {
                    let strength = 1.0 - into_halo / halo_width;
                    canvas.blend(x, y, color, halo_strength * strength * strength);
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
        Phase::Success | Phase::SubmitFailed => {
            glyph_checkmark(canvas, cx, cy, radius * 0.42, radius * 0.16, GLYPH_COLOR);
        }
        Phase::NoSpeech => {
            glyph_muted_mic(canvas, cx, cy, radius * 0.36, radius * 0.13, GLYPH_COLOR);
        }
        Phase::Copied | Phase::ClipboardFallback => {
            glyph_clipboard(canvas, cx, cy, radius * 0.36, radius * 0.13, GLYPH_COLOR);
        }
        Phase::Cancelled => {
            fill_square(canvas, cx, cy, radius * 0.3, GLYPH_COLOR);
        }
        Phase::Error | Phase::ConfigError => {
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
/// fraction, with the breath's amplitude following the live voice level
/// (`levels`), so the frame pulses with speech and settles calm in silence;
/// with `live_waveform` a bright highlight also sweeps once around the frame per
/// cycle, standing in for the "actively capturing" signal the other styles show
/// as a waveform (see `OverlayConfig::live_waveform`).
fn paint_border(
    canvas: &mut Canvas,
    phase: Phase,
    t: f32,
    live_waveform: bool,
    levels: &LevelHistory,
) {
    use std::f32::consts::{PI, TAU};
    let width = canvas.width as f32;
    let height = canvas.height as f32;
    let (cx, cy) = (width / 2.0, height / 2.0);
    // Bolder, brighter hues than the base phase colour (the captain's ask).
    // The fade is alpha-only: the glow keeps this hue and only loses opacity
    // (the captain's review: red must fade by opacity, never blend toward a
    // pale or greenish white).
    let color = bolden(phase_color(phase));
    let lf = recording_level(levels);
    let carrier = 0.5 + 0.5 * (t * TAU).sin();
    let pulse = 0.5 + 0.5 * carrier * (0.3 + 0.7 * lf);
    let head = t * TAU - PI;
    // The sweep is a recording-only "actively capturing" cue, matching every
    // other style's `Phase::Recording if live_waveform` branch; the voice
    // level scales its brightness but never turns it off.
    let sweeping = live_waveform && matches!(phase, Phase::Recording);
    let sweep_strength = 0.85 * (0.5 + 0.5 * lf);
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
                intensity = (intensity + sweep * sweep * sweep_strength).min(1.0);
            }
            if intensity <= 0.0 {
                continue;
            }
            canvas.blend(x as i32, y as i32, color, intensity);
        }
    }
}

/// The `HalfCircle` style's per-pixel glow intensity in `0.0..=1.0` at radial
/// distance `r` from the dome's centre (the flat edge's midpoint, on the top
/// edge). Inside `radius` it is a soft solid fill, brightest at the core and
/// dimming toward the curved edge; outside, it dissolves smoothly to zero by
/// `radius + glow` with a `smoothstep` that starts flat, so the falloff has no
/// hard band edge. Because it depends only on `r`, the shape and its glow are
/// mirror-symmetric about the vertical centre line.
fn half_circle_profile(r: f32, radius: f32, glow: f32) -> f32 {
    if r <= radius {
        1.0 - HALF_CIRCLE_FILL_FALLOFF * (r / radius).powi(2)
    } else {
        let into = ((r - radius) / glow).clamp(0.0, 1.0);
        (1.0 - HALF_CIRCLE_FILL_FALLOFF) * (1.0 - smoothstep(into))
    }
}

/// A glowing, pulsing phase-coloured semicircle resting flat on the top edge of
/// the screen at top-centre - the `HalfCircle` style. The dome's flat side is
/// the screen's top edge and its centre is that edge's midpoint, so it reads as
/// a half-sun rising from the top bezel. It is drawn through the same per-pixel
/// alpha-blending path every other style uses: a soft solid fill over the
/// shared phase colour dissolving into a smooth radial glow. The whole dome
/// breathes on the elapsed-time fraction, with the breath's amplitude following
/// the live voice level (`levels`), so the dome pulses with speech and settles
/// calm in silence; with `live_waveform` a bright highlight additionally sweeps
/// along the arc, standing in for the "actively capturing" signal the badge
/// styles show as a waveform (see `OverlayConfig::live_waveform`).
fn paint_half_circle(
    canvas: &mut Canvas,
    phase: Phase,
    t: f32,
    live_waveform: bool,
    levels: &LevelHistory,
) {
    use std::f32::consts::{PI, TAU};
    let width = canvas.width as f32;
    let height = canvas.height as f32;
    let cx = width / 2.0;
    let radius = height * HALF_CIRCLE_RADIUS_FRACTION;
    let glow = height * HALF_CIRCLE_GLOW_FRACTION;
    let color = phase_color(phase);
    // The dome's fade is alpha-only, same as the border: the phase hue never
    // shifts toward white as it dissolves outward.
    let lf = recording_level(levels);
    let carrier = 0.5 + 0.5 * (t * TAU).sin();
    let pulse = 0.5 + 0.5 * carrier * (0.3 + 0.7 * lf);
    // The sweep is a recording-only "actively capturing" cue, matching every
    // other style's `Phase::Recording if live_waveform` branch; the voice
    // level scales its brightness but never turns it off.
    let sweeping = live_waveform && matches!(phase, Phase::Recording);
    let sweep_strength = 0.7 * (0.5 + 0.5 * lf);
    // The visible semicircle spans the polar angles `0..PI` (screen-space `y`
    // grows downward), so the highlight sweeps that full range across a cycle.
    let head = t * PI;

    for y in 0..canvas.height {
        for x in 0..canvas.width {
            let px = x as f32 + 0.5;
            let py = y as f32 + 0.5;
            let dx = px - cx;
            let r = (dx * dx + py * py).sqrt();
            let mut intensity = half_circle_profile(r, radius, glow) * pulse;
            if intensity <= 0.0 {
                continue;
            }
            if sweeping {
                let delta = (py.atan2(dx) - head).abs();
                let angular = smoothstep((1.0 - delta / HALF_CIRCLE_SWEEP).clamp(0.0, 1.0));
                // Weight the highlight toward the curved edge, so it reads as a
                // bright bead travelling along the arc rather than a hard pie
                // wedge from the dome's centre.
                let radial = (r / radius).clamp(0.0, 1.0).powi(2);
                intensity = (intensity + angular * radial * sweep_strength).min(1.0);
            }
            canvas.blend(x as i32, y as i32, color, intensity);
        }
    }
}

/// Renders one overlay frame into a premultiplied-alpha BGRA8 buffer (wl_shm
/// `Argb8888`, little-endian) - the exact byte layout and alpha convention the
/// layer surface consumes, so offline frames match what the compositor shows.
/// `examples/overlay_style_png.rs` reorders it to PNG's RGBA (and
/// unpremultiplies, since PNG alpha is straight). `style`
/// and `phase` use the same string values the config and feedback events use
/// (`badge`/`minimal`/`pill`/`blob`/`border`/`half-circle`/`long-pill`/
/// `live-text`/`live-text-minimal`/`live-text-box`;
/// `recording`/`transcribing`/`success`/`no-speech`/`copied`/
/// `clipboard-fallback`/`submit-failed`/`cancelled`/`error`), and
/// `None` clears to fully transparent. `levels` is the live microphone history
/// the recording animations follow, oldest first - an empty slice means
/// silence, and a representative pattern is how the offline mockups show the
/// speaking look.
/// Returns `(width, height, pixels)` with `width * height * 4` bytes. The
/// content-sized caption styles return the size their content measures; for the
/// full-screen `border` style this is the representative `BORDER_PREVIEW` size,
/// not a compositor-provided output size.
#[doc(hidden)]
pub fn render_frame_pixels(
    style: &str,
    phase: Option<&str>,
    t: f32,
    live_waveform: bool,
    levels: &[f32],
) -> (u32, u32, Vec<u8>) {
    let style = style_for(style);
    let phase = phase.map(|name| match name {
        "transcribing" => Phase::Transcribing,
        "success" => Phase::Success,
        "no-speech" => Phase::NoSpeech,
        "copied" => Phase::Copied,
        "clipboard-fallback" => Phase::ClipboardFallback,
        "submit-failed" => Phase::SubmitFailed,
        "cancelled" => Phase::Cancelled,
        "error" => Phase::Error,
        _ => Phase::Recording,
    });
    let mut history = LevelHistory::new();
    for level in levels {
        history.push(*level);
    }
    // The caption families are content-sized (an empty caption or a short
    // phrase renders the pill/box it actually needs); the fixed styles use
    // their representative size.
    let (width, height) = if is_caption_style(style) {
        caption_surface_size(style, phase, None)
    } else {
        surface_size_for(style)
    };
    let mut pixels = vec![0u8; (width * height * 4) as usize];
    {
        let mut canvas = Canvas {
            pixels: &mut pixels,
            width,
            height,
        };
        paint(
            &mut canvas,
            phase,
            t,
            t,
            live_waveform,
            style,
            OverlayFrame {
                levels: &history,
                live_text: None,
            },
        );
    }
    (width, height, pixels)
}

/// Offline renderer for a `live-text` caption variant: paints the card with
/// `committed`/`tentative` text (or its listening pad when both are empty) into
/// a pixel buffer, for the design-preview example and the headless layout
/// tests. `style` names any caption variant (`live-text`, `live-text-minimal`,
/// `live-text-box`); an unknown name falls back to the glass variant. `levels`
/// is accepted for parity with `render_frame_pixels`, but the caption never
/// draws a waveform, so it does not affect the frame.
pub fn render_live_text_pixels(
    style: &str,
    committed: &str,
    tentative: &str,
    levels: &[f32],
) -> (u32, u32, Vec<u8>) {
    let style = caption_variant(style_for(style));
    let mut history = LevelHistory::new();
    for level in levels {
        history.push(*level);
    }
    let live_text = LiveText {
        committed: committed.to_string(),
        tentative: tentative.to_string(),
    };
    let (width, height) = caption_surface_size(style, Some(Phase::Recording), Some(&live_text));
    let mut pixels = vec![0u8; (width * height * 4) as usize];
    {
        let mut canvas = Canvas {
            pixels: &mut pixels,
            width,
            height,
        };
        paint(
            &mut canvas,
            Some(Phase::Recording),
            0.0,
            0.0,
            true,
            style,
            OverlayFrame {
                levels: &history,
                live_text: Some(&live_text),
            },
        );
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

        // The anchor (position) and the target outputs (monitor) are baked into
        // the surface at creation too, so each must move the spec.
        let mut other_position = base.clone();
        other_position.position = "bottom-left".into();
        assert_ne!(base_spec, layer_spec(&other_position, Style::Badge));

        let mut other_monitor = base.clone();
        other_monitor.monitor = "HDMI-A-1".into();
        assert_ne!(base_spec, layer_spec(&other_monitor, Style::Badge));

        // Folding the legacy "active" into the all-screens default means it
        // yields the same spec as the new "all" default.
        let mut legacy_active = base.clone();
        legacy_active.monitor = "active".into();
        assert_eq!(base_spec, layer_spec(&legacy_active, Style::Badge));

        // An identical config must not force a recreation.
        assert_eq!(base_spec, layer_spec(&base.clone(), Style::Badge));
    }

    #[test]
    fn target_for_folds_all_active_and_empty_into_every_output() {
        assert_eq!(target_for("all"), Target::All);
        assert_eq!(target_for("active"), Target::All);
        assert_eq!(target_for(""), Target::All);
        assert_eq!(target_for("HDMI-A-1"), Target::Named("HDMI-A-1".into()));
    }

    #[test]
    fn target_includes_output_selects_the_right_outputs() {
        // "all" wants every output, even before its name is known.
        assert!(target_includes_output(&Target::All, Some("DP-1")));
        assert!(target_includes_output(&Target::All, None));

        // A named target matches only its own output, and waits (excludes)
        // while the name is still unknown.
        let named = Target::Named("DP-1".into());
        assert!(target_includes_output(&named, Some("DP-1")));
        assert!(!target_includes_output(&named, Some("HDMI-A-1")));
        assert!(!target_includes_output(&named, None));
    }

    #[test]
    fn selected_output_indices_covers_all_or_only_the_named_match() {
        // `All` picks every connected output, in order, including any whose
        // name the compositor has not reported yet.
        let names = [Some("DP-1"), Some("HDMI-A-1"), None];
        assert_eq!(selected_output_indices(&Target::All, &names), vec![0, 1, 2]);

        // A named target picks exactly its match, and nothing when it is not
        // connected (so it stays dark instead of leaking onto another screen).
        assert_eq!(
            selected_output_indices(&Target::Named("HDMI-A-1".into()), &names),
            vec![1]
        );
        assert_eq!(
            selected_output_indices(&Target::Named("DP-9".into()), &names),
            Vec::<usize>::new()
        );

        // No outputs selected and no outputs connected both yield no indices;
        // the `All` caller substitutes its single fallback surface, a named
        // caller does not.
        assert_eq!(
            selected_output_indices(&Target::All, &[]),
            Vec::<usize>::new()
        );
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
    fn canvas_blend_stores_premultiplied_alpha() {
        // wl_shm `Argb8888` is premultiplied: a half-opacity red pixel must be
        // stored as roughly (112, 25, 25) at alpha 128, never as full-strength
        // (224, 49, 49). Storing the straight colour is exactly what made the
        // compositor show full-brightness glows instead of fades.
        let mut pixels = vec![0u8; 4];
        let mut canvas = Canvas {
            pixels: &mut pixels,
            width: 1,
            height: 1,
        };
        canvas.blend(0, 0, (224, 49, 49), 0.5);
        assert_eq!(pixels[3], 128, "alpha should be 0.5");
        assert!(
            (pixels[2] as i32 - 112).abs() <= 1,
            "premultiplied red should be ~112, got {}",
            pixels[2]
        );
        assert!(
            (pixels[1] as i32 - 25).abs() <= 1,
            "premultiplied green should be ~25, got {}",
            pixels[1]
        );
        assert!(
            (pixels[0] as i32 - 25).abs() <= 1,
            "premultiplied blue should be ~25, got {}",
            pixels[0]
        );

        // The premultiplied invariant holds after any blend, including over an
        // existing translucent pixel: no colour channel may exceed alpha, or
        // the compositor would show a colour that cannot fade out.
        let mut canvas = Canvas {
            pixels: &mut pixels,
            width: 1,
            height: 1,
        };
        canvas.blend(0, 0, (224, 49, 49), 0.25);
        canvas.blend(0, 0, (255, 255, 255), 0.3);
        assert!(
            pixels[0] <= pixels[3] && pixels[1] <= pixels[3] && pixels[2] <= pixels[3],
            "premultiplied channels must never exceed alpha: {pixels:?}"
        );
    }

    #[test]
    fn glows_fade_by_opacity_without_shifting_hue() {
        // Every translucent "glow" pixel keeps the phase hue: for a red
        // recording it stays red-dominant with green and blue together, never
        // a pale/whitened tint. Covers the border glow, the half-circle glow
        // and the blob halo, the three styles with wide soft fades.
        let is_faded_red = |pixel: &[u8]| -> bool {
            let (b, g, r, a) = (pixel[0] as i32, pixel[1] as i32, pixel[2] as i32, pixel[3]);
            a > 0 && a < 250 && r > g + 8 && (g - b).abs() <= 2
        };
        for style in [Style::Border, Style::HalfCircle, Style::Blob] {
            let (width, height) = surface_size_for(style);
            let pixels = paint_to_pixels(Some(Phase::Recording), 0.0, false, style);
            let faded = (0..width * height)
                .map(|i| {
                    let index = (i * 4) as usize;
                    &pixels[index..index + 4]
                })
                .filter(|pixel| is_faded_red(pixel))
                .count();
            assert!(
                faded >= 50,
                "{style:?}: expected a red-dominant opacity fade, only {faded} faded red pixels"
            );
            // ...and no translucent pixel may be a whitened tint (green and
            // blue clearly above the phase colour's level and close to red).
            let whitened = (0..width * height)
                .map(|i| {
                    let index = (i * 4) as usize;
                    &pixels[index..index + 4]
                })
                .filter(|pixel| {
                    let (b, g, r, a) = (
                        pixel[0] as i32,
                        pixel[1] as i32,
                        pixel[2] as i32,
                        pixel[3] as i32,
                    );
                    // whiteness also means the *stored* colour survived at near
                    // full strength while alpha is low: premultiplied channels
                    // must never exceed alpha.
                    a > 0 && (b > a + 4 || g > a + 4 || r > a + 4)
                })
                .count();
            assert_eq!(
                whitened, 0,
                "{style:?}: {whitened} pixels store colour brighter than their alpha"
            );
        }
    }

    const ALL_STYLES: [Style; 10] = [
        Style::Badge,
        Style::Minimal,
        Style::Pill,
        Style::Blob,
        Style::Border,
        Style::HalfCircle,
        Style::LongPill,
        Style::LiveText,
        Style::LiveTextMinimal,
        Style::LiveTextBox,
    ];

    /// A quiet room: a faint steady hiss far below speech, which every style
    /// should render as calm/flat.
    fn quiet_history() -> LevelHistory {
        let mut history = LevelHistory::new();
        for i in 0..LEVEL_HISTORY {
            history.push(0.0015 + 0.0004 * (i as f32 * 0.7).sin());
        }
        history
    }

    /// A voice-like burst pattern with quiet gaps, so the recorded waveform
    /// has tall bars and dots rather than one flat level.
    fn speaking_history() -> LevelHistory {
        let mut history = LevelHistory::new();
        for i in 0..LEVEL_HISTORY {
            let sample = i as f32;
            let in_word = ((sample / 64.0 * 5.0) % 1.0) < 0.78;
            let syllable = 0.35 + 0.65 * (sample * 0.9).sin().abs();
            let volume = 0.04 + 0.14 * (sample * 0.23).cos().abs();
            history.push(if in_word { syllable * volume } else { 0.001 });
        }
        history
    }

    fn paint_to_pixels(phase: Option<Phase>, t: f32, live_waveform: bool, style: Style) -> Vec<u8> {
        paint_to_pixels_with_breath(phase, t, t, live_waveform, style)
    }

    fn paint_to_pixels_with_breath(
        phase: Option<Phase>,
        t: f32,
        breath: f32,
        live_waveform: bool,
        style: Style,
    ) -> Vec<u8> {
        paint_to_pixels_with_levels(phase, t, breath, live_waveform, style, &speaking_history())
    }

    fn paint_to_pixels_with_levels(
        phase: Option<Phase>,
        t: f32,
        breath: f32,
        live_waveform: bool,
        style: Style,
        levels: &LevelHistory,
    ) -> Vec<u8> {
        paint_to_pixels_with_live_text(phase, t, breath, live_waveform, style, levels, None)
    }

    fn paint_to_pixels_with_live_text(
        phase: Option<Phase>,
        t: f32,
        breath: f32,
        live_waveform: bool,
        style: Style,
        levels: &LevelHistory,
        live_text: Option<&LiveText>,
    ) -> Vec<u8> {
        let (width, height) = surface_size_for(style);
        let mut pixels = vec![0u8; (width * height * 4) as usize];
        {
            let mut canvas = Canvas {
                pixels: &mut pixels,
                width,
                height,
            };
            paint(
                &mut canvas,
                phase,
                t,
                breath,
                live_waveform,
                style,
                OverlayFrame { levels, live_text },
            );
        }
        pixels
    }

    /// The long pill's bar as it lands in the premultiplied buffer at row `y`:
    /// its vertical-gradient colour scaled by `LIVE_TEXT_BACKGROUND_OPACITY`
    /// and stored at that same opacity, blended source-over onto the
    /// transparent surface (see `Canvas::blend`). Returned BGRA, matching
    /// `render_frame_pixels`' byte order.
    fn long_pill_bar_at(y: u32) -> [u8; 4] {
        let (_, height) = surface_size_for(Style::LongPill);
        let half_height = height as f32 / 2.0;
        let py = y as f32 + 0.5 - half_height;
        let gradient = ((py + half_height) / (2.0 * half_height)).clamp(0.0, 1.0);
        let color = mix_color(
            LIVE_TEXT_BACKGROUND_TOP,
            LIVE_TEXT_BACKGROUND_BOTTOM,
            gradient,
        );
        let opacity = LIVE_TEXT_BACKGROUND_OPACITY;
        let premultiply =
            |channel: u8| -> u8 { (channel as f32 / 255.0 * opacity * 255.0).round() as u8 };
        let alpha = (opacity * 255.0).round() as u8;
        [
            premultiply(color.2),
            premultiply(color.1),
            premultiply(color.0),
            alpha,
        ]
    }

    #[test]
    fn clipboard_events_map_to_dwelling_static_phases() {
        assert_eq!(Phase::from_event(FeedbackEvent::Copied), Phase::Copied);
        assert_eq!(
            Phase::from_event(FeedbackEvent::ClipboardFallback),
            Phase::ClipboardFallback
        );
        assert_eq!(Phase::Copied.dwell(), Some(COPIED_DWELL));
        assert_eq!(
            Phase::ClipboardFallback.dwell(),
            Some(CLIPBOARD_FALLBACK_DWELL)
        );
        assert!(!Phase::Copied.is_animated());
        assert!(!Phase::ClipboardFallback.is_animated());
    }

    #[test]
    fn no_speech_event_maps_to_a_dwelling_static_phase() {
        assert_eq!(Phase::from_event(FeedbackEvent::NoSpeech), Phase::NoSpeech);
        assert_eq!(Phase::NoSpeech.dwell(), Some(NO_SPEECH_DWELL));
        assert!(!Phase::NoSpeech.is_animated());
    }

    #[test]
    fn submit_failure_maps_to_its_own_dwelling_static_phase() {
        assert_eq!(
            Phase::from_event(FeedbackEvent::SubmitFailed),
            Phase::SubmitFailed
        );
        assert_eq!(Phase::SubmitFailed.dwell(), Some(SUBMIT_FAILED_DWELL));
        assert!(!Phase::SubmitFailed.is_animated());
        assert_ne!(
            phase_color(Phase::SubmitFailed),
            phase_color(Phase::Success),
            "a failed submit must not paint as a plain success"
        );
    }

    #[test]
    fn truncated_event_maps_to_the_failure_cross_not_the_success_checkmark() {
        let phase = Phase::from_event(FeedbackEvent::Truncated);
        assert_eq!(phase, Phase::Error);
        assert_ne!(phase, Phase::Success);
        assert_eq!(phase.dwell(), Some(ERROR_DWELL));
        assert!(!phase.is_animated());
    }

    #[test]
    fn rejected_config_event_captions_as_a_config_error_not_a_transcription_failure() {
        let phase = Phase::from_event(FeedbackEvent::ConfigError);
        assert_eq!(phase, Phase::ConfigError);
        assert_eq!(phase.dwell(), Some(ERROR_DWELL));
        assert!(!phase.is_animated());
        let message = phase_message(phase).expect("config error must caption a message");
        assert!(message.contains("tonguetyped status"));
        assert_ne!(message, phase_message(Phase::Error).unwrap());
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
                Phase::Copied,
                Phase::ClipboardFallback,
                Phase::SubmitFailed,
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
    fn live_text_style_is_recognized_and_is_a_wide_pill() {
        assert_eq!(style_for("live-text"), Style::LiveText);
        assert_eq!(surface_size_for(Style::LiveText), LIVE_TEXT_SURFACE);
        let (width, height) = LIVE_TEXT_SURFACE;
        assert!(width > height * 4, "the caption pill is a wide bar");
    }

    #[test]
    fn the_single_line_caption_pill_is_content_sized() {
        let empty = caption_surface_size(Style::LiveText, Some(Phase::Recording), None);
        assert_eq!(empty.1, LIVE_TEXT_SURFACE.1);
        assert!(empty.0 >= LIVE_TEXT_MIN_WIDTH);
        assert!(
            empty.0 < LIVE_TEXT_SURFACE.0,
            "the empty listening-pad pill must be narrower than the ceiling"
        );

        let word = |text: &str| LiveText {
            committed: text.to_string(),
            tentative: String::new(),
        };
        let phrase = caption_surface_size(
            Style::LiveText,
            Some(Phase::Recording),
            Some(&word("hello there friend")),
        );
        assert!(
            phrase.0 > empty.0,
            "a spoken phrase grows the pill past the empty state"
        );

        let long = caption_surface_size(
            Style::LiveText,
            Some(Phase::Recording),
            Some(&word(&"word ".repeat(200))),
        );
        assert_eq!(
            long.0, LIVE_TEXT_SURFACE.0,
            "a long line clamps to the ceiling"
        );

        // The single-line height hugs the phase chip: the reviewed 88px capsule
        // plus the background inset, not a fixed bar.
        assert_eq!(
            empty.1,
            single_line_surface_height(),
            "the single-line height is the chip capsule plus its inset"
        );

        // The minimal variant shares the content sizing and the small floor.
        let minimal_empty =
            caption_surface_size(Style::LiveTextMinimal, Some(Phase::Recording), None);
        assert!(minimal_empty.0 >= LIVE_TEXT_MIN_WIDTH);
        assert!(minimal_empty.0 < LIVE_TEXT_SURFACE.0);

        // The box is content-sized too: an empty one is just the chip row,
        // narrower-fixed and shorter than its ceiling.
        let box_empty = caption_surface_size(Style::LiveTextBox, Some(Phase::Recording), None);
        assert_eq!(box_empty.0, LIVE_TEXT_BOX_SURFACE.0);
        assert_eq!(box_empty.1, LIVE_TEXT_BOX_MIN_HEIGHT);
        assert!(
            box_empty.1 < LIVE_TEXT_BOX_SURFACE.1,
            "the empty box must hug its chip row, not fill the ceiling"
        );
    }

    #[test]
    fn the_configured_style_is_authoritative_and_never_switches_to_the_caption() {
        // The active model and any incoming live-text snapshot never change the
        // painted style: `resolved_style` is exactly the configured value, so a
        // non-caption style stays on screen even for a streaming model, and the
        // caption appears only when one of the live-text styles is chosen.
        for configured in STYLE_VALUES {
            assert_eq!(
                resolved_style(configured),
                style_for(configured),
                "{configured}"
            );
        }
        assert_eq!(resolved_style("blob"), Style::Blob);
        assert_eq!(resolved_style("live-text"), Style::LiveText);
    }

    #[test]
    fn a_live_text_snapshot_is_painted_only_by_a_live_text_style() {
        // A streaming model keeps forwarding partials whatever the style, but
        // only a live-text style paints them: with a non-caption style the
        // snapshot leaves the frame byte-identical to the same frame without it
        // (so no live text appears on screen), while a caption style changes to
        // show the words.
        let live = LiveText {
            committed: "today I am".to_string(),
            tentative: " testing".to_string(),
        };
        for style in ALL_STYLES.into_iter().filter(|s| !is_caption_style(*s)) {
            let without = paint_to_pixels_with_live_text(
                Some(Phase::Recording),
                0.25,
                0.25,
                true,
                style,
                &speaking_history(),
                None,
            );
            let with = paint_to_pixels_with_live_text(
                Some(Phase::Recording),
                0.25,
                0.25,
                true,
                style,
                &speaking_history(),
                Some(&live),
            );
            assert_eq!(
                without, with,
                "{style:?}: a non-caption style must ignore the live-text snapshot"
            );
        }
        let caption_without = paint_to_pixels_with_live_text(
            Some(Phase::Recording),
            0.25,
            0.25,
            true,
            Style::LiveText,
            &speaking_history(),
            None,
        );
        let caption_with = paint_to_pixels_with_live_text(
            Some(Phase::Recording),
            0.25,
            0.25,
            true,
            Style::LiveText,
            &speaking_history(),
            Some(&live),
        );
        assert_ne!(
            caption_without, caption_with,
            "a live-text style must paint the snapshot's words"
        );
    }

    #[test]
    fn a_live_text_captions_empty_state_is_the_listening_pad_not_the_waveform() {
        // A live-text style's empty recording state is always the listening pad,
        // never the waveform, so the two never appear together. The waveform's
        // grey bars reach across the card while the listening pad stays on the
        // left, so bright grey pixels in the right half would betray a fallback.
        let (width, _) = surface_size_for(Style::LiveText);
        let grey_in_right_half = |pixels: &[u8]| -> usize {
            pixels
                .as_chunks::<4>()
                .0
                .iter()
                .enumerate()
                .filter(|(index, pixel)| {
                    (index % width as usize) >= width as usize / 2
                        && pixel[0] > 150
                        && pixel[1] > 150
                        && pixel[2] > 150
                })
                .count()
        };
        let empty = paint_to_pixels_with_live_text(
            Some(Phase::Recording),
            0.0,
            0.0,
            true,
            Style::LiveText,
            &speaking_history(),
            None,
        );
        assert_eq!(
            grey_in_right_half(&empty),
            0,
            "a live-text caption must not paint waveform bars"
        );
        assert!(
            empty.iter().any(|&byte| byte != 0),
            "the listening pad must paint something"
        );
    }

    #[test]
    fn a_caption_with_text_holds_the_words_and_a_caret() {
        // While words stream the caption paints the text (no waveform), with the
        // caret after the newest word.
        let live = LiveText {
            committed: "today I am testing".to_string(),
            tentative: " local speech".to_string(),
        };
        let words = paint_to_pixels_with_live_text(
            Some(Phase::Recording),
            0.25,
            0.25,
            true,
            Style::LiveText,
            &speaking_history(),
            Some(&live),
        );
        let empty = paint_to_pixels_with_live_text(
            Some(Phase::Recording),
            0.25,
            0.25,
            true,
            Style::LiveText,
            &speaking_history(),
            None,
        );
        assert_ne!(words, empty, "a caption with words differs from the pad");
        assert!(
            words.iter().any(|&byte| byte != 0),
            "the caption with text must paint pixels"
        );
    }

    #[test]
    fn boxed_caption_status_message_does_not_overlap_the_chip() {
        // The box stacks its content below the leading chip, so a terminal
        // status message with no captured words must paint in that same row
        // rather than on top of the chip. Render the same phase with the word
        // "Ready" captured - which already draws below the chip - and compare the
        // chip's own region: an intruding status message would change it.
        let (width, _) = surface_size_for(Style::LiveTextBox);
        let chip_right = (LIVE_TEXT_PADDING + LIVE_TEXT_CHIP_RADIUS * 2.0) as u32;
        let chip_region = |pixels: &[u8]| -> Vec<u8> {
            let mut out = Vec::new();
            for y in LIVE_TEXT_PADDING as u32..=chip_right {
                for x in LIVE_TEXT_PADDING as u32..=chip_right {
                    let index = ((y * width + x) * 4) as usize;
                    out.extend_from_slice(&pixels[index..index + 4]);
                }
            }
            out
        };
        let status = paint_to_pixels_with_live_text(
            Some(Phase::Success),
            0.0,
            0.0,
            true,
            Style::LiveTextBox,
            &speaking_history(),
            None,
        );
        let words = paint_to_pixels_with_live_text(
            Some(Phase::Success),
            0.0,
            0.0,
            true,
            Style::LiveTextBox,
            &speaking_history(),
            Some(&LiveText {
                committed: "Ready".to_string(),
                tentative: String::new(),
            }),
        );
        assert_eq!(
            chip_region(&status),
            chip_region(&words),
            "the box's no-words status message must not paint over the phase chip"
        );
    }

    #[test]
    fn live_text_renders_words_and_distinguishes_committed_from_tentative() {
        let (width, height, words) =
            render_live_text_pixels("live-text", "today I am", " testing", &[]);
        assert_eq!(
            height, LIVE_TEXT_SURFACE.1,
            "the single-line caption keeps its chip-sized height"
        );
        assert!(
            width < LIVE_TEXT_SURFACE.0,
            "the words measure a content-sized width, narrower than the ceiling"
        );
        assert!(
            words.iter().any(|&byte| byte != 0),
            "a caption with words must paint pixels"
        );
        let (_, _, empty) = render_live_text_pixels("live-text", "", "", &[]);
        assert_ne!(
            words, empty,
            "the word caption differs from the empty fallback"
        );

        // The frozen prefix and the revisable tail use different colours, so the
        // same text rendered as committed vs tentative must differ.
        let (_, _, committed_only) = render_live_text_pixels("live-text", "hello", "", &[]);
        let (_, _, tentative_only) = render_live_text_pixels("live-text", "", "hello", &[]);
        assert_ne!(
            committed_only, tentative_only,
            "committed and tentative text must be visually distinct"
        );
    }

    /// The lowest row (scanning up from the bottom) holding a bright, mostly
    /// opaque pixel - the caption's text or phase glyph, as opposed to the dark
    /// glass background. Measures how much padding a card leaves below its last
    /// drawn line, so a dead band under the text is caught.
    fn lowest_bright_row(width: u32, height: u32, pixels: &[u8]) -> Option<u32> {
        (0..height).rev().find(|&y| {
            (0..width).any(|x| {
                let index = ((y * width + x) * 4) as usize;
                let (b, g, r, a) = (
                    pixels[index],
                    pixels[index + 1],
                    pixels[index + 2],
                    pixels[index + 3],
                );
                a > 150 && r > 90 && g > 90 && b > 90
            })
        })
    }

    #[test]
    fn the_scrolling_box_grows_with_its_lines_then_caps() {
        let box_height = |text: &str| {
            caption_surface_size(
                Style::LiveTextBox,
                Some(Phase::Recording),
                Some(&LiveText {
                    committed: text.to_string(),
                    tentative: String::new(),
                }),
            )
            .1
        };
        let empty = box_height("");
        assert_eq!(
            empty, LIVE_TEXT_BOX_MIN_HEIGHT,
            "the empty box is just the chip row"
        );
        let one = box_height("hello");
        let two = box_height(&"word ".repeat(9));
        let many = box_height(&"word ".repeat(400));
        assert!(
            one > empty,
            "a first word opens a text row below the chip: {one} <= {empty}"
        );

        assert!(
            two > one,
            "a second wrapped line grows the box: {two} <= {one}"
        );
        assert!(
            many > two,
            "more lines keep growing the box: {many} <= {two}"
        );
        assert!(
            many <= LIVE_TEXT_BOX_SURFACE.1,
            "the box clamps to the surface ceiling: {many}"
        );

        // Past the wrap cap the box stops growing and scrolls instead.
        let capped = box_height(&"word ".repeat(800));
        assert_eq!(capped, many, "past the cap the box height stops growing");

        // Shrinking the text (a revised partial) shrinks the box back.
        assert!(
            box_height("one two three") < many,
            "a short phrase must shrink the box back down"
        );
    }

    #[test]
    fn the_empty_box_reserves_a_row_only_for_a_drawn_status_line() {
        // With no captured words the box must reserve a text row only when a
        // status line is actually drawn below the chip: a terminal phase draws
        // one. Recording (the listening pad shares the chip row), Transcribing
        // (the spinner chip is the whole caption), and idle draw none, so the
        // box hugs the chip row instead of leaving a dead band.
        let height = |phase: Option<Phase>| caption_surface_size(Style::LiveTextBox, phase, None).1;
        assert_eq!(
            height(Some(Phase::Recording)),
            LIVE_TEXT_BOX_MIN_HEIGHT,
            "the listening pad shares the chip row"
        );
        assert_eq!(
            height(Some(Phase::Transcribing)),
            LIVE_TEXT_BOX_MIN_HEIGHT,
            "transcribing with no words draws no line below the chip"
        );
        assert_eq!(
            height(None),
            LIVE_TEXT_BOX_MIN_HEIGHT,
            "the idle box draws nothing, so it must not reserve a row"
        );
        assert!(
            height(Some(Phase::Success)) > LIVE_TEXT_BOX_MIN_HEIGHT,
            "a terminal status message reserves its one drawn row"
        );
    }

    #[test]
    fn the_scrolling_box_leaves_only_its_padding_below_the_last_line() {
        // A few wrapped lines fill a short card. The lowest text row must sit
        // within the intended bottom padding of the surface edge - no tall dead
        // band under it (the pre-fix card was a fixed 320px with the text
        // hugging the top).
        let (width, height, pixels) = render_live_text_pixels(
            "live-text-box",
            "one two three four five six seven eight nine ten eleven twelve thirteen fourteen",
            " fifteen sixteen",
            &[],
        );
        assert!(
            height < LIVE_TEXT_BOX_SURFACE.1,
            "a few lines must not fill the tall ceiling: {height}"
        );
        let lowest = lowest_bright_row(width, height, &pixels).expect("the box paints its text");
        let dead_band = height - 1 - lowest;
        // The bottom padding (`LIVE_TEXT_PADDING`) plus the line box's descender
        // slack (`CAPTION_LINE_HEIGHT` over the glyphs) and the quantise slack.
        // The pre-fix fixed card left ~150px here.
        let allowed =
            LIVE_TEXT_PADDING as u32 + text::CAPTION_LINE_HEIGHT as u32 + LIVE_TEXT_SIZE_QUANTUM;
        assert!(
            dead_band <= allowed,
            "dead space below the last line was {dead_band}px (height {height}, lowest row {lowest})"
        );
        assert!(
            dead_band >= LIVE_TEXT_VPADDING as u32 / 2,
            "the box must still breathe below the text: {dead_band}px"
        );
    }

    #[test]
    fn the_single_line_caption_hugs_its_text_height() {
        // The single-line caption centres its one line and hugs it: the pill is
        // the chip capsule, not a fixed tall bar, so the bottom gap is only the
        // intended padding rather than a dead band.
        let (width, height, pixels) =
            render_live_text_pixels("live-text", "today I am testing", "", &[]);
        assert_eq!(
            height,
            single_line_surface_height(),
            "the pill is exactly its chip-capsule height"
        );
        let lowest = lowest_bright_row(width, height, &pixels).expect("the pill paints its text");
        let dead_band = height - 1 - lowest;
        let allowed =
            LIVE_TEXT_VPADDING as u32 + text::CAPTION_LINE_HEIGHT as u32 + LIVE_TEXT_SIZE_QUANTUM;
        assert!(
            dead_band <= allowed,
            "the pill leaves only its padding below the text: {dead_band}px (height {height})"
        );
    }

    #[test]
    fn caption_sizes_quantise_to_whole_steps() {
        assert_eq!(quantize_size(0.0), 0);
        assert_eq!(quantize_size(1.0), LIVE_TEXT_SIZE_QUANTUM);
        assert_eq!(
            quantize_size(LIVE_TEXT_SIZE_QUANTUM as f32),
            LIVE_TEXT_SIZE_QUANTUM
        );
        assert_eq!(
            quantize_size(LIVE_TEXT_SIZE_QUANTUM as f32 + 0.5),
            2 * LIVE_TEXT_SIZE_QUANTUM,
            "a size past a step rounds up to the next boundary"
        );
    }

    #[test]
    fn live_waveform_paints_a_different_recording_glyph_than_the_default_pulse_in_every_style() {
        // Blob and LongPill ignore `live_waveform` by design: the blob carries
        // its reaction in its body (the captain asked to remove the bars), and
        // the long pill's squiggle is the whole style. The caption styles ignore
        // it too: they paint words or the listening pad, never a waveform.
        for style in ALL_STYLES.into_iter().filter(|style| {
            !matches!(style, Style::Blob | Style::LongPill) && !is_caption_style(*style)
        }) {
            let default_pixels = paint_to_pixels(Some(Phase::Recording), 0.25, false, style);
            let streaming_pixels = paint_to_pixels(Some(Phase::Recording), 0.25, true, style);
            assert_ne!(
                default_pixels, streaming_pixels,
                "{style:?}: the live waveform should visually differ from the default pulse"
            );
        }
    }

    #[test]
    fn blob_long_pill_and_caption_styles_ignore_the_live_waveform_toggle() {
        for style in [Style::Blob, Style::LongPill].into_iter().chain([
            Style::LiveText,
            Style::LiveTextMinimal,
            Style::LiveTextBox,
        ]) {
            let off = paint_to_pixels(Some(Phase::Recording), 0.25, false, style);
            let on = paint_to_pixels(Some(Phase::Recording), 0.25, true, style);
            assert_eq!(
                off, on,
                "{style:?} should not change with live_waveform: it never draws the bars"
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
        let long_pill = paint_to_pixels(Some(Phase::Recording), 0.0, false, Style::LongPill);
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
            pill.len(),
            long_pill.len(),
            "LongPill should use a differently-sized surface from the small Pill"
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
            Phase::Copied,
            Phase::ClipboardFallback,
            Phase::SubmitFailed,
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
    fn blob_recording_body_is_a_bold_saturated_red_and_keeps_its_gloss() {
        // The captain asked the recording blob be a bold, saturated "blood
        // red" rather than the pale, washed-out pink it used to paint, while
        // keeping its gloss. Render it and check both: the opaque body stays
        // strongly red-dominant and saturated, and a bright desaturated
        // highlight (the specular dot / sheen) still stands out of the body.
        let mut saturations: Vec<f32> = Vec::new();
        let mut opaque = 0usize;
        let mut red_dominant = 0usize;
        let mut gloss = 0usize;
        for t in [0.0_f32, 0.25, 0.5, 0.75] {
            let pixels = paint_to_pixels(Some(Phase::Recording), t, false, Style::Blob);
            for pixel in pixels.as_chunks::<4>().0 {
                let (b, g, r, a) = (
                    pixel[0] as i32,
                    pixel[1] as i32,
                    pixel[2] as i32,
                    pixel[3] as i32,
                );
                if a < 200 {
                    continue;
                }
                opaque += 1;
                let max = r.max(g).max(b);
                let min = r.min(g).min(b);
                let saturation = if max == 0 {
                    0.0
                } else {
                    (max - min) as f32 / max as f32
                };
                saturations.push(saturation);
                if r > g && r > b {
                    red_dominant += 1;
                }
                // The gloss the blob must keep: a bright pixel that is far less
                // saturated than the red body, so a pale wash cannot fake it.
                if min > 150 && saturation < 0.35 {
                    gloss += 1;
                }
            }
        }
        assert!(opaque > 0, "the recording blob painted no opaque pixels");
        saturations.sort_by(|a, b| a.partial_cmp(b).unwrap());
        let median = saturations[saturations.len() / 2];
        assert!(
            median > 0.6,
            "the blob body should be boldly saturated, median saturation was {median}"
        );
        let dominant_fraction = red_dominant as f32 / opaque as f32;
        assert!(
            dominant_fraction > 0.95,
            "the blob body should be red-dominant, only {dominant_fraction} of pixels were"
        );
        assert!(
            gloss > 0,
            "the blob must keep its gloss: no bright, desaturated highlight pixel was painted"
        );
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
            let start = blob_edge(radius, angle, 0.0, 1.0);
            let wrap = blob_edge(radius, angle, 1.0, 1.0);
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
    fn border_live_waveform_sweeps_a_travelling_highlight() {
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

    #[test]
    fn half_circle_paints_a_dome_resting_on_the_top_edge_with_a_transparent_margin() {
        let (width, height) = surface_size_for(Style::HalfCircle);
        let pixels = paint_to_pixels(Some(Phase::Recording), 0.0, false, Style::HalfCircle);

        // The flat side rests on the top edge: the top-centre of the surface is
        // painted (the dome's apex region), so the shape touches the very top.
        assert!(
            alpha_at(&pixels, width, width / 2, 0) > 0,
            "the dome should be painted at the top edge"
        );

        // Away from the dome the surface is transparent: the four corners and a
        // band along the bottom all stay clear, so the glow has faded out
        // before the surface edge rather than clipping.
        assert_eq!(alpha_at(&pixels, width, 0, 0), 0, "top-left corner");
        assert_eq!(
            alpha_at(&pixels, width, width - 1, 0),
            0,
            "top-right corner"
        );
        assert_eq!(
            alpha_at(&pixels, width, 0, height - 1),
            0,
            "bottom-left corner"
        );
        assert_eq!(
            alpha_at(&pixels, width, width - 1, height - 1),
            0,
            "bottom-right corner"
        );
        for y in height - 6..height {
            for x in 0..width {
                assert_eq!(
                    alpha_at(&pixels, width, x, y),
                    0,
                    "bottom pixel ({x},{y}) should be transparent (glow faded before the edge)"
                );
            }
        }
    }

    #[test]
    fn half_circle_glow_fades_smoothly_and_symmetrically() {
        // The radial profile is the whole falloff, so check it directly: it
        // only ever weakens as the distance from the dome's centre grows, and
        // it reaches exactly zero at the dome's radius plus its glow reach.
        let radius = HALF_CIRCLE_SURFACE.1 as f32 * HALF_CIRCLE_RADIUS_FRACTION;
        let glow = HALF_CIRCLE_SURFACE.1 as f32 * HALF_CIRCLE_GLOW_FRACTION;
        let mut previous = half_circle_profile(0.0, radius, glow);
        let mut r = 0.0;
        while r <= radius + glow + 2.0 {
            let current = half_circle_profile(r, radius, glow);
            assert!(
                current <= previous + 1e-6,
                "glow should fade monotonically outward: {previous} then {current} at r={r}"
            );
            previous = current;
            r += 0.25;
        }
        assert_eq!(
            half_circle_profile(radius + glow, radius, glow),
            0.0,
            "the glow should reach zero exactly at radius + glow"
        );

        // The dome's centre is the top-edge midpoint, and the profile depends
        // only on distance from it, so the painted frame is mirror-symmetric
        // about the vertical centre line.
        let (width, _) = surface_size_for(Style::HalfCircle);
        let pixels = paint_to_pixels(Some(Phase::Recording), 0.0, false, Style::HalfCircle);
        for y in 0..2 {
            for x in 0..width {
                assert_eq!(
                    alpha_at(&pixels, width, x, y),
                    alpha_at(&pixels, width, width - 1 - x, y),
                    "column {x} and its mirror should match at row {y}"
                );
            }
        }
    }

    #[test]
    fn half_circle_uses_the_phase_colour() {
        // Buffer is BGRA, so channel 2 is red and channel 0 is blue. Sample a
        // pixel well inside the dome so the fill dominates.
        let (width, _) = surface_size_for(Style::HalfCircle);
        let index = ((width / 2) * 4) as usize;
        let recording = paint_to_pixels(Some(Phase::Recording), 0.0, false, Style::HalfCircle);
        let transcribing =
            paint_to_pixels(Some(Phase::Transcribing), 0.0, false, Style::HalfCircle);
        assert!(
            recording[index + 2] > recording[index],
            "recording dome should be red-dominant"
        );
        assert!(
            transcribing[index] > transcribing[index + 2],
            "transcribing dome should be blue-dominant"
        );
    }

    #[test]
    fn half_circle_pulses_across_its_animation_cycle() {
        let frames: Vec<Vec<u8>> = [0.0, 0.25, 0.5, 0.75]
            .iter()
            .map(|&t| paint_to_pixels(Some(Phase::Recording), t, false, Style::HalfCircle))
            .collect();
        for pair in frames.windows(2) {
            assert_ne!(
                pair[0], pair[1],
                "consecutive half-circle frames should differ (pulsing glow)"
            );
        }
    }

    #[test]
    fn half_circle_live_waveform_sweeps_a_travelling_highlight() {
        let sum =
            |pixels: &[u8]| -> u64 { pixels.as_chunks::<4>().0.iter().map(|p| p[3] as u64).sum() };
        let default = paint_to_pixels(Some(Phase::Recording), 0.15, false, Style::HalfCircle);
        let streaming = paint_to_pixels(Some(Phase::Recording), 0.15, true, Style::HalfCircle);
        assert!(
            sum(&streaming) > sum(&default),
            "the streaming sweep should add extra glow over the default dome"
        );
        // The highlight advances along the arc across the cycle, so two
        // instants with the same base pulse (sin 0 and sin pi) still differ.
        let a = paint_to_pixels(Some(Phase::Recording), 0.0, true, Style::HalfCircle);
        let b = paint_to_pixels(Some(Phase::Recording), 0.5, true, Style::HalfCircle);
        assert_ne!(
            a, b,
            "the streaming highlight should travel along the arc, not sit still"
        );
    }

    /// Is `(x, y)` (a pixel center in surface coordinates) inside the filled
    /// shape a style paints its streaming waveform into? `Badge` is the disc,
    /// `Minimal` the ring's inner edge (no filled background, just the
    /// outline), `Pill` the capsule, `Blob` the blob's glow extent, `Border`
    /// the glow band hugging the screen edge, `HalfCircle` the dome plus its
    /// radial glow, `LongPill` the whole dark bar.
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
                // The blob's breath, voice spike and glow can push its edge out
                // to about 1.79x the nominal radius (1.61x silhouette plus the
                // 0.18x halo), so treat 1.8x as the shape's outer bound.
                let outer = width.min(height) as f32 * 0.26 * 1.8;
                (x - cx).powi(2) + (y - cy).powi(2) <= outer * outer
            }
            Style::Border => {
                // Every painted pixel lies within the soft glow's reach of the
                // nearest edge (see `border_glow`). Add a pixel of slack for the
                // anti-aliased boundary.
                let reach = width.min(height) as f32 * BORDER_GLOW_FRACTION + 1.0;
                x.min(width as f32 - x).min(y.min(height as f32 - y)) < reach
            }
            Style::HalfCircle => {
                // The dome's flat edge sits on the top edge (`y = 0`), so its
                // centre is the top-edge midpoint; every painted pixel lies
                // within the dome's radius plus its glow reach (see
                // `half_circle_profile`), with slack for the boundary.
                let radius = height as f32 * HALF_CIRCLE_RADIUS_FRACTION;
                let glow = height as f32 * HALF_CIRCLE_GLOW_FRACTION;
                let reach = radius + glow + 1.0;
                (x - cx).powi(2) + y.powi(2) <= reach * reach
            }
            Style::LongPill => {
                // The bar is a plain capsule of the full surface, so the
                // painted pixels (dark fill, trace, button) are all inside it.
                let half_segment = (cx - cy).max(0.0);
                let proj_x = x.clamp(cx - half_segment, cx + half_segment);
                (x - proj_x).powi(2) + (y - cy).powi(2) <= cy * cy + 1.0
            }
            Style::LiveText | Style::LiveTextMinimal => {
                // A capsule inset from the surface edge; the waveform fallback
                // and the caption text both stay inside it.
                let half_height = cy - LIVE_TEXT_INSET;
                let half_segment = (cx - half_height).max(0.0);
                let proj_x = x.clamp(cx - half_segment, cx + half_segment);
                (x - proj_x).powi(2) + (y - cy).powi(2) <= half_height * half_height + 1.0
            }
            Style::LiveTextBox => {
                // A rounded rectangle inset from the surface edge.
                x >= LIVE_TEXT_INSET
                    && x <= width as f32 - LIVE_TEXT_INSET
                    && y >= LIVE_TEXT_INSET
                    && y <= height as f32 - LIVE_TEXT_INSET
            }
        }
    }

    #[test]
    fn live_waveform_pixels_stay_inside_every_style_shape() {
        // Identify the reactive pixels by comparing a silent recording against a
        // speaking one and check every pixel that changed sits inside the
        // style's shape. For the waveform styles those are the bars; for
        // `border`/`half-circle` the level scales the glow pulse and sweep. The
        // caption styles have no waveform, so their listening pad is compared
        // across two times instead - it breathes with time, not the mic level -
        // and those pixels must equally stay inside the caption's shape.
        for style in ALL_STYLES {
            let (width, height) = surface_size_for(style);
            let quiet = quiet_history();
            let speaking = speaking_history();
            let (base, loud) = if is_caption_style(style) {
                (
                    paint_to_pixels_with_levels(
                        Some(Phase::Recording),
                        0.0,
                        0.0,
                        true,
                        style,
                        &quiet,
                    ),
                    paint_to_pixels_with_levels(
                        Some(Phase::Recording),
                        0.35,
                        0.35,
                        true,
                        style,
                        &quiet,
                    ),
                )
            } else {
                (
                    paint_to_pixels_with_levels(
                        Some(Phase::Recording),
                        0.0,
                        0.0,
                        true,
                        style,
                        &quiet,
                    ),
                    paint_to_pixels_with_levels(
                        Some(Phase::Recording),
                        0.0,
                        0.0,
                        true,
                        style,
                        &speaking,
                    ),
                )
            };
            let mut reactive_pixels = 0u32;
            for y in 0..height {
                for x in 0..width {
                    let index = ((y * width + x) * 4) as usize;
                    if base[index..index + 4] == loud[index..index + 4] {
                        continue;
                    }
                    reactive_pixels += 1;
                    let (px, py) = (x as f32 + 0.5, y as f32 + 0.5);
                    assert!(
                        inside_style_shape(style, px, py),
                        "{style:?}: reactive pixel ({x},{y}) painted outside the shape"
                    );
                }
            }
            assert!(
                reactive_pixels >= 30,
                "{style:?}: expected a visible reactive animation inside the shape, only {reactive_pixels} pixels"
            );
        }
    }

    #[test]
    fn level_history_ring_keeps_a_rolling_window_newest_last() {
        let mut history = LevelHistory::new();
        assert_eq!(history.waveform_sample(4, 0), 0.0);
        assert_eq!(history.recent_peak(5), 0.0);

        // Fewer samples than the window: silence pads the left, the newest
        // sample is the right-most bar.
        history.push(0.1);
        history.push(0.2);
        assert_eq!(
            (0..4)
                .map(|i| history.waveform_sample(4, i))
                .collect::<Vec<_>>(),
            vec![0.0, 0.0, 0.1, 0.2]
        );

        // More samples than the ring can hold: the oldest are dropped, and
        // the window still ends on the newest.
        for i in 0..(LEVEL_HISTORY + 10) {
            history.push(i as f32 + 1.0);
        }
        assert_eq!(history.len, LEVEL_HISTORY);
        assert_eq!(
            history.waveform_sample(LEVEL_HISTORY, LEVEL_HISTORY - 1),
            (LEVEL_HISTORY + 10) as f32
        );
        assert_eq!(
            history.waveform_sample(LEVEL_HISTORY, 0),
            11.0,
            "the oldest 10 samples should have been dropped"
        );

        // A window peak looks only at the newest samples.
        history.clear();
        history.push(1.0);
        for _ in 0..9 {
            history.push(0.01);
        }
        assert_eq!(history.recent_peak(5), 0.01);
        assert_eq!(history.recent_peak(10), 1.0);
    }

    #[test]
    fn bar_heights_are_flat_at_silence_and_grow_with_level() {
        // Silence (and room noise below the floor) leaves the tiny dot, never
        // no bar at all, and the reference's quiet stretches stay flat.
        const MIN: f32 = 0.10;
        assert_eq!(bar_height_fraction(0.0), MIN);
        assert_eq!(bar_height_fraction(0.0005), MIN);
        assert_eq!(level_fraction(0.0), 0.0);
        assert_eq!(level_fraction(0.0005), 0.0);

        // Louder speech only ever grows the bar, up to the full height.
        let levels = [0.001, 0.005, 0.01, 0.02, 0.05, 0.1, 0.3, 0.9];
        let fractions: Vec<f32> = levels.iter().copied().map(bar_height_fraction).collect();
        assert!(
            fractions.windows(2).all(|pair| pair[1] > pair[0]),
            "bar heights should grow monotonically with level: {fractions:?}"
        );
        assert!(
            fractions[0] < 0.15 && *fractions.last().unwrap() > 0.9,
            "quiet speech should be short and loud speech near full height: {fractions:?}"
        );
        assert!((bar_height_fraction(1.0) - 1.0).abs() < 1e-6);
    }

    #[test]
    fn live_waveform_reacts_to_the_level_and_puts_the_newest_sample_on_the_right() {
        let width = surface_size_for(Style::Pill).0;
        let white_extent = |pixels: &[u8], x: u32| -> u32 {
            (0..surface_size_for(Style::Pill).1)
                .filter(|&y| {
                    let index = ((y * width + x) * 4) as usize;
                    pixels[index..index + 4] == [255, 255, 255, 255]
                })
                .count() as u32
        };
        let quiet = paint_to_pixels_with_levels(
            Some(Phase::Recording),
            0.0,
            0.0,
            true,
            Style::Pill,
            &quiet_history(),
        );
        let speaking = paint_to_pixels_with_levels(
            Some(Phase::Recording),
            0.0,
            0.0,
            true,
            Style::Pill,
            &speaking_history(),
        );

        // The speaking frame paints many more bar pixels than the quiet one,
        // and reaches visibly higher at a loud syllable's column.
        let painted = |pixels: &[u8]| -> u32 { (0..width).map(|x| white_extent(pixels, x)).sum() };
        assert!(
            painted(&speaking) > painted(&quiet) * 2,
            "speaking should paint a far more prominent waveform: {} vs {}",
            painted(&speaking),
            painted(&quiet)
        );

        // A single loud sample near the end: the newest sample is drawn as
        // the right-most bar, so a burst one sample fresher sits further right
        // than the same burst one sample older.
        let ending_with = |tail: &[f32]| -> LevelHistory {
            let mut history = LevelHistory::new();
            for _ in 0..(LEVEL_HISTORY - tail.len()) {
                history.push(0.0015);
            }
            for level in tail {
                history.push(*level);
            }
            history
        };
        let newest = ending_with(&[0.0015, 0.5]);
        let older = ending_with(&[0.5, 0.0015]);
        let newest_frame = paint_to_pixels_with_levels(
            Some(Phase::Recording),
            0.0,
            0.0,
            true,
            Style::Pill,
            &newest,
        );
        let older_frame = paint_to_pixels_with_levels(
            Some(Phase::Recording),
            0.0,
            0.0,
            true,
            Style::Pill,
            &older,
        );
        let tallest_column =
            |pixels: &[u8]| -> u32 { (0..width).max_by_key(|&x| white_extent(pixels, x)).unwrap() };
        assert!(
            tallest_column(&newest_frame) > width / 2,
            "the newest sample's tall bar should sit on the right"
        );
        assert!(
            tallest_column(&newest_frame) > tallest_column(&older_frame),
            "the newest sample should occupy a bar further right than the older one"
        );
    }

    #[test]
    fn every_recording_style_reacts_to_the_live_level() {
        // The captain's ask covers every level-reactive style, not just the
        // pill's waveform: each recording animation must visibly change when
        // the voice level changes, in both the simple-pulse and live-waveform
        // treatments. The caption styles are excluded: they never draw a
        // waveform and their empty state is the time-breathed listening pad, so
        // their reaction to time is covered by
        // `live_waveform_pixels_stay_inside_every_style_shape` instead.
        // Opaque-fill styles (badge/pill) cannot show growth through alpha, so
        // compare their painted white response; the long pill's trace is
        // colour-only over its opaque bar, so count non-background pixels; the
        // others grow in alpha.
        let response = |style: Style, pixels: &[u8]| -> u64 {
            if matches!(style, Style::Badge | Style::Pill) {
                pixels
                    .as_chunks::<4>()
                    .0
                    .iter()
                    .filter(|pixel| *pixel == &[255, 255, 255, 255])
                    .count() as u64
            } else if style == Style::LongPill {
                // Count the bright trace/button pixels (any channel well above
                // the dark glass bar's reach), so a louder voice paints more.
                pixels
                    .as_chunks::<4>()
                    .0
                    .iter()
                    .filter(|pixel| pixel[0] > 140 || pixel[1] > 140 || pixel[2] > 140)
                    .count() as u64
            } else {
                pixels.as_chunks::<4>().0.iter().map(|p| p[3] as u64).sum()
            }
        };
        for style in ALL_STYLES
            .into_iter()
            .filter(|style| !is_caption_style(*style))
        {
            for live_waveform in [false, true] {
                let quiet = paint_to_pixels_with_levels(
                    Some(Phase::Recording),
                    0.0,
                    0.0,
                    live_waveform,
                    style,
                    &quiet_history(),
                );
                let speaking = paint_to_pixels_with_levels(
                    Some(Phase::Recording),
                    0.0,
                    0.0,
                    live_waveform,
                    style,
                    &speaking_history(),
                );
                assert_ne!(
                    quiet, speaking,
                    "{style:?} (live_waveform={live_waveform}) should react to the microphone level"
                );
                assert!(
                    response(style, &speaking) > response(style, &quiet),
                    "{style:?} (live_waveform={live_waveform}): a louder voice should paint a bigger response ({} vs {})",
                    response(style, &speaking),
                    response(style, &quiet)
                );
            }
        }
    }

    #[test]
    fn long_pill_paints_a_dark_bar_with_a_phase_status_button() {
        // The reference's dark neutral bar fill, sampled well above the trace's
        // reach (y = 4 is above even a full-excursion peak) and inside the
        // capsule.
        let recording = paint_to_pixels(Some(Phase::Recording), 0.0, false, Style::LongPill);
        let (width, height) = surface_size_for(Style::LongPill);
        let fill = ((4 * width + width / 8) * 4) as usize;
        assert_eq!(
            &recording[fill..fill + 4],
            &long_pill_bar_at(4),
            "the long pill's bar should be the dark glass gradient, faintly translucent"
        );
        // Its corners are transparent - the bar is a capsule, not a rectangle.
        assert_eq!(recording[3], 0, "top-left corner should be transparent");

        // Recording: the vivid red button with the reference's white dot.
        let button_radius = height as f32 * 0.376;
        let button_cx = width as f32 - height as f32 * 0.516;
        let button_cy = height as f32 / 2.0;
        let at = |pixels: &[u8], x: f32, y: f32| -> [u8; 4] {
            let index = ((y as u32 * width + x as u32) * 4) as usize;
            pixels[index..index + 4].try_into().unwrap()
        };
        let button_edge = at(
            &recording,
            button_cx + button_radius * 0.7,
            button_cy - button_radius * 0.7,
        );
        assert!(
            button_edge[2] > 180 && button_edge[1] < 90,
            "the recording button should be red-dominant: {button_edge:?}"
        );
        let dot = at(&recording, button_cx, button_cy);
        assert_eq!(
            dot,
            [255, 255, 255, 255],
            "the recording button should have a white dot"
        );

        // Success: the vivid green button and a white checkmark.
        let success = paint_to_pixels(Some(Phase::Success), 0.0, false, Style::LongPill);
        let button_edge = at(
            &success,
            button_cx + button_radius * 0.7,
            button_cy - button_radius * 0.7,
        );
        assert!(
            button_edge[1] > 150 && button_edge[2] < 120,
            "the success button should be green-dominant: {button_edge:?}"
        );
        assert!(
            success.as_chunks::<4>().0.contains(&[255, 255, 255, 255]),
            "the success button should carry a white checkmark"
        );
    }

    #[test]
    fn long_pill_trace_is_continuous_and_grows_with_the_level() {
        let (width, height) = surface_size_for(Style::LongPill);
        let button_radius = height as f32 * 0.376;
        let button_cx = width as f32 - height as f32 * 0.516;
        let line_start = (width as f32 * 0.039) as u32;
        let line_end = (button_cx - button_radius - height as f32 * 0.21) as u32;
        let is_trace = |pixels: &[u8], x: u32, y: u32| -> bool {
            let index = ((y * width + x) * 4) as usize;
            let pixel = &pixels[index..index + 4];
            pixel[0] > 120 && pixel[1] > 120 && pixel[2] > 120
        };

        for (label, history) in [("quiet", quiet_history()), ("speaking", speaking_history())] {
            let pixels = paint_to_pixels_with_levels(
                Some(Phase::Recording),
                0.0,
                0.0,
                false,
                Style::LongPill,
                &history,
            );
            let mut min_y = height;
            let mut max_y = 0;
            for x in line_start..=line_end {
                let column: Vec<u32> = (0..height).filter(|&y| is_trace(&pixels, x, y)).collect();
                assert!(
                    !column.is_empty(),
                    "{label}: the trace should be continuous at x={x}"
                );
                min_y = min_y.min(*column.first().unwrap());
                max_y = max_y.max(*column.last().unwrap());
            }
            let excursion = max_y - min_y;
            if label == "quiet" {
                assert!(
                    excursion <= height / 4,
                    "a quiet room should leave the trace near flat, got {excursion}px"
                );
            } else {
                // The reference's excursions span about half the bar height.
                assert!(
                    excursion >= height / 3,
                    "speech should visibly wave the trace, got only {excursion}px"
                );
            }
        }

        // The trace rolls with the history: a shifted window paints differently.
        let mut shifted = speaking_history();
        shifted.push(0.2);
        let original = paint_to_pixels_with_levels(
            Some(Phase::Recording),
            0.0,
            0.0,
            false,
            Style::LongPill,
            &speaking_history(),
        );
        let rolled = paint_to_pixels_with_levels(
            Some(Phase::Recording),
            0.0,
            0.0,
            false,
            Style::LongPill,
            &shifted,
        );
        assert_ne!(
            original, rolled,
            "new audio should roll the long pill's trace"
        );
    }

    #[test]
    fn long_pill_envelope_keeps_the_wave_even() {
        // A constant voice gives a constant envelope away from the padded
        // start of a fresh history, so the wave's amplitude is steady.
        let mut steady = LevelHistory::new();
        for _ in 0..LEVEL_HISTORY {
            steady.push(0.05);
        }
        let envelope = long_pill_envelope(&steady);
        let expected = envelope[LEVEL_HISTORY / 2];
        assert!(expected > 0.0);
        assert!(
            envelope[8..LEVEL_HISTORY - 8]
                .iter()
                .all(|value| (value - expected).abs() < 1e-5),
            "a steady level should render an even envelope"
        );

        // A spiky level is low-passed: the envelope never jumps anywhere near
        // as hard as the raw level does, which is what stops the rendered
        // trace from looking jagged.
        let mut spiky = LevelHistory::new();
        for i in 0..LEVEL_HISTORY {
            spiky.push(if i % 2 == 0 { 0.2 } else { 0.001 });
        }
        let raw_fractions: Vec<f32> = (0..LEVEL_HISTORY)
            .map(|i| level_fraction(spiky.waveform_sample(LEVEL_HISTORY, i)))
            .collect();
        let envelope = long_pill_envelope(&spiky);
        let raw_jump = raw_fractions
            .windows(2)
            .map(|pair| (pair[1] - pair[0]).abs())
            .fold(0.0, f32::max);
        let envelope_jump = envelope[8..LEVEL_HISTORY - 8]
            .windows(2)
            .map(|pair| (pair[1] - pair[0]).abs())
            .fold(0.0, f32::max);
        assert!(
            raw_jump > 0.5,
            "the fixture should actually alternate: {raw_jump}"
        );
        assert!(
            envelope_jump < raw_jump / 4.0,
            "the envelope should smooth the raw jolts ({envelope_jump} vs {raw_jump})"
        );
    }

    #[test]
    fn blob_spikes_on_voice_peaks_and_stays_a_soft_ball_in_silence() {
        let radius = 25.0;
        let samples = 720;
        let edge_stats = |lf: f32| -> (f32, f32) {
            let mut max = 0.0f32;
            let mut total = 0.0f32;
            let mut count = 0.0f32;
            for step in 0..8 {
                let t = step as f32 / 8.0;
                for i in 0..samples {
                    let angle = i as f32 / samples as f32 * std::f32::consts::TAU;
                    let edge = blob_edge(radius, angle, t, lf);
                    max = max.max(edge);
                    total += edge;
                    count += 1.0;
                }
            }
            (max, total / count)
        };
        let (quiet_max, quiet_mean) = edge_stats(0.0);
        let (loud_max, loud_mean) = edge_stats(1.0);

        // Silence: a soft, round ball - the silhouette never strays far from
        // the body radius.
        assert!(
            quiet_max <= radius * 1.09,
            "silence should stay round, got max {quiet_max}"
        );
        assert!(
            (quiet_mean - radius).abs() < radius * 0.05,
            "silence should average the body radius, got {quiet_mean}"
        );

        // Voice peaks: long sharp spikes - the silhouette reaches well past
        // the body, while the average stays close to it (the spikes are
        // narrow rather than a lumpy swell).
        assert!(
            loud_max >= radius * 1.4,
            "voice peaks should sprout long spikes, got max {loud_max}"
        );
        assert!(
            loud_mean < radius * 1.3,
            "the spikes should be narrow, got mean {loud_mean}"
        );
        assert!(
            loud_max > quiet_max + radius * 0.3,
            "spikes should protrude when the voice is loud ({loud_max} vs {quiet_max})"
        );
    }

    #[test]
    fn style_for_recognizes_every_configured_value_and_falls_back_to_badge() {
        assert_eq!(style_for("badge"), Style::Badge);
        assert_eq!(style_for("minimal"), Style::Minimal);
        assert_eq!(style_for("pill"), Style::Pill);
        assert_eq!(style_for("blob"), Style::Blob);
        assert_eq!(style_for("border"), Style::Border);
        assert_eq!(style_for("half-circle"), Style::HalfCircle);
        assert_eq!(style_for("long-pill"), Style::LongPill);
        assert_eq!(style_for("nonsense"), Style::Badge);
    }
}
