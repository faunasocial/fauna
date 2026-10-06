//! The file-sync chunk plane's **one** write-side seal: frame → encrypt → re-key.
//!
//! # The invariant this module makes structural
//!
//! On this plane the value hashed for the AEAD nonce and the value the AEAD
//! encrypts are **different objects**: the nonce salt is the BLAKE3 digest of
//! the *unframed* plaintext chunk (`ChunkManifest::chunk_hashes[i]`, the
//! address the content-addressed store already keys it by), while the AEAD
//! plaintext is the *framed* body — `0x00 ‖ P` or `0x01 ‖ zstd(P)`
//! (`crate::compress` § Pipeline order: `hash (on raw data) -> compress ->
//! encrypt`). `chunk_crypto` derives a deterministic (key, nonce) from
//! `(root, plaintext hash)` so identical chunks dedup; that is only safe while
//! **one plaintext hash ever meets one AEAD plaintext**, i.e. while framing is
//! a single fixed function of the chunk.
//!
//! Until 2026-09-03 that was an unenforced convention spanning a Rust crate
//! and a Go binary, and it did not hold: the sync engine sealed the framed
//! body while the Go WebDAV MDA sealed the raw chunk through a bare
//! `encrypt_chunk` FFI export. Two ciphertexts of one
//! chunk under one (key, nonce) with plaintexts one byte apart — XOR the two
//! and the keystream cancels; the plaintext unrolls with no key, and the
//! Poly1305 one-time key is reused. The reader tolerated both shapes, so no
//! test, log line or error ever surfaced it.
//!
//! So the door is now the type system, not a comment:
//!
//! - [`chunk_crypto::encrypt_chunk`](crate::chunk_crypto) is `pub(crate)`. No
//!   raw `&[u8]` can reach the AEAD from outside this crate.
//! - The only production constructor of a [`FramedChunk`] is
//!   [`FramedChunk::frame`], which **verifies** the caller's hash against the
//!   plaintext and frames it with the file-sync corpus's one framing
//!   ([`ChunkFraming::FILE_SYNC`]). A third writer — another binary, another
//!   language, a feature-off build — cannot frame differently because it
//!   cannot frame at all; it calls [`seal_chunk_body`].
//! - There is no build-time switch on the framing any more: the old
//!   `compress_chunks` cargo feature was retired with this module, because a
//!   feature-off build was itself a second writer sealing raw under the same
//!   (key, nonce).
//!
//! # Why not "hash the framed body instead"
//!
//! That would make the hashed value and the encrypted value one object, but it
//! re-keys every chunk sealed since compression landed (a different nonce for
//! the same content — an at-rest migration, not a patch) and it breaks the
//! design's dedup premise, where the plaintext hash is the address every reader
//! and the manifest already carry. Fixing the *framing* rather than the
//! *hashing* keeps every existing ciphertext valid and every store key stable.
//!
//! # Where it is called from
//!
//! `fauna_sync_engine::seal` re-exports [`seal_chunk_body`] as the engine's
//! per-chunk pipeline (batch seal, streaming upload, the share leg's serve-side
//! re-derivation); the transfer worker's drain map, the Go WebDAV MDA (via `fauna_ffi::webdav_seal_file`) and the
//! e2e agent's `fauna_folder_seal_file` all seal through here. One
//! implementation — `docs/goal/behavior/p2p.md` § Chunk bodies re-derive
//! through the ONE per-chunk seal pipeline.

use anyhow::{Result, bail};

use crate::compress::{ChunkFraming, compress_chunk_framed};
use crate::data::ContentHash;

/// A chunk body ready for the AEAD: the **framed** plaintext, paired with the
/// hash of the **unframed** plaintext it was framed from.
///
/// Fields are private on purpose — outside this crate the only way to obtain
/// one is [`FramedChunk::frame`], which is what makes "every writer frames
/// identically" a property of the type rather than of every caller's
/// discipline (module doc).
pub struct FramedChunk {
    plain_hash: ContentHash,
    body: Vec<u8>,
}

/// Hash + framed length only — never the body, which is user content.
impl std::fmt::Debug for FramedChunk {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("FramedChunk")
            .field("plain_hash", &hex::encode(self.plain_hash.digest()))
            .field("framed_len", &self.body.len())
            .finish()
    }
}

impl FramedChunk {
    /// Frame `plain` for sealing, after verifying that `plain_hash` really is
    /// its digest.
    ///
    /// The verify is the second half of the invariant: `chunk_crypto` derives
    /// the (key, nonce) from `plain_hash`, so a caller passing some *other*
    /// chunk's hash would pair that chunk's (key, nonce) with this plaintext —
    /// the same two-plaintexts-one-nonce failure, reached through a bug rather
    /// than a second framing. Cheap (BLAKE3 runs at several GB/s; zstd below
    /// is the slow stage) and it turns a silent keystream reuse into an error.
    pub fn frame(plain_hash: &ContentHash, plain: &[u8]) -> Result<Self> {
        let actual = ContentHash::of_raw(plain);
        if actual != *plain_hash {
            bail!(
                "chunk_seal: refusing to seal — the plaintext hashes to {} but the caller \
                 named {} (sealing under another chunk's hash would reuse its key and nonce)",
                hex::encode(actual.digest()),
                hex::encode(plain_hash.digest())
            );
        }
        Ok(Self {
            plain_hash: *plain_hash,
            body: compress_chunk_framed(plain, ChunkFraming::FILE_SYNC),
        })
    }

    /// The hash of the unframed plaintext — the manifest's `chunk_hashes[i]`,
    /// and the AEAD key/nonce salt.
    pub fn plain_hash(&self) -> &ContentHash {
        &self.plain_hash
    }

    /// The framed body — what the AEAD encrypts, and what a *plaintext*
    /// (unsealed) manifest stores as-is under `plain_hash`.
    pub fn body(&self) -> &[u8] {
        &self.body
    }

    /// Consume into the framed body (the plaintext-manifest store shape).
    pub fn into_body(self) -> Vec<u8> {
        self.body
    }

    /// **Fixtures only:** an arbitrary `body` under `plain_hash`, bypassing both
    /// the verify and the framing — for tests that must reproduce what a
    /// *raw writer* (a live raw-body writer, read back by the settled raw
    /// fallback) or an *adversary* (a different plaintext
    /// under a victim's salt) put in the store. Compiled only into test builds;
    /// a production caller cannot reach it.
    ///
    /// The `debug_assertions` arm is convention 15 rule (a)'s shared-crate
    /// visibility gate (`e2e-automation-surface-gating.md` § The convention):
    /// it is what lets a plain debug build of an in-process consumer reach the
    /// seam — `just tui-debug` and the harness's own `_ensure_client_built`
    /// pass no features, so a feature-only gate is compiled out of the very
    /// build the e2e suite runs. Release strips it either way.
    #[cfg(any(test, debug_assertions, feature = "test-helpers"))]
    pub fn arbitrary_for_fixtures(plain_hash: ContentHash, body: Vec<u8>) -> Self {
        Self { plain_hash, body }
    }
}

/// [`FramedChunk::frame`] over `(plaintext hash, plaintext)` pairs — the
/// chunker's output shape — in order.
pub fn frame_chunks(chunks: &[(ContentHash, Vec<u8>)]) -> Result<Vec<FramedChunk>> {
    chunks
        .iter()
        .map(|(hash, plain)| FramedChunk::frame(hash, plain))
        .collect()
}

/// Seal one already-framed chunk under `root`, returning `(store key, body)`
/// where the store key is the **ciphertext** hash (the content-addressed
/// blob-store key, recorded in `ChunkManifest::stored_hashes`).
///
/// The two-step form of [`seal_chunk_body`], for a caller that seals one
/// framing under several roots (the transfer worker's drain map tries every
/// retained content-key generation): frame once, seal per root.
pub fn seal_framed_chunk(framed: &FramedChunk, root: &[u8; 32]) -> Result<(ContentHash, Vec<u8>)> {
    let ciphertext = crate::chunk_crypto::encrypt_chunk(root, &framed.plain_hash, &framed.body)?;
    Ok((ContentHash::of_raw(&ciphertext), ciphertext))
}

/// Seal ONE chunk — frame, then encrypt keyed by the chunk's **plaintext** hash
/// — returning `(store key, body)` where the store key is the **ciphertext**
/// hash.
///
/// This is the single per-chunk pipeline behind the sync engine's batch seal,
/// its streaming upload loop and the share leg's serve-side chunk
/// re-derivation (which must reproduce a previously uploaded body byte-for-byte
/// from `(plaintext range, generation key)` alone), and the Go WebDAV MDA's
/// PUT. One implementation: a second copy of
/// compress-then-encrypt is a fork that drifts, and on this plane drift is not
/// an interop bug but a keystream reuse (module doc).
pub fn seal_chunk_body(
    plain_hash: &ContentHash,
    plain: &[u8],
    root: &[u8; 32],
) -> Result<(ContentHash, Vec<u8>)> {
    seal_framed_chunk(&FramedChunk::frame(plain_hash, plain)?, root)
}

/// [`seal_chunk_body`] over `(plaintext hash, plaintext)` pairs, in order.
pub fn seal_chunk_bodies(
    chunks: &[(ContentHash, Vec<u8>)],
    root: &[u8; 32],
) -> Result<Vec<(ContentHash, Vec<u8>)>> {
    chunks
        .iter()
        .map(|(hash, plain)| seal_chunk_body(hash, plain, root))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::chunk_crypto::{ENCRYPTION_OVERHEAD, decrypt_chunk, encrypt_chunk};
    use crate::compress::unframe_verified_chunk;

    const ROOT: [u8; 32] = [7u8; 32];

    fn plain(n: usize) -> Vec<u8> {
        (0..n).map(|i| (i * 7 + 13) as u8).collect()
    }

    /// The seal IS frame-then-encrypt under the file-sync framing — pinned
    /// against the two primitives it composes, so a change to either shows
    /// here as a byte difference.
    #[test]
    fn seal_chunk_body_is_frame_then_encrypt_under_the_file_sync_framing() {
        for plain in [plain(100), vec![b'a'; 8192]] {
            let hash = ContentHash::of_raw(&plain);
            let (store_key, body) = seal_chunk_body(&hash, &plain, &ROOT).unwrap();
            let expected = encrypt_chunk(
                &ROOT,
                &hash,
                &compress_chunk_framed(&plain, ChunkFraming::FILE_SYNC),
            )
            .unwrap();
            assert_eq!(body, expected);
            assert_eq!(store_key, ContentHash::of_raw(&body));
            // And it opens back to the plaintext through the shared reader.
            let opened = decrypt_chunk(&ROOT, &hash, &body).unwrap();
            assert_eq!(unframe_verified_chunk(opened, &hash).unwrap(), plain);
        }
    }

    /// `chunker::MAX_STORED_CHUNK_BODY` is the sealed size of the worst case
    /// the seal can produce: a maximum-size chunk that zstd cannot shrink. A
    /// puller refuses any declared body length above it, so the constant being
    /// even one byte low would refuse a legitimate chunk.
    #[test]
    fn the_largest_sealed_chunk_is_exactly_max_stored_chunk_body() {
        let mut incompressible = vec![0u8; crate::chunker::MAX_CHUNK as usize];
        blake3::Hasher::new()
            .update(b"incompressible")
            .finalize_xof()
            .fill(&mut incompressible);
        let hash = ContentHash::of_raw(&incompressible);
        let (_, body) = seal_chunk_body(&hash, &incompressible, &ROOT).unwrap();
        assert_eq!(
            body.len() as u64,
            crate::chunker::MAX_STORED_CHUNK_BODY,
            "a sealed maximum-size incompressible chunk must fill the bound exactly"
        );
    }

    /// Every public door yields the same bytes — one plaintext, one ciphertext.
    #[test]
    fn every_seal_door_yields_one_ciphertext_per_plaintext() {
        let chunks: Vec<(ContentHash, Vec<u8>)> = [plain(10), plain(5000), vec![b'z'; 6000]]
            .into_iter()
            .map(|p| (ContentHash::of_raw(&p), p))
            .collect();
        let batch = seal_chunk_bodies(&chunks, &ROOT).unwrap();
        let framed = frame_chunks(&chunks).unwrap();
        for (i, (hash, plain)) in chunks.iter().enumerate() {
            let single = seal_chunk_body(hash, plain, &ROOT).unwrap();
            let two_step = seal_framed_chunk(&framed[i], &ROOT).unwrap();
            assert_eq!(batch[i], single);
            assert_eq!(two_step, single);
        }
    }

    /// The verify half of the invariant: a hash that is not the plaintext's is
    /// refused, never sealed under the other chunk's (key, nonce).
    #[test]
    fn frame_refuses_a_hash_that_is_not_the_plaintexts() {
        let a = plain(64);
        let b = plain(65);
        let err = FramedChunk::frame(&ContentHash::of_raw(&a), &b).unwrap_err();
        assert!(
            err.to_string().contains("refusing to seal"),
            "unexpected error: {err}"
        );
        assert!(seal_chunk_body(&ContentHash::of_raw(&a), &b, &ROOT).is_err());
    }

    /// The finding this module exists for, kept as a witness: seal one chunk
    /// RAW through the crate-private primitive and FRAMED through the door,
    /// strip the two Poly1305 tags, and the keystream cancels — because the
    /// framed plaintext is the raw one shifted by a byte, `a[0]^b[0] = P[0]`
    /// and `P[i] = P[i-1] ^ (a[i]^b[i])`. The plaintext unrolls with no key.
    /// This is why `encrypt_chunk` is `pub(crate)` and every writer frames
    /// through [`FramedChunk::frame`]: a second framing is not an interop
    /// bug, it is a two-time pad.
    #[test]
    fn a_raw_and_a_framed_seal_of_one_chunk_leak_the_plaintext_with_no_key() {
        // Under the 4096-byte floor and incompressible-ish, so the frame is
        // exactly `0x00 ‖ P` (the shift-by-one shape).
        let p = plain(512);
        let hash = ContentHash::of_raw(&p);
        let raw = encrypt_chunk(&ROOT, &hash, &p).unwrap(); // the old MDA door
        let (_, framed) = seal_chunk_body(&hash, &p, &ROOT).unwrap(); // the engine
        assert_eq!(raw.len(), p.len() + ENCRYPTION_OVERHEAD);
        assert_eq!(framed.len(), p.len() + 1 + ENCRYPTION_OVERHEAD);
        let a = &raw[..p.len()];
        let b = &framed[..p.len()];

        let mut recovered = Vec::with_capacity(p.len());
        recovered.push(a[0] ^ b[0]);
        for i in 1..p.len() {
            let prev = recovered[i - 1];
            recovered.push(prev ^ a[i] ^ b[i]);
        }
        assert_eq!(
            recovered, p,
            "two framings under one (key, nonce) recover the plaintext by XOR"
        );
    }

    /// The fixtures door exists for tests only and really does bypass framing.
    #[test]
    fn the_fixtures_door_seals_an_arbitrary_body_under_a_hash() {
        let p = plain(32);
        let hash = ContentHash::of_raw(&p);
        let arbitrary = FramedChunk::arbitrary_for_fixtures(hash, p.clone());
        let (_, body) = seal_framed_chunk(&arbitrary, &ROOT).unwrap();
        assert_eq!(body, encrypt_chunk(&ROOT, &hash, &p).unwrap());
    }
}
