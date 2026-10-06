//! The nest's own task-delegation lease participation (slice 6 —
//! `docs/goal/behavior/participants.md` § Task delegation → *Policy order* +
//! *Dispatch by kind*): while this box holds a sufficient user-minted
//! capability grant for a nest-runnable task kind, the nest heartbeats that
//! `(owner, kind)` lease in its own [`LeaseRegistry`] as
//! `ParticipantClass::AlwaysOnNest`, so every app's Task-delegation page
//! shows the nest as the kind's runner and no client contends for it.
//!
//! # Why in-process, and why the nest never preempts
//!
//! The lease blackboard lives in this process, so the nest participates by
//! writing it directly — no wire kind, no self-connection. The design plan's
//! § 5 obligation ("a nest heartbeating a lease it runs … rides the
//! capability-grant authorization") is exactly the sufficiency scan below:
//! the nest claims a lease **only** while an approved, enrolled holder on
//! this box holds a live grant sufficient for the kind — holding the grant
//! IS the assignment, and revoking it unassigns (participants.md § Dispatch
//! by kind), which the runner makes prompt via [`LeaseRegistry::release`].
//!
//! Unlike a client, the nest cannot evaluate `current_candidates`: the
//! user's pins live in the client-sealed `fauna.state.delegation` entries, which the nest can never
//! read. Its lease policy is therefore deliberately minimal — renew its own
//! lease, claim a free or stale one, and **never preempt a fresh foreign
//! holder**. All pin/tier intelligence stays client-side (transport design
//! § 2: "all decisions are client-side"): tier-1 handover *into* the nest
//! happens by the eligible client yielding and the stale lease being claimed
//! here, not by the nest out-ranking anyone. The client learns the nest is
//! grant-holding from the observed lease holder's own `AlwaysOnNest` class —
//! see `fauna_client_delegation::LeaseCoordinator::step`, which explains why
//! that and not the client's grant-event log.
//!
//! # The two kinds, and their enforcement halves
//!
//! - [`KIND_CONTENT_RESCORE`] — sufficiency is a live capability grant
//!   ([`content_rescore_sufficient_owners`]); the enforcement half is the drain
//!   gate in `bridge_blob_handlers::rescore_worklist_handler`, which admits an
//!   owner's re-score obligations only while this nest may claim (or already
//!   holds) that owner's lease — the nest-side analogue of the client
//!   `LeaseCoordinator`'s `with_lease_gate`. In-flight work is never
//!   interrupted (`submit_scores` is ungated), matching the client gate's
//!   per-pass semantics.
//! - [`KIND_BACKUP_UPLOAD`] — sufficiency is a granted `NestBackupKey`, a
//!   registered destination, **and** passes that are not stalled
//!   ([`backup_upload_sufficient_owners`]). It needs no drain gate: the first
//!   two conjuncts are `NestBackupWorker`'s sweep set, and the third only ever
//!   *narrows* the lease relative to it — the worker deliberately keeps
//!   retrying an owner whose lease it released, which is what lets a recovered
//!   nest re-claim. A drain gate would instead stop the retries and make the
//!   release permanent.
//!
//! Each kind keeps its own held-lease memory ([`HeldLeases`]) — an owner can be
//! sufficient for one and not the other.

use std::collections::{HashMap, HashSet};
use std::sync::Arc;

use fauna_core::data::ParticipantRef;
use fauna_core::delegation::{
    HEARTBEAT_PERIOD_MS, KIND_BACKUP_UPLOAD, KIND_CONTENT_RESCORE, LEASE_STALE_MS, ParticipantClass,
};
use fauna_protocol::PushEvent;
use fauna_protocol::push_events::LeaseChangedPayload;

use crate::routes::AppState;

// The kinds this nest runs are the shared `fauna_core::delegation` constants —
// the one list the lease blackboard admits — never a nest-local copy. `index`
// is on that list too but is client-run only (`LIVE_TASK_KINDS`), so the nest
// never heartbeats it. `KIND_BACKUP_UPLOAD` is nest-run since the slice-5 flip
// (`docs/goal/behavior/backup-restore.md` § Background Tasks):
// `crate::segment_backup::NestBackupWorker` uploads every enrolled owner's
// segments with no client awake, so the nest must hold the lease that says so.

/// This nest as a lease participant, keyed by its Ed25519 identity pubkey —
/// the same key `router_status` serves as the nest id, so clients can
/// correlate the holder they observe with the nest they are talking to.
pub fn nest_self_ref(state: &AppState) -> ParticipantRef {
    ParticipantRef::Nest {
        actor_pubkey: state.nest_identity.public_key_bytes(),
    }
}

/// The owners for whom this box currently holds a grant sufficient to run
/// `content-rescore`: a live `content.read{mail}` **and** a
/// `content.label-write` (unscoped or mail-scoped) tuple, each declared in a
/// grant held by an approved enrolled bridge service user of this box — the
/// drain needs the former to unseal and the latter to submit, and the real
/// client mint issues both in one grant. Blob scope tuples are declared
/// plaintext metadata; the wrapped keys stay opaque to the nest (KMH rule #4
/// is untouched — sufficiency reads the declaration, never a key).
pub async fn content_rescore_sufficient_owners(state: &AppState) -> anyhow::Result<Vec<[u8; 32]>> {
    let now = crate::db::now_epoch_secs();
    let blobs = state
        .db
        .fetch_live_enrolled_capability_grant_blobs(now)
        .await?;
    // (has content.read{mail}, has content.label-write{None|mail}) per owner,
    // across all of the owner's grants (the scopes need not share one blob).
    let mut acc: HashMap<[u8; 32], (bool, bool)> = HashMap::new();
    for bytes in &blobs {
        let Ok(blob) = fauna_mls::wrapped_blob::GrantBlob::from_canonical_bytes(bytes) else {
            continue;
        };
        // Holding the grant IS the assignment, so this is an authorization and
        // owes the whole window — the storage filter above can only say "not
        // expired". Without this, a post-dated grant would make
        // this box claim a lease before its window opens.
        if !fauna_mls::wrapped_blob::grant_window_is_open(&blob, now) {
            continue;
        }
        let Ok(owner): Result<[u8; 32], _> = blob.index.0.as_slice().try_into() else {
            continue;
        };
        let entry = acc.entry(owner).or_default();
        for sc in &blob.scope {
            if sc.class == "content.read" && sc.kind.as_deref() == Some("mail") {
                entry.0 = true;
            }
            if sc.class == "content.label-write"
                && (sc.kind.is_none() || sc.kind.as_deref() == Some("mail"))
            {
                entry.1 = true;
            }
        }
    }
    let mut owners: Vec<[u8; 32]> = acc
        .into_iter()
        .filter(|(_, (read, write))| *read && *write)
        .map(|(owner, _)| owner)
        .collect();
    owners.sort_unstable();
    Ok(owners)
}

/// The owners this nest currently backs up — i.e. for whom it may claim the
/// [`KIND_BACKUP_UPLOAD`] lease. Three conjuncts, **capability and progress**:
///
/// 1. a granted `NestBackupKey` (the seal grant, without which the nest cannot
///    produce openable backups);
/// 2. at least one registered push destination
///    ([`is_push_destination`](crate::segment_backup::is_push_destination) —
///    without which there is nowhere to send them);
/// 3. the owner's passes are **not stalled** —
///    [`BackupPassHealth`](crate::segment_backup::BackupPassHealth): fewer than
///    `MAX_CONSECUTIVE_FAILED_PASSES` consecutive wholly-failed sweeps.
///
/// **Why (3) exists**. (1)+(2) are
/// *configuration-shaped*: a nest whose every pass fails keeps satisfying them
/// and heartbeats forever, and since the slice-5 flip a client observing a fresh
/// `AlwaysOnNest` holder stands down — so nobody backs the owner up while the
/// Task-delegation row says the nest does. (3) makes the predicate
/// progress-shaped, and because
/// [`run_pass_for_kind`] *releases* on lost sufficiency the handover is prompt
/// rather than a `LEASE_STALE_MS` wait.
///
/// **(1)+(2) still stay exactly [`NestBackupWorker`](crate::segment_backup::NestBackupWorker)'s
/// sweep set** — `run_once` walks `list_nest_backup_key_owners()` and skips any
/// owner whose `open_for_owner` yields no push destination — so
/// this set can only ever be a **subset** of what the worker sweeps. That
/// direction is the safe one: claiming a lease for an owner the worker skips
/// would show the nest running backups it is not running, while the reverse
/// (the worker still retrying an owner whose lease it released) is exactly what
/// lets a stalled nest recover and re-claim.
pub async fn backup_upload_sufficient_owners(state: &AppState) -> anyhow::Result<Vec<[u8; 32]>> {
    let owner_ids = state.db.list_nest_backup_key_owners().await?;
    let mut owners = Vec::with_capacity(owner_ids.len());
    for owner_bytes in owner_ids {
        let Ok(owner) = <[u8; 32]>::try_from(owner_bytes.as_slice()) else {
            // Mirrors the worker, which warns and skips a malformed row rather
            // than failing the whole sweep.
            tracing::warn!(
                owner = %hex::encode(&owner_bytes),
                "delegation runner: skipping malformed owner id in the backup-key store"
            );
            continue;
        };
        let has_destination = state
            .db
            .list_backup_destinations(&owner)
            .await?
            .iter()
            .any(crate::segment_backup::is_push_destination);
        if has_destination && !state.backup_pass_health.is_stalled(&owner) {
            owners.push(owner);
        }
    }
    owners.sort_unstable();
    Ok(owners)
}

/// Whether the nest may claim (or already holds) the `(owner, task_kind)`
/// lease **right now**: free, stale, or self-held ⇒ yes; a fresh foreign
/// holder ⇒ no (the never-preempt rule — see the module doc).
pub fn may_claim(state: &AppState, owner: [u8; 32], task_kind: &str) -> bool {
    let self_ref = nest_self_ref(state);
    let observed = state
        .delegation_leases
        .observe(owner, &[task_kind.to_string()])
        .into_iter()
        .next();
    // A fresh foreign holder blocks the claim; free, stale, or self-held allow it.
    !matches!(observed, Some(lease) if lease.age_ms < LEASE_STALE_MS && lease.holder != self_ref)
}

/// Heartbeat the `(owner, task_kind)` lease as this nest, pushing the
/// best-effort `lease_changed` re-observe nudge to the owner's connections on
/// a holder change (first claim / takeover) — renews stay silent, exactly as
/// the wire handler behaves.
pub fn claim(state: &AppState, owner: [u8; 32], task_kind: &str) {
    // `None` only for a kind off the shared list — never one the runner names,
    // since it names only `fauna_core::delegation::KIND_*`.
    let Some((_, holder_changed)) = state.delegation_leases.heartbeat(
        owner,
        task_kind,
        nest_self_ref(state),
        ParticipantClass::AlwaysOnNest,
    ) else {
        tracing::warn!(
            kind = task_kind,
            "delegation runner: refused to claim an unlisted kind"
        );
        return;
    };
    if holder_changed {
        push_lease_changed(state, owner, task_kind);
    }
}

fn push_lease_changed(state: &AppState, owner: [u8; 32], task_kind: &str) {
    state.ws.notify_push(
        &owner,
        PushEvent::LeaseChanged(LeaseChangedPayload {
            task_kind: task_kind.to_string(),
            extra: Default::default(),
        }),
    );
}

/// One runner pass for **one kind**: heartbeat every claimable sufficient
/// `(owner, task_kind)` lease, and **release** (with a push) any lease this nest
/// held for an owner whose sufficiency lapsed — a revoke frees the kind
/// immediately instead of after [`LEASE_STALE_MS`]. `held` is the runner's
/// memory of the owners it heartbeated for this kind on the previous pass, so
/// each kind keeps its own set (an owner can be sufficient for one and not the
/// other — e.g. enrolled for backup with no content grant).
///
/// `owners` is that kind's sufficiency set, already scanned by the caller.
pub fn run_pass_for_kind(
    state: &AppState,
    task_kind: &str,
    owners: &[[u8; 32]],
    held: &mut HashSet<[u8; 32]>,
) {
    let sufficient: HashSet<[u8; 32]> = owners.iter().copied().collect();
    let self_ref = nest_self_ref(state);

    // Sufficiency lost (grant revoked / expired / holder disenrolled / last
    // destination removed): hand the kind back promptly. `release` is
    // holder-checked, so a lease another participant has since taken over is
    // left alone.
    for owner in held.iter() {
        if !sufficient.contains(owner)
            && state
                .delegation_leases
                .release(*owner, task_kind, &self_ref)
        {
            push_lease_changed(state, *owner, task_kind);
        }
    }
    held.retain(|owner| sufficient.contains(owner));

    for owner in owners {
        if may_claim(state, *owner, task_kind) {
            claim(state, *owner, task_kind);
            held.insert(*owner);
        } else {
            // A fresh foreign holder (e.g. a client-side runner the user
            // pinned) — stand by; we are not the holder.
            held.remove(owner);
        }
    }
}

/// The runner's memory across passes: the owners whose lease this nest holds,
/// per kind. Kept separate because sufficiency is per-kind.
#[derive(Default)]
pub struct HeldLeases {
    content_rescore: HashSet<[u8; 32]>,
    backup_upload: HashSet<[u8; 32]>,
}

/// One runner pass over **every** nest-run kind: scan each kind's sufficiency
/// set and reconcile its leases. A scan failure skips that kind only — a
/// transient error reading one store must not stop the nest heartbeating the
/// other kind's leases (which would make an unrelated Task-delegation row go
/// stale).
pub async fn run_pass(state: &AppState, held: &mut HeldLeases) {
    match content_rescore_sufficient_owners(state).await {
        Ok(owners) => run_pass_for_kind(
            state,
            KIND_CONTENT_RESCORE,
            &owners,
            &mut held.content_rescore,
        ),
        Err(e) => {
            tracing::warn!(error = %e, kind = KIND_CONTENT_RESCORE, "delegation runner: sufficiency scan failed");
        }
    }
    match backup_upload_sufficient_owners(state).await {
        Ok(owners) => {
            run_pass_for_kind(state, KIND_BACKUP_UPLOAD, &owners, &mut held.backup_upload)
        }
        Err(e) => {
            tracing::warn!(error = %e, kind = KIND_BACKUP_UPLOAD, "delegation runner: sufficiency scan failed");
        }
    }
}

/// The lease runner loop: an immediate first pass, then one per
/// [`HEARTBEAT_PERIOD_MS`] — or sooner when `delegation_runner_wake` is
/// poked (the mint/renew/revoke handlers do, so a fresh grant is claimed and
/// a revoke released without waiting out a period). Spawned once from
/// `build_app`; lease slots are in-memory, so a nest restart simply re-claims
/// on the first pass.
pub async fn run(state: Arc<AppState>) {
    let mut held = HeldLeases::default();
    let period = std::time::Duration::from_millis(HEARTBEAT_PERIOD_MS);
    let mut interval = tokio::time::interval(period);
    interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
    loop {
        tokio::select! {
            _ = interval.tick() => {}
            _ = state.delegation_runner_wake.notified() => {}
        }
        run_pass(&state, &mut held).await;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use fauna_mls::wrapped_blob::{GrantBlob, GrantIndex, GrantWindow, ScopeTuple};

    fn scope(class: &str, kind: Option<&str>) -> ScopeTuple {
        ScopeTuple {
            class: class.into(),
            kind: kind.map(Into::into),
            tier: None,
            set: None,
            factor: None,
        }
    }

    fn grant_blob(
        owner: &[u8; 32],
        grant_id: u8,
        holder: &[u8; 32],
        scopes: Vec<ScopeTuple>,
    ) -> Vec<u8> {
        GrantBlob {
            version: 1,
            kind: GrantBlob::KIND.to_string(),
            index: GrantIndex(owner.to_vec(), vec![grant_id; 16]),
            holder: serde_bytes::ByteBuf::from(holder.to_vec()),
            window: GrantWindow(0, u64::MAX),
            scope: scopes,
            wrapped_keys: Vec::new(),
        }
        .to_canonical_bytes()
        .expect("encode grant blob")
    }

    async fn state_with_approved_holder(holder: &[u8; 32]) -> Arc<AppState> {
        use crate::db::bridge_service_users::BridgeRole;
        let db = Arc::new(crate::db::CacheDb::open_in_memory().unwrap());
        let state = Arc::new(AppState::for_test(db));
        // Enroll + approve a content-processor service user carrying the
        // holder x25519 pubkey the grants are sealed to.
        let cp = [0xAAu8; 32];
        state
            .db
            .create_pending_bridge_service_user(&cp, BridgeRole::ContentProcessor, "cp-1")
            .await
            .unwrap();
        state.db.upsert_bridge_x25519(&cp, holder).await.unwrap();
        state
            .db
            .approve_bridge_service_user(&cp, None)
            .await
            .unwrap();
        state
    }

    fn full_scopes() -> Vec<ScopeTuple> {
        vec![
            scope("content.read", Some("mail")),
            scope("content.label-write", None),
        ]
    }

    /// `backup-upload` sufficiency — the nest-run half of the slice-5 flip
    /// (backup-restore.md § Flip status (slice 5)). The predicate must be a
    /// **subset** of the set `NestBackupWorker` sweeps, and never a superset:
    /// claiming the lease for an owner the worker skips is just as wrong as
    /// claiming none at all. The proper-subset case is the stall conjunct
    /// (below) — the worker deliberately keeps retrying an owner whose
    /// lease it released.
    mod backup_upload_sufficiency {
        use super::*;
        use crate::segment_backup::{MAX_CONSECUTIVE_FAILED_PASSES, PassOutcome};

        fn all_failed() -> PassOutcome {
            PassOutcome {
                attempted: 2,
                failed: 2,
            }
        }

        fn partly_failed() -> PassOutcome {
            PassOutcome {
                attempted: 2,
                failed: 1,
            }
        }

        /// Drive `n` wholly-failed sweeps for `owner`, as the worker would.
        fn fail_passes(state: &AppState, owner: &[u8; 32], n: u32) {
            for _ in 0..n {
                state.backup_pass_health.record_pass(*owner, all_failed());
            }
        }

        async fn plain_state() -> Arc<AppState> {
            let db = Arc::new(crate::db::CacheDb::open_in_memory().unwrap());
            Arc::new(AppState::for_test(db))
        }

        async fn enroll(state: &AppState, owner: &[u8; 32]) {
            state
                .db
                .put_nest_backup_key(owner, &[9u8; 32])
                .await
                .unwrap();
        }

        async fn register_destination(state: &AppState, owner: &[u8; 32], id: &str) {
            state
                .db
                .put_backup_destination(owner, id, "wss://dest.example", &[5u8; 32])
                .await
                .unwrap();
        }

        #[tokio::test]
        async fn needs_both_a_seal_grant_and_a_destination() {
            let state = plain_state().await;
            let key_only = [0x11u8; 32];
            let dest_only = [0x22u8; 32];
            let both = [0x33u8; 32];

            enroll(&state, &key_only).await;
            register_destination(&state, &dest_only, "d-1").await;
            enroll(&state, &both).await;
            register_destination(&state, &both, "d-2").await;

            let owners = backup_upload_sufficient_owners(&state).await.unwrap();
            assert_eq!(
                owners,
                vec![both],
                "a NestBackupKey with nowhere to send it, and a destination with no key to seal \
                 under, are each insufficient — the worker backs up neither"
            );
        }

        #[tokio::test]
        async fn ignores_a_destination_the_push_sweep_does_not_dial() {
            // `NestBackupCoordinator::open_for_owner` keeps only push
            // destinations (`segment_backup::is_push_destination`), so a
            // client-device custodian — pulled by its device, never dialled —
            // must not make the owner look sufficient here either.
            let state = plain_state().await;
            let owner = [0x44u8; 32];
            enroll(&state, &owner).await;
            state
                .db
                .put_backup_destination_of_kind(
                    &owner,
                    "d-device",
                    "",
                    &[],
                    fauna_core::data::DESTINATION_KIND_CLIENT_DEVICE,
                    Some("dev-abc"),
                    None,
                )
                .await
                .unwrap();

            assert!(
                backup_upload_sufficient_owners(&state)
                    .await
                    .unwrap()
                    .is_empty(),
                "only push destinations count"
            );
        }

        #[tokio::test]
        async fn a_pass_claims_the_backup_lease_and_releases_it_when_the_last_destination_goes() {
            // The defect this whole track closes: before it, an enrolled owner
            // being backed up by `NestBackupWorker` saw `backup-upload` with no
            // runner at all on the Task-delegation page.
            let state = plain_state().await;
            let owner = [0x66u8; 32];
            enroll(&state, &owner).await;
            register_destination(&state, &owner, "d-1").await;

            let mut held = HeldLeases::default();
            run_pass(&state, &mut held).await;
            let leases = state
                .delegation_leases
                .observe(owner, &[KIND_BACKUP_UPLOAD.to_string()]);
            assert_eq!(leases.len(), 1, "the nest claimed the free backup lease");
            assert_eq!(leases[0].holder, nest_self_ref(&state));
            assert_eq!(leases[0].holder_class, ParticipantClass::AlwaysOnNest);
            assert!(held.backup_upload.contains(&owner));
            assert!(
                held.content_rescore.is_empty(),
                "no content grant ⇒ the other kind is untouched; the two sets are independent"
            );

            // Removing the last destination un-assigns the kind on the next pass
            // — the same promptness the capability revoke gets.
            state
                .db
                .delete_backup_destination(&owner, "d-1")
                .await
                .unwrap();
            run_pass(&state, &mut held).await;
            assert!(
                state
                    .delegation_leases
                    .observe(owner, &[KIND_BACKUP_UPLOAD.to_string()])
                    .is_empty(),
                "losing the last destination frees the lease"
            );
            assert!(held.backup_upload.is_empty());
        }

        #[tokio::test]
        async fn a_pass_never_preempts_a_fresh_client_backup_driver() {
            // windows / apple / android still ship in-app drivers during the
            // flip window. A pinned one holding the lease fresh must be left
            // alone — the nest never preempts (participants.md:59).
            let state = plain_state().await;
            let owner = [0x77u8; 32];
            enroll(&state, &owner).await;
            register_destination(&state, &owner, "d-1").await;

            let desktop = ParticipantRef::Device {
                device_id: "dev-a".into(),
            };
            state.delegation_leases.heartbeat(
                owner,
                KIND_BACKUP_UPLOAD,
                desktop.clone(),
                ParticipantClass::PluggedInDesktop,
            );

            let mut held = HeldLeases::default();
            run_pass(&state, &mut held).await;
            let leases = state
                .delegation_leases
                .observe(owner, &[KIND_BACKUP_UPLOAD.to_string()]);
            assert_eq!(
                leases[0].holder, desktop,
                "the nest must defer to a fresh foreign holder"
            );
            assert!(held.backup_upload.is_empty());
        }

        #[tokio::test]
        async fn a_revoked_seal_grant_drops_the_owner() {
            let state = plain_state().await;
            let owner = [0x55u8; 32];
            enroll(&state, &owner).await;
            register_destination(&state, &owner, "d-1").await;
            assert_eq!(
                backup_upload_sufficient_owners(&state).await.unwrap(),
                vec![owner]
            );

            state.db.delete_nest_backup_key(&owner).await.unwrap();
            assert!(
                backup_upload_sufficient_owners(&state)
                    .await
                    .unwrap()
                    .is_empty(),
                "revoking the seal grant makes the nest insufficient, which is what \
                 releases the lease (participants.md:59 — holding the grant IS the assignment)"
            );
        }

        // ── sufficiency is progress-shaped, not only configuration-shaped ──

        #[tokio::test]
        async fn a_nest_whose_every_pass_fails_stops_being_sufficient() {
            // The defect: grant + destination are configuration. A nest
            // that never succeeds keeps satisfying them, heartbeats forever, and
            // — since the slice-5 flip — every app stands down on seeing the
            // fresh AlwaysOnNest holder. Nobody backs the owner up.
            let state = plain_state().await;
            let owner = [0x88u8; 32];
            enroll(&state, &owner).await;
            register_destination(&state, &owner, "d-1").await;

            fail_passes(&state, &owner, MAX_CONSECUTIVE_FAILED_PASSES - 1);
            assert_eq!(
                backup_upload_sufficient_owners(&state).await.unwrap(),
                vec![owner],
                "below the threshold the nest keeps the kind — a transient blip must not \
                 park it on whichever client picks it up (the nest cannot preempt back)"
            );

            fail_passes(&state, &owner, 1);
            assert!(
                backup_upload_sufficient_owners(&state)
                    .await
                    .unwrap()
                    .is_empty(),
                "a sustained run of wholly-failed passes hands the kind back"
            );
        }

        #[tokio::test]
        async fn one_reachable_destination_keeps_the_nest_sufficient() {
            // Any success resets: a nest reaching one of several destinations is
            // making progress, and a client takeover would not fix the
            // destination that is actually broken.
            let state = plain_state().await;
            let owner = [0x89u8; 32];
            enroll(&state, &owner).await;
            register_destination(&state, &owner, "d-1").await;

            fail_passes(&state, &owner, MAX_CONSECUTIVE_FAILED_PASSES);
            state.backup_pass_health.record_pass(owner, partly_failed());

            assert_eq!(
                backup_upload_sufficient_owners(&state).await.unwrap(),
                vec![owner],
                "a partly-successful pass clears the stall"
            );
        }

        #[tokio::test]
        async fn a_stalled_owners_lease_is_released_not_merely_left_to_go_stale() {
            // The handover has to be prompt. `run_pass_for_kind` releases on lost
            // sufficiency (holder-checked, plus a lease_changed push), so a client
            // sees a FREE lease immediately instead of waiting out LEASE_STALE_MS
            // — no wall-clock anywhere in this assertion (convention 14).
            let state = plain_state().await;
            let owner = [0x8au8; 32];
            enroll(&state, &owner).await;
            register_destination(&state, &owner, "d-1").await;

            let mut held = HeldLeases::default();
            run_pass(&state, &mut held).await;
            assert!(held.backup_upload.contains(&owner), "claimed while healthy");

            fail_passes(&state, &owner, MAX_CONSECUTIVE_FAILED_PASSES);
            run_pass(&state, &mut held).await;

            assert!(
                state
                    .delegation_leases
                    .observe(owner, &[KIND_BACKUP_UPLOAD.to_string()])
                    .is_empty(),
                "the stalled nest RELEASED the lease — a client can claim it now, rather \
                 than after LEASE_STALE_MS of a lease the nest is not honouring"
            );
            assert!(held.backup_upload.is_empty());
        }

        #[tokio::test]
        async fn a_recovered_nest_reclaims_the_kind() {
            // Why the worker must keep sweeping a released owner: if it stood
            // down too, nothing would ever re-establish success and the release
            // would be permanent.
            let state = plain_state().await;
            let owner = [0x8bu8; 32];
            enroll(&state, &owner).await;
            register_destination(&state, &owner, "d-1").await;

            let mut held = HeldLeases::default();
            fail_passes(&state, &owner, MAX_CONSECUTIVE_FAILED_PASSES);
            run_pass(&state, &mut held).await;
            assert!(held.backup_upload.is_empty(), "released while stalled");

            state
                .backup_pass_health
                .record_pass(owner, PassOutcome::default());
            state.backup_pass_health.record_pass(
                owner,
                PassOutcome {
                    attempted: 1,
                    failed: 0,
                },
            );
            run_pass(&state, &mut held).await;

            let leases = state
                .delegation_leases
                .observe(owner, &[KIND_BACKUP_UPLOAD.to_string()]);
            assert_eq!(
                leases.len(),
                1,
                "a recovered nest claims the free lease back"
            );
            assert_eq!(leases[0].holder, nest_self_ref(&state));
            assert!(held.backup_upload.contains(&owner));
        }
    }

    #[tokio::test]
    async fn sufficiency_needs_read_and_label_write_from_an_approved_holder() {
        let holder = [0x33u8; 32];
        let state = state_with_approved_holder(&holder).await;
        let read_only_owner = [0x11u8; 32];
        let full_owner = [0x22u8; 32];
        state
            .db
            .put_capability_grant(
                &read_only_owner,
                &[1u8; 16],
                &holder,
                i64::MAX,
                &grant_blob(
                    &read_only_owner,
                    1,
                    &holder,
                    vec![scope("content.read", Some("mail"))],
                ),
            )
            .await
            .unwrap();
        state
            .db
            .put_capability_grant(
                &full_owner,
                &[2u8; 16],
                &holder,
                i64::MAX,
                &grant_blob(&full_owner, 2, &holder, full_scopes()),
            )
            .await
            .unwrap();

        let owners = content_rescore_sufficient_owners(&state).await.unwrap();
        assert_eq!(owners, vec![full_owner], "read-only grant is insufficient");
    }

    #[tokio::test]
    async fn sufficiency_ignores_a_grant_to_an_unenrolled_holder() {
        let holder = [0x33u8; 32];
        let state = state_with_approved_holder(&holder).await;
        let stranger = [0x44u8; 32]; // never registered/approved
        let owner = [0x11u8; 32];
        state
            .db
            .put_capability_grant(
                &owner,
                &[1u8; 16],
                &stranger,
                i64::MAX,
                &grant_blob(&owner, 1, &stranger, full_scopes()),
            )
            .await
            .unwrap();
        let owners = content_rescore_sufficient_owners(&state).await.unwrap();
        assert!(
            owners.is_empty(),
            "unenrolled holder cannot make the box sufficient"
        );
    }

    #[tokio::test]
    async fn pass_claims_sufficient_owner_and_releases_on_revoke() {
        let holder = [0x33u8; 32];
        let state = state_with_approved_holder(&holder).await;
        let owner = [0x11u8; 32];
        state
            .db
            .put_capability_grant(
                &owner,
                &[1u8; 16],
                &holder,
                i64::MAX,
                &grant_blob(&owner, 1, &holder, full_scopes()),
            )
            .await
            .unwrap();

        let mut held = HeldLeases::default();
        run_pass(&state, &mut held).await;
        let leases = state
            .delegation_leases
            .observe(owner, &[KIND_CONTENT_RESCORE.to_string()]);
        assert_eq!(leases.len(), 1, "the nest claimed the free lease");
        assert_eq!(leases[0].holder, nest_self_ref(&state));
        assert_eq!(leases[0].holder_class, ParticipantClass::AlwaysOnNest);
        assert!(held.content_rescore.contains(&owner));

        // Revoke → the next pass releases the lease immediately (no 90 s wait).
        state
            .db
            .delete_capability_grant(&owner, &[1u8; 16])
            .await
            .unwrap();
        run_pass(&state, &mut held).await;
        assert!(
            state
                .delegation_leases
                .observe(owner, &[KIND_CONTENT_RESCORE.to_string()])
                .is_empty(),
            "revoking the grant frees the lease"
        );
        assert!(held.content_rescore.is_empty());
    }

    #[tokio::test]
    async fn pass_never_preempts_a_fresh_foreign_holder() {
        let holder = [0x33u8; 32];
        let state = state_with_approved_holder(&holder).await;
        let owner = [0x11u8; 32];
        state
            .db
            .put_capability_grant(
                &owner,
                &[1u8; 16],
                &holder,
                i64::MAX,
                &grant_blob(&owner, 1, &holder, full_scopes()),
            )
            .await
            .unwrap();

        // A (future) client runner holds the lease fresh.
        let desktop = ParticipantRef::Device {
            device_id: "dev-a".into(),
        };
        state.delegation_leases.heartbeat(
            owner,
            KIND_CONTENT_RESCORE,
            desktop.clone(),
            ParticipantClass::PluggedInDesktop,
        );

        let mut held = HeldLeases::default();
        run_pass(&state, &mut held).await;
        let leases = state
            .delegation_leases
            .observe(owner, &[KIND_CONTENT_RESCORE.to_string()]);
        assert_eq!(
            leases[0].holder, desktop,
            "the nest must defer to a fresh foreign holder"
        );
        assert!(held.content_rescore.is_empty());
    }
}
