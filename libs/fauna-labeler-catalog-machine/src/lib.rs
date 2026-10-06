//! Page-level community-labeler-catalog state machine.
//!
//! Mirrors `fauna-devices-machine`: a shared-Rust `#[uniffi::Object]` state
//! machine (`LabelerCatalogMachine`) driving an observer-rendered page,
//! exposed to native apps via UniFFI (`libs/fauna-ffi`) and to web via
//! WASM. Browse (`fauna.labelers.list`), inspect-before-subscribe
//! (`fauna.labelers.inspect`), and (un)subscribe
//! (`fauna.labelers.{subscribe,unsubscribe}`) — the `labeler-catalog` page and
//! the personalization home's subscribed-labelers facet (which filters this
//! same snapshot to `subscribed == true`) both render off one
//! `LabelerCatalogSnapshot`.
//!
//! Under the `no-http-ws-rpc-everywhere` directive the production seam
//! consumes only the `fauna.labelers.*` WS-RPC kinds, through
//! `fauna-client-labelers::LabelersClient` — there is no HTTP impl.
//!
//! See `docs/goal/architecture/content-moderation-and-ranking.md` § Tier-3
//! community models & background re-processing.

#[cfg(feature = "uniffi")]
uniffi::setup_scaffolding!("fauna_labeler_catalog_machine");

pub mod machine;
pub mod nest_api;
pub mod observer;
pub mod snapshots;

pub use machine::{LabelerCatalogMachine, LabelerGrantSeams};
#[cfg(all(feature = "rpc-glue", not(target_arch = "wasm32")))]
pub use nest_api::build_labeler_catalog_machine;
#[cfg(feature = "rpc-glue")]
pub use nest_api::build_labeler_catalog_machine_with_grants;
#[cfg(any(test, feature = "test-helpers"))]
pub use nest_api::{FakeCall, FakeLabelerCatalogNestApi};
pub use nest_api::{InspectResult, LabelerCatalogApiError, LabelerCatalogNestApi};
pub use observer::LabelerCatalogObserver;
pub use snapshots::*;
