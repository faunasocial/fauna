//! The **sink-agnostic whole-blob seal** — chunk → compress → encrypt → re-key → manifest,
//! with no network and no store in sight.
//!
//! # Why this is its own function
//!
//! Two consumers must produce **byte-identical** artifacts from the same bytes:
//!
//! - `fauna_sync_engine::SyncEngine`'s upload path, which ships them to a nest
//!   destination's blob store;
//! - the **client-device custodian** (`fauna_sync_engine::custodian_store`), which writes
//!   them to a local blob store on the owner's own device;
//! - the Go WebDAV MDA's PUT leg (over the FFI, `webdav_seal_file`); and
//! - the Media page's upload into a **content-keyed** set (served or shared —
//!   `fauna_media_machine`, `docs/goal/ui/media.md` § Encryption at rest), the
//!   consumer that moved this module down from `fauna-sync-engine` on
//!   2026-09-26: a page machine compiled for wasm cannot depend on the engine
//!   crate, and a second seal would be the drift this doc warns of.
//!
//! That is not a nice-to-have. `docs/goal/architecture/message-segment-store.md`
//! § Client-device custodian (pull) makes it load-bearing: *"the deterministic
//! seal makes its local artifacts **byte-identical** to a nest destination's, so
//! restore, audit sampling, and any future custodian-to-custodian comparison work
//! uniformly with no per-kind artifact branch."*
//!
//! Before this module the pipeline lived inline in `upload_chunked_bytes`,
//! welded between `check_chunks` and `upload_chunks` — so a local sink could only
//! reuse it by **re-implementing** chunk → compress → encrypt → re-key. Two
//! implementations of a deterministic seal are two implementations that drift,
//! and the drift is invisible until a restore opens nothing: identical plaintext
//! would seal to different ciphertext, different store keys, a different manifest
//! hash. The tree used to carry a live example of exactly that hazard — two
//! forked compression framings that differed in their small-chunk threshold and
//! their "did it help" comparison. That fork is **gone as of 2026-08-16**: there
//! is one implementation, [`crate::compress::compress_chunk_framed`], and
//! the two shipped framings are named constants on
//! [`crate::compress::ChunkFraming`].
//!
//! **Since 2026-09-03 the per-chunk seal itself lives one crate down, in
//! [`crate::chunk_seal`]** — re-exported here as [`seal_chunk_body`] — and
//! the framing is no longer this module's decision *or* a cargo feature's: the
//! drift hazard turned out to be worse than a dedup miss. The Go WebDAV MDA
//! sealed the *raw* chunk while this engine sealed the *framed* one, under the
//! same deterministic (key, nonce) — two plaintexts one byte apart under one
//! nonce, i.e. a two-time pad that recovers the user's bytes with no key
//! (`chunk_seal`'s module doc and witness test). So the AEAD primitive is now
//! crate-private to `fauna-core` and every writer — this engine, the MDA over
//! the FFI, the e2e agent — frames through the one verified door.
//! This corpus is sealed with [`crate::compress::ChunkFraming::FILE_SYNC`],
//! chosen once in `chunk_seal`; changing it there re-keys every newly sealed
//! chunk under 4 KiB.
//!
//! # What stays with the caller
//!
//! Choosing the seal root, and the fail-closed refusal when an owner-only folder
//! engine holds no key. Both are engine-state questions
//! (`SyncEngine::effective_backup_key` /
//! `content_seal_root`), and answering them here would mean this function
//! needed an engine — which is exactly what the custodian does not have.

use crate::chunk::ChunkManifest;
use crate::data::ContentHash;
use anyhow::{Context, Result};

/// One blob, sealed and ready for any sink.
///
/// `chunks` and `manifest_bytes` are exactly the bodies a sink must persist;
/// `manifest_hash` is the value a custody record carries
/// (`docs/goal/architecture/message-segment-store.md` § GC-safety).
pub struct SealedBlob {
    /// The chunk manifest, as the writer's **plaintext view**: `stored_hashes`
    /// is populated iff the blob was sealed, and `file_hash`/`chunk_hashes`
    /// are in the clear for the writer's own use (progress, the held-body
    /// index).
    pub manifest: ChunkManifest,
    /// Canonical dag-cbor encoding of `manifest`'s **wire form**
    /// ([`ChunkManifest::wire_form`]: a sealed blob's plaintext hashes ride
    /// only in `sealed_hashes`) — the bytes a sink stores under
    /// [`Self::manifest_hash`].
    pub manifest_bytes: Vec<u8>,
    /// `blake3(manifest_bytes)`.
    pub manifest_hash: ContentHash,
    /// `(store key, body)` per chunk, in manifest order. The store key is the
    /// **ciphertext** hash for a sealed blob and the plaintext hash for a
    /// plaintext one — the discriminator every reader already applies via
    /// [`ChunkManifest::store_keys`].
    pub chunks: Vec<(ContentHash, Vec<u8>)>,
    /// The M2 content-key generation the chunks were sealed under, to stamp into
    /// a change record. `None` for owner-only / plaintext seals.
    pub content_key_version: Option<u64>,
}

impl SealedBlob {
    /// Bytes this blob occupies in a store that holds every chunk plus the
    /// manifest — what a capacity cap is measured against
    /// (`docs/goal/behavior/backup-destinations.md` § Third destination kind).
    ///
    /// Deliberately counts the **stored** (sealed, compressed) bodies rather than
    /// the plaintext size: a cap is about disk, and the two differ in both
    /// directions (compression shrinks, the AEAD frame grows).
    pub fn stored_bytes(&self) -> u64 {
        let chunks: u64 = self.chunks.iter().map(|(_, b)| b.len() as u64).sum();
        chunks.saturating_add(self.manifest_bytes.len() as u64)
    }
}

/// Seal ONE chunk's body — frame, then encrypt keyed by the chunk's
/// **plaintext** hash, returning `(store key, body)` where the store key is the
/// **ciphertext** hash.
///
/// This is the single per-chunk pipeline behind [`seal_blob`]'s batch form and
/// the streaming upload path's per-chunk loop — and the share leg's serve-side
/// chunk re-derivation, which must reproduce a previously uploaded body
/// byte-for-byte from `(plaintext range, generation key)` alone. It IS
/// [`crate::chunk_seal::seal_chunk_body`], re-exported: the owner moved
/// down to `fauna-core` on 2026-09-03 so that the Go WebDAV MDA (over the FFI)
/// and the e2e agent seal through the same function this engine does — a
/// second copy of compress-then-encrypt is not merely a fork that drifts into
/// "peer cannot serve this chunk"; on this plane it is a keystream reuse
/// (module doc).
pub use crate::chunk_seal::seal_chunk_body;

/// Seal `bytes` into storable artifacts.
///
/// `seal_root` is `Some((convergent chunk root, content-key generation))` to seal,
/// `None` to leave the chunks plaintext. Passing `None` for content that must rest
/// sealed is the caller's error to prevent — see the module doc.
///
/// Deterministic: the same `(bytes, seal_root)` always yields the same manifest
/// hash and the same store keys. That is what makes a destination's dedup and a
/// custodian's local store converge on retries, and what the byte-identity
/// property above rests on.
pub fn seal_blob(bytes: &[u8], seal_root: Option<([u8; 32], Option<u64>)>) -> Result<SealedBlob> {
    let mut chunks: Vec<(ContentHash, Vec<u8>)> = Vec::new();
    let sealed = seal_reader(bytes, seal_root, |key, body| {
        chunks.push((key, body));
        Ok(())
    })?;
    Ok(SealedBlob {
        manifest: sealed.manifest,
        manifest_bytes: sealed.manifest_bytes,
        manifest_hash: sealed.manifest_hash,
        chunks,
        content_key_version: sealed.content_key_version,
    })
}

/// A blob sealed by [`seal_reader`]: everything [`SealedBlob`] carries except
/// the chunk bodies, which went to the caller's sink as they were made.
pub struct SealedManifest {
    /// The chunk manifest, as [`SealedBlob::manifest`] (the plaintext view).
    pub manifest: ChunkManifest,
    /// As [`SealedBlob::manifest_bytes`] (the wire form).
    pub manifest_bytes: Vec<u8>,
    /// `blake3(manifest_bytes)`.
    pub manifest_hash: ContentHash,
    /// As [`SealedBlob::content_key_version`].
    pub content_key_version: Option<u64>,
    /// As [`SealedBlob::stored_bytes`]: every body handed to the sink, in
    /// manifest order and repeats included, plus the manifest.
    pub stored_bytes: u64,
}

/// [`seal_blob`] over a **reader** — the form a caller uses whose blob is too
/// large to hold, such as a custodian sealing a segment it streamed to disk
/// (`docs/goal/architecture/message-segment-store.md` § Segment size: a
/// transfer's memory is a constant chunk, never the body).
///
/// Each `(store key, body)` goes to `sink` in manifest order as soon as it is
/// sealed, so what this holds at once is one chunk (at most `MAX_CHUNK`) and
/// FastCDC's own window, never the blob. The result is byte-identical to
/// [`seal_blob`] over the same bytes, which is now a thin wrapper over this:
/// the chunking follows [`chunk_file`]'s rules (a blob under its single-chunk
/// threshold is one chunk; a larger one is cut by FastCDC at the same
/// parameters, whose streaming form cuts where the whole-buffer form does), and
/// every chunk seals through [`seal_chunk_body`].
///
/// [`chunk_file`]: crate::chunker::chunk_file
pub fn seal_reader(
    mut reader: impl std::io::Read,
    seal_root: Option<([u8; 32], Option<u64>)>,
    mut sink: impl FnMut(ContentHash, Vec<u8>) -> Result<()>,
) -> Result<SealedManifest> {
    use crate::chunker::{AVG_CHUNK, MAX_CHUNK, MIN_CHUNK, SINGLE_CHUNK_THRESHOLD};
    use std::io::Read;

    let mut chunk_hashes = Vec::new();
    let mut chunk_sizes = Vec::new();
    let mut store_keys = Vec::new();
    let mut body_bytes: u64 = 0;
    let mut file_hasher = blake3::Hasher::new();

    // Seal (compress + encrypt + re-key) when a root was supplied, through the
    // one per-chunk pipeline ([`seal_chunk_body`]).
    //
    // Every sealed chunk is keyed in the blob store by its **ciphertext** hash
    // (recorded in `manifest.stored_hashes`), not the plaintext hash — that is
    // what lets an AEAD body satisfy the nest route's F9 check
    // (`blake3(body) == X-Content-Hash`) with no route change (FS-BIND, PIECE 6;
    // `mls-group-key-material.md` § M2). The plaintext hash stays in
    // `manifest.chunk_hashes` as the AEAD key/nonce salt + integrity anchor.
    // Both roots seal through the deterministic `chunk_crypto` primitive
    // (key+nonce derive from the content hash), so the ciphertext hash is a
    // stable content address: it dedups across group members at the same
    // generation (content key), and across an owner's backup passes / devices
    // (`BackupKey::convergent_chunk_root` — FS-BIND FOLLOW-ON A, user-ratified
    // 2026-07-07; the destination learns only which of this owner's chunks are
    // equal).
    //
    // The unsealed path frames through the same door (the framed plaintext IS
    // the stored body) — the store key stays the plaintext hash (the manifest
    // records no `stored_hashes`).
    let mut seal_one = |plain: &[u8]| -> Result<()> {
        let plain_hash = ContentHash::of_raw(plain);
        let (key, body) = match seal_root {
            Some((secret, _)) => seal_chunk_body(&plain_hash, plain, &secret)?,
            None => (
                plain_hash,
                crate::chunk_seal::FramedChunk::frame(&plain_hash, plain)?.into_body(),
            ),
        };
        chunk_hashes.push(plain_hash);
        chunk_sizes.push(plain.len() as u64);
        store_keys.push(key);
        body_bytes = body_bytes.saturating_add(body.len() as u64);
        sink(key, body)
    };

    // Read up to the single-chunk threshold first: a blob that ends before it
    // is one chunk, exactly as `chunk_file` decides by length.
    // Grown by hand in bounded reads so its capacity never passes the
    // threshold (one `MAX_CHUNK`) — a caller measuring this seal's largest
    // allocation sees a chunk, whatever the blob.
    let mut head = Vec::new();
    let mut piece = vec![0u8; 64 * 1024];
    while (head.len() as u64) < SINGLE_CHUNK_THRESHOLD {
        let want = piece
            .len()
            .min((SINGLE_CHUNK_THRESHOLD - head.len() as u64) as usize);
        let n = match reader.read(&mut piece[..want]) {
            Ok(0) => break,
            Ok(n) => n,
            Err(e) if e.kind() == std::io::ErrorKind::Interrupted => continue,
            Err(e) => return Err(e).context("seal_reader: reading the blob"),
        };
        head.extend_from_slice(&piece[..n]);
    }
    drop(piece);
    let total_size = if (head.len() as u64) < SINGLE_CHUNK_THRESHOLD {
        file_hasher.update(&head);
        seal_one(&head)?;
        head.len() as u64
    } else {
        let mut total = 0u64;
        let stream = std::io::Cursor::new(head).chain(reader);
        for chunk in fastcdc::v2020::StreamCDC::new(stream, MIN_CHUNK, AVG_CHUNK, MAX_CHUNK) {
            let chunk = chunk.map_err(|e| anyhow::anyhow!("seal_reader: chunking: {e}"))?;
            file_hasher.update(&chunk.data);
            total += chunk.data.len() as u64;
            seal_one(&chunk.data)?;
        }
        total
    };

    let manifest = ChunkManifest {
        file_hash: ContentHash::from_digest_raw(*file_hasher.finalize().as_bytes()),
        total_size,
        chunk_hashes,
        chunk_sizes,
        stored_hashes: seal_root.map(|_| store_keys),
        sealed_hashes: None,
        min_reader: None,
    };
    let manifest_bytes = crate::encoding::canonical_encode(
        &manifest.wire_form(seal_root.map(|(secret, _)| secret).as_ref())?,
    )
    .context("seal_blob: serializing manifest")?;
    let manifest_hash = ContentHash::of_raw(&manifest_bytes);

    Ok(SealedManifest {
        stored_bytes: body_bytes.saturating_add(manifest_bytes.len() as u64),
        manifest,
        manifest_bytes,
        manifest_hash,
        content_key_version: seal_root.and_then(|(_, version)| version),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::chunker::{chunk_file, extract_chunks};

    const ROOT_A: [u8; 32] = [7u8; 32];
    const ROOT_B: [u8; 32] = [9u8; 32];

    fn body() -> Vec<u8> {
        // Larger than the sync-engine compressor's 4 KiB floor and highly
        // compressible, so the compression arm is actually exercised.
        b"the owner's mail segment, over and over. "
            .iter()
            .cycle()
            .take(40_000)
            .copied()
            .collect()
    }

    /// A sealed-chunk manifest names no plaintext hash: the seal moves
    /// `file_hash`/`chunk_hashes` into `sealed_hashes` under the same root and
    /// stamps `min_reader = 2`, so the destination holding the manifest has no
    /// candidate-plaintext oracle (`mls-group-key-material.md` § M2 *Sealed
    /// manifest hashes*) — and the holder of the root gets them back intact.
    /// The unsealed arm keeps its plaintext hashes: the stored bodies ARE the
    /// plaintext there.
    #[test]
    fn a_sealed_blobs_manifest_carries_no_plaintext_hashes() {
        let bytes = body();
        let sealed = seal_blob(&bytes, Some((ROOT_A, Some(4)))).unwrap();
        let decoded: ChunkManifest = crate::encoding::canonical_decode(&sealed.manifest_bytes)
            .expect("decode the uploaded manifest");
        assert!(
            decoded.chunk_hashes.is_empty(),
            "plaintext chunk hashes on the wire"
        );
        assert_eq!(decoded.file_hash, crate::chunk::blank_file_hash());
        assert!(
            decoded.sealed_hashes.is_some(),
            "no sealed hashes on the wire"
        );
        assert_eq!(decoded.min_reader, Some(2));
        decoded
            .check_hash_shape()
            .expect("the writer's shape is the reader's");

        let opened = decoded.unseal_hashes(&ROOT_A).expect("the root opens it");
        let plain = chunk_file(&bytes);
        assert_eq!(opened.file_hash, plain.file_hash);
        assert_eq!(opened.chunk_hashes, plain.chunk_hashes);

        let unsealed = seal_blob(&bytes, None).unwrap();
        assert_eq!(unsealed.manifest.chunk_hashes, plain.chunk_hashes);
        assert!(unsealed.manifest.sealed_hashes.is_none());
    }

    /// The property the whole module exists for: the same bytes under the same
    /// root seal to the same artifacts, every time and on every device. A nest
    /// destination and a client custodian therefore hold identical bytes.
    #[test]
    fn the_seal_is_deterministic_so_two_custodians_hold_identical_bytes() {
        let bytes = body();
        let a = seal_blob(&bytes, Some((ROOT_A, None))).unwrap();
        let b = seal_blob(&bytes, Some((ROOT_A, None))).unwrap();

        assert_eq!(a.manifest_hash, b.manifest_hash);
        assert_eq!(a.manifest_bytes, b.manifest_bytes);
        assert_eq!(
            a.chunks.iter().map(|(h, _)| *h).collect::<Vec<_>>(),
            b.chunks.iter().map(|(h, _)| *h).collect::<Vec<_>>(),
        );
        assert_eq!(
            a.chunks.iter().map(|(_, c)| c.clone()).collect::<Vec<_>>(),
            b.chunks.iter().map(|(_, c)| c.clone()).collect::<Vec<_>>(),
        );
    }

    /// Convergence is **per owner**: a different root must not produce the same
    /// ciphertext, or one owner's dedup would leak equality to another's.
    #[test]
    fn a_different_root_seals_to_different_bytes() {
        let bytes = body();
        let a = seal_blob(&bytes, Some((ROOT_A, None))).unwrap();
        let b = seal_blob(&bytes, Some((ROOT_B, None))).unwrap();

        assert_ne!(a.manifest_hash, b.manifest_hash);
        assert_ne!(
            a.chunks.iter().map(|(h, _)| *h).collect::<Vec<_>>(),
            b.chunks.iter().map(|(h, _)| *h).collect::<Vec<_>>(),
        );
        // The plaintext anchors are identical either way — only the store keys,
        // the bodies and the sealed hashes move.
        assert_eq!(a.manifest.chunk_hashes, b.manifest.chunk_hashes);
    }

    /// A sealed manifest must carry `stored_hashes`, and they must be the keys
    /// the bodies actually hash to — that pairing is what every reader's
    /// `store_keys()` walk depends on.
    #[test]
    fn store_keys_are_the_ciphertext_hashes_the_bodies_hash_to() {
        let sealed = seal_blob(&body(), Some((ROOT_A, None))).unwrap();

        let stored = sealed
            .manifest
            .stored_hashes
            .as_ref()
            .expect("a sealed manifest records its store keys");
        assert_eq!(stored.len(), sealed.manifest.chunk_hashes.len());
        for ((key, data), recorded) in sealed.chunks.iter().zip(stored) {
            assert_eq!(*key, ContentHash::of_raw(data));
            assert_eq!(key, recorded);
        }
        assert_eq!(sealed.manifest.store_keys(), *stored);
    }

    /// A plaintext seal keeps the store key equal to the plaintext hash — the
    /// discriminator `resolve_chunk_open_policy` reads.
    #[test]
    fn a_plaintext_seal_records_no_store_keys() {
        let sealed = seal_blob(&body(), None).unwrap();

        assert!(sealed.manifest.stored_hashes.is_none());
        assert_eq!(sealed.content_key_version, None);
        assert_eq!(sealed.manifest.store_keys(), sealed.manifest.chunk_hashes);
    }

    /// The generation stamp rides through untouched — the change record needs it
    /// to pick the right content key at open time.
    #[test]
    fn the_content_key_generation_rides_through() {
        let sealed = seal_blob(&body(), Some((ROOT_A, Some(4)))).unwrap();
        assert_eq!(sealed.content_key_version, Some(4));
    }

    /// The cap is measured against what lands on disk, not the plaintext size.
    #[test]
    fn stored_bytes_counts_every_chunk_plus_the_manifest() {
        let sealed = seal_blob(&body(), Some((ROOT_A, None))).unwrap();
        let expected: u64 = sealed
            .chunks
            .iter()
            .map(|(_, b)| b.len() as u64)
            .sum::<u64>()
            + sealed.manifest_bytes.len() as u64;
        assert_eq!(sealed.stored_bytes(), expected);
    }

    /// `seal_chunk_body` IS the batch seal's per-chunk pipeline: sealing each
    /// chunk individually reproduces `seal_blob`'s store keys and bodies
    /// exactly. The streaming upload path and the share leg's serve-side
    /// chunk re-derivation both ride it, so this equality is what keeps three
    /// call sites one implementation (the module doc's drift hazard).
    #[test]
    fn seal_chunk_body_is_the_batch_seals_per_chunk_pipeline() {
        let bytes = body();
        let sealed = seal_blob(&bytes, Some((ROOT_A, Some(3)))).unwrap();

        let manifest = chunk_file(&bytes);
        let plain = extract_chunks(&bytes, &manifest);
        assert_eq!(plain.len(), sealed.chunks.len());
        for ((plain_hash, plain_data), (store_key, body)) in plain.iter().zip(&sealed.chunks) {
            let (k, b) = seal_chunk_body(plain_hash, plain_data, &ROOT_A).unwrap();
            assert_eq!(&k, store_key);
            assert_eq!(&b, body);
        }
    }

    /// An empty blob is still a blob: it must seal, and its manifest must be
    /// storable. A custodian pulling a freshly-created empty segment would
    /// otherwise fail its whole pass.
    #[test]
    fn an_empty_blob_seals_without_panicking() {
        let sealed = seal_blob(&[], Some((ROOT_A, None))).unwrap();
        assert!(!sealed.manifest_bytes.is_empty());
        assert_eq!(sealed.manifest.total_size, 0);
    }

    /// Deterministic incompressible bytes, so FastCDC finds real cut points.
    fn noise(len: usize) -> Vec<u8> {
        let mut x: u64 = 0x9E37_79B9_7F4A_7C15;
        (0..len)
            .map(|_| {
                x ^= x << 13;
                x ^= x >> 7;
                x ^= x << 17;
                x as u8
            })
            .collect()
    }

    /// [`seal_reader`] is the seal over a **reader**, and it must be the same
    /// seal. Over every size class `chunk_file` distinguishes — empty, one
    /// chunk just under its threshold, exactly at it, and several FastCDC
    /// chunks — it yields the manifest `chunk_file` defines and, chunk by
    /// chunk, the bodies `seal_chunk_body` makes of it. That equality is what
    /// lets a custodian stream a segment off disk and still hold artifacts
    /// byte-identical to a nest destination's (module doc).
    #[test]
    fn seal_reader_reproduces_chunk_file_and_the_per_chunk_seal_at_every_size() {
        let threshold = crate::chunker::SINGLE_CHUNK_THRESHOLD as usize;
        for len in [0, 1, threshold - 1, threshold, threshold + 20 * 1024 * 1024] {
            let bytes = noise(len);
            let mut streamed: Vec<(ContentHash, Vec<u8>)> = Vec::new();
            let sealed = seal_reader(&bytes[..], Some((ROOT_A, Some(2))), |key, body| {
                streamed.push((key, body));
                Ok(())
            })
            .unwrap();

            let mut expected = chunk_file(&bytes);
            let bodies: Vec<(ContentHash, Vec<u8>)> = extract_chunks(&bytes, &expected)
                .iter()
                .map(|(h, d)| seal_chunk_body(h, d, &ROOT_A).unwrap())
                .collect();
            expected.stored_hashes = Some(bodies.iter().map(|(k, _)| *k).collect());
            let expected_bytes =
                crate::encoding::canonical_encode(&expected.clone().seal_hashes(&ROOT_A).unwrap())
                    .unwrap();

            assert!(
                sealed.manifest_bytes == expected_bytes,
                "manifest differs at len {len}"
            );
            assert_eq!(sealed.manifest_hash, ContentHash::of_raw(&expected_bytes));
            assert!(streamed == bodies, "sealed bodies differ at len {len}");
            assert_eq!(sealed.content_key_version, Some(2));
            assert_eq!(
                sealed.stored_bytes,
                bodies.iter().map(|(_, b)| b.len() as u64).sum::<u64>()
                    + sealed.manifest_bytes.len() as u64,
            );
            if len > threshold {
                assert!(
                    expected.chunk_hashes.len() > 2,
                    "len {len} must cut several chunks"
                );
            }
        }
    }
}
