//! The lived-in recovery, composed over a native app's own connections — the
//! one place every native shell's recovery gesture is wired
//! (`segment-backup-protocol.md` § Client-device custodian (pull) → *Restore*
//! → *Recovery into the lived-in nest that regressed*, part (1): "the owner's
//! app drives … the leg runs in the app; no custodian store is touched").
//!
//! The order and the result are the shared driver's
//! ([`fauna_client_backup::recover::run_recovery`]); the leg is the shared
//! [`fauna_client_backup::recover_leg::RecoveryDelivery`], run as the native
//! `Send` [`crate::reseed::NativeRecoveryLeg`] (web composes the same leg in
//! `fauna-wasm`'s `backupRecoverFromDestination`). What this module adds is only the
//! seams a native app supplies: the owner's own authenticated connection to the
//! destination — held to the identity the owner enrolled, the same refusal
//! `fauna_client_backup::trust::connect_destination` makes — the destination's
//! open hash-verified byte routes, the source's owner-session byte plane, and
//! this device's audit store, whose notice record names the sets to recover
//! and is cleared by the leg's duty.
//!
//! **Two compositions, one leg.** [`recover_from_destination`] reads the copy
//! off a nest destination and runs in the app. A client-device custodian
//! carries no address (`ui/backups.md` § Audit-alert surface, the client-device
//! arm), so its copy is read off this device's own store instead —
//! [`recover_from_own_store`], run in the process that owns the store (the
//! desktop sync agent's store job, the phone's in-process call;
//! *Recovery from this device's own store*, question (2)) — and
//! [`recover_from_destination`] refuses a client-device row by name.

use std::sync::Arc;

use fauna_client::NestClient;
use fauna_client_backup::audit::AuditStateStore;
use fauna_client_backup::recover::{
    RecoveryError, RecoveryOutcome, regressed_sets, run_recovery, run_store_recovery,
};
use fauna_core::crypto::NestBackupKey;
use fauna_core::data::{BackupDestination, DestinationKind};
use fauna_core::identity::ActorKeypair;

use crate::reseed::{
    AuditStoreRegressionRecord, CustodianStoreRegressionRecord, NativeRecoveryLeg, NestRecords,
    RecoveryDelivery, RecoveryDestination, RecoverySource, RecoveryStore, SyncClientSink,
};

fauna_client_backup::impl_backup_nest_seam!(struct DestinationSeam<Arc<NestClient>>);

/// Everything one recovery run needs from a native app.
pub struct NativeRecovery<'a> {
    /// The owner's live connection to their own (regressed) nest.
    pub source: Arc<NestClient>,
    /// The owner's identity seed — the destination session's keypair and the
    /// `NestBackupKey` the copy is opened under both derive from it.
    pub owner_secret: [u8; 32],
    /// This device's sync device id — the rows the leg records carry it, on
    /// both nests.
    pub device_id: [u8; 32],
    /// The destination row whose notice the owner acted on.
    pub destination: &'a BackupDestination,
    /// This device's audit store: the notice's record names the sets, and the
    /// leg's duty clears it.
    pub audit: &'a dyn AuditStateStore,
}

/// Run the recovery from `input.destination` into the owner's nest.
///
/// `Err` is a run that did not reach the shared driver (the destination row is
/// not a nest, the audit store will not load, the destination will not connect
/// or proves another identity) or that the driver stopped
/// ([`fauna_client_backup::recover::RecoveryError`]) — in every case nothing
/// the owner can lose: a re-run resumes by content address.
pub async fn recover_from_destination(
    input: NativeRecovery<'_>,
) -> Result<RecoveryOutcome, String> {
    let destination = input.destination;
    if !matches!(destination.kind_view(), DestinationKind::Nest { .. }) {
        return Err(format!(
            "{} is not a nest, so there is no copy to recover from over the network",
            destination.destination_id
        ));
    }
    let snapshot = input
        .audit
        .load()
        .map_err(|e| format!("reading this device's backup checks: {e}"))?;
    let sets = snapshot
        .records
        .iter()
        .find(|r| r.state.destination_id == destination.destination_id)
        .map(regressed_sets)
        .unwrap_or_default();
    if sets.is_empty() {
        // Nothing stands: the notice cleared since it was painted.
        return Ok(RecoveryOutcome::default());
    }

    let scope_id = ActorKeypair::from_secret(input.owner_secret).actor_id().0;
    let url = destination.destination_nest_url.clone();
    let dest = connect_enrolled_destination(destination, input.owner_secret, "recover").await?;

    let seam = DestinationSeam {
        client: fauna_client_backup::BackupClient::new(Arc::clone(&dest)),
    };
    let fetcher = fauna_client::ForeignPublicChunkFetcher::new(&url);
    let bytes =
        crate::nest_client::SyncClient::new(Arc::clone(input.source.auth()), &input.device_id);
    let sink = SyncClientSink { client: &bytes };
    let record = AuditStoreRegressionRecord {
        store: input.audit,
        destination_id: &destination.destination_id,
        scope_id,
    };
    let device_hex = hex::encode(input.device_id);
    let dest_records = NestRecords::new(&*dest);
    let source_records = NestRecords::new(&*input.source);
    let leg = NativeRecoveryLeg(RecoveryDelivery::new(
        RecoveryDestination {
            seam: &seam,
            bytes: &fetcher,
            records: &dest_records,
            device_id: device_hex.clone(),
        },
        RecoverySource {
            bytes: &sink,
            nest: &*input.source,
            records: &source_records,
            device_id: device_hex,
        },
        scope_id,
        &NestBackupKey::derive(&input.owner_secret),
        &record,
    ));
    run_recovery(&leg, &sets).await.map_err(|e| e.to_string())
}

/// The owner's own authenticated connection to a nest destination, held to the
/// identity the owner enrolled it under — the refusal
/// `fauna_client_backup::trust::connect_destination` makes. `act` names the
/// run in the refusal ("recover", "restore").
pub(crate) async fn connect_enrolled_destination(
    destination: &BackupDestination,
    owner_secret: [u8; 32],
    act: &str,
) -> Result<Arc<NestClient>, String> {
    let url = destination.destination_nest_url.clone();
    let dest = NestClient::new(url.clone(), ActorKeypair::from_secret(owner_secret));
    dest.connect()
        .await
        .map_err(|e| format!("connect to backup destination {url}: {e}"))?;
    let bound = fauna_client::trust::connection_bound_identity(&*dest, &url)
        .await
        .map_err(|e| format!("backup destination {url} identity: {e}"))?;
    if bound != destination.destination_actor_pubkey {
        return Err(format!(
            "{url} proved identity {}, not the destination enrolled as {} — refusing \
             to {act} from it",
            fauna_core::hex32::encode(&bound),
            fauna_core::hex32::encode(&destination.destination_actor_pubkey),
        ));
    }
    Ok(dest)
}

/// Everything one recovery from this device's own custodian store needs from
/// the process that owns the store.
pub struct OwnStoreRecovery<'a> {
    /// The store, opened read-write: the caller is its one writer for the run
    /// (the agent's store-job slot; the phone's stopped custodian work and pass
    /// guard).
    pub store: &'a crate::custodian_store::CustodianStore,
    /// The owner's own (regressed) nest — the grant, the skip's listing, the
    /// landed custody rows and the verb ride it.
    pub source: &'a NestClient,
    /// The owner-session bulk-byte plane to the same nest.
    pub bytes: &'a crate::nest_client::SyncClient,
    /// The owner's actor — the reserved sets' scope.
    pub scope_id: [u8; 32],
    /// This device's sync device id, hex — the landed rows carry it.
    pub device_id: String,
    /// The client-only key the store rests under; used solely to open.
    pub local: fauna_core::crypto::OwnerSealKey,
    /// The root the landed custody is re-sealed under and the grant hands the
    /// nest.
    pub nest_key: &'a NestBackupKey,
    /// Unix seconds the duty's local tombstones are stamped with.
    pub now: i64,
}

/// Run the recovery from this device's own store into the owner's nest
/// (`segment-backup-protocol.md` § Client-device custodian (pull) → *Restore*
/// → *Recovery from this device's own store*): the sets the store's own record
/// stands for, the grant, then the shared driver over the store end of
/// [`RecoveryDelivery`] with the store-backed duties.
///
/// `Err` is a run that did not reach the driver (the store's record will not
/// load) or that the driver stopped — nothing the owner can lose: a re-run
/// resumes by content address and the verb recovers nothing twice.
pub async fn recover_from_own_store(
    input: OwnStoreRecovery<'_>,
) -> Result<RecoveryOutcome, RecoveryError> {
    let record = input.store.audit_record().await;
    let sets = fauna_client_backup::recover::sets_of_ledger_keys(
        record.source_regressions.keys().map(String::as_str),
    );
    let sink = SyncClientSink {
        client: input.bytes,
    };
    let regression = CustodianStoreRegressionRecord {
        store: input.store,
        scope_id: input.scope_id,
    };
    let store = RecoveryStore::new(input.store, input.local, input.now);
    let records = NestRecords::new(input.source);
    let leg = NativeRecoveryLeg(RecoveryDelivery::from_store(
        &store,
        RecoverySource {
            bytes: &sink,
            nest: input.source,
            records: &records,
            device_id: input.device_id,
        },
        input.scope_id,
        input.nest_key,
        &regression,
    ));
    run_store_recovery(
        &fauna_client_backup::BackupClient::new(input.source),
        input.nest_key.to_bytes().to_vec(),
        &leg,
        &sets,
    )
    .await
}
