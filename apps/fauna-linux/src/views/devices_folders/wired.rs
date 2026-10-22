//! The seat's wired `DevicesMachine` and the type only its builder can mint.
//!
//! A source guard on the page builder cannot pin that the builder *keeps using*
//! the wired machine: every respelling of a rebinding is another string to
//! catch (`review-method.md` § Pinning a pure predicate does not pin its use).
//! So the seam is the type system's. [`WiredDevicesMachine`]'s field is private
//! to this module and [`build_wired_devices_machine`] is the only fn here that
//! builds one; the page builder's remaining wiring and its render loop take
//! `&WiredDevicesMachine`, so a plain `Arc<DevicesMachine>` — built by the
//! shared `build_devices_machine`, aliased or not, bound by `let`, by a pattern
//! or by a shadow — fails to compile wherever the builder uses it, and the seat
//! can never ship without the fleet-removal door and the MLS join-filter.

use std::ops::Deref;
use std::sync::Arc;

use fauna_devices_machine::{DevicesMachine, DevicesObserver, build_devices_machine};

use super::LinuxMlsQuery;

/// A `DevicesMachine` that [`build_wired_devices_machine`] built and wired.
/// Derefs to the `Arc` so read-only helpers keep their `&Arc<DevicesMachine>`
/// signatures; the wiring and the render loop take this type itself.
#[derive(Clone)]
pub(crate) struct WiredDevicesMachine(Arc<DevicesMachine>);

impl Deref for WiredDevicesMachine {
    type Target = Arc<DevicesMachine>;
    fn deref(&self) -> &Arc<DevicesMachine> {
        &self.0
    }
}

/// Wire the fleet-scope removal door (`devices.md` § Removing a Device) — the
/// shared adapter, unconditional: it reads this process's handle fresh per
/// call, and refuses the removal while there is none. Called by
/// [`build_wired_devices_machine`], the one place the seat's machine is wired.
fn wire_fleet_removal(machine: &DevicesMachine) {
    machine.set_fleet_removal(Arc::new(
        fauna_client_account_runtime::fleet_removal::RuntimeFleetRemoval::new(
            crate::account_runtime::handle,
        ),
    ));
    // This device's own peer-participation door (`p2p.md` § Per-device
    // participation), the same shared impl over the same handle read; when
    // the co-located agent holds the engine, the switch asks it for its pass
    // through the seat's holder nudge (`crate::sync_agent` publishes it).
    machine.set_p2p_participation_door(Arc::new(
        fauna_client_account_runtime::p2p_participation::RuntimeP2pParticipation::new(
            crate::account_runtime::handle,
        )
        .with_holder_nudge(
            fauna_client_account_runtime::p2p_participation::EngineHolderNudgeSlot::seat(),
        ),
    ));
}

/// Build the page machine and wire everything that needs no GTK context or
/// `FaunaClient`: the member-row join-filter and the fleet-scope doors.
/// [`super::build_devices_and_folders_pages`] calls this and a test calls it
/// too, so a pin on the result reds when this fn stops wiring (the `fauna-ffi`
/// `build_devices_machine` shape).
pub(super) fn build_wired_devices_machine(
    nest: Arc<fauna_client::NestClient>,
    custody: Option<Arc<dyn fauna_client_folders::FolderKeyStore>>,
    observer: Arc<dyn DevicesObserver>,
) -> WiredDevicesMachine {
    let machine = build_devices_machine(nest, custody, observer);
    // Wire the B3 member-row join-filter: the centralized
    // `DevicesMachine` filter delegates to this per-app `MlsQuery` to drop
    // shared-with-me rows this client has not MLS-joined. Until wired the machine is
    // fail-safe (drops all member rows), so this MUST precede the first refresh for a
    // joined shared set to render.
    machine.set_mls_query(Arc::new(LinuxMlsQuery));
    wire_fleet_removal(&machine);
    WiredDevicesMachine(machine)
}
