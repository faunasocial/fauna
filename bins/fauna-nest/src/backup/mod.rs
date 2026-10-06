pub mod check;
pub(crate) mod custody_charge;
pub mod diff;
pub mod gc;
pub mod gc_scheduler;
pub(crate) mod materialize;
pub mod prune;
pub mod recover;
pub mod retention;
pub mod scheduler;
pub mod service;
pub mod stats;
pub mod version_prune;

use anyhow::Result;
use fauna_core::crypto::BackupKey;

pub const SCHEMA_BACKUP_SNAPSHOTS: &str = "
    CREATE TABLE IF NOT EXISTS backup_snapshots (
        id          INTEGER PRIMARY KEY AUTOINCREMENT,
        blob_hash   BLOB NOT NULL,
        size_bytes  INTEGER NOT NULL,
        format      TEXT NOT NULL,
        created_at  INTEGER NOT NULL
    );
";

/// The bound every [`decode_blob`] read carries: no stored blob decodes past
/// it, whatever its prefix byte claims (`backup-restore.md` § 10).
///
/// Some writers store a request body **verbatim** (video segments, blob PUTs,
/// rail and payload spills), so a client chooses the at-rest prefix, and a
/// `0x01` ‖ zstd body of a few KB would otherwise expand ~32,500:1 inside
/// whichever reader touches it — the anonymous chunk GET, or the GC walk of a
/// live post's `blob_refs`. The bound is the shared
/// chunk bomb guard, which sits above every request body that can reach the
/// store (the pins below): a legitimate blob always fits, so exceeding it
/// means the bytes were never a nest-framed body — refused, never expanded.
///
/// The one population above it is the nest's own self-backup snapshot (a
/// whole-database hot copy or logical dump, written by `BackupService`, never
/// served by any route); its reader bounds the decode by the snapshot's
/// recorded size through [`decode_snapshot_blob`].
pub const MAX_DECODED_BLOB: usize = fauna_core::compress::MAX_DECOMPRESSED_CHUNK;

const _: () = {
    assert!(MAX_DECODED_BLOB >= crate::CHUNK_BLOB_BODY_LIMIT);
    assert!(MAX_DECODED_BLOB >= crate::blob_routes::BLOB_BODY_LIMIT);
    assert!(MAX_DECODED_BLOB >= crate::routes::MAX_WS_MESSAGE_SIZE);
    assert!(MAX_DECODED_BLOB >= fauna_mail::body_ref::MAIL_BODY_CHUNK_BYTES);
};

/// Decode a stored blob: decrypt (if key present) then decompress.
///
/// This is the standard decode pipeline for all blob store reads:
/// `load → decrypt → decompress → use`
///
/// The inverse (encode) pipeline is:
/// `data → hash (on raw data) → compress → encrypt → store`
///
/// Both chunks and manifests follow this pipeline. The `decompress_chunk`
/// function uses a self-describing prefix byte (`0x00` = uncompressed,
/// `0x01` = zstd). Data without a recognized prefix is passed through as-is —
/// a LIVE arm, not a compatibility remnant: this store also holds raw blobs
/// written today (link-preview images, video segments, dag-cbor rail bodies,
/// spilled payloads) that `decode_blob` readers serve by hash
/// (`version-compatibility.md` § Dimension 2, program 4 tranche B3).
///
/// Bounded at [`MAX_DECODED_BLOB`]: a blob whose decode would exceed it is an
/// `Err`, never an expansion. The self-backup snapshot reader is the one caller
/// that needs a different bound, and uses [`decode_snapshot_blob`].
pub fn decode_blob(raw: &[u8], encryption_key: Option<&BackupKey>) -> Result<Vec<u8>> {
    decode_blob_bounded(raw, encryption_key, MAX_DECODED_BLOB)
}

/// [`decode_blob`] at a caller-stated bound — for a population whose size the
/// caller knows from its own record (a self-backup snapshot's `size_bytes`).
pub fn decode_blob_bounded(
    raw: &[u8],
    encryption_key: Option<&BackupKey>,
    max_len: usize,
) -> Result<Vec<u8>> {
    let decrypted = match encryption_key {
        Some(key) => fauna_core::crypto::decrypt_backup_chunk(key, raw)?,
        None => raw.to_vec(),
    };
    fauna_core::compress::decompress_chunk_bounded(&decrypted, max_len)
}

/// Decode a nest self-backup snapshot blob, bounded by the `size_bytes` its
/// `backups/<hash>.manifest.json` sidecar recorded at write time — the one
/// population above [`MAX_DECODED_BLOB`] (`backup-restore.md` § 10, § 11). A
/// blob with no sidecar is no snapshot, and is refused.
pub fn decode_snapshot_blob(
    blob_root: &std::path::Path,
    digest: &[u8; 32],
    encoded: &[u8],
    encryption_key: Option<&BackupKey>,
) -> Result<Vec<u8>> {
    let path = blob_root
        .join("backups")
        .join(format!("{}.manifest.json", hex::encode(digest)));
    let bytes = std::fs::read(&path)
        .map_err(|e| anyhow::anyhow!("no backup snapshot record at {}: {e}", path.display()))?;
    let record: serde_json::Value = serde_json::from_slice(&bytes)?;
    let size_bytes = record["size_bytes"]
        .as_u64()
        .ok_or_else(|| anyhow::anyhow!("backup snapshot record carries no size_bytes"))?;
    decode_blob_bounded(encoded, encryption_key, usize::try_from(size_bytes)?)
}

/// Encode data for storage: compress (if enabled) then encrypt (if key present).
///
/// This is the standard encode pipeline for all blob store writes:
/// `data → hash (on raw data) → compress → encrypt → store`
///
/// The hash must be computed on the raw (pre-encode) data by the caller,
/// since content-addressing is based on plaintext for deduplication.
///
/// The output is **always self-describing**: even when `compress` is false the
/// payload is framed with `compress_chunk`'s `PREFIX_UNCOMPRESSED` (`0x00`) byte
/// (via [`fauna_core::compress::frame_uncompressed`]), so [`decode_blob`] is the
/// exact inverse for *every* `(key, compress)` combination. Returning the data
/// un-prefixed in the `compress=false` branch is what previously let `decode_blob`
/// (which always runs `decompress_chunk`) mis-strip a `0x00`/`0x01`-leading raw
/// chunk — the latent corruption that bit `restore_snapshot`.
pub fn encode_blob(
    data: &[u8],
    encryption_key: Option<&BackupKey>,
    compress: bool,
) -> Result<Vec<u8>> {
    let compressed = if compress {
        fauna_core::compress::compress_chunk(data)
    } else {
        fauna_core::compress::frame_uncompressed(data)
    };
    match encryption_key {
        Some(key) => fauna_core::crypto::encrypt_backup_chunk(key, &compressed)
            .map_err(|e| anyhow::anyhow!("{e}")),
        None => Ok(compressed),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn decode_blob_plaintext_no_key() {
        // Simulate a compressed blob: prefix 0x00 + raw data
        let data = b"hello manifest";
        let stored = fauna_core::compress::compress_chunk(data);
        let decoded = decode_blob(&stored, None).unwrap();
        assert_eq!(decoded, data);
    }

    #[test]
    fn decode_blob_encrypted_and_compressed() {
        let key = fauna_core::crypto::BackupKey::from_bytes([0x42u8; 32]);
        let data = b"hello encrypted manifest";
        let compressed = fauna_core::compress::compress_chunk(data);
        let encrypted = fauna_core::crypto::encrypt_backup_chunk(&key, &compressed).unwrap();
        let decoded = decode_blob(&encrypted, Some(&key)).unwrap();
        assert_eq!(decoded, data);
    }

    #[test]
    fn decode_blob_raw_blob_without_prefix_passes_through() {
        // A raw blob (no frame prefix, no encryption) — the shape the store's raw
        // writers put today; decompress_chunk's passthrough returns it as-is.
        let data = vec![0x42u8, 1, 2, 3]; // first byte 0x42 = unknown prefix
        let decoded = decode_blob(&data, None).unwrap();
        assert_eq!(decoded, data);
    }

    #[test]
    fn decode_blob_legacy_data_starting_with_0x00() {
        // Edge case: legacy *un-framed* data whose first byte is 0x00 (matches
        // PREFIX_UNCOMPRESSED). decompress_chunk treats 0x00 as "uncompressed
        // prefix" and strips it. `encode_blob` no longer *produces* un-framed
        // output (every branch now self-describes via compress_chunk /
        // frame_uncompressed), so this can only arise for blobs written before the
        // self-describing fix or stored by some path outside encode_blob.
        let data = vec![0x00u8, 1, 2, 3];
        let decoded = decode_blob(&data, None).unwrap();
        // decompress_chunk strips the 0x00 prefix byte — returns data[1..]
        assert_eq!(decoded, vec![1, 2, 3]);
    }

    #[test]
    fn decode_blob_empty_input() {
        let decoded = decode_blob(&[], None).unwrap();
        assert!(decoded.is_empty());
    }

    /// A raw-stored `0x01` ‖ zstd bomb — the shape a verbatim writer (a video
    /// segment) lets a client choose — is refused at the bound, in the clear and
    /// under an at-rest key alike, rather than expanded.
    #[test]
    fn decode_blob_refuses_a_zstd_bomb_past_the_bound() {
        let mut bomb = vec![0x01u8];
        bomb.extend(zstd::encode_all(&vec![0u8; MAX_DECODED_BLOB + 1][..], 19).unwrap());
        assert!(decode_blob(&bomb, None).is_err());

        let key = fauna_core::crypto::BackupKey::from_bytes([0x42u8; 32]);
        let sealed = fauna_core::crypto::encrypt_backup_chunk(&key, &bomb).unwrap();
        assert!(decode_blob(&sealed, Some(&key)).is_err());
    }

    /// The bound is on the OUTPUT of every arm, the passthrough included — and a
    /// blob exactly at the bound still decodes (a legitimate maximum never fails).
    #[test]
    fn decode_blob_bound_covers_every_arm_and_admits_the_maximum() {
        let at_bound = encode_blob(&vec![7u8; MAX_DECODED_BLOB], None, true).unwrap();
        assert_eq!(
            decode_blob(&at_bound, None).unwrap().len(),
            MAX_DECODED_BLOB
        );

        let framed_over = encode_blob(&vec![7u8; MAX_DECODED_BLOB + 1], None, false).unwrap();
        assert!(decode_blob(&framed_over, None).is_err());

        let mut raw_over = vec![0x42u8];
        raw_over.resize(MAX_DECODED_BLOB + 1, 0);
        assert!(decode_blob(&raw_over, None).is_err());
    }

    /// A self-backup snapshot above the store-wide bound decodes at its recorded
    /// size — and not one byte under it.
    #[test]
    fn decode_blob_bounded_admits_a_snapshot_at_its_recorded_size() {
        let snapshot = vec![3u8; MAX_DECODED_BLOB + 4096];
        let encoded = encode_blob(&snapshot, None, true).unwrap();
        assert!(decode_blob(&encoded, None).is_err());
        assert_eq!(
            decode_blob_bounded(&encoded, None, snapshot.len()).unwrap(),
            snapshot
        );
        assert!(decode_blob_bounded(&encoded, None, snapshot.len() - 1).is_err());
    }

    /// The restore reader takes its bound from the snapshot's sidecar, which
    /// the writers record as `size_bytes`; no sidecar, no decode.
    #[test]
    fn decode_snapshot_blob_bounds_by_the_sidecar_record() {
        let root = tempfile::tempdir().unwrap();
        let snapshot = vec![5u8; MAX_DECODED_BLOB + 1];
        let digest = *blake3::hash(&snapshot).as_bytes();
        let encoded = encode_blob(&snapshot, None, true).unwrap();

        assert!(decode_snapshot_blob(root.path(), &digest, &encoded, None).is_err());

        let backups = root.path().join("backups");
        std::fs::create_dir_all(&backups).unwrap();
        let sidecar = backups.join(format!("{}.manifest.json", hex::encode(digest)));
        let record = |size: usize| serde_json::json!({ "format": "sqlite", "size_bytes": size });
        std::fs::write(&sidecar, record(snapshot.len()).to_string()).unwrap();
        assert_eq!(
            decode_snapshot_blob(root.path(), &digest, &encoded, None).unwrap(),
            snapshot
        );

        std::fs::write(&sidecar, record(1024).to_string()).unwrap();
        assert!(decode_snapshot_blob(root.path(), &digest, &encoded, None).is_err());
    }

    #[test]
    fn encode_decode_roundtrip_compressed_encrypted() {
        let key = fauna_core::crypto::BackupKey::from_bytes([0x42u8; 32]);
        let data = b"hello roundtrip";
        let encoded = encode_blob(data, Some(&key), true).unwrap();
        let decoded = decode_blob(&encoded, Some(&key)).unwrap();
        assert_eq!(decoded, data);
    }

    #[test]
    fn encode_decode_roundtrip_no_encryption() {
        let data = b"plaintext roundtrip";
        let encoded = encode_blob(data, None, true).unwrap();
        let decoded = decode_blob(&encoded, None).unwrap();
        assert_eq!(decoded, data);
    }

    #[test]
    fn encode_decode_roundtrip_no_compression() {
        let data = b"uncompressed roundtrip";
        let encoded = encode_blob(data, None, false).unwrap();
        let decoded = decode_blob(&encoded, None).unwrap();
        assert_eq!(decoded, data);
    }

    #[test]
    fn encode_decode_roundtrip_encrypted_no_compression() {
        let key = fauna_core::crypto::BackupKey::from_bytes([0x55u8; 32]);
        let data = b"encrypted only";
        let encoded = encode_blob(data, Some(&key), false).unwrap();
        let decoded = decode_blob(&encoded, Some(&key)).unwrap();
        assert_eq!(decoded, data);
    }

    /// Regression: an uncompressed blob whose first byte is `0x00`
    /// (`PREFIX_UNCOMPRESSED`) must round-trip losslessly. Before `encode_blob`
    /// was made self-describing, the `compress=false` branch returned the data
    /// un-prefixed, so `decode_blob` ran `decompress_chunk` on it and mis-stripped
    /// the leading `0x00` — silently dropping a byte (~0.4% of binary chunks).
    /// This is the latent corruption that bit `restore_snapshot`.
    #[test]
    fn encode_decode_roundtrip_no_compression_zero_leading() {
        let data = [0x00u8, 0xAB, 0xCD, 0x01, 0x00, 0xEF];
        let encoded = encode_blob(&data, None, false).unwrap();
        let decoded = decode_blob(&encoded, None).unwrap();
        assert_eq!(
            decoded, data,
            "0x00-leading raw blob must not be mis-stripped"
        );
    }

    /// Same regression through the encrypt-without-compression config: the
    /// decrypted plaintext is fed to `decompress_chunk`, so a `0x00`-leading
    /// payload was mis-stripped after decryption.
    #[test]
    fn encode_decode_roundtrip_encrypted_no_compression_zero_leading() {
        let key = fauna_core::crypto::BackupKey::from_bytes([0x33u8; 32]);
        let data = [0x01u8, 0x00, 0x11, 0x22];
        let encoded = encode_blob(&data, Some(&key), false).unwrap();
        let decoded = decode_blob(&encoded, Some(&key)).unwrap();
        assert_eq!(
            decoded, data,
            "0x01-leading encrypted blob must not be mis-stripped"
        );
    }

    /// Simulate the restore round-trip:
    /// write a SQLite DB → read raw bytes → encode_blob → decode_blob → write back → query.
    #[test]
    fn restore_round_trip() {
        let tmp = tempfile::tempdir().unwrap();
        let db_path = tmp.path().join("test.db");

        // Create DB with some data.
        let conn = rusqlite::Connection::open(&db_path).unwrap();
        conn.execute_batch("CREATE TABLE test (id INTEGER PRIMARY KEY, val TEXT);")
            .unwrap();
        conn.execute("INSERT INTO test (val) VALUES ('hello')", [])
            .unwrap();
        drop(conn);

        // Read raw bytes and encode (compress, no encryption).
        let raw = std::fs::read(&db_path).unwrap();
        let encoded = encode_blob(&raw, None, true).unwrap();

        // Decode and verify bytes match original.
        let decoded = decode_blob(&encoded, None).unwrap();
        assert_eq!(raw, decoded);

        // Write decoded bytes to a new path and query.
        let restored_path = tmp.path().join("restored.db");
        std::fs::write(&restored_path, &decoded).unwrap();
        let conn2 = rusqlite::Connection::open(&restored_path).unwrap();
        let val: String = conn2
            .query_row("SELECT val FROM test", [], |r| r.get(0))
            .unwrap();
        assert_eq!(val, "hello");
    }
}
