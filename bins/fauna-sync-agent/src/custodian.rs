//! Hosting this device's **client-device backup custodian** — slice 3d's
//! desktop arm (`docs/goal/behavior/backup-destinations.md` § State & data shape
//! → Third destination kind, the owner since the 2026-08-05 split out of
//! `ui/backups.md`; `docs/goal/architecture/message-segment-store.md`
//! § Client-device custodian).
//!
//! Every moving part of the pull is shared Rust
//! (`fauna_sync_engine::custodian_pull` + `custodian_store`). What lives here is
//! the three things the goal doc names as per-platform glue: **where the store
//! lives**, **the eviction posture**, and **the background-scheduling hook**.
//!
//! # Why the agent, and how it finds its own assignment
//!
//! The desktop host is this agent, not the app: a backup that only advances
//! while the user has a window open is not a backup, and
//! `behavior/backup-destinations.md` § Third destination kind puts desktop
//! stores under "the sync-agent's per-user data dir".
//!
//! But this process is **bearer-only** — it holds a `BackupKey`, an actor id and
//! a renewable bearer, and deliberately never the identity seed
//! (`sync-agent.md` § Credential model; `key-material-hierarchy.md` rules #6/#7).
//! So it cannot open the `fauna.state.backup` plane entries, where the authoritative
//! destination rows live, and cannot discover its own custodian row that way.
//!
//! It does not need to. `sync-agent.md` § Control plane split already settles
//! where a destination row is read from: *"policy through the nest (folders,
//! mode, cadence, retention, selective sync, **destinations** — nest rows,
//! edited from any client, read by the agent), **never over local IPC**"*. The
//! nest's destination registry is exactly that row, the enrollment already
//! writes this device's id and cap into it
//! (`fauna_client_config::enroll_client_custodian`), and this agent already has
//! an authed control plane to read it back with
//! ([`SyncServiceState::nest_rpc_client`]). Hence
//! [`BackupClient::custodian_assignment`] rather than a new IPC op — which would
//! also have cost the windows C# codec a wire-pinned variant + golden-hex regen,
//! for a row the nest already holds.
//!
//! A consequence worth stating, because it is the *reason* the rule reads that
//! way: the cap is the kind's only knob, "cap reached" means the backup has
//! **stopped**, and reading policy from the nest is what lets the owner raise it
//! from their phone and have this laptop pick it up. A provisioned-over-IPC copy
//! could only change while the app was running on this machine.

use std::sync::Arc;
use std::time::Duration;

use anyhow::{Context, Result};
use fauna_client_backup::BackupClient;
use fauna_core::crypto::{BackupKey, OwnerSealKey};
use fauna_core::data::CustodianAssignment;
use fauna_sync_engine::custodian_host::CustodianHost;
use fauna_sync_engine::custodian_store::{
    CloudBackupExclusion, CustodianStore, custodian_store_root,
};
use fauna_sync_engine::segment_backup::SourceBinding;
use tokio_util::sync::CancellationToken;

use crate::state::{IDLE_RECHECK_SECS, SyncServiceState};

/// How often a *running* driver re-reads its assignment from the registry, so a
/// cap the owner edited on another device reaches this host without a restart.
///
/// Reuses the ratified coordinator cadence by reference rather than declaring a
/// sibling constant: `behavior/backup-destinations.md` § Scheduling settles one cadence for every
/// destination tuple, and a second value here would be free to drift from it —
/// the same argument slice 3c's scheduling hook records.
fn rediscover_interval() -> Duration {
    fauna_sync_engine::segment_backup::PERIODIC_INTERVAL
}

/// Retry delay after a failed registry read. The read fails for the ordinary
/// reasons (laptop asleep, nest restarting, bearer mid-renewal), so this backs
/// off rather than hammering — but never gives up, because the alternative is a
/// custodian that silently stops pulling after one bad poll.
const READ_BACKOFF_START_SECS: u64 = 60;
const READ_BACKOFF_MAX_SECS: u64 = 900;

/// How this platform keeps the sealed store out of the OS's own cloud backup.
///
/// `Err` is the deliberate answer for a platform whose posture nobody has
/// established yet: [`CustodianStore::ensure_root`] takes no default and no
/// "unknown" arm, and the failure being prevented is invisible on the device —
/// a full sealed corpus replicated into the same vendor cloud that holds the
/// keychain with the seed to open it. A loud refusal to host is the honest
/// outcome; a guessed `NotApplicable` would be the silent one.
///
/// # Why macOS is `NotApplicable` and **not** `ExcludedByShell` (established
/// 2026-08-05, on a Mac)
///
/// macOS carried the refusal above until its posture was checked, and the note
/// it carried predicted the imperative arm: apple is paired with
/// `URL.setResourceValue(true, forKey: .isExcludedFromBackup)`. **Measured on
/// macOS 15, that call is a Time Machine exclusion, not a cloud one** — it
/// writes `com.apple.metadata:com_apple_backup_excludeItem` with the bplist
/// value `com.apple.backupd` (the Time Machine daemon), after which
/// `tmutil isexcluded` reports `[Excluded]`. Nothing about iCloud is touched.
/// That pairing is true of **iOS**, where the same key excludes from iCloud
/// Backup, which is why the FFI enum keeps `ExcludedByShell` for the mobile arm
/// and omits `NotApplicable` outright.
///
/// So wiring `ExcludedByShell` here would not have been harmless insurance: it
/// would answer a question the obligation never asked while **removing the
/// sealed corpus from the user's own local backups** — strictly reducing the
/// durability of a feature whose entire purpose is durability.
///
/// The cloud question itself has an honest negative answer on macOS, in the two
/// halves the smear needs:
///
/// 1. **The corpus cannot reach the vendor cloud.** macOS has no iCloud device
///    backup at all (the iOS/iPadOS feature has no macOS counterpart), and
///    iCloud Drive syncs `~/Library/Mobile Documents` (plus `~/Desktop` +
///    `~/Documents`, which it *relocates into* that root when the user enables
///    Desktop & Documents Folders). The store is
///    `~/Library/Application Support/Fauna/sync/<actor>/…` (the user-domain
///    root, `fauna_core::platform_ids::apple_user_domain_home` — moved out of
///    the app-group container 2026-08-25, `installers/macos.md` § Identifier
///    domain, item 5), which iCloud Drive never syncs. The app-group container
///    the File Provider extension still uses would reach iCloud Drive only if
///    a target published it with an iCloud/ubiquity entitlement or
///    `NSUbiquitousContainers`; **no Fauna target declares one**, and that is
///    *checked, not asserted*, by
///    [`tests::no_apple_target_publishes_its_container_to_icloud`] — the macOS
///    twin of android's `CloudBackupPostureTest`, so a target that later opts
///    into iCloud goes red here rather than silently invalidating this claim.
/// 2. **The seed is device-bound by default anyway.** `KeychainStore` writes
///    every credential row `kSecAttrAccessibleAfterFirstUnlockThisDeviceOnly` +
///    non-synchronizable (`apps/ios.md` § Credential Storage); the opt-in
///    "Back up identity to iCloud Keychain" toggle *adds* a circle copy rather
///    than moving one. This is a second, independent reason, not the load-bearing
///    one — half 1 stands on its own even for an opted-in user.
///
/// Time Machine is deliberately left alone: it is local (or a user's own network
/// volume), it is not the vendor cloud holding the keychain, and a sealed corpus
/// surviving a disk failure is the feature working.
///
/// **The arms themselves moved to shared Rust 2026-08-26** —
/// [`CloudBackupExclusion::platform_desktop`] — when the account store became
/// the second store to state its posture (its writer key is restore-excluded
/// on the phones, so its dir must be too; `apps/common.md` § Credential
/// storage). This binary delegates so its custodian host and its account host
/// cannot state the desktop differently; the measurement record above stays
/// here, where it was made.
pub(crate) fn cloud_backup_exclusion() -> Result<CloudBackupExclusion> {
    Ok(CloudBackupExclusion::platform_desktop())
}

/// Everything one driver stint needs, resolved from the capability slot.
struct HostInputs {
    nest_url: String,
    actor_id: [u8; 32],
    /// The capability's `device_id` verbatim — the string form the enrollment
    /// recorded in the registry, and therefore the form the match is made on.
    device_id: String,
    /// The same id decoded, as the byte plane's client wants it.
    device_id_bytes: [u8; 32],
    backup_key: [u8; 32],
}

/// Read the capability slot into [`HostInputs`], or `None` when nothing usable
/// is provisioned. A capability whose actor id, backup key or device id is
/// absent/malformed is *not* an error here — it is the pre-provision state as
/// seen mid-flight, and the next poll gets a better one.
///
/// Unlike [`crate::engine_driver`], a device id that does not hex-decode is a
/// refusal here rather than a zero fallback: this host would otherwise present
/// an all-zero device identity on the source nest's byte plane while claiming to
/// be the custodian of a row keyed on the real one.
async fn host_inputs(state: &SyncServiceState) -> Option<HostInputs> {
    let cap = state.capability.read().await;
    let cap = cap.as_ref()?;
    let device_id = cap.device_id.trim();
    if device_id.is_empty() {
        return None;
    }
    let device_id_bytes = hex::decode(device_id)
        .ok()
        .and_then(|v| <[u8; 32]>::try_from(v).ok())?;
    Some(HostInputs {
        nest_url: cap.nest_url.clone(),
        actor_id: cap.actor_id_array()?,
        device_id: device_id.to_string(),
        device_id_bytes,
        backup_key: cap.backup_key_array()?,
    })
}

/// Read this device's assignment from the source nest's destination registry.
async fn read_assignment(
    state: &SyncServiceState,
    device_id: &str,
) -> Result<Option<CustodianAssignment>> {
    let nest = state
        .nest_rpc_client()
        .await
        .map_err(|e| anyhow::anyhow!("{e}"))?;
    match BackupClient::new(nest.as_ref())
        .custodian_assignment(device_id)
        .await
    {
        Ok(found) => Ok(found),
        // No invalidation: the retained client reconnects on its own, and a
        // rebuild per failed read restarted the dial curve once a minute
        // (`SyncServiceState::nest_rpc`).
        Err(e) => Err(anyhow::anyhow!("{e}").context("fauna.backup.destination.list")),
    }
}

/// The custodian host task. Spawned by `run_agent` beside the renewal loop;
/// returns when `shutdown` flips.
///
/// Idles cheaply on every device that is not an enrolled custodian, which is
/// most of them: one capability read plus one registry read per minute.
pub async fn run(state: Arc<SyncServiceState>, mut shutdown: tokio::sync::watch::Receiver<bool>) {
    let mut backoff = fauna_protocol::reconnect::Backoff::new(
        Duration::from_secs(READ_BACKOFF_START_SECS),
        Duration::from_secs(READ_BACKOFF_MAX_SECS),
    );
    loop {
        // A running re-seed is this store's writer for now: host nothing until
        // it ends (the `reseed_job` slot's one-writer rule).
        let reseeding = matches!(
            *state.reseed_job.lock().await,
            fauna_ipc::sync::CustodianReseedState::Running
        );
        let delay = match host_inputs(&state).await {
            _ if reseeding => Duration::from_secs(1),
            None => Duration::from_secs(IDLE_RECHECK_SECS),
            Some(inputs) => match read_assignment(&state, &inputs.device_id).await {
                Err(e) => {
                    tracing::debug!(
                        error = %e,
                        retry_in_secs = backoff.ceiling().as_secs(),
                        "custodian: registry read failed; will retry"
                    );
                    let d = backoff.ceiling();
                    backoff.grow();
                    d
                }
                Ok(None) => {
                    backoff.reset();
                    Duration::from_secs(IDLE_RECHECK_SECS)
                }
                Ok(Some(assignment)) => {
                    backoff.reset();
                    // Blocks for as long as the assignment stands. Returns to
                    // re-resolve when the driver exits, the assignment changes,
                    // or the capability goes away.
                    if let Err(e) = host_stint(&state, &inputs, &assignment, &mut shutdown).await {
                        tracing::warn!(
                            destination_id = %assignment.destination_id,
                            error = %e,
                            "custodian: host stint ended with an error; will re-resolve"
                        );
                        Duration::from_secs(IDLE_RECHECK_SECS)
                    } else {
                        Duration::from_secs(1)
                    }
                }
            },
        };

        tokio::select! {
            _ = tokio::time::sleep(delay) => {}
            _ = shutdown.changed() => {
                if *shutdown.borrow() {
                    return;
                }
            }
        }
    }
}

/// Drive one assignment until it stops being the truth.
///
/// # The teardown rule this function exists to hold
///
/// [`CustodianPull::run_forever`] spawns two push-pump tasks and aborts them in
/// its own tail — but only on the path where it *returns*. Dropping the future
/// (letting a `select!` arm race it to completion) skips that tail entirely, and
/// the pumps survive as detached subscribers to a WS that this stint is done
/// with. Every rebuild would leak another pair. So the driver is **always** torn
/// down through its [`CancellationToken`], never by dropping it: the watcher
/// cancels and then both are awaited to completion.
async fn host_stint(
    state: &SyncServiceState,
    inputs: &HostInputs,
    assignment: &CustodianAssignment,
    shutdown: &mut tokio::sync::watch::Receiver<bool>,
) -> Result<()> {
    let exclusion = cloud_backup_exclusion()?;
    let actor_hex = hex::encode(inputs.actor_id);
    let root = custodian_store_root(&state.paths.flat_base_dir(), &actor_hex)
        .context("resolve custodian store root")?;
    let store = CustodianStore::ensure_root(root, exclusion).await?;

    let auth = crate::bearer::bearer_only_auth_client(
        Arc::clone(&state.capability),
        inputs.nest_url.clone(),
        inputs.actor_id,
    );
    let ws_client = state
        .nest_rpc_client()
        .await
        .map_err(|e| anyhow::anyhow!("{e}"))?;
    let source = SourceBinding {
        // Log/diagnostic context and a `sync_db` segment-state key the custodian
        // does not use (it keys off its own held index). The owner's own nest is
        // the source, and its URL is the identifier this process actually has —
        // it never learns the nest's node pubkey.
        source_nest_id: inputs.nest_url.clone(),
        sync_client: Arc::new(fauna_sync_engine::nest_client::SyncClient::new(
            auth,
            &inputs.device_id_bytes,
        )),
        ws_client: Arc::clone(&ws_client),
    };

    // Assembly is shared with the mobile hosts (`CustodianHost`), so what a
    // custodian *is* — scope, seal key, destination, cap — cannot drift between
    // this arm and theirs. `from_parts` rather than `build` because this process
    // is bearer-only and has no seed to derive from: it was handed the actor id
    // and `BackupKey` through its capability slot, and built the byte plane
    // above with a bearer source of its own.
    let host = CustodianHost::from_parts(
        source,
        Arc::clone(&ws_client),
        store,
        inputs.actor_id,
        OwnerSealKey::Client(BackupKey::from_bytes(inputs.backup_key)),
        assignment.clone(),
    );

    tracing::info!(
        destination_id = %assignment.destination_id,
        cap_bytes = ?assignment.capacity_cap_bytes,
        store = %state.paths.flat_base_dir().display(),
        "custodian: hosting this device's backup replica"
    );

    // Shared rather than moved, so the test-only pass poke can drive *this*
    // host — the one actually hosting — instead of assembling a second one over
    // a second store root. `run_forever` takes `&self`, so the driver below is
    // unchanged by the `Arc`.
    let host = std::sync::Arc::new(host);
    let cancel = CancellationToken::new();
    // Published with its token, not before it: the slot's whole contract is
    // that a reader holding a handle can stop the stint it names.
    publish_hosted_custodian(state, &host, &cancel).await;

    let driver = {
        let cancel = cancel.clone();
        let host = std::sync::Arc::clone(&host);
        async move {
            let outcome = host.run_forever(cancel.clone()).await;
            // The driver can also end on its own (the source disconnected and
            // both pumps died). Cancelling here is what releases the watcher.
            cancel.cancel();
            outcome
        }
    };
    let watcher = watch_assignment(state, inputs, assignment, &host, &cancel, shutdown);

    let (driver_outcome, _) = tokio::join!(driver, watcher);
    // Cleared on EVERY exit path, including the error one: the slot's emptiness
    // is the only "not hosting" signal, so a stint that ended while leaving it
    // full would let a poke drive a host whose source connection is already gone.
    clear_hosted_custodian(state).await;
    driver_outcome
}

/// Publish the running host — and the token that stops it — for the two
/// callers of [`SyncServiceState::hosted_custodian`]: the production
/// `ReclaimCustodianStore` handler, which must stop this writer before deleting
/// the store under it, and the test-only pass poke.
async fn publish_hosted_custodian(
    state: &SyncServiceState,
    host: &std::sync::Arc<CustodianHost>,
    cancel: &CancellationToken,
) {
    *state.hosted_custodian.lock().await = Some(crate::state::HostedCustodian {
        host: std::sync::Arc::clone(host),
        cancel: cancel.clone(),
    });
}

async fn clear_hosted_custodian(state: &SyncServiceState) {
    *state.hosted_custodian.lock().await = None;
}

/// Cancel the driver once its assignment stops being the truth: the cap or the
/// destination changed, the row was removed, or the capability went away.
///
/// A failed poll is *not* a reason to tear the driver down — a custodian that
/// stopped pulling because one registry read timed out would be exactly the
/// silent stop the check-in surface exists to make visible. Only a successful
/// read that disagrees ends the stint.
///
/// The re-check runs on the one rediscovery cadence, and at once on the two
/// signals that the answer has probably changed (`sync-agent.md` § A7): this
/// host's check-in refused as not assigned (the owner removed the destination,
/// or it now names another device), and a change to the capability slot
/// ([`recheck_now`]: a provision, an un-provision). Either signal only brings
/// the re-check forward; the re-check still decides.
async fn watch_assignment(
    state: &SyncServiceState,
    inputs: &HostInputs,
    current: &CustodianAssignment,
    host: &CustodianHost,
    cancel: &CancellationToken,
    shutdown: &mut tokio::sync::watch::Receiver<bool>,
) {
    watch_until_ended(
        || assignment_stands(state, inputs, current),
        || async {
            tokio::select! {
                () = host.not_assigned() => {}
                () = state.custodian_wake.notified() => {}
            }
        },
        cancel,
        shutdown,
    )
    .await;
}

/// Wake a running stint to re-check its capability and assignment now —
/// called wherever the capability slot changes, beside
/// [`crate::account_host::recheck_now`].
pub(crate) fn recheck_now(state: &SyncServiceState) {
    state.custodian_wake.notify_one();
}

/// One re-check of the stint's standing: `false` only on evidence that it
/// ended — the capability no longer names this device, or a successful
/// registry read disagrees with `current`. A failed read keeps it.
async fn assignment_stands(
    state: &SyncServiceState,
    inputs: &HostInputs,
    current: &CustodianAssignment,
) -> bool {
    let still_provisioned = host_inputs(state)
        .await
        .is_some_and(|now| now.device_id == inputs.device_id);
    if !still_provisioned {
        tracing::info!("custodian: capability changed; stopping this device's replica");
        return false;
    }
    match read_assignment(state, &inputs.device_id).await {
        Ok(Some(found)) if &found == current => true,
        Ok(found) => {
            tracing::info!(
                was = %current.destination_id,
                now = ?found.as_ref().map(|a| &a.destination_id),
                "custodian: assignment changed; re-resolving"
            );
            false
        }
        Err(e) => {
            tracing::debug!(
                error = %e,
                "custodian: assignment re-check failed; keeping the driver running"
            );
            true
        }
    }
}

/// [`watch_assignment`]'s loop, over its two effects, so the cadence rule is
/// testable without a nest: `stands` re-checks (false = the stint ended), and
/// `woken` resolves when something says to re-check now.
async fn watch_until_ended<S, SF, W, WF>(
    mut stands: S,
    mut woken: W,
    cancel: &CancellationToken,
    shutdown: &mut tokio::sync::watch::Receiver<bool>,
) where
    S: FnMut() -> SF,
    SF: std::future::Future<Output = bool>,
    W: FnMut() -> WF,
    WF: std::future::Future<Output = ()>,
{
    loop {
        let recheck = tokio::select! {
            _ = cancel.cancelled() => return,
            _ = shutdown.changed() => {
                if *shutdown.borrow() {
                    cancel.cancel();
                    return;
                }
                false
            }
            _ = tokio::time::sleep(rediscover_interval()) => true,
            () = woken() => true,
        };
        if recheck && !stands().await {
            cancel.cancel();
            return;
        }
    }
}

/// The re-seed job: run the shared ceremony driver over this device's store
/// against the nest this agent is provisioned for (`backup-destinations.md`
/// § Re-seed → *Where the ceremony runs*).
///
/// Everything it moves rides this process's own authed connections, the same
/// two a custodian stint uses: the WS control plane for the grant, the custody
/// records and the materialize, and the bearer byte plane for the chunks. After
/// a same-identity rebuild both already point at the rebuilt nest, because the
/// owner's sign-in there is what re-provisioned this agent.
///
/// The caller has stopped any stint and holds the `reseed_job` slot at
/// `Running`, so this is the store's only writer. The key lives in the caller's
/// `Zeroizing` copy and is dropped with the job.
pub(crate) async fn run_reseed(
    state: &Arc<SyncServiceState>,
    nest_key: &[u8; 32],
) -> fauna_ipc::sync::CustodianReseedState {
    use fauna_client_backup::reseed::run_reseed as drive;
    use fauna_core::crypto::NestBackupKey;
    use fauna_ipc::sync::CustodianReseedState;
    use fauna_sync_engine::reseed::{ReseedDelivery, SyncClientSink};

    let failed = |phase: &str, detail: String| CustodianReseedState::Failed {
        phase: phase.into(),
        detail,
    };
    let Some(inputs) = host_inputs(state).await else {
        return failed(
            "store",
            "this agent is not signed in to a nest yet; nothing was sent".into(),
        );
    };
    let root =
        match custodian_store_root(&state.paths.flat_base_dir(), &hex::encode(inputs.actor_id)) {
            Ok(root) => root,
            Err(e) => return failed("store", format!("resolve the backup store: {e}")),
        };
    // `at`, never `ensure_root`: a re-seed only reads the store, and must not
    // create one where this device never held a copy.
    let store = CustodianStore::at(root);

    let nest = match state.nest_rpc_client().await {
        Ok(nest) => nest,
        Err(e) => return failed("grant", format!("connect to the nest: {e}")),
    };
    let auth = crate::bearer::bearer_only_auth_client(
        Arc::clone(&state.capability),
        inputs.nest_url.clone(),
        inputs.actor_id,
    );
    let sync_client =
        fauna_sync_engine::nest_client::SyncClient::new(auth, &inputs.device_id_bytes);
    let sink = SyncClientSink {
        client: &sync_client,
    };
    let nest_backup_key = NestBackupKey::from_bytes(*nest_key);
    let mut leg = ReseedDelivery::new(
        &store,
        &sink,
        nest.as_ref(),
        inputs.actor_id,
        inputs.device_id.clone(),
        OwnerSealKey::Client(BackupKey::from_bytes(inputs.backup_key)),
        &nest_backup_key,
    );
    // The folder custody rows are writer-signed: the machine principal signs,
    // each folder's nonce from the agent's own content-key resolution — read
    // afresh, because the app created the target sets just before this job
    // (`writer-signed-change-records.md` ruling (7)(a)). No principal (not
    // enrolled with SyncWrite) or no resolution → the leg cannot sign, and the
    // driver holds each folder set `rehome_unsigned`.
    crate::content_keys::refresh_now(state).await;
    let nonces = state
        .content_keys
        .read()
        .await
        .as_ref()
        .filter(|r| r.is_for(inputs.actor_id))
        .map(|r| r.set_lineages_by_name());
    let signer = fauna_sync_engine::principal_bundle::load_change_signer(
        &fauna_sync_engine::account_runtime::production_credential_store(),
        &inputs.actor_id,
    );
    if let (Some(nonces), Some(signer)) = (nonces, signer) {
        leg = leg.with_record_signing(fauna_client_sync::RecordSigning {
            signer: Arc::new(signer),
            // The full lineage, serve window included — the one source every
            // agent reader takes (ruling (7)(b)(ii) rule (2)).
            set_nonce: fauna_client_sync::SetNonceSource::ByFolder(Arc::new(nonces)),
        });
    }

    tracing::info!("custodian: re-seeding the provisioned nest from this device's copy");
    let outcome = drive(&BackupClient::new(nest.as_ref()), nest_key.to_vec(), &leg).await;
    fauna_client_sync::reseed_wire::state_from_driver(outcome)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::bearer::tests::refusing_nest;

    /// **After the terminal refusal, nothing dials.** Renewal was answered
    /// `not_registered` on the store principal, so the slot's bearer is dropped
    /// (`renewal::refuse_renewal`). The control-plane client already running
    /// over it — its supervisor mid-curve — and every later once-a-minute read
    /// must then make no dial at all (one in-flight dial at the moment of the
    /// refusal is allowed: a cached bearer is only cleared by the 401 it
    /// earns). An app's next bearer re-arms it.
    #[tokio::test(start_paused = true)]
    async fn a_refused_renewal_dials_nothing_until_an_app_re_arms_it() {
        let (url, dials) = refusing_nest().await;
        let state = {
            let (shutdown_tx, _rx) = tokio::sync::watch::channel(false);
            let (event_tx, _) = tokio::sync::broadcast::channel(16);
            SyncServiceState::new(
                crate::config::SyncConfig::default(),
                shutdown_tx,
                event_tx,
                crate::config::SyncPaths::new(None),
            )
        };
        *state.capability.write().await = Some(fauna_ipc::sync::SyncCapability::new(
            vec![1u8; 32],
            vec![7u8; 32],
            url,
            "ab".repeat(32),
            fauna_ipc::sync::BearerToken::new("dead".into(), u64::MAX),
        ));
        // A live control-plane client, dialling on its curve.
        let _ = read_assignment(&state, "dev").await;
        tokio::time::sleep(Duration::from_secs(120)).await;
        assert!(dials.load(std::sync::atomic::Ordering::SeqCst) > 0);

        if let Some(cap) = state.capability.write().await.as_mut() {
            crate::bearer::mark_renewal_refused(cap);
        }
        let at_refusal = dials.load(std::sync::atomic::Ordering::SeqCst);
        for _ in 0..30 {
            assert!(read_assignment(&state, "dev").await.is_err());
            tokio::time::sleep(Duration::from_secs(60)).await;
        }
        let after = dials.load(std::sync::atomic::Ordering::SeqCst) - at_refusal;
        assert!(
            after <= 1,
            "{after} dials in the thirty minutes after the nest refused renewal for good"
        );

        // Re-armed by an app's bearer: dialling resumes on the curve.
        if let Some(cap) = state.capability.write().await.as_mut() {
            cap.bearer = fauna_ipc::sync::BearerToken::new("fresh".into(), u64::MAX);
        }
        let before = dials.load(std::sync::atomic::Ordering::SeqCst);
        tokio::time::sleep(Duration::from_secs(120)).await;
        assert!(
            dials.load(std::sync::atomic::Ordering::SeqCst) > before,
            "an app-pushed bearer must re-arm the connection"
        );
    }

    /// **The 2026-09-24 flood's shape, driven through the real code.** A machine
    /// whose credential has died keeps running the custodian loop, which asks the
    /// nest for its assignment once a minute over the retained control-plane
    /// client; every read fails. Each failure used to throw the client away
    /// (stranding its still-dialling supervisor) and the next read built a new
    /// one, so the agent ran one more reconnect loop per minute for as long as it
    /// lived — thousands against one nest after four days. However the reads go,
    /// the process must dial that nest on ONE backoff ladder: after the opening
    /// burst, about one dial per ceiling interval (a jittered draw under 60 s).
    #[tokio::test(start_paused = true)]
    async fn a_dead_credential_is_dialled_on_one_ladder_not_one_per_failed_read() {
        let (url, dials) = refusing_nest().await;
        let state = {
            let (shutdown_tx, _rx) = tokio::sync::watch::channel(false);
            let (event_tx, _) = tokio::sync::broadcast::channel(16);
            SyncServiceState::new(
                crate::config::SyncConfig::default(),
                shutdown_tx,
                event_tx,
                crate::config::SyncPaths::new(None),
            )
        };
        *state.capability.write().await = Some(fauna_ipc::sync::SyncCapability::new(
            vec![1u8; 32],
            vec![7u8; 32],
            url,
            "ab".repeat(32),
            fauna_ipc::sync::BearerToken::new("dead".into(), u64::MAX),
        ));

        let minutes = 30u64;
        for _ in 0..minutes {
            let read = read_assignment(&state, "dev").await;
            assert!(
                read.is_err(),
                "a nest refusing every upgrade answers no read"
            );
            tokio::time::sleep(Duration::from_secs(60)).await;
        }

        // One ladder over the window: the dial and its one re-mint retry, then
        // the curve — 1, 2, 4 … 32 s, then ~30 s on average at the 60 s ceiling.
        // Expected ≈ 70 over thirty minutes; a supervisor per failed read made
        // several hundred.
        let made = dials.load(std::sync::atomic::Ordering::SeqCst);
        let bound = 2 + 7 + (minutes * 60 / 30) as usize + 30;
        assert!(
            made <= bound,
            "{made} dials in {minutes} minutes against one nest; one backoff ladder makes at \
             most ~{bound}"
        );
    }

    /// A watcher over counting fakes: `stands` answers `verdict` and counts its
    /// calls; the returned `Notify` is the "re-check now" signal.
    struct Watch {
        cancel: CancellationToken,
        rechecks: Arc<std::sync::atomic::AtomicUsize>,
        wake: Arc<tokio::sync::Notify>,
        task: tokio::task::JoinHandle<()>,
        _shutdown: tokio::sync::watch::Sender<bool>,
    }

    fn spawn_watch(verdict: bool) -> Watch {
        let cancel = CancellationToken::new();
        let rechecks = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let wake = Arc::new(tokio::sync::Notify::new());
        let (shutdown_tx, mut shutdown) = tokio::sync::watch::channel(false);
        let task = {
            let (cancel, rechecks, wake) =
                (cancel.clone(), Arc::clone(&rechecks), Arc::clone(&wake));
            tokio::spawn(async move {
                watch_until_ended(
                    || {
                        rechecks.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                        async move { verdict }
                    },
                    || {
                        let wake = Arc::clone(&wake);
                        async move { wake.notified().await }
                    },
                    &cancel,
                    &mut shutdown,
                )
                .await;
            })
        };
        Watch {
            cancel,
            rechecks,
            wake,
            task,
            _shutdown: shutdown_tx,
        }
    }

    /// **The removal row's fix.** The owner removed this device's destination
    /// and the next pass's check-in came back refused as not assigned: the
    /// stint must end at that signal, not up to one rediscovery interval
    /// (15 min) later while every pass is refused and a new enrollment on this
    /// device waits unhosted. Time is paused, so the one-second bound is exact:
    /// without the wake arm, nothing happens before the interval.
    #[tokio::test(start_paused = true)]
    async fn a_wake_re_checks_at_once_and_a_disagreeing_answer_ends_the_stint() {
        let w = spawn_watch(false);
        w.wake.notify_one();

        tokio::time::timeout(Duration::from_secs(1), w.task)
            .await
            .expect("the stint must end on the wake, not at the next rediscovery tick")
            .unwrap();
        assert!(
            w.cancel.is_cancelled(),
            "the driver is stopped through its token"
        );
        assert_eq!(w.rechecks.load(std::sync::atomic::Ordering::SeqCst), 1);
    }

    /// The wake is a reason to look, never the verdict: a re-check that finds
    /// the assignment standing keeps the driver running, and the watcher goes
    /// back to its one cadence rather than re-checking in a loop.
    #[tokio::test(start_paused = true)]
    async fn a_wake_whose_re_check_agrees_keeps_the_stint() {
        let w = spawn_watch(true);
        w.wake.notify_one();
        tokio::time::sleep(Duration::from_secs(60)).await;

        assert!(!w.cancel.is_cancelled());
        assert!(!w.task.is_finished());
        assert_eq!(
            w.rechecks.load(std::sync::atomic::Ordering::SeqCst),
            1,
            "one wake, one re-check; the next is the rediscovery tick's"
        );
        w.cancel.cancel();
    }

    /// No second cadence (`backup-destinations.md` § Scheduling): with no wake,
    /// the watcher re-checks exactly on the rediscovery interval.
    #[tokio::test(start_paused = true)]
    async fn without_a_wake_the_re_check_rides_the_rediscovery_interval() {
        let w = spawn_watch(true);
        tokio::time::sleep(rediscover_interval() - Duration::from_secs(1)).await;
        assert_eq!(w.rechecks.load(std::sync::atomic::Ordering::SeqCst), 0);
        tokio::time::sleep(Duration::from_secs(2)).await;
        assert_eq!(w.rechecks.load(std::sync::atomic::Ordering::SeqCst), 1);
        w.cancel.cancel();
    }

    /// The three desktops whose posture has been established state it, and the
    /// claim names the platform so a reviewer can check it rather than take it.
    ///
    /// macOS joined them 2026-08-05 — see [`super::cloud_backup_exclusion`]'s
    /// docs for what was measured, and why the answer is `NotApplicable` rather
    /// than the `ExcludedByShell` the refusal note had predicted.
    #[cfg(any(target_os = "linux", windows, target_os = "macos"))]
    #[test]
    fn an_established_desktop_states_its_posture() {
        match cloud_backup_exclusion()
            .expect("linux/windows/macos must state a posture, not refuse")
        {
            CloudBackupExclusion::NotApplicable { platform } => {
                assert!(
                    !platform.trim().is_empty(),
                    "a bare 'nothing to do' is exactly what the enum refuses"
                );
                // The macOS posture names the store's root by a hard-coded
                // literal (`platform` is `&'static str`; `concat!` can't splice
                // a `const` in) — pin it against the shared resolver so the two
                // can't drift the way the pre-fork literal did, and
                // pin that it is NOT the app-group container, which the agent
                // may never open (2026-08-25).
                #[cfg(target_os = "macos")]
                {
                    let root = fauna_core::platform_ids::apple_user_domain_home(Some(
                        std::path::PathBuf::from("/Users/u"),
                    ));
                    let tail = root
                        .strip_prefix("/Users/u/")
                        .expect("resolver hangs off the passed home")
                        .to_string_lossy()
                        .into_owned();
                    assert!(
                        platform.contains(&format!("~/{tail}/Fauna/sync")),
                        "macOS posture string names the wrong store root: {platform}"
                    );
                    assert!(
                        !platform.contains("Group Containers/"),
                        "macOS posture string must not place the store in the container: {platform}"
                    );
                }
            }
            other => panic!("unexpected exclusion arm for this platform: {other:?}"),
        }
    }

    /// A platform nobody has established **refuses to host**, rather than
    /// guessing `NotApplicable`.
    ///
    /// This is the arm that matters: the failure it prevents is invisible on the
    /// device — a full sealed corpus replicated into the same vendor cloud that
    /// holds the keychain with the seed to open it — so a wrong "nothing to do"
    /// here would never be noticed, while a refusal is loud in the log and
    /// simply leaves the custodian unhosted.
    ///
    /// The set it guards shrank to the other unixes when macOS was established;
    /// it is deliberately *not* deleted, because the next desktop this agent is
    /// ported to must arrive at its own answer the same way.
    #[cfg(not(any(target_os = "linux", windows, target_os = "macos")))]
    #[test]
    fn an_unestablished_platform_refuses_to_host() {
        // Since 2026-08-26 the refusal is the posture's OWN apply-time arm
        // (`CloudBackupExclusion::platform_desktop`): stating it never fails,
        // honouring it does — at the one place every store already aborts on
        // a failed exclusion.
        let tmp = tempfile::tempdir().expect("temp dir");
        assert!(
            cloud_backup_exclusion()
                .expect("stating the posture is infallible")
                .apply(tmp.path())
                .is_err(),
            "a platform with no established posture must refuse, not guess"
        );
    }

    /// The posture is not just *stated* but **usable**: an established desktop
    /// resolves a store root and opens a store through it, which is the step
    /// [`host_stint`] aborted at for as long as this platform refused.
    ///
    /// Uses a temp root rather than the real per-user one — `ensure_root`
    /// creates what it is given, and a test must not touch the user's own
    /// corpus. What it proves is the *glue*: exclusion arm → `ensure_root` →
    /// a store, with no arm this platform cannot honour.
    #[cfg(any(target_os = "linux", windows, target_os = "macos"))]
    #[tokio::test]
    async fn an_established_desktop_opens_a_store_through_its_posture() {
        let tmp = tempfile::tempdir().expect("temp dir");
        let root = custodian_store_root(tmp.path(), &"ab".repeat(32))
            .expect("a 64-hex actor resolves a store root");
        let exclusion = cloud_backup_exclusion().expect("an established desktop states a posture");

        CustodianStore::ensure_root(root.clone(), exclusion)
            .await
            .expect("the stated posture must be one this platform can actually honour");

        assert!(
            root.is_dir(),
            "ensure_root must leave the store root in place at {}",
            root.display()
        );
    }

    /// macOS's `NotApplicable` rests on the app-group container not being
    /// published to iCloud. That is a property of the **apple targets'**
    /// entitlements, which this crate does not build and which a future apple
    /// session could change without ever opening this file — so it is checked
    /// here rather than asserted, exactly as android's `CloudBackupPostureTest`
    /// reads its merged manifest instead of trusting the claim.
    ///
    /// Runs on every platform (the entitlement files are tracked sources, not
    /// build products), so a target opting into iCloud goes red on the Linux
    /// merge gates too, not only on a Mac.
    ///
    /// Vendored/build directories are skipped — `.build/checkouts` holds
    /// third-party dependency source, which this project's review policy does
    /// not read, and `generated/` + `*.xcframework` are build products.
    #[test]
    fn no_apple_target_publishes_its_container_to_icloud() {
        /// Keys that would lift a container into iCloud Drive / iCloud Backup.
        const ICLOUD_KEYS: &[&str] = &[
            "com.apple.developer.icloud-container-identifiers",
            "com.apple.developer.ubiquity-container-identifiers",
            "com.apple.developer.icloud-services",
            "NSUbiquitousContainers",
        ];

        let apple = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../../apps/fauna-apple")
            .canonicalize()
            .expect("apps/fauna-apple resolves from this crate");

        let mut checked = 0usize;
        let mut stack = vec![apple.clone()];
        while let Some(dir) = stack.pop() {
            for entry in std::fs::read_dir(&dir).expect("read apple tree") {
                let path = entry.expect("dir entry").path();
                let name = path.file_name().unwrap_or_default().to_string_lossy();
                if path.is_dir() {
                    let vendored = name == ".build"
                        || name == "generated"
                        || name == ".swiftpm"
                        || name.ends_with(".xcframework");
                    if !vendored {
                        stack.push(path);
                    }
                    continue;
                }
                if !(name.ends_with(".entitlements") || name == "Info.plist") {
                    continue;
                }
                let body = std::fs::read_to_string(&path).unwrap_or_default();
                for key in ICLOUD_KEYS {
                    assert!(
                        !body.contains(key),
                        "{} declares `{key}`, which can publish the app-group container to \
                         iCloud — the custodian store lives in that container, and \
                         `cloud_backup_exclusion()` claims macos NotApplicable on the \
                         strength of no target doing this. Either drop the entitlement or \
                         re-establish the macOS posture (it may have to become \
                         ExcludedByShell, and note that on macOS that arm excludes from \
                         Time Machine, not iCloud).",
                        path.display()
                    );
                }
                checked += 1;
            }
        }

        assert!(
            checked >= 8,
            "expected the apple targets' entitlements + Info.plists to be found \
             (saw {checked}); a walk that silently finds nothing would pass this test \
             while checking nothing"
        );
    }
}
