//! A surface TongueTyped owns for the user-initiated typing round trip.
//!
//! A hidden capability probe can prove that a typing backend is *able* to
//! inject, but never that a keystroke arrived: the Wayland virtual-keyboard and
//! libei protocols are write-only, and XTEST goes to whatever has focus. The
//! honest round trip therefore stays in `tonguetyped doctor --test-type`, and
//! this module gives it a surface of its own so the test can never type into an
//! unrelated application.
//!
//! The surface is a small layer-shell panel with `OnDemand` keyboard
//! interactivity. The user clicks it, the doctor types the test line into it,
//! and the tool reads the key events back from that surface. No application
//! loses text, and the result is a real delivery check rather than a
//! capability guess.
//!
//! When no layer-shell Wayland session is available (headless, X11, a
//! compositor without `zwlr_layer_shell_v1`), [`run`] returns
//! [`SurfaceTest::Unavailable`] and the caller falls back to the existing
//! focused-application test.

use crate::output::Delivery;
use smithay_client_toolkit::compositor::{CompositorHandler, CompositorState};
use smithay_client_toolkit::output::{OutputHandler, OutputState};
use smithay_client_toolkit::reexports::calloop::timer::{TimeoutAction, Timer};
use smithay_client_toolkit::reexports::calloop::{EventLoop, LoopSignal};
use smithay_client_toolkit::reexports::calloop_wayland_source::WaylandSource;
use smithay_client_toolkit::reexports::client::{
    globals::registry_queue_init,
    protocol::{wl_keyboard, wl_output, wl_seat, wl_shm, wl_surface},
    Connection, QueueHandle,
};
use smithay_client_toolkit::registry::{ProvidesRegistryState, RegistryState};
use smithay_client_toolkit::seat::keyboard::{
    KeyEvent, KeyboardHandler, Keysym, Modifiers, RawModifiers,
};
use smithay_client_toolkit::seat::{Capability, SeatHandler, SeatState};
use smithay_client_toolkit::shell::wlr_layer::{
    Anchor, KeyboardInteractivity, Layer, LayerShell, LayerShellHandler, LayerSurface,
    LayerSurfaceConfigure,
};
use smithay_client_toolkit::shell::WaylandSurface;
use smithay_client_toolkit::shm::slot::SlotPool;
use smithay_client_toolkit::shm::{Shm, ShmHandler};
use smithay_client_toolkit::{delegate_dispatch2, delegate_registry, registry_handlers};
use std::time::Duration;

const SURFACE_WIDTH: u32 = 420;
const SURFACE_HEIGHT: u32 = 120;

/// The result of one owned-surface round trip.
pub enum SurfaceTest {
    /// No layer-shell Wayland session is available; the caller should use its
    /// fallback.
    Unavailable,
    /// The panel opened but never received keyboard focus, so no test text was
    /// sent.
    NoFocus,
    /// The text was sent. `delivery` is the output layer's result and
    /// `captured` is what the owned surface actually received.
    Completed {
        delivery: anyhow::Result<Delivery>,
        captured: String,
    },
}

impl SurfaceTest {
    /// Whether the round trip proved exact delivery.
    pub fn matched(&self, expected: &str) -> bool {
        matches!(
            self,
            SurfaceTest::Completed {
                delivery: Ok(Delivery::Inserted),
                captured,
            } if captured == expected
        )
    }
}

/// Opens the test panel and types through `send` once the panel has focus.
///
/// `send` runs exactly once, from the keyboard-enter event, and must not be
/// called again: a blind retry after a failed type call could duplicate text
/// that was partially delivered.
pub fn run<F>(expected: &str, timeout: Duration, send: F) -> SurfaceTest
where
    F: FnOnce() -> anyhow::Result<Delivery> + 'static,
{
    match run_inner(expected, timeout, send) {
        Ok(outcome) => outcome,
        Err(error) => {
            tracing::debug!("typing test surface unavailable: {error:#}");
            SurfaceTest::Unavailable
        }
    }
}

fn run_inner<F>(expected: &str, timeout: Duration, send: F) -> anyhow::Result<SurfaceTest>
where
    F: FnOnce() -> anyhow::Result<Delivery> + 'static,
{
    let conn = Connection::connect_to_env()?;
    let (globals, event_queue) = registry_queue_init::<SurfaceState>(&conn)?;
    let qh = event_queue.handle();

    let compositor = CompositorState::bind(&globals, &qh)?;
    let layer_shell = LayerShell::bind(&globals, &qh)?;
    let shm = Shm::bind(&globals, &qh)?;

    let surface = compositor.create_surface(&qh);
    let layer = layer_shell.create_layer_surface(
        &qh,
        surface,
        Layer::Overlay,
        Some("tonguetyped-typing-test"),
        None,
    );
    layer.set_anchor(Anchor::TOP | Anchor::LEFT);
    layer.set_margin(48, 0, 0, 48);
    layer.set_size(SURFACE_WIDTH, SURFACE_HEIGHT);
    layer.set_exclusive_zone(0);
    layer.set_keyboard_interactivity(KeyboardInteractivity::OnDemand);
    // The first commit maps the surface; the compositor answers with a
    // configure, and the buffer is attached from there.
    layer.commit();

    let pool = SlotPool::new((SURFACE_WIDTH * SURFACE_HEIGHT * 4) as usize, &shm)?;

    let mut event_loop: EventLoop<SurfaceState> = EventLoop::try_new()?;
    let handle = event_loop.handle();
    let loop_signal = event_loop.get_signal();

    WaylandSource::new(conn, event_queue)
        .insert(handle.clone())
        .map_err(|error| anyhow::anyhow!("failed to register the Wayland event source: {error}"))?;
    handle
        .insert_source(
            Timer::from_duration(timeout),
            move |_, _, state: &mut SurfaceState| {
                state.loop_signal.stop();
                TimeoutAction::Drop
            },
        )
        .map_err(|error| anyhow::anyhow!("failed to register the test timeout: {error}"))?;

    let mut state = SurfaceState {
        registry_state: RegistryState::new(&globals),
        seat_state: SeatState::new(&globals, &qh),
        output_state: OutputState::new(&globals, &qh),
        shm,
        pool,
        layer,
        keyboard: None,
        focused: false,
        expected: expected.to_string(),
        captured: String::new(),
        send: Some(Box::new(send)),
        delivery: None,
        loop_signal,
    };

    eprintln!(
        "A TongueTyped test window is open. Click it, then the test line will be typed into it."
    );
    event_loop.run(None, &mut state, |_| {})?;

    if state.delivery.is_none() {
        return Ok(SurfaceTest::NoFocus);
    }
    Ok(SurfaceTest::Completed {
        delivery: state.delivery.take().unwrap(),
        captured: state.captured,
    })
}

struct SurfaceState {
    registry_state: RegistryState,
    seat_state: SeatState,
    output_state: OutputState,
    shm: Shm,
    pool: SlotPool,
    layer: LayerSurface,
    keyboard: Option<wl_keyboard::WlKeyboard>,
    focused: bool,
    expected: String,
    captured: String,
    send: Option<Box<dyn FnOnce() -> anyhow::Result<Delivery>>>,
    delivery: Option<anyhow::Result<Delivery>>,
    loop_signal: LoopSignal,
}

impl SurfaceState {
    /// Runs the one-shot send, then stops once the exact test line has arrived.
    fn send_once(&mut self) {
        if let Some(send) = self.send.take() {
            let result = send();
            // Only an actual insertion can still produce keystrokes on the
            // panel. A failure, a clipboard fallback, or `none` is final, so
            // do not wait out the timeout for text that will never arrive.
            let inserted = matches!(
                &result,
                Ok(Delivery::Inserted) | Ok(Delivery::SubmitFailed(_))
            );
            self.delivery = Some(result);
            if !inserted {
                self.loop_signal.stop();
                return;
            }
        }
        if self.captured == self.expected {
            self.loop_signal.stop();
        }
    }

    fn draw(&mut self, width: u32, height: u32) {
        let stride = width as i32 * 4;
        let Ok((buffer, pixels)) = self.pool.create_buffer(
            width as i32,
            height as i32,
            stride,
            wl_shm::Format::Argb8888,
        ) else {
            return;
        };
        // A plain, readable panel: a dark body and a warm strip along the top
        // so the click target is obvious without rendering text into a surface
        // this test is trying to receive keystrokes for.
        for y in 0..height {
            for x in 0..width {
                let index = ((y * width + x) * 4) as usize;
                let pixel = if y < 4 {
                    [245, 158, 11, 255]
                } else {
                    [22, 22, 26, 255]
                };
                pixels[index..index + 4].copy_from_slice(&pixel);
            }
        }
        self.layer
            .wl_surface()
            .damage_buffer(0, 0, width as i32, height as i32);
        let _ = buffer.attach_to(self.layer.wl_surface());
        self.layer.commit();
    }
}

impl ProvidesRegistryState for SurfaceState {
    fn registry(&mut self) -> &mut RegistryState {
        &mut self.registry_state
    }

    registry_handlers![OutputState];
}

impl CompositorHandler for SurfaceState {
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

impl OutputHandler for SurfaceState {
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

impl ShmHandler for SurfaceState {
    fn shm_state(&mut self) -> &mut Shm {
        &mut self.shm
    }
}

impl LayerShellHandler for SurfaceState {
    fn closed(&mut self, _conn: &Connection, _qh: &QueueHandle<Self>, _layer: &LayerSurface) {
        self.loop_signal.stop();
    }

    fn configure(
        &mut self,
        _conn: &Connection,
        _qh: &QueueHandle<Self>,
        layer: &LayerSurface,
        configure: LayerSurfaceConfigure,
        _serial: u32,
    ) {
        if &self.layer != layer {
            return;
        }
        let (width, height) = configure.new_size;
        let width = if width == 0 { SURFACE_WIDTH } else { width };
        let height = if height == 0 { SURFACE_HEIGHT } else { height };
        self.draw(width, height);
    }
}

impl SeatHandler for SurfaceState {
    fn seat_state(&mut self) -> &mut SeatState {
        &mut self.seat_state
    }

    fn new_seat(&mut self, _conn: &Connection, _qh: &QueueHandle<Self>, _seat: wl_seat::WlSeat) {}

    fn new_capability(
        &mut self,
        _conn: &Connection,
        qh: &QueueHandle<Self>,
        seat: wl_seat::WlSeat,
        capability: Capability,
    ) {
        if capability == Capability::Keyboard && self.keyboard.is_none() {
            self.keyboard = self.seat_state.get_keyboard(qh, &seat, None).ok();
        }
    }

    fn remove_capability(
        &mut self,
        _conn: &Connection,
        _qh: &QueueHandle<Self>,
        _seat: wl_seat::WlSeat,
        capability: Capability,
    ) {
        if capability == Capability::Keyboard {
            if let Some(keyboard) = self.keyboard.take() {
                keyboard.release();
            }
        }
    }

    fn remove_seat(&mut self, _conn: &Connection, _qh: &QueueHandle<Self>, _seat: wl_seat::WlSeat) {
    }
}

impl KeyboardHandler for SurfaceState {
    fn enter(
        &mut self,
        _conn: &Connection,
        _qh: &QueueHandle<Self>,
        _keyboard: &wl_keyboard::WlKeyboard,
        surface: &wl_surface::WlSurface,
        _serial: u32,
        _raw: &[u32],
        _keysyms: &[Keysym],
    ) {
        if self.layer.wl_surface() == surface && !self.focused {
            self.focused = true;
            self.send_once();
        }
    }

    fn leave(
        &mut self,
        _conn: &Connection,
        _qh: &QueueHandle<Self>,
        _keyboard: &wl_keyboard::WlKeyboard,
        surface: &wl_surface::WlSurface,
        _serial: u32,
    ) {
        if self.layer.wl_surface() == surface {
            self.focused = false;
        }
    }

    fn press_key(
        &mut self,
        _conn: &Connection,
        _qh: &QueueHandle<Self>,
        _keyboard: &wl_keyboard::WlKeyboard,
        _serial: u32,
        event: KeyEvent,
    ) {
        if !self.focused {
            return;
        }
        // Enter is a control key for the panel, not part of the test line.
        if matches!(event.keysym, Keysym::Return | Keysym::KP_Enter) {
            return;
        }
        if let Some(text) = event.utf8 {
            self.captured.push_str(&text);
        }
        if self.captured == self.expected {
            self.loop_signal.stop();
        }
    }

    fn repeat_key(
        &mut self,
        _conn: &Connection,
        _qh: &QueueHandle<Self>,
        _keyboard: &wl_keyboard::WlKeyboard,
        _serial: u32,
        _event: KeyEvent,
    ) {
    }

    fn release_key(
        &mut self,
        _conn: &Connection,
        _qh: &QueueHandle<Self>,
        _keyboard: &wl_keyboard::WlKeyboard,
        _serial: u32,
        _event: KeyEvent,
    ) {
    }

    fn update_modifiers(
        &mut self,
        _conn: &Connection,
        _qh: &QueueHandle<Self>,
        _keyboard: &wl_keyboard::WlKeyboard,
        _serial: u32,
        _modifiers: Modifiers,
        _raw_modifiers: RawModifiers,
        _layout: u32,
    ) {
    }
}

delegate_registry!(SurfaceState);
delegate_dispatch2!(SurfaceState);

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_an_exact_captured_line_counts_as_a_round_trip() {
        let exact = SurfaceTest::Completed {
            delivery: Ok(Delivery::Inserted),
            captured: "TongueTyped typing test".to_string(),
        };
        assert!(exact.matched("TongueTyped typing test"));
        assert!(!exact.matched("TongueTyped typing"));

        let mismatched = SurfaceTest::Completed {
            delivery: Ok(Delivery::Inserted),
            captured: "TongueTyped typin".to_string(),
        };
        assert!(!mismatched.matched("TongueTyped typing test"));

        let fallback = SurfaceTest::Completed {
            delivery: Ok(Delivery::CopiedAfterFailure("wtype failed".to_string())),
            captured: String::new(),
        };
        assert!(!fallback.matched("TongueTyped typing test"));
        assert!(!SurfaceTest::NoFocus.matched("TongueTyped typing test"));
        assert!(!SurfaceTest::Unavailable.matched("TongueTyped typing test"));
    }
}
