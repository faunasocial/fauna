//! The **nest-held pull-back**, hosted natively — the re-seed ceremony's
//! delivery leg when the surviving copy of a lost nest's corpus is held by
//! **another nest**.
//!
//! Owner: `docs/goal/architecture/segment-backup-protocol.md` § Client-device
//! custodian (pull) → *Restore* → *The nest-held pull-back*; the ceremony is
//! `docs/goal/behavior/backup-destinations.md` § Third destination kind →
//! *Re-seed*.
//!
//! The leg itself — [`NestPullBack`], its two ends, the coverage-row folder
//! names — lives in [`fauna_client_sync::reseed_pull`] since 2026-10-18, so the
//! browser runs the same body (`fauna-wasm`'s pull-back door); it is
//! re-exported here so every native caller keeps its import path. What stays
//! here is what only a native host has: the `Send` leg the driver runs on a
//! spawned task ([`NativePullBackLeg`]) and the one native composition
//! ([`pull_back_from_destination`]).

use fauna_client_backup::reseed::{DeliveredCorpus, DeliveredSet, FolderRehome};
#[cfg(feature = "pull-back")]
use fauna_core::data::BackupDestination;
#[cfg(feature = "pull-back")]
use fauna_core::file_download::FileDownloadKeys;

use crate::reseed::BlobPushSink;

pub use fauna_client_sync::reseed_pull::{
    CUSTODY_UNADDRESSABLE, FolderNaming, NestPullBack, PullBackDestination, PullBackTarget,
    fetch_as_is, folder_names_from_coverage,
};

/// The leg as the driver runs it natively: [`NestPullBack`] over the owner's
/// own authed WS-RPC connection to the target, whose request future is
/// `Send`, so the ceremony can run on a spawned task (as `ReseedDelivery`'s).
/// A newtype because the leg's generic body cannot promise `Send` for an
/// arbitrary transport — the split [`crate::reseed::NativeRecoveryLeg`] makes
/// for the lived-in recovery.
pub struct NativePullBackLeg<'a, P: BlobPushSink>(
    pub NestPullBack<'a, P, fauna_client::NestClient>,
);

#[async_trait::async_trait]
impl<P: BlobPushSink> fauna_client_backup::reseed::ReseedDeliveryLeg for NativePullBackLeg<'_, P> {
    async fn deliver(&self) -> std::result::Result<DeliveredCorpus, String> {
        self.0.deliver_corpus().await
    }

    async fn sign_rehome(&self, set: &DeliveredSet) -> FolderRehome {
        self.0.sign_folder_rehome(set).await
    }
}

/// Everything one pull-back needs from a native app.
#[cfg(feature = "pull-back")]
pub struct NativePullBack<'a> {
    /// The owner's live connection to the rebuilt nest — the target the
    /// corpus lands on, and the restored nest the destination re-enrolls to.
    pub target: std::sync::Arc<fauna_client::NestClient>,
    /// The owner's identity seed: the destination session's keypair, the
    /// `NestBackupKey` granted to the target and the owner seal a folder's
    /// sealed name opens under all derive from it.
    pub owner_secret: [u8; 32],
    /// This device's sync device id, registered write-capable on the target.
    pub device_id: [u8; 32],
    /// The surviving nest-kind destination row the owner pressed the gesture on.
    pub destination: &'a BackupDestination,
    /// The owner's account-plane backup list store — the coverage rows the
    /// folders' names come from, and where the re-enrollment writes.
    pub store: &'a dyn fauna_client_config::BackupStateStore,
    /// The account's folder custody: each restored folder's target set is
    /// prepared in it, and the re-homed rows are signed under its nonces.
    pub folder_keys: std::sync::Arc<dyn fauna_client_folders::FolderKeyStore>,
}

/// How a pull-back ended once the shared driver reached its verdict.
#[cfg(feature = "pull-back")]
#[derive(Debug)]
pub struct PullBackResult {
    pub outcome: fauna_client_backup::reseed::ReseedOutcome,
    /// The post-ceremony re-enrollment's failure: the data IS back, the
    /// destination is not yet backing the restored nest up.
    pub reenroll_error: Option<String>,
}

/// **The nest-held pull-back, hosted** — the confirmed
/// `backup-destination-reseed-confirm-button` on a nest-kind row
/// (`backup-destinations.md` § Re-seed; `segment-backup-protocol.md`
/// § Client-device custodian (pull) → *Restore* → *The nest-held pull-back*).
/// It runs in the app on every platform: no custodian store is involved, so
/// it needs no host process of its own.
///
/// The one composition every app calls (the ffi and wasm faces wrap it), so
/// the order cannot drift per shell: connect to the destination as the owner,
/// held to its enrolled identity; read each covered folder's name off the
/// box's coverage rows ([`folder_names_from_coverage`]) and prepare its target
/// set (`writer-signed-change-records.md` ruling (7)(a)(i)); run the shared
/// driver over [`NestPullBack`]; and on a whole outcome only, re-enroll the
/// destination to the restored nest, moving its writer seat
/// (`fauna_client_config::reenroll_nest_destination_after_reseed`).
///
/// `Err` is a run that stopped before the driver's verdict — nothing was made
/// live, and a re-run resumes by content address.
#[cfg(feature = "pull-back")]
pub async fn pull_back_from_destination(
    input: NativePullBack<'_>,
) -> std::result::Result<PullBackResult, String> {
    use std::sync::Arc;

    use fauna_core::crypto::{BackupKey, NestBackupKey, OwnerSealKey};
    use fauna_core::data::DestinationKind;
    use fauna_core::identity::ActorKeypair;

    let destination = input.destination;
    if !matches!(destination.kind_view(), DestinationKind::Nest { .. }) {
        return Err(format!(
            "{} is not a nest, so there is no copy to pull back over the network",
            destination.destination_id
        ));
    }
    let keypair = ActorKeypair::from_secret(input.owner_secret);
    let target = input.target;
    let target_url = target.nest_url();
    let restored_nest_id = fauna_client::trust::connection_bound_identity(&*target, &target_url)
        .await
        .map_err(|e| format!("this nest's identity: {e}"))?;
    let dest =
        crate::recovery::connect_enrolled_destination(destination, input.owner_secret, "restore")
            .await?;

    // The box's own list, as the Backups page reads it: after a rebuild it is
    // the lost box's (`backup-destinations.md` § *A rebuilt box keeps its list
    // too*), so its coverage rows name the folders the destination holds.
    let coverage =
        fauna_client_config::load_backup_state_refiled(input.store, &*target, restored_nest_id)
            .await
            .map_err(|e| format!("reading this nest's backup list: {e}"))?;
    let seal =
        FileDownloadKeys::owner(OwnerSealKey::Client(BackupKey::derive(&input.owner_secret)));
    let names = folder_names_from_coverage(
        &coverage.backup.destinations,
        &destination.destination_id,
        Some(&seal),
    );
    let display_names: Vec<String> = names
        .values()
        .filter_map(|n| n.display_name.clone())
        .collect();
    let files = fauna_client_folders::FoldersClient::new(Arc::clone(&target));
    fauna_client_folders::prepare_reseed_targets_logged(
        &files,
        &*input.folder_keys,
        &display_names,
    )
    .await;

    let url = destination.destination_nest_url.clone();
    let seam = PullSeam {
        client: fauna_client_backup::BackupClient::new(Arc::clone(&dest)),
    };
    let fetcher = fauna_client::ForeignPublicChunkFetcher::new(&url);
    let bytes = crate::nest_client::SyncClient::new(Arc::clone(target.auth()), &input.device_id);
    let sink = crate::reseed::SyncClientSink { client: &bytes };
    let custody: Arc<dyn fauna_client_folders::FolderKeyReader> = input.folder_keys.clone();
    let leg = NativePullBackLeg(
        NestPullBack::new(
            PullBackDestination {
                seam: &seam,
                bytes: &fetcher,
            },
            PullBackTarget {
                bytes: &sink,
                nest: &*target,
                device_id: hex::encode(input.device_id),
            },
            keypair.actor_id().0,
        )
        .with_folder_names(names)
        .with_rehome_signing(fauna_client_folders::record_signing(
            Arc::clone(&target),
            &keypair,
            custody,
        )),
    );

    let nest_key = NestBackupKey::derive(&input.owner_secret);
    let outcome = fauna_client_backup::reseed::run_reseed(
        &fauna_client_backup::BackupClient::new(Arc::clone(&target)),
        nest_key.to_bytes().to_vec(),
        &leg,
    )
    .await
    .map_err(|e| e.to_string())?;
    drop(nest_key);

    let reenroll_error = fauna_client_config::reenroll_nest_destination_after_reseed(
        Arc::clone(&target),
        Arc::clone(&dest),
        input.store,
        input.owner_secret,
        restored_nest_id,
        destination,
        &outcome,
    )
    .await
    .and_then(std::result::Result::err)
    .map(|e| e.to_string());
    Ok(PullBackResult {
        outcome,
        reenroll_error,
    })
}

#[cfg(feature = "pull-back")]
fauna_client_backup::impl_backup_nest_seam!(struct PullSeam<std::sync::Arc<fauna_client::NestClient>>);
