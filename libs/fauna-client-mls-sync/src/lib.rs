//! Cross-device MLS state-replica sync — the shared client half of the `__mls`
//! reserved folder (`docs/goal/behavior/devices.md` § Cross-device MLS
//! group-state sync; `docs/goal/behavior/file-sync.md` § MLS state replica;
//! design tracked internally).
//!
//! The **first and only** client consumer of the `fauna.mls.{get,put}` plane. It
//! pairs `fauna-mls`'s `state_replica` (the `provider` snapshot + three-way
//! merge) and `fauna-conversations`'s `store::history` (the per-channel
//! `history/<hex>` slice + commutative merge) with the transport + CAS + the
//! cross-device cursor, so the six app legs (slice 5) stay pure trigger glue.
//!
//! A leg constructs an [`MlsStateSync`] over its WS-RPC transport, then
//!
//! * on launch: `let r = mls.load().await?;` → restore `r.provider` into the
//!   `MlsEngine` (`ProviderReplica::restore_into`) + each `r.history` slice into
//!   the `ThreadStore` (`restore_channel_slice`), then resume each channel's poll
//!   from `mls.processed_seq(&channel)`;
//! * after an engine/store change (debounced): `mls.save_provider_if_changed(
//!   &ProviderReplica::from_engine(&engine)).await?` and, per touched channel,
//!   `mls.save_history_if_changed(&store.snapshot_channel_slice(id, hex,
//!   mls.processed_seq(&channel)).unwrap()).await?`.
//!
//! Mirrors `fauna-client-drafts` for `__drafts` (a [`seal`] module implementing
//! the exact at-rest pipeline the nest stores — zstd → ChaCha20-Poly1305 under
//! the owner's `BackupKey` — and a [`store`] module with the typed
//! `fauna.mls.{get,put}` call surface generic over the `fauna_protocol::RpcRequester`
//! seam), plus `fauna-client-config`'s CAS merge-retry (replica data is
//! user-irrecoverable, so the put carries the `fauna.mls.conflict` precondition
//! from day one). Replica blobs are owner-only (no signing). There is **no HTTP**
//! here.

// `MlsReplicaTransport` (and this crate's other seam traits) are bounded by
// `MaybeSendSync` (`Send + Sync` natively, empty on wasm32), so an
// `Arc<dyn MlsReplicaTransport>`-holding type is correctly `!Send`/`!Sync` on
// wasm32 (single-threaded, never crosses a real thread) but trips
// `arc_with_non_send_sync` there. wasm32-scoped so native, where the same
// bound resolves to `Send + Sync`, keeps the lint's protection.
#![cfg_attr(target_arch = "wasm32", allow(clippy::arc_with_non_send_sync))]

pub mod commit_gate;
pub mod gate_impl;
// The shared tokio *trigger* (the `MlsSyncLauncher` the FFI factory + tui
// inject) — native-only: the wasm leg's trigger is web's own chokepoint.
#[cfg(not(target_arch = "wasm32"))]
pub mod launcher;
pub mod orchestration;
mod seal;
pub mod store;
pub mod succession;
pub mod sync;
#[cfg(test)]
pub(crate) mod test_conv;
#[cfg(test)]
pub(crate) mod test_tracing;

pub use commit_gate::{CommitCatchUp, CommitGateError, GatedCommitSend};
pub use gate_impl::{BackendCatchUp, BackendChannelSend, FaunaCommitGate, MlsSyncCursor};
// Follows `mod launcher`'s own native-only gate above — the wasm leg has no
// tokio trigger to configure, so it has no `SuccessionReseal` to re-export.
#[cfg(not(target_arch = "wasm32"))]
pub use launcher::SuccessionReseal;
pub use orchestration::{
    ReplicaSnapshot, RestoreRetryEnd, SaveReplicaError, bind_restored_slice, restore_and_wire,
    restore_and_wire_with_retry, save_snapshot, snapshot_replica,
};
pub use seal::{ReplicaSealError, backup_key_from_seed, seal_replica, unseal_replica};
pub use store::{
    MlsReplicaClient, MlsReplicaClientError, MlsReplicaTransport, MlsTransportError, PATH_PROVIDER,
    PutOutcome, history_path, rpc_transport_get, rpc_transport_get_hash, rpc_transport_put,
};
pub use succession::{ReplicaResealOutcome, ReplicaResealProgress};
pub use sync::{AdoptedChannel, LoadedReplica, MlsStateSync};

// Re-export the at-rest key type so consumers depend on this crate's surface
// rather than reaching into `fauna_core::crypto` directly.
pub use fauna_core::crypto::BackupKey;
// Re-export the CAS base so a leg's concrete `MlsReplicaTransport` impl (its
// `put` signature names it) depends on this crate's surface rather than
// reaching into `fauna_protocol` — same rationale as `BackupKey` above.
pub use fauna_protocol::mls_replica::ReplicaBase;
