//! Page-level Backups state machine — the **snapshot half** of the Backups page.
//!
//! Mirrors `fauna-devices-machine`: a shared-Rust `#[uniffi::Object]` state
//! machine ([`BackupsMachine`]) driving an observer-rendered page, exposed to
//! native apps via UniFFI (`libs/fauna-ffi`) and to web via WASM
//! (`libs/fauna-wasm-backups`). tui and linux consume this crate directly.
//!
//! Scope is deliberately the snapshot half only — the folder selector, the
//! snapshot list, and create / delete / prune / check. The *destination* half of
//! the same page (add/edit/remove destinations, per-destination status rows, the
//! audit-alert banners) keeps its existing shared seams
//! (`fauna_client_config::{enroll,edit,deregister}_backup_destination`,
//! `read_backup_status`), which were ratified and built earlier and are not
//! part of this machine.
//!
//! Under the `no-http-ws-rpc-everywhere` directive the production seam consumes
//! only WS-RPC kinds (`fauna.folders.list`,
//! `fauna.filesync.snapshot.{list,create_folder,delete,delete_immediate,prune_set_policy,check}`)
//! through the shared `fauna-client-{snapshots,folders}` adapters — there is no
//! HTTP impl.
//!
//! Three things the machine *derives* rather than reads, each reconciling a
//! measured six-app divergence (`docs/goal/ui/backups.md` § Snapshot-list shape):
//! `last_backed_up` (the selected set's newest snapshot `created_at` — not the
//! selector's cached column, and not `last_change_at`), per-row `integrity`
//! (from a check reply's `structured_errors`, never nest state), and the
//! single-flight `in_progress_op` gate.
//!
//! See `docs/goal/ui/backups.md` §§ Snapshot-list shape / Where logic lives.

// `BackupsNestApi` is bounded by `MaybeSendSync` (`Send + Sync` natively, empty
// on wasm32), so an `Arc<dyn ...>`-holding type is correctly `!Send`/`!Sync` on
// wasm32 but trips `arc_with_non_send_sync` there. wasm32-scoped so native,
// where the bound resolves to `Send + Sync`, keeps the lint's protection.
#![cfg_attr(target_arch = "wasm32", allow(clippy::arc_with_non_send_sync))]

#[cfg(feature = "uniffi")]
uniffi::setup_scaffolding!("fauna_backups_machine");

pub mod machine;
pub mod nest_api;
pub mod observer;
pub mod snapshots;

pub use machine::BackupsMachine;
#[cfg(feature = "rpc-glue")]
pub use nest_api::build_backups_machine;
pub use nest_api::{BackupsApiError, BackupsNestApi};
#[cfg(any(test, feature = "test-helpers"))]
pub use nest_api::{FakeBackupsNestApi, FakeCall};
pub use observer::BackupsObserver;
#[cfg(any(test, feature = "test-observer"))]
pub use observer::{CountingObserver, NullObserver};
pub use snapshots::*;
