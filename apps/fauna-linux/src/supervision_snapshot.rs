//! linux's storage half of the **last-known supervision snapshot** — clause 2
//! of `docs/goal/behavior/family-safety.md` § Content policy's unfetched-policy
//! ruling. The tui twin is `apps/fauna-tui/src/family.rs`
//! (`persist_supervision_snapshot` / `restore_supervision_snapshot`).
//!
//! **Every decision is shared Rust.** The record, its at-rest format, and the
//! fold from a successful `fauna.family.status` reply — including the
//! graduation gate that keys each pillar on `supervised_by` rather than on the
//! policy document — are [`fauna_client_family::SupervisionSnapshot`], and the
//! slot is [`fauna_client_accounts::AccountRegistry`]'s. This module holds only
//! the two linux-side bindings: which account the slot is keyed by, and the
//! `Option` plumbing around a store read.
//!
//! **Deliberately GTK-free.** Applying a loaded snapshot touches widgets (the
//! `family-tab` row, the `supervised-indicator`) and so lives at the post-auth
//! hydration site in `app.rs`, beside the read it runs ahead of.
//!
//! Where this is tested — and why not here — is stated at the bottom of the
//! file; the short version is that both functions reach the developer's real
//! keyring, and everything they do is pinned where it can be driven without
//! one.

use fauna_client_family::SupervisionSnapshot;

/// Persist `snapshot` for the active account, after a **successful** status
/// read only (clause 1 — a failed read never moves enforcement state).
///
/// A no-op when no account is active: the slot is keyed by actor, and the only
/// caller runs off an authenticated read anyway.
pub(crate) fn persist(snapshot: &SupervisionSnapshot) {
    let registry = crate::account_registry();
    let Some(actor_id) = registry.active() else {
        return;
    };
    registry.set_supervision_snapshot_json(&actor_id, &snapshot.to_json());
}

/// The active account's last-known snapshot, or `None` when this device has
/// never completed one successful status read for it (clause 3's declared
/// residual) — or when the slot is unreadable, which is the same "no
/// information" a failed read yields.
pub(crate) fn load() -> Option<SupervisionSnapshot> {
    let registry = crate::account_registry();
    let actor_id = registry.active()?;
    registry
        .supervision_snapshot_json(&actor_id)
        .as_deref()
        .and_then(SupervisionSnapshot::from_json)
}

// No unit tests here, deliberately, and the reason is a real constraint rather
// than an omission: both functions resolve their store through
// `crate::account_registry()` -> `client::secret_store()`, i.e. the developer's
// REAL libsecret namespace. The only isolation linux has is
// `FAUNA_E2E_CREDENTIAL_DIR`, a process-global env var this crate's own tests
// document as racy under the parallel runner, and the registry-construction
// census (`account_registry_census_test`) rightly forbids minting a registry
// over an in-memory store to get around that.
//
// Nothing is left uncovered by that. The storage behaviour is not linux's: the
// slot's per-account isolation and its sweep on account removal are pinned in
// `fauna-client-accounts`, and the record's format, the fold and its graduation
// gate in `fauna-client-family`. What IS linux's — that a reply naming no
// guardian yields nothing enforceable — is pinned on the pure producer, in
// `client.rs::family_status_loaded_tests`, which needs no store at all.
