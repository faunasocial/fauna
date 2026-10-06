//! Folder creation wizard state machine for the Devices page.
//!
//! Mirrors `fauna-onboarding-machine`: a shared-Rust `#[uniffi::Object]` state
//! machine (`FolderWizardMachine`) driving an observer-rendered, multi-step
//! wizard, exposed to native apps via UniFFI (`libs/fauna-ffi`) and to web
//! via WASM (`libs/fauna-wasm-folders`). This realizes the `FolderWizardState`
//! the Devices target-state doc calls for; its serializable renderable form is
//! `FolderWizardSnapshot` (the type that slots into `DevicesSnapshot.wizard`).
//!
//! See `docs/goal/ui/devices.md` §§ State & data shape / Where logic lives.

// `FolderNestApi` is bounded by `MaybeSendSync` (`Send + Sync` natively,
// empty on wasm32), so an `Arc<dyn FolderNestApi>`-holding type is correctly
// `!Send`/`!Sync` on wasm32 but trips `arc_with_non_send_sync` there.
// wasm32-scoped so native, where the bound resolves to `Send + Sync`, keeps
// the lint's protection.
#![cfg_attr(target_arch = "wasm32", allow(clippy::arc_with_non_send_sync))]

#[cfg(feature = "uniffi")]
uniffi::setup_scaffolding!("fauna_folders_machine");

pub mod error;
pub mod machine;
pub mod nest_api;
pub mod nest_place;
pub mod observer;
pub mod on_demand_presence;
pub mod photo_library;
pub mod snapshots;
pub mod state;

pub use error::FolderWizardError;
pub use machine::FolderWizardMachine;
#[cfg(any(test, feature = "test-helpers"))]
pub use nest_api::FakeFolderNestApi;
#[cfg(feature = "rpc-glue")]
pub use nest_api::build_folder_wizard_machine;
pub use nest_api::{FolderApiError, FolderNestApi};
pub use nest_place::*;
pub use observer::FolderWizardObserver;
pub use snapshots::*;
pub use state::*;
