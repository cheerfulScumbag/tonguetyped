//! Side-effect-free capability probes for the Wayland session.
//!
//! The typing path needs two facts from the compositor before it can promise
//! anything: whether `zwp_virtual_keyboard_manager_v1` is advertised (the
//! protocol `wtype` types through) and whether the seat currently has a
//! keyboard capability. Both are read from a registry roundtrip on a private
//! connection. No surface is created, no focus is requested, and no key event
//! is sent, so the probe can run at daemon start without disturbing the
//! session.
//!
//! A failure to connect is reported as `None`, not `false`: the caller may
//! still fall back to the desktop-name heuristic when there is no display to
//! ask (a headless test sandbox, or a daemon started before the compositor).

use smithay_client_toolkit::reexports::client::protocol::wl_seat;
use smithay_client_toolkit::reexports::client::{
    globals::registry_queue_init, Connection, Dispatch, QueueHandle,
};
use smithay_client_toolkit::registry::{ProvidesRegistryState, RegistryState};
use smithay_client_toolkit::{delegate_registry, registry_handlers};

/// What the compositor advertises to a fresh client.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct VirtualKeyboardCapability {
    /// `zwp_virtual_keyboard_manager_v1` is in the registry.
    pub manager: bool,
    /// A `wl_seat` exists and currently advertises a keyboard.
    pub seat_keyboard: bool,
}

impl VirtualKeyboardCapability {
    /// Whether `wtype` has the protocol and a place to send keys. Both are
    /// required: a manager with no keyboard-capable seat accepts the protocol
    /// but has no focused surface to deliver to.
    pub fn usable(self) -> bool {
        self.manager && self.seat_keyboard
    }
}

struct ProbeState {
    registry: RegistryState,
    seat_keyboard: bool,
}

impl ProvidesRegistryState for ProbeState {
    fn registry(&mut self) -> &mut RegistryState {
        &mut self.registry
    }

    registry_handlers![];
}

impl Dispatch<wl_seat::WlSeat, ()> for ProbeState {
    fn event(
        state: &mut Self,
        _seat: &wl_seat::WlSeat,
        event: wl_seat::Event,
        _data: &(),
        _conn: &Connection,
        _qh: &QueueHandle<Self>,
    ) {
        if let wl_seat::Event::Capabilities { capabilities } = event {
            if let Ok(capabilities) = capabilities.into_result() {
                state.seat_keyboard = capabilities.contains(wl_seat::Capability::Keyboard);
            }
        }
    }
}

delegate_registry!(ProbeState);

/// Reads the virtual-keyboard capability from the session's Wayland display.
///
/// Returns `None` when no display connection can be made or the registry
/// roundtrip fails, so the caller can distinguish "this compositor says no"
/// from "there is nothing to ask".
pub fn virtual_keyboard() -> Option<VirtualKeyboardCapability> {
    let conn = Connection::connect_to_env().ok()?;
    let (globals, mut queue) = registry_queue_init::<ProbeState>(&conn).ok()?;

    let (manager, has_seat) = globals.contents().with_list(|list| {
        let manager = list
            .iter()
            .any(|global| global.interface == "zwp_virtual_keyboard_manager_v1");
        let has_seat = list.iter().any(|global| global.interface == "wl_seat");
        (manager, has_seat)
    });

    let mut state = ProbeState {
        registry: RegistryState::new(&globals),
        seat_keyboard: false,
    };

    if has_seat {
        // Keep the proxy alive until the roundtrip has delivered its
        // `capabilities` event. Binding sends no input and changes no focus.
        if let Ok(_seat) = globals.bind::<wl_seat::WlSeat, _, _>(&queue.handle(), 1..=9, ()) {
            let _ = queue.roundtrip(&mut state);
        }
    }

    Some(VirtualKeyboardCapability {
        manager,
        seat_keyboard: state.seat_keyboard,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn usable_requires_both_the_protocol_and_a_keyboard_seat() {
        for (manager, seat_keyboard, expected) in [
            (true, true, true),
            (true, false, false),
            (false, true, false),
            (false, false, false),
        ] {
            let capability = VirtualKeyboardCapability {
                manager,
                seat_keyboard,
            };
            assert_eq!(capability.usable(), expected, "{capability:?}");
        }
    }
}
