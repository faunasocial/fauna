//! UniFFI façade for the `fauna.drafts.{get,put}` kinds — the native-client
//! `__drafts` persistence seam the draft-persistence v2 client legs drive. Two
//! handles over the shared `fauna-client-drafts` crate (both seal under the
//! owner's `BackupKey` and call `fauna.drafts.{get,put}` over the shared
//! `NestClient`); the native twins of the Rust-native Linux app reaching the
//! crate directly:
//!
//! * [`FfiDraftsClient`] — the thin, stateless per-call `load(path)` / `save(path,
//!   bytes)` over `DraftsClient`. Used by the windows leg (a C#-side launch-gate +
//!   debounce wraps it).
//! * [`FfiDraftsSync`] — the **canonical** stateful wrapper over `DraftsSync`
//!   (launch gate + last-saved baseline), built once per session and held, so the
//!   per-app leg stays pure trigger glue (restore on launch + a debounced
//!   save). Used by the android leg; macos/ios consume it next, and the windows
//!   convergence follow-up moves windows onto it too (reserved-folders.md § Drafts Sync).
//!
//! Construct via [`crate::nest_client::FfiNestClient::drafts`] /
//! [`crate::nest_client::FfiNestClient::drafts_sync`]; the methods are exposed to
//! Swift as `async throws` and Kotlin as `suspend fun`.
//!
//! Per-app glue is just the trigger; the seal + the WS call are NOT
//! re-implemented per client (`docs/goal/behavior/reserved-folders.md` § Drafts Sync).
//! The behavioural round-trip (seal → `fauna.drafts.put` → `fauna.drafts.get` →
//! unseal, byte-equal; `None` on an empty rail; independent paths) and the
//! `DraftsSync` gate/baseline are owned + tested by the shared
//! `fauna-client-drafts` crate (`store.rs` / `sync.rs` tests) and exercised
//! end-to-end by the tier_3 e2e; these wrappers are thin transport-bound
//! delegations with no logic of their own beyond the `DraftsClientError` →
//! `FfiError` mapping.

use std::sync::Arc;

use fauna_client::NestClient;
use fauna_client_drafts::{DraftsClient, DraftsSync};
use fauna_core::identity::ActorKeypair;

use crate::{FfiError, general_err};

/// `fauna_client_drafts::autosave_debounce` → the shared draft-autosave
/// debounce window in whole milliseconds, so every door-crossing app's
/// composer coalesces edits on the same cadence as the Rust-native shells
/// (priority #2 — one shared window, no per-app drift;
/// `docs/goal/behavior/reserved-folders.md` § Drafts Sync). `u32`, matching
/// the web `autosaveDebounceMs` face's return shape.
///
/// It reads the shared accessor rather than the constant behind it, so the
/// harness's window seam reaches apple, android and windows through the one
/// door they already call — see that function for why the seam lengthens the
/// window rather than shortening it. Called per schedule, so the answer is
/// live rather than latched at launch.
#[uniffi::export]
pub fn autosave_debounce_ms() -> u32 {
    fauna_client_drafts::autosave_debounce().as_millis() as u32
}

/// UniFFI handle for the `fauna.drafts.{get,put}` kinds. Thin wrapper over the
/// shared `fauna_client_drafts::DraftsClient` bound to this connection's
/// `NestClient` and the owner's derived `BackupKey`. Cheap to hold (a transport
/// handle + the derived key); construct one per actor via
/// [`FfiNestClient::drafts`](crate::nest_client::FfiNestClient::drafts).
#[derive(uniffi::Object)]
pub struct FfiDraftsClient {
    inner: DraftsClient<Arc<NestClient>>,
}

impl FfiDraftsClient {
    pub(crate) fn from_nest(nest: Arc<NestClient>, keypair: &ActorKeypair) -> Arc<Self> {
        Arc::new(Self {
            inner: DraftsClient::new(nest, keypair),
        })
    }
}

#[fauna_uniffi_async::export]
impl FfiDraftsClient {
    /// `fauna.drafts.get` — fetch + unseal the calling actor's draft blob for
    /// `path` (e.g. `"conversations"`). `None` when the actor has never persisted
    /// drafts for this rail (first run) — the caller keeps its empty store. A
    /// present-but-undecryptable blob is a hard error, never masked as "no drafts".
    /// Hand the returned bytes to `ConversationsManager.restore_drafts_at`, with
    /// the identity epoch read before this call.
    pub async fn load(&self, path: String) -> Result<Option<Vec<u8>>, FfiError> {
        self.inner.load(&path).await.map_err(general_err)
    }

    /// `fauna.drafts.put` — seal + persist `snapshot_bytes` (from
    /// `ConversationsManager.drafts_snapshot_bytes`) for the calling actor's
    /// `path` rail, overwriting any prior blob. Idempotent overwrite, so an
    /// unchanged draft set re-uploads with no effect.
    pub async fn save(&self, path: String, snapshot_bytes: Vec<u8>) -> Result<(), FfiError> {
        self.inner
            .save(&path, &snapshot_bytes)
            .await
            .map_err(general_err)
    }
}

/// UniFFI handle for one actor's per-rail draft *autosync* — the **canonical**
/// stateful layer above [`FfiDraftsClient`]. Wraps the shared
/// `fauna_client_drafts::DraftsSync` (the launch gate + last-saved baseline), so
/// it must be built once at login and held for the session: a fresh instance per
/// call would re-close the gate and lose the dedup baseline. Where
/// [`FfiDraftsClient`] is a thin per-call `load`/`save`, this owns the two
/// no-data-loss safety properties (never PUT before the launch GET succeeds;
/// don't re-upload an unchanged set), so the per-app leg stays pure trigger
/// glue. Construct via
/// [`FfiNestClient::drafts_sync`](crate::nest_client::FfiNestClient::drafts_sync).
#[derive(uniffi::Object)]
pub struct FfiDraftsSync {
    sync: DraftsSync<Arc<NestClient>>,
}

impl FfiDraftsSync {
    /// Build over the live connection's transport + identity, for one rail
    /// (`"conversations"`; later `"posts"` / `"events"`). The at-rest `BackupKey`
    /// is derived inside the wrapped `DraftsClient` from the connection secret —
    /// the same secret the WS connection authenticated with (mirrors
    /// `FfiCaldavClient`'s keypair rebuild from `nest.auth()`).
    pub(crate) fn from_nest(nest: Arc<NestClient>, rail: String) -> Arc<Self> {
        Arc::new(Self {
            sync: drafts_sync_from_nest(nest, rail),
        })
    }
}

/// The `DraftsSync` half of [`FfiDraftsSync::from_nest`] / [`crate::event_drafts::
/// FfiEventDraftsSync::from_nest`] — both rebuild the same connection-derived
/// keypair the same way, differing only in which rail they hand `DraftsSync`.
/// Infallible: called only on an already-authenticated connection, where the
/// keypair is always present.
pub(crate) fn drafts_sync_from_nest(
    nest: Arc<NestClient>,
    rail: String,
) -> DraftsSync<Arc<NestClient>> {
    let secret = *nest
        .auth()
        .keypair()
        .expect("identity keypair required for drafts")
        .secret_bytes();
    let keypair = ActorKeypair::from_secret(secret);
    DraftsSync::new(nest, &keypair, rail)
}

#[fauna_uniffi_async::export]
impl FfiDraftsSync {
    /// Fetch + unseal this rail's persisted drafts for the launch / cross-device
    /// catch-up (`fauna.drafts.get`). Returns the canonical snapshot bytes the
    /// caller hands to `ConversationsManager.restoreDraftsAt` (with the
    /// `identityEpoch()` it read before this call), or `None` on first
    /// run. Records the baseline + lifts the save gate; a transport/seal failure
    /// leaves the gate closed (so a later `save_if_changed` can't clobber the
    /// unread blob) and surfaces the error string.
    pub async fn load(&self) -> Result<Option<Vec<u8>>, FfiError> {
        self.sync.load().await.map_err(general_err)
    }

    /// Seal + persist `snapshot` for this rail (`fauna.drafts.put`) **iff** a
    /// launch [`load`](Self::load) has succeeded *and* `snapshot` differs from the
    /// last-saved baseline. Returns `true` when it wrote, `false` when skipped (the
    /// pre-load gate or an unchanged set). The client glue calls this from a
    /// debounce with `ConversationsManager.draftsSnapshotBytes()`.
    pub async fn save_if_changed(&self, snapshot: Vec<u8>) -> Result<bool, FfiError> {
        self.sync
            .save_if_changed(&snapshot)
            .await
            .map_err(general_err)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The exported window IS the shared constant — a door-crossing app that
    /// re-declared its own literal would silently decouple from the value every
    /// other app reads (the failure mode `reserved-folders.md` § Drafts Sync
    /// names this face to end).
    #[test]
    fn autosave_debounce_mirrors_the_shared_constant() {
        assert_eq!(
            autosave_debounce_ms() as u128,
            fauna_client_drafts::AUTOSAVE_DEBOUNCE.as_millis()
        );
    }
}
