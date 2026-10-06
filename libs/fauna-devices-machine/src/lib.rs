//! Page-level Devices state machine.
//!
//! Mirrors `fauna-folders-machine`: a shared-Rust `#[uniffi::Object]` state
//! machine (`DevicesMachine`) driving an observer-rendered page, exposed to
//! native apps via UniFFI (`libs/fauna-ffi`) and to web via WASM. Where the
//! wizard machine owns the folder *creation* flow, this owns the *page*: the
//! device / folder / conflict reads + the page-level write gestures, plus the
//! embedded wizard (`DevicesSnapshot.wizard`). It realizes the *Broader
//! DevicesSnapshot* the Devices target-state doc calls for.
//!
//! Under the `no-http-ws-rpc-everywhere` directive the production seam consumes
//! only WS-RPC kinds (`fauna.sync.devices.{list,delete}`, `fauna.folders.*`,
//! `fauna.sync.conflicts.*`) through the shared `fauna-client-{folders,sync}`
//! adapters — there is no HTTP impl.
//!
//! See `docs/goal/ui/devices.md` §§ State & data shape / Where logic lives.

// `DevicesNestApi`/`WizardFactory` are bounded by `MaybeSendSync` (`Send +
// Sync` natively, empty on wasm32), so an `Arc<dyn ...>`-holding type is
// correctly `!Send`/`!Sync` on wasm32 but trips `arc_with_non_send_sync`
// there. wasm32-scoped so native, where the bound resolves to `Send + Sync`,
// keeps the lint's protection.
#![cfg_attr(target_arch = "wasm32", allow(clippy::arc_with_non_send_sync))]

#[cfg(feature = "uniffi")]
uniffi::setup_scaffolding!("fauna_devices_machine");

pub mod keyless_posture;
pub mod lease_status;
pub mod machine;
pub mod nest_api;
pub mod observer;
pub mod p2p_participation;
pub mod places;
#[cfg(feature = "account-port")]
pub mod port;
pub mod snapshots;
pub mod this_device;

pub use machine::{
    DevicesMachine, FleetMembersView, FleetRemoval, FleetRemovalRefusal, FollowedFoldersSource,
    ForeignSetRow, ForeignSetsSource, MlsQuery, NestDeletion, P2pParticipation, UnaccountedMember,
    devices_refreshes_json,
};
#[cfg(feature = "rpc-glue")]
pub use nest_api::build_devices_machine;
// Both sources now exist on both targets (the wasm twin landed with web's
// follow surface), so these re-exports are plain `rpc-glue` like
// `build_devices_machine` — each arm's `pub use` picks its own concrete client.
pub use keyless_posture::keyless_posture;
pub use lease_status::folder_lease_status;
#[cfg(feature = "rpc-glue")]
pub use nest_api::ws_rpc::{CustodyForeignSetsSource, StoreFollowedFoldersSource};
pub use nest_api::{
    CandidateVerdict, ChosenWinner, DevicesApiError, DevicesNestApi, WizardFactory,
};
#[cfg(any(test, feature = "test-helpers"))]
pub use nest_api::{FakeCall, FakeDevicesNestApi, FakeWizardFactory};
pub use observer::DevicesObserver;
pub use p2p_participation::{P2pParticipationPaint, p2p_participation_paint};
pub use places::place_rows;
pub use snapshots::*;
pub use this_device::this_device_row;

// Re-export the wizard types clients render the embedded wizard from, so a
// consumer reaches the whole Devices surface through this one crate.
pub use fauna_folders_machine::{
    DeviceOption, FolderWizardMachine, FolderWizardObserver, FolderWizardSnapshot,
};
