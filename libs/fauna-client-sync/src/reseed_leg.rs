//! The pieces both **re-seed delivery legs** share — the custodian leg
//! (`fauna_sync_engine::reseed::ReseedDelivery`, native: it reads this
//! device's custodian store) and the nest-held pull-back
//! ([`crate::reseed_pull::NestPullBack`], every app, web included).
//!
//! Owner: `docs/goal/architecture/segment-backup-protocol.md` § Client-device
//! custodian (pull) → *Restore*. They live here, wasm-clean, since the
//! pull-back moved out of the native-only sync engine (2026-10-18): the
//! report a pass counts in, the bytes-then-custody landing on the target, and
//! the owner's signature over every folder row a materialize re-homes
//! (`writer-signed-change-records.md` ruling (7)(a)(ii)) — which needs the
//! host's [`crate::RecordSigning`], the reason this crate and not
//! `fauna-client-backup` is their home.

use anyhow::Result;
use fauna_client_backup::recover_leg::BlobPushSink;
use fauna_client_backup::reseed::{DeliveredSet, FolderRehome};
use fauna_core::blob_seal::SealedBlob;
use fauna_protocol::RpcRequester;
use fauna_protocol::sync::{SyncChangeRecordReply, SyncChangeRecordRequest};
use fauna_protocol::sync_writer_sig::SignedChange;

/// Is `leaf` the hex spelling of a 32-byte `path_hash` — the one shape a
/// covered-folder mirror's local leaf can have?
///
/// The custodian pull writes `hex::encode(&entry.path_hash)` and nothing else,
/// and the source nest's own folder pass records exactly the same spelling, so
/// this is the delivery door's admission rule rather than a defensive guess:
/// lowercase, so two spellings of one hash can never fork the target's path
/// keying.
pub fn is_path_hash_hex(leaf: &str) -> bool {
    fauna_core::hex32::is_lowercase_hex64(leaf)
}

/// What one delivery pass moved.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ReseedReport {
    /// Custody paths recorded on the target this pass, in push order.
    pub delivered_paths: Vec<String>,
    /// Every custody set this pass delivered at least one path into, in
    /// delivery order — what the ceremony driver materializes
    /// (`fauna_client_backup::reseed::run_reseed`). A set the source holds
    /// nothing for is absent, so phase 3 never names a set phase 2 left empty.
    pub delivered_sets: Vec<DeliveredSet>,
    /// Segments delivered **without** a sidecar because the source holds none
    /// for them — a store that never completed a pull pass after the
    /// 2026-08-29 widening, or a destination holding the `.dat` alone. Their
    /// `.dat` landed (it is real custody, and a later delivery converges), but
    /// materialization cannot reopen them until the sidecar arrives; a ceremony
    /// driver must show this rather than report the corpus whole.
    pub sidecarless_segments: Vec<u32>,
    /// Covered-folder paths delivered **without** a sealed name because the
    /// source holds none for them. Their bytes and custody landed, but the
    /// folder materialize arm re-homes from `(path_hash, path_sealed,
    /// manifest_hash)` and cannot re-home a row with no name. A ceremony driver
    /// must show this rather than report the folder whole.
    pub folder_paths_without_seal: Vec<String>,
    /// Chunk bodies actually uploaded (post-dedup).
    pub chunks_uploaded: usize,
    /// Chunk bodies the target already held.
    pub chunks_deduped: usize,
    /// Manifests uploaded — one per delivered path.
    pub manifests_uploaded: usize,
    /// Sum of the delivered paths' **plaintext** sizes: what the custody rows
    /// declare, matching the nest coordinator's own `size_bytes`.
    pub plaintext_bytes: u64,
}

impl ReseedReport {
    pub fn absorb(&mut self, other: ReseedReport) {
        self.delivered_paths.extend(other.delivered_paths);
        self.delivered_sets.extend(other.delivered_sets);
        self.sidecarless_segments.extend(other.sidecarless_segments);
        self.folder_paths_without_seal
            .extend(other.folder_paths_without_seal);
        self.chunks_uploaded += other.chunks_uploaded;
        self.chunks_deduped += other.chunks_deduped;
        self.manifests_uploaded += other.manifests_uploaded;
        self.plaintext_bytes = self.plaintext_bytes.saturating_add(other.plaintext_bytes);
    }
}

/// Bytes first, then the manifest — the shared
/// [`fauna_client_backup::recover_leg::push_sealed`], read back as a
/// [`ReseedReport`] for the two legs that count in those terms.
pub async fn push_sealed<P: BlobPushSink>(sink: &P, sealed: &SealedBlob) -> Result<ReseedReport> {
    let pushed = fauna_client_backup::recover_leg::push_sealed(sink, sealed).await?;
    Ok(ReseedReport {
        chunks_uploaded: pushed.chunks_uploaded,
        chunks_deduped: pushed.chunks_deduped,
        manifests_uploaded: 1,
        ..ReseedReport::default()
    })
}

/// Custody second — the client-authed reserved-set arm, shared by both legs.
///
/// `path_sealed` is `None`, and must be: a reserved backup destination's
/// custody paths are machine-authored routing keys, which is exactly the class
/// `record_change_core` exempts from the S9 seal requirement. Sending a seal
/// here would not be safer — it would be a different path key than the one
/// every other arm of the stack derives.
pub async fn record_custody<R: RpcRequester>(
    nest: &R,
    set: &str,
    device_id: &str,
    path: &str,
    sealed: &SealedBlob,
    plaintext_bytes: u64,
) -> Result<()> {
    let req = SyncChangeRecordRequest {
        folder: set.to_string(),
        device_id: device_id.to_string(),
        path: path.to_string(),
        manifest_hash: Some(hex::encode(sealed.manifest_hash.digest())),
        // The nest DERIVES the custody charge from the bytes it actually holds
        // and never meters this figure; it is declared as the source's
        // plaintext size so a delivered row reads identically to one the source
        // nest's own coordinator wrote.
        size_bytes: plaintext_bytes as i64,
        change_type: "create".to_string(),
        ..Default::default()
    };
    let _: SyncChangeRecordReply = nest
        .request(
            "fauna.sync.changes.record",
            fauna_protocol::folders::addressed(req),
        )
        .await
        .map_err(|e| anyhow::anyhow!("{e}"))?;
    Ok(())
}

/// One custody row a folder materialize re-homes, as the owner's re-home
/// statement names it (`writer-signed-change-records.md` ruling (7)(a)(ii)):
/// the source `path_hash` (the row's leaf), the mirrored manifest, the
/// manifest's own `total_size` and the source's sealed name, verbatim.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RehomeRow {
    pub path_hash: [u8; 32],
    pub manifest_hash: [u8; 32],
    /// The manifest's `total_size` — never a custody row's declared or derived
    /// size: both halves read the one figure both hold (ruling (7)(a)(ii),
    /// *Where the statement reads its size*).
    pub total_size: u64,
    pub path_sealed: Vec<u8>,
}

/// The host's signer and the nonce of the live set `set` re-homes into — the
/// set named by its display name, resolved through the host's
/// [`crate::SetNonceSource`] as every other record's is. No signer, no display
/// name, or no nonce → the [`FolderRehome::Unsigned`] the driver holds the set
/// with.
pub async fn rehome_nonce<'s>(
    signing: Option<&'s crate::RecordSigning>,
    set: &DeliveredSet,
) -> std::result::Result<(&'s crate::RecordSigning, [u8; 32]), FolderRehome> {
    let unsigned = |reason: String| FolderRehome::Unsigned { reason };
    let Some(signing) = signing else {
        return Err(unsigned(
            "this device holds no change signer for the restored folder".into(),
        ));
    };
    let Some(display_name) = set.folder_display_name.as_deref() else {
        return Err(unsigned(format!("no display name for {}", set.set_name)));
    };
    match signing.set_nonce.lookup(display_name).await {
        Ok(Some(nonce)) => Ok((signing, nonce)),
        Ok(None) => Err(unsigned(format!(
            "this device holds no set nonce for the folder {display_name}"
        ))),
        Err(e) => Err(unsigned(format!(
            "reading the set nonce for the folder {display_name}: {e:#}"
        ))),
    }
}

/// The owner's signature over each row, one [`SignedChange::for_rehome`] per
/// row under `nonce` — what both delivery legs hand the driver.
pub fn sign_rehome_rows(
    signing: &crate::RecordSigning,
    nonce: [u8; 32],
    rows: &[RehomeRow],
) -> FolderRehome {
    let owner = signing.signer.actor_id();
    let signatures = rows
        .iter()
        .map(|row| {
            let statement = SignedChange::for_rehome(
                nonce,
                owner,
                row.path_hash,
                row.manifest_hash,
                row.total_size as i64,
                &row.path_sealed,
            );
            fauna_protocol::backup::RehomeSignature {
                path_hash: fauna_protocol::ByteBuf::from(row.path_hash.to_vec()),
                signature: fauna_protocol::ByteBuf::from(
                    signing.signer.sign_statement(&statement).to_vec(),
                ),
                ..Default::default()
            }
        })
        .collect();
    FolderRehome::Signed {
        signer_key: signing.signer.signer_key().to_vec(),
        signatures,
    }
}
