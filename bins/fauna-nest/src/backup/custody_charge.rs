//! Destination-derived custody quota charge.
//!
//! On a **reserved** backup destination set the custody writer is a foreign
//! nest under a user-minted grant, and the ratified rate cap on its supersede
//! power is the owner's own storage quota (`message-segment-store.md`
//! § *Reclaim rate-cap — RULED not-a-parameter*). A charge computed from the
//! writer's *declared* `size_bytes` cannot bind that cap — declaring `0` makes
//! a supersede storm free — so the destination derives the charge from what it
//! **actually holds**: the server-measured stored sizes (`blob_metadata`,
//! written load-bearingly by the chunk/manifest POST routes — a failed
//! metadata write fails the upload, so an ACKed blob is always metered) of the
//! named manifest plus every chunk its `store_keys()` reference, deduped.
//!
//! A record naming bytes this nest does not hold is **refused**, typed and
//! retryable — never charged as zero or as declared (fail closed). An honest
//! writer never sees the refusal: both shipped coordinators upload chunks +
//! manifest *before* recording custody (`message-segment-store.md`
//! § record-on-upload), so by record time everything referenced is held and
//! metered. The refusal also narrows what a rogue writer can pin at all:
//! custody on a reserved set now only ever references decodable manifests
//! whose chunk sets are fully held — the same set GC can walk.

use std::sync::Arc;

use crate::blob_store::BlobStoreBackend;
use crate::db::CacheDb;
use fauna_core::crypto::BackupKey;

/// Why a custody record was refused rather than charged. Every arm is the
/// writer's to fix (upload the bytes, then re-record); none may fall open to a
/// zero or declared charge.
#[derive(Debug)]
pub(crate) enum CustodyChargeRefusal {
    /// The named manifest is not in this nest's blob store, or has no
    /// `blob_metadata` row (an ACKed upload always has one).
    ManifestNotHeld,
    /// The named blob is held but does not decode as a `ChunkManifest`, so its
    /// chunk set is not enumerable — GC fails closed on exactly the same test.
    ManifestNotAManifest,
    /// This many of the manifest's referenced chunks are not held here.
    ChunksNotHeld { missing: usize },
}

impl std::fmt::Display for CustodyChargeRefusal {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::ManifestNotHeld => write!(f, "the named manifest is not held on this nest"),
            Self::ManifestNotAManifest => {
                write!(f, "the named blob does not decode as a chunk manifest")
            }
            Self::ChunksNotHeld { missing } => write!(
                f,
                "{missing} chunk(s) the manifest references are not held on this nest"
            ),
        }
    }
}

/// The manifest this nest holds under `manifest_hash`, decoded — the one read
/// both [`derive_held_custody_charge`] and the covered-folder materialize arm
/// take (the arm reads its `total_size` as the re-home statement's size,
/// `writer-signed-change-records.md` ruling (7)(a)(ii)).
///
/// The outer `Err` is a storage fault; the inner `Err` is the typed refusal for
/// a manifest not held or not decodable.
pub(crate) async fn held_manifest(
    store: &Arc<dyn BlobStoreBackend>,
    at_rest_key: Option<&BackupKey>,
    manifest_hash: &[u8; 32],
) -> anyhow::Result<Result<fauna_core::chunk::ChunkManifest, CustodyChargeRefusal>> {
    let hash = fauna_core::data::ContentHash::from_digest_raw(*manifest_hash);
    let Some(raw) = store.get(&hash).await? else {
        return Ok(Err(CustodyChargeRefusal::ManifestNotHeld));
    };
    Ok(super::gc::decode_manifest(&raw, at_rest_key)
        .map_err(|_| CustodyChargeRefusal::ManifestNotAManifest))
}

/// Derive the bytes this nest actually holds — and this custody row will pin —
/// under `manifest_hash`: the manifest blob's own stored size plus the stored
/// size of every distinct chunk its `store_keys()` reference.
///
/// The outer `Err` is a storage fault (the caller maps it to `internal` — fail
/// closed); the inner `Err` is a typed refusal for bytes the writer has not
/// uploaded yet.
pub(crate) async fn derive_held_custody_charge(
    db: &CacheDb,
    store: &Arc<dyn BlobStoreBackend>,
    at_rest_key: Option<&BackupKey>,
    manifest_hash: &[u8; 32],
) -> anyhow::Result<Result<i64, CustodyChargeRefusal>> {
    let manifest = match held_manifest(store, at_rest_key, manifest_hash).await? {
        Ok(manifest) => manifest,
        Err(refusal) => return Ok(Err(refusal)),
    };
    let Some(manifest_meta) = db.get_blob_metadata(manifest_hash).await? else {
        return Ok(Err(CustodyChargeRefusal::ManifestNotHeld));
    };

    // Dedup: a manifest may reference the same chunk under several indices,
    // but the content-addressed store holds — and this row pins — it once.
    let mut keys: Vec<[u8; 32]> = manifest.store_keys().iter().map(|h| h.digest()).collect();
    keys.sort_unstable();
    keys.dedup();

    let sizes = db.get_blob_sizes(&keys).await?;
    let missing = keys.iter().filter(|k| !sizes.contains_key(*k)).count();
    if missing > 0 {
        return Ok(Err(CustodyChargeRefusal::ChunksNotHeld { missing }));
    }

    let mut total: i64 = manifest_meta.size_bytes;
    for k in &keys {
        total = total
            .checked_add(sizes[k])
            .ok_or_else(|| anyhow::anyhow!("custody charge overflows i64"))?;
    }
    Ok(Ok(total))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::blob_store::DiskBlobStore;
    use fauna_core::chunk::ChunkManifest;
    use fauna_core::data::ContentHash;

    async fn fixture() -> (CacheDb, Arc<dyn BlobStoreBackend>, tempfile::TempDir) {
        let dir = tempfile::tempdir().unwrap();
        let store: Arc<dyn BlobStoreBackend> = Arc::new(DiskBlobStore::new(dir.path()).unwrap());
        (CacheDb::open_in_memory().unwrap(), store, dir)
    }

    /// Store `body` the way the chunk/manifest POST routes do: at-rest framed
    /// in the store, wire length metered in `blob_metadata`.
    async fn seed_blob(
        db: &CacheDb,
        store: &Arc<dyn BlobStoreBackend>,
        body: &[u8],
        kind: &str,
    ) -> [u8; 32] {
        let digest = ContentHash::of_raw(body).digest();
        let framed = crate::backup::encode_blob(body, None, false).unwrap();
        store
            .put(&ContentHash::from_digest_raw(digest), &framed)
            .await
            .unwrap();
        db.put_blob_metadata(&digest, body.len() as i64, kind, None, None)
            .await
            .unwrap();
        digest
    }

    fn manifest_over(chunks: &[(&[u8; 32], u64)]) -> Vec<u8> {
        let m = ChunkManifest {
            file_hash: ContentHash::from_digest_raw([0x0Fu8; 32]),
            total_size: chunks.iter().map(|(_, s)| s).sum(),
            chunk_hashes: chunks
                .iter()
                .map(|(h, _)| ContentHash::from_digest_raw(**h))
                .collect(),
            chunk_sizes: chunks.iter().map(|(_, s)| *s).collect(),
            stored_hashes: None,
            sealed_hashes: None,
            min_reader: None,
        };
        fauna_core::encoding::canonical_encode(&m).unwrap()
    }

    #[tokio::test]
    async fn the_charge_is_the_held_manifest_plus_its_distinct_chunks() {
        let (db, store, _dir) = fixture().await;
        let c1 = seed_blob(&db, &store, &[0xAAu8; 100], "chunk").await;
        let c2 = seed_blob(&db, &store, &[0xBBu8; 250], "chunk").await;
        // The same chunk referenced twice must be charged once.
        let body = manifest_over(&[(&c1, 100), (&c2, 250), (&c1, 100)]);
        let mh = seed_blob(&db, &store, &body, "manifest").await;

        let charge = derive_held_custody_charge(&db, &store, None, &mh)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(charge, body.len() as i64 + 100 + 250);
    }

    #[tokio::test]
    async fn an_unheld_manifest_refuses_rather_than_charging_zero() {
        let (db, store, _dir) = fixture().await;
        let never_uploaded = [0x77u8; 32];
        let refusal = derive_held_custody_charge(&db, &store, None, &never_uploaded)
            .await
            .unwrap()
            .unwrap_err();
        assert!(matches!(refusal, CustodyChargeRefusal::ManifestNotHeld));
    }

    #[tokio::test]
    async fn a_held_blob_that_is_not_a_manifest_refuses() {
        let (db, store, _dir) = fixture().await;
        let garbage = seed_blob(&db, &store, b"not a manifest at all", "chunk").await;
        let refusal = derive_held_custody_charge(&db, &store, None, &garbage)
            .await
            .unwrap()
            .unwrap_err();
        assert!(matches!(
            refusal,
            CustodyChargeRefusal::ManifestNotAManifest
        ));
    }

    #[tokio::test]
    async fn a_manifest_referencing_an_unheld_chunk_refuses() {
        let (db, store, _dir) = fixture().await;
        let held = seed_blob(&db, &store, &[0xCCu8; 64], "chunk").await;
        let missing = [0x99u8; 32];
        let body = manifest_over(&[(&held, 64), (&missing, 4096)]);
        let mh = seed_blob(&db, &store, &body, "manifest").await;

        let refusal = derive_held_custody_charge(&db, &store, None, &mh)
            .await
            .unwrap()
            .unwrap_err();
        assert!(matches!(
            refusal,
            CustodyChargeRefusal::ChunksNotHeld { missing: 1 }
        ));
    }
}
