//! The account store — the local replica floor of the account data plane.
//!
//! Target state: `docs/goal/architecture/account-data-plane.md` § The account
//! store + § Store logical schema. This crate is the W1 (account-data-plane.md § Workstreams) slice-1 floor: the
//! extracted [`db`] state-dir/SQLite layer (formerly `fauna_sync_engine::db`;
//! `fauna-sync-engine` re-exports it so no call site re-paths), joined by the
//! store's own journal / state-entry / frontier / store-meta planes behind a
//! trait-abstracted physical backend. No consumer wiring happens here — W3
//! inverts read paths later.
//!
//! Crate-level invariants (from the extraction contract,
//! `app-guidelines.md` § crate layering, and the charter):
//!
//! - **Dependency floor:** `fauna-core` + `rusqlite` + `anyhow` plus
//!   foundation facades (`tracing`, `serde`, `serde_json`); on wasm32 the
//!   platform's storage bindings (`web-sys`/`js-sys`/`wasm-bindgen`) stand
//!   where `rusqlite` stands natively. Never the engine graph
//!   (tokio/reqwest/notify/MLS) — a change needing one is a layering question
//!   to raise, not a dep to add.
//! - **Key-less operation is first-class** (charter § Replica posture, R7 (account-data-plane.md § The ratified decisions)):
//!   every API in this crate opens and operates with no read keys — value and
//!   block bytes are opaque; projections (the only key-bearing plane) live
//!   above this crate and are never load-bearing for store integrity.
//! - **wasm32 compiles the seam:** the [`db`] floor and the SQLite backend are
//!   native-only; the store types and backend trait compile for
//!   `wasm32-unknown-unknown` so web's IndexedDB/OPFS backend can implement
//!   the same store API (charter § Store logical schema, Physical realization).
//!
//! **The physical arms** behind [`backend::StoreBackend`]:
//!
//! - `sqlite` — the native replica: one SQLite DB (WAL) per actor plus a
//!   segment file area beside it. Native-only.
//! - `indexeddb` — web's replica: the same logical schema over IndexedDB
//!   object stores, adopted segment bytes in OPFS. wasm32-only.
//! - `memory` — a **test double, a wasm-compilation proof, and the
//!   box-recovery cold read's throwaway replica; never a shipped one**
//!   (`account-client-lifecycle.md` ruling (3)). Ungated for that read; a
//!   replica an account runtime keeps is always one of the two arms above.
//!
//! One suite grades every arm: `conformance` (gated
//! `cfg(any(test, feature = "test-helpers"))`), instantiated per
//! arm natively and, for the web arm, in a browser through `wasm-bindgen-test`.

pub mod backend;
#[cfg(any(test, feature = "test-helpers"))]
pub mod conformance;
#[cfg(not(target_arch = "wasm32"))]
pub mod db;
#[cfg(target_arch = "wasm32")]
pub mod indexeddb;
#[cfg(not(target_arch = "wasm32"))]
pub mod locks;
/// Web's leg of the store's locks, under the same path and names as the
/// native leg (the engine-singleton role over the Web Locks API).
#[cfg(target_arch = "wasm32")]
#[path = "locks_web.rs"]
pub mod locks;
pub mod memory;
mod physical;
#[cfg(not(target_arch = "wasm32"))]
pub mod root;
/// Web's leg of the store root, under the same path and names as the native
/// leg: a per-origin, per-actor store name in place of a directory.
#[cfg(target_arch = "wasm32")]
#[path = "root_web.rs"]
pub mod root;
pub mod segments;
#[cfg(not(target_arch = "wasm32"))]
pub mod sqlite;
pub mod store;
#[cfg(not(target_arch = "wasm32"))]
pub mod succession_drain;
#[cfg(not(target_arch = "wasm32"))]
pub mod succession_progress;
pub mod types;
