//! Shared bridge-approval test fixture for the handler unit tests
//! (`#[cfg(test)] mod tests` blocks inside `src/*_handlers.rs`) — distinct
//! from `tests/common::approve_bridge`, the integration-test side's own
//! already-shared fixture (a different tier, reached only from `tests/*.rs`,
//! which does not see `cfg(test)` items in the library).
//!
//! `approve_bridge` was hand-copied byte-for-byte across
//! `bridge_caldav_handlers.rs`, `bridge_carddav_handlers.rs`,
//! `bridge_routing_handlers.rs`, `bridge_imap_handlers.rs`, and twice in
//! `bridge_blob_handlers.rs` (once verbatim, once as a parameterized
//! `approve_bridge_with_x25519` — a strict superset of the other five).

use crate::db::CacheDb;
use crate::db::bridge_service_users::BridgeRole;

/// Approve a bridge service user with a caller-chosen x25519 pubkey and
/// bridge id — the parameterized shape [`approve_bridge`] defaults.
pub(crate) async fn approve_bridge_with_x25519(
    db: &CacheDb,
    pk: &[u8; 32],
    role: BridgeRole,
    x25519_pubkey: &[u8; 32],
    bridge_id: &str,
) {
    db.create_pending_bridge_service_user(pk, role, bridge_id)
        .await
        .unwrap();
    db.upsert_bridge_x25519(pk, x25519_pubkey).await.unwrap();
    db.approve_bridge_service_user(pk, None).await.unwrap();
}

/// Approve a bridge service user with the standard fixture x25519 key
/// (`[9u8; 32]`) and bridge id (`"b1"`) — what every hand-copy actually used.
pub(crate) async fn approve_bridge(db: &CacheDb, pk: &[u8; 32], role: BridgeRole) {
    approve_bridge_with_x25519(db, pk, role, &[9u8; 32], "b1").await
}
