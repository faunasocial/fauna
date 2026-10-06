//! Assembling a driveable client-device custodian — the owning bundle every
//! host shell builds, so *how* a custodian is put together exists once.
//!
//! [`CustodianPull`] is deliberately all borrows: it is one pass's view over a
//! source, a store and a nest surface, and borrowing is what lets a caller keep
//! those three wherever they already live. But **every** host has to own them
//! somewhere, and assembling them is not trivial — the byte-plane
//! [`SyncClient`], the `scope_id`/`seal_key` derivation, the `destination_id`
//! and cap taken from the device's own registry assignment. The desktop sync
//! agent did that inline; a second shell doing it inline again is how the two
//! drift, and the drift would be silent (a wrong `scope_id` seals a correct-
//! looking store nobody can restore from).
//!
//! So this module owns the assembly, and nothing else: no discovery loop, no
//! cadence, no lifecycle. Those genuinely differ per host and stay there —
//! the desktop agent runs a forever-loop with backoff and re-resolves its
//! assignment on a timer, while a mobile shell is construct-run-drop under an
//! OS scheduler (`../../docs/goal/behavior/backup-restore.md` § Background
//! Tasks). What must NOT differ is what a custodian *is*, which is this.
//!
//! # The two constructors, and why there are two
//!
//! [`CustodianHost::build`] is for a shell holding the **identity seed** (the
//! mobile apps): it derives the actor id from the seed and the `BackupKey` via
//! `BackupKey::derive`, exactly as the deleted upload coordinator's `build`
//! did, so no two derivations can disagree about which scope a device is
//! backing up.
//!
//! [`CustodianHost::from_parts`] is for a **bearer-only** host — the desktop
//! per-user sync agent, which deliberately never holds the seed
//! (`../../docs/goal/architecture/apps/sync-agent.md` § Credential model) and
//! therefore cannot derive anything: it is handed a `BackupKey` and an actor id
//! through its capability slot, and builds its own bearer-authed byte-plane
//! client. Giving it the seed-taking constructor would be exactly the wrong
//! shape — it has no seed to give.

use std::sync::Arc;

use anyhow::Result;
use fauna_client_backup::BackupClient;
use fauna_core::crypto::{BackupKey, OwnerSealKey};
use fauna_core::data::CustodianAssignment;
use tokio_util::sync::CancellationToken;

use crate::custodian_pull::{CustodianPull, PullReport};
use crate::custodian_store::CustodianStore;
use crate::nest_client::SyncClient;
use crate::segment_backup::SourceBinding;

/// Inputs for a **seed-holding** host — the mobile apps, which run the pull in
/// their own process and already hold the owner's identity seed.
///
/// The fields a shell already holds at app-init (`source_ws_client`,
/// `source_nest_url`, `secret`, `device_id`) — the same value set the deleted
/// upload coordinator's params took, so nothing new to plumb.
/// What replaces its `data_dir` + `destinations` is a *ready* [`CustodianStore`]
/// and this device's [`CustodianAssignment`]: the store because opening one
/// requires a stated [`crate::custodian_store::CloudBackupExclusion`] this crate
/// must not guess, and the assignment because *whether this device is a
/// custodian at all* is the caller's discovery step
/// ([`BackupClient::custodian_assignment`]), not a thing to re-derive here.
pub struct CustodianHostParams {
    /// The app's already-connected, already-authed WS-RPC client for the
    /// **source** (home) nest — the control plane and the check-in path.
    pub source_ws_client: Arc<fauna_client::NestClient>,
    /// Origin URL of the source nest, for the HTTP segment-byte client.
    pub source_nest_url: String,
    /// The owner's 32-byte Ed25519 secret seed. Reconstructs the signing keypair
    /// for the byte plane and derives the `BackupKey` this custodian seals
    /// under — a pull-only custodian seals *for itself* and needs no
    /// `NestBackupKey` grant (`../../docs/goal/behavior/backup-destinations.md` § Third
    /// destination kind).
    pub secret: [u8; 32],
    /// The owner's 32-byte device id — the same stable sync id enrollment
    /// recorded in the registry row this assignment came from.
    pub device_id: [u8; 32],
    /// This device's local sealed store, already opened with a stated
    /// cloud-backup exclusion.
    pub store: CustodianStore,
    /// This device's own registry row, as discovered by the caller.
    pub assignment: CustodianAssignment,
}

/// One device's custodian host: everything a pull pass borrows, owned.
///
/// Cheap to hold and safe to drive repeatedly — it owns no database and no
/// background task, so a scheduler-driven shell may construct one per pass and
/// drop it, exactly as android's periodic worker does with the upload
/// coordinator.
pub struct CustodianHost {
    source: SourceBinding,
    store: CustodianStore,
    /// The source nest's `fauna.backup.*` surface — the check-in path. Held
    /// built rather than made per pass because [`CustodianPull`] borrows it,
    /// and `BackupClient` is a newtype over the transport, so owning one costs
    /// an `Arc` clone.
    backup: BackupClient<Arc<fauna_client::NestClient>>,
    destination_id: String,
    scope_id: [u8; 32],
    seal_key: OwnerSealKey,
    cap_bytes: Option<u64>,
    /// The device this host *is*, taken from the assignment it was built from —
    /// so the check-in names the same device the match was made on.
    device_id: String,
    /// Fired by a pass whose check-in the nest refused as not assigned
    /// ([`CustodianPull::not_assigned`]); awaited through
    /// [`Self::not_assigned`].
    not_assigned: tokio::sync::Notify,
}

impl CustodianHost {
    /// Assemble from the owner's **seed** (the mobile shells).
    ///
    /// Derives `scope_id` and the `BackupKey` exactly as the upload coordinator
    /// does from the same seed, and builds a self-authenticating byte-plane
    /// client for the segment GETs.
    pub fn build(params: CustodianHostParams) -> Self {
        use fauna_client::AuthClient;
        use fauna_core::identity::ActorKeypair;

        let CustodianHostParams {
            source_ws_client,
            source_nest_url,
            secret,
            device_id,
            store,
            assignment,
        } = params;

        let scope_id = ActorKeypair::from_secret(secret).actor_id().0;
        let backup_key = BackupKey::derive(&secret);
        let auth = Arc::new(AuthClient::new(
            source_nest_url.clone(),
            ActorKeypair::from_secret(secret),
        ));

        Self::from_parts(
            SourceBinding {
                // The source nest's URL is the identifier a host actually has:
                // it never learns the nest's node pubkey. Matches the desktop
                // arm, so a log line reads the same on either.
                source_nest_id: source_nest_url,
                sync_client: Arc::new(SyncClient::new(auth, &device_id)),
                ws_client: Arc::clone(&source_ws_client),
            },
            source_ws_client,
            store,
            scope_id,
            OwnerSealKey::Client(backup_key),
            assignment,
        )
    }

    /// Assemble from already-built parts — the **bearer-only** desktop agent,
    /// which holds no seed and builds its own bearer-authed byte plane.
    pub fn from_parts(
        source: SourceBinding,
        ws: Arc<fauna_client::NestClient>,
        store: CustodianStore,
        scope_id: [u8; 32],
        seal_key: OwnerSealKey,
        assignment: CustodianAssignment,
    ) -> Self {
        Self {
            source,
            store,
            backup: BackupClient::new(ws),
            destination_id: assignment.destination_id,
            scope_id,
            seal_key,
            cap_bytes: assignment.capacity_cap_bytes,
            device_id: assignment.device_id,
            not_assigned: tokio::sync::Notify::new(),
        }
    }

    /// Resolves once a pass of this host has had its check-in refused because
    /// the source nest's registry no longer assigns this destination to this
    /// device (the typed `fauna.backup.custodian_not_assigned`). A refusal that
    /// lands while nobody is waiting is kept for the next call.
    ///
    /// It is the signal to re-read the assignment now and not at the next
    /// rediscovery; it is not the verdict. The desktop agent's watcher selects
    /// on it beside its timer and still ends the stint only on a successful
    /// read that disagrees (`docs/goal/architecture/apps/sync-agent.md` § A7).
    pub async fn not_assigned(&self) {
        self.not_assigned.notified().await;
    }

    /// The `destination_id` this host drives — the registry row's id, which is
    /// what a status row and a check-in are keyed on.
    pub fn destination_id(&self) -> &str {
        &self.destination_id
    }

    /// Borrow one pass's view. Private: every drive goes through the methods
    /// below, so no caller can assemble a pass with a mismatched scope or cap.
    fn pull(&self) -> CustodianPull<'_, SourceBinding, Arc<fauna_client::NestClient>> {
        CustodianPull {
            source: &self.source,
            store: &self.store,
            backup: &self.backup,
            destination_id: self.destination_id.clone(),
            scope_id: self.scope_id,
            seal_key: self.seal_key.clone(),
            cap_bytes: self.cap_bytes,
            device_id: Some(self.device_id.clone()),
            // The wire binding speaks both planes, so every host — desktop
            // agent and mobile shell alike — pulls covered folders with no
            // per-shell wiring (`backup-destinations.md` § Ordinary-folder
            // coverage).
            folder_source: Some(&self.source),
            not_assigned: Some(&self.not_assigned),
        }
    }

    /// One pass over every backed-up kind — the entry point for a shell whose
    /// cadence an OS scheduler owns (android `WorkManager`, iOS
    /// `BGProcessingTask`).
    pub async fn run_all_kinds(&self, now: i64) -> Vec<PullReport> {
        self.pull().run_all_kinds(now).await
    }

    /// The push-debounce loop only — the foreground wake for a scheduler-owned
    /// platform, which must **not** also run the periodic arm or it doubles the
    /// period the OS already drives
    /// (`../../docs/goal/behavior/backup-restore.md` § Background Tasks).
    ///
    /// Cancel on teardown: this loop does not self-exit when the source
    /// disconnects, because with no periodic tick nothing wakes it to notice.
    pub async fn run_push_debounce(&self, cancel: CancellationToken) -> Result<()> {
        self.pull().run_push_debounce(cancel).await
    }

    /// This store's standing self-audit verdict — what the *next* check-in will
    /// carry, and what the last one did.
    ///
    /// Read from the store's persisted record rather than from a pass's return
    /// value, for the same reason `CustodianPull::run_once` reads it there: the
    /// audit is debounced to `AUDIT_MIN_INTERVAL`, so most passes run none, and
    /// a caller that took "this pass did not audit" as "no verdict" would let a
    /// rotted store look healthy again on the very next pull.
    ///
    /// `None` is *never audited yet* — never a pass.
    pub async fn audit_verdict(&self) -> Option<fauna_client_backup::custodian::SelfAudit> {
        self.store.audit_record().await.verdict()
    }

    /// Periodic **and** push — the always-on desktop driver, where one driver
    /// owns both cadences.
    pub async fn run_forever(&self, cancel: CancellationToken) -> Result<()> {
        self.pull().run_forever(cancel).await
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use fauna_core::identity::ActorKeypair;

    const SEED: [u8; 32] = [7u8; 32];
    const DEVICE: [u8; 32] = [9u8; 32];

    fn assignment(cap: Option<u64>) -> CustodianAssignment {
        CustodianAssignment {
            destination_id: "dest-abc".to_string(),
            capacity_cap_bytes: cap,
            device_id: "dev-abc".to_string(),
        }
    }

    fn host_from_seed(cap: Option<u64>) -> CustodianHost {
        let ws = fauna_client::NestClient::new(
            "wss://nest.example".to_string(),
            ActorKeypair::from_secret(SEED),
        );
        CustodianHost::build(CustodianHostParams {
            source_ws_client: ws,
            source_nest_url: "wss://nest.example".to_string(),
            secret: SEED,
            device_id: DEVICE,
            store: CustodianStore::at(std::env::temp_dir().join("custodian-host-test")),
            assignment: assignment(cap),
        })
    }

    /// The seed-holding constructor derives the scope and the seal key exactly
    /// as the upload coordinator does from the same seed.
    ///
    /// This is the drift this module exists to prevent, and it is silent: a
    /// custodian sealing under a mismatched `scope_id` builds a store that looks
    /// correct on disk and that nothing can restore from, because the paths it
    /// keyed on name an actor the source never lists.
    #[test]
    fn build_derives_the_same_scope_and_seal_as_the_upload_coordinator() {
        let host = host_from_seed(None);

        assert_eq!(
            host.scope_id,
            ActorKeypair::from_secret(SEED).actor_id().0,
            "scope is the owner's actor id, derived the coordinator's way"
        );
        assert_eq!(
            host.seal_key.convergent_chunk_root(),
            BackupKey::derive(&SEED).convergent_chunk_root(),
            "a custodian seals for itself under the owner's BackupKey"
        );
    }

    /// The assignment's two fields reach the pass verbatim. Both are silent when
    /// wrong: a dropped cap makes an uncapped custodian of a capped one (the cap
    /// is the only thing between a custodian and a full disk), and a wrong
    /// `destination_id` check-ins against a row describing another device.
    #[test]
    fn the_assignment_reaches_the_pass_verbatim() {
        let capped = host_from_seed(Some(4_096));
        assert_eq!(capped.destination_id(), "dest-abc");
        assert_eq!(capped.cap_bytes, Some(4_096));

        let uncapped = host_from_seed(None);
        assert_eq!(
            uncapped.cap_bytes, None,
            "absent means uncapped — never a substituted zero, which would \
             report cap-reached forever having stored nothing"
        );
    }
}
