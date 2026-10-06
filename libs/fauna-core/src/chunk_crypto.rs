//! Per-chunk authenticated encryption using ChaCha20-Poly1305.
//!
//! Each chunk is encrypted with a key + nonce derived from a root secret and
//! the chunk's **content hash** (the BLAKE3 digest of the **unframed**
//! plaintext chunk that the content-addressed blob store already keys it by).
//! Deriving from content — not file position — gives **convergent
//! encryption**: identical plaintext under the same root secret yields
//! byte-identical ciphertext, so the blob store dedups it; distinct plaintext
//! yields a distinct key *and* nonce, so a ChaCha20 keystream is never reused
//! across two *distinct* chunks at one epoch. This is the same convergent
//! pattern `fauna-mls::blob_crypto::encrypt_blob` uses.
//!
//! **What the AEAD actually encrypts is not the hashed value.** The file-sync
//! plane hashes the raw chunk and encrypts its *framed* body
//! (`crate::compress` § Pipeline order), so the nonce salt and the AEAD
//! plaintext are different objects, and "distinct plaintext ⇒ distinct
//! nonce" only protects the plane while every writer frames one chunk the
//! same way. That is why the seal primitive here ([`encrypt_chunk`]) is
//! **`pub(crate)`** and the only door to it is
//! [`crate::chunk_seal`], whose [`FramedChunk`](crate::chunk_seal::FramedChunk)
//! can be built solely by the one verified framing step. The decrypt side stays
//! public: a reader is harmless. (The Go WebDAV MDA sealed raw through a bare
//! export of `encrypt_chunk` until 2026-09-03 — two plaintexts under one
//! (key, nonce), plaintext recoverable by XOR; `chunk_seal`'s module doc and
//! its witness test carry the full account.)
//!
//! The root secret is an MLS group's exported `epoch_secret` (label
//! `"fauna.chunk.v1"`) for cross-user shared folders — see
//! `docs/goal/architecture/mls-group-key-material.md` § Audience: an MLS group.
//! (An earlier revision keyed off the chunk's *file position*, which broke
//! dedup and risked nonce reuse across files at one epoch; corrected to
//! content-hash keying (tracked internally).)

use crate::data::ContentHash;
use anyhow::Result;
use chacha20poly1305::{
    ChaCha20Poly1305, Nonce,
    aead::{Aead, KeyInit},
};

/// Overhead added by encryption (Poly1305 tag).
pub const ENCRYPTION_OVERHEAD: usize = 16;

/// Derive a per-chunk encryption key from a root secret and the chunk's
/// content hash.
///
/// Two-step BLAKE3 derivation matching `derive_post_key` in
/// `subscription/crypto.rs`: `derive_key("fauna.chunk.v1", root_secret)` for
/// context separation, then `keyed_hash(intermediate, content_hash)` for the
/// per-chunk salt. Keying off the content hash (not file position) is what
/// makes dedup convergent and keystream reuse impossible across distinct
/// chunks: distinct plaintext has a distinct BLAKE3 digest, so it gets a
/// distinct key. (Across two *framings* of one chunk it is not the key that
/// protects — see [`derive_chunk_nonce`].)
fn derive_chunk_key(root_secret: &[u8; 32], content_hash: &ContentHash) -> [u8; 32] {
    crate::domain_key::derive_domain_key("fauna.chunk.v1", root_secret, &content_hash.digest())
}

/// Derive a per-chunk nonce from the chunk's content hash.
///
/// The nonce need not be secret; it only needs to be deterministic per content
/// (so identical chunks dedup) and distinct across distinct content (so the
/// per-chunk key is never paired with a colliding nonce on different
/// plaintext). The first holds because the content hash is the BLAKE3 digest
/// of the **unframed** plaintext. The second needs one more thing, because
/// the AEAD plaintext is the **framed** body, not the hashed bytes: the
/// framing must be a single fixed function of the chunk, so that one content
/// hash — hence one (key, nonce) — ever meets one AEAD plaintext. That is
/// enforced structurally, not by convention: [`encrypt_chunk`] is
/// `pub(crate)`, and its only external door, [`crate::chunk_seal`], frames
/// every chunk with the one file-sync framing after verifying the caller's
/// hash against the plaintext. Two writers framing one chunk differently
/// would put two plaintexts under one nonce — the finding that made this
/// sentence precise (the Go WebDAV MDA sealed raw while the sync engine
/// sealed framed, 2026-09-03; `chunk_seal`'s witness test recovers the
/// plaintext from such a pair by XOR alone).
///
/// Crucially, `encrypt_chunk` returns the ciphertext WITHOUT a
/// nonce prefix — the reader re-derives it from the manifest's chunk hash —
/// so the plaintext-derived value never rests in the sealed bytes — nor one
/// field over: a sealed-chunk manifest carries its `chunk_hashes` only sealed
/// under the same root (`ChunkManifest::seal_hashes`,
/// `docs/goal/architecture/mls-group-key-material.md` § M2 *Sealed manifest
/// hashes*), so the destination holds no candidate-plaintext confirmation
/// oracle. `blob_crypto` differs on exactly that axis: its blob
/// STORES its nonce as a cleartext prefix, so an unkeyed plaintext hash there
/// was a keyless confirmation oracle, and its derivation is keyed
/// (`blake3::keyed_hash(nonce_key, plaintext)[..12]`) since 2026-08-31.
fn derive_chunk_nonce(content_hash: &ContentHash) -> Nonce {
    crate::nonce_truncate::nonce_from_digest(&content_hash.digest())
}

/// Encrypt a single chunk, keyed by its content hash.
///
/// `pub(crate)`: `plaintext` must be the chunk's **framed** body and
/// `content_hash` the digest of its **unframed** plaintext, and nothing here
/// checks either — [`crate::chunk_seal`] is the door that does (module doc).
pub(crate) fn encrypt_chunk(
    root_secret: &[u8; 32],
    content_hash: &ContentHash,
    plaintext: &[u8],
) -> Result<Vec<u8>> {
    let key = derive_chunk_key(root_secret, content_hash);
    let cipher = ChaCha20Poly1305::new_from_slice(&key).expect("32-byte key is valid");
    let nonce = derive_chunk_nonce(content_hash);

    cipher
        .encrypt(&nonce, plaintext)
        .map_err(|e| anyhow::anyhow!("chunk encryption failed: {e}"))
}

/// Decrypt a single chunk. `content_hash` must be the hash of the original
/// plaintext (the manifest's chunk hash), since the key+nonce derive from it.
pub fn decrypt_chunk(
    root_secret: &[u8; 32],
    content_hash: &ContentHash,
    ciphertext: &[u8],
) -> Result<Vec<u8>> {
    decrypt_chunk_with_key(
        &derive_chunk_key(root_secret, content_hash),
        content_hash,
        ciphertext,
    )
}

/// The per-chunk key [`encrypt_chunk`] seals `content_hash`'s chunk under,
/// exposed so a holder of the root can hand ONE chunk's key to someone who
/// must not hold the root — the fragment-keyed share link's key envelope
/// (`docs/goal/behavior/share-links.md` § The private-file extension). The
/// derivation is one-way: a chunk key reveals nothing about the root, about
/// any other chunk's key, or about a sibling link's envelope.
///
/// A reader door, not a writer one: sealing stays behind
/// [`crate::chunk_seal`]. A chunk key in a recipient's hands lets them forge a
/// ciphertext for that one chunk, which they could only ever hand to
/// themselves — they hold no write path into the owner's folder, so the
/// one-framing-per-hash invariant the seal door guards is untouched.
pub fn chunk_key_for(root_secret: &[u8; 32], content_hash: &ContentHash) -> [u8; 32] {
    derive_chunk_key(root_secret, content_hash)
}

/// [`decrypt_chunk`]'s twin for a caller holding the per-chunk key
/// ([`chunk_key_for`]) rather than the root. `content_hash` is still the
/// chunk's plaintext hash — the nonce derives from it. Returns the **framed**
/// body; [`open_chunk_with_key`] is the whole read (decrypt → unframe →
/// verify).
pub fn decrypt_chunk_with_key(
    chunk_key: &[u8; 32],
    content_hash: &ContentHash,
    ciphertext: &[u8],
) -> Result<Vec<u8>> {
    let cipher = ChaCha20Poly1305::new_from_slice(chunk_key).expect("32-byte key is valid");
    let nonce = derive_chunk_nonce(content_hash);

    cipher
        .decrypt(&nonce, ciphertext)
        .map_err(|e| anyhow::anyhow!("chunk decryption failed: {e}"))
}

/// Open one sealed file-sync chunk with its per-chunk key: decrypt, strip the
/// frame, and verify the plaintext against `content_hash` — the one read a
/// key-envelope holder (the share-link viewer's wasm, every app's author
/// crate) runs per chunk, so no caller reassembles unverified bytes.
///
/// # Errors
/// A wrong key or tampered ciphertext (AEAD tag), a body with no frame, or
/// plaintext that does not hash to `content_hash`.
pub fn open_chunk_with_key(
    chunk_key: &[u8; 32],
    content_hash: &ContentHash,
    ciphertext: &[u8],
) -> Result<Vec<u8>> {
    let framed = decrypt_chunk_with_key(chunk_key, content_hash, ciphertext)?;
    crate::compress::unframe_verified_chunk(framed, content_hash)
        .ok_or_else(|| anyhow::anyhow!("chunk does not verify against its plaintext hash"))
}

// NOTE: there is deliberately no batch `encrypt_chunks` any more — the public
// batch seal is [`crate::chunk_seal::seal_chunk_bodies`], and a crate-private
// batch AEAD would exist only to be tested.

/// Decrypt chunks in order. `hashes[i]` must be the content hash of the
/// plaintext underlying `encrypted_chunks[i]` (the manifest's chunk hash), since
/// the per-chunk key+nonce derive from it.
pub fn decrypt_chunks(
    root_secret: &[u8; 32],
    hashes: &[ContentHash],
    encrypted_chunks: &[Vec<u8>],
) -> Result<Vec<Vec<u8>>> {
    if hashes.len() != encrypted_chunks.len() {
        anyhow::bail!(
            "decrypt_chunks: {} hashes but {} ciphertexts",
            hashes.len(),
            encrypted_chunks.len()
        );
    }
    hashes
        .iter()
        .zip(encrypted_chunks)
        .map(|(hash, data)| decrypt_chunk(root_secret, hash, data))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn hash_of(data: &[u8]) -> ContentHash {
        ContentHash::of_raw(data)
    }

    #[test]
    fn roundtrip_single_chunk() {
        let secret = [42u8; 32];
        let plaintext = b"hello world";
        let h = hash_of(plaintext);

        let encrypted = encrypt_chunk(&secret, &h, plaintext).unwrap();
        assert_ne!(&encrypted[..], &plaintext[..]);
        assert_eq!(encrypted.len(), plaintext.len() + ENCRYPTION_OVERHEAD);

        let decrypted = decrypt_chunk(&secret, &h, &encrypted).unwrap();
        assert_eq!(&decrypted[..], &plaintext[..]);
    }

    #[test]
    fn identical_content_dedups_to_identical_ciphertext() {
        // The convergent-encryption property: same plaintext under the same
        // root secret → byte-identical ciphertext, so the content-addressed
        // blob store stores it once. (This inverts the old positional scheme,
        // where the same bytes at different positions diverged.)
        let secret = [1u8; 32];
        let plaintext = b"the same chunk bytes";
        let h = hash_of(plaintext);

        let a = encrypt_chunk(&secret, &h, plaintext).unwrap();
        let b = encrypt_chunk(&secret, &h, plaintext).unwrap();
        assert_eq!(a, b, "identical content must produce identical ciphertext");

        // And across "file positions" via the batch helper: the same chunk at
        // index 0 and index 2 dedups, while a distinct chunk at index 1 does not.
        let same = b"the same chunk bytes".to_vec();
        let other = b"a different chunk".to_vec();
        let chunks = [
            (hash_of(&same), same.clone()),
            (hash_of(&other), other.clone()),
            (hash_of(&same), same.clone()),
        ];
        let enc: Vec<Vec<u8>> = chunks
            .iter()
            .map(|(h, d)| encrypt_chunk(&secret, h, d).unwrap())
            .collect();
        assert_eq!(
            enc[0], enc[2],
            "same content at positions 0 and 2 must dedup"
        );
        assert_ne!(enc[0], enc[1], "distinct content must not collide");
    }

    #[test]
    fn distinct_content_yields_distinct_key_and_nonce() {
        // No keystream reuse across distinct chunks at one epoch: distinct
        // plaintext → distinct content hash → distinct key AND distinct nonce.
        let secret = [9u8; 32];
        let a = b"alpha content".to_vec();
        let b = b"bravo content".to_vec();
        let ha = hash_of(&a);
        let hb = hash_of(&b);

        assert_ne!(
            derive_chunk_nonce(&ha),
            derive_chunk_nonce(&hb),
            "distinct content must yield distinct nonces"
        );
        assert_ne!(
            derive_chunk_key(&secret, &ha),
            derive_chunk_key(&secret, &hb),
            "distinct content must yield distinct keys"
        );
        // Same content → same nonce (the dedup precondition).
        assert_eq!(derive_chunk_nonce(&ha), derive_chunk_nonce(&hash_of(&a)));
    }

    #[test]
    fn wrong_root_secret_fails() {
        let secret_a = [1u8; 32];
        let secret_b = [2u8; 32];
        let plaintext = b"secret data";
        let h = hash_of(plaintext);

        let encrypted = encrypt_chunk(&secret_a, &h, plaintext).unwrap();
        let result = decrypt_chunk(&secret_b, &h, &encrypted);
        assert!(result.is_err());
    }

    #[test]
    fn wrong_content_hash_fails() {
        // Decrypting with the wrong content hash derives the wrong key → the
        // Poly1305 tag check fails. The hash thus binds ciphertext to content.
        let secret = [1u8; 32];
        let plaintext = b"data";
        let h = hash_of(plaintext);
        let wrong = hash_of(b"other");

        let encrypted = encrypt_chunk(&secret, &h, plaintext).unwrap();
        let result = decrypt_chunk(&secret, &wrong, &encrypted);
        assert!(result.is_err());
    }

    #[test]
    fn roundtrip_multiple_chunks() {
        let secret = [99u8; 32];
        let c0 = b"chunk zero".to_vec();
        let c1 = b"chunk one".to_vec();
        let c2 = b"chunk two".to_vec();
        let chunks = [
            (hash_of(&c0), c0.clone()),
            (hash_of(&c1), c1.clone()),
            (hash_of(&c2), c2.clone()),
        ];

        let encrypted: Vec<Vec<u8>> = chunks
            .iter()
            .map(|(h, d)| encrypt_chunk(&secret, h, d).unwrap())
            .collect();
        assert_eq!(encrypted.len(), 3);

        let hashes: Vec<ContentHash> = chunks.iter().map(|(h, _)| *h).collect();
        let decrypted = decrypt_chunks(&secret, &hashes, &encrypted).unwrap();
        assert_eq!(decrypted[0], c0);
        assert_eq!(decrypted[1], c1);
        assert_eq!(decrypted[2], c2);
    }

    #[test]
    fn decrypt_chunks_rejects_length_mismatch() {
        let secret = [3u8; 32];
        let data = b"x".to_vec();
        let h = hash_of(&data);
        let enc = encrypt_chunk(&secret, &h, &data).unwrap();
        // One ciphertext, zero hashes → error, not a silent zip-truncation.
        assert!(decrypt_chunks(&secret, &[], &[enc]).is_err());
    }

    /// The share-link key envelope's premise: the key [`chunk_key_for`]
    /// exposes opens exactly what the one seal door sealed under the root —
    /// through the whole read (decrypt → unframe → verify), not just the AEAD.
    #[test]
    fn a_derived_chunk_key_opens_what_chunk_seal_sealed() {
        let root = [0x5Au8; 32];
        for plain in [b"one small chunk".to_vec(), vec![b'q'; 9000]] {
            let hash = hash_of(&plain);
            let (_store_key, body) =
                crate::chunk_seal::seal_chunk_body(&hash, &plain, &root).unwrap();
            let key = chunk_key_for(&root, &hash);
            assert_eq!(open_chunk_with_key(&key, &hash, &body).unwrap(), plain);
            // The framed twin agrees with the root-keyed reader byte for byte.
            assert_eq!(
                decrypt_chunk_with_key(&key, &hash, &body).unwrap(),
                decrypt_chunk(&root, &hash, &body).unwrap()
            );
        }
    }

    /// A chunk key is per chunk: it opens nothing else, and a wrong key or a
    /// hash the plaintext does not match fails closed.
    #[test]
    fn a_chunk_key_opens_only_its_own_chunk() {
        let root = [0x5Au8; 32];
        let a = b"chunk a".to_vec();
        let b = b"chunk b".to_vec();
        let (ha, hb) = (hash_of(&a), hash_of(&b));
        let (_, body_b) = crate::chunk_seal::seal_chunk_body(&hb, &b, &root).unwrap();
        let key_a = chunk_key_for(&root, &ha);
        assert_ne!(key_a, chunk_key_for(&root, &hb));
        assert!(open_chunk_with_key(&key_a, &hb, &body_b).is_err());
        assert!(open_chunk_with_key(&chunk_key_for(&[1u8; 32], &hb), &hb, &body_b).is_err());
        // The right key under a hash the bytes do not address: the nonce moves,
        // so the tag fails before verification would.
        assert!(open_chunk_with_key(&chunk_key_for(&root, &hb), &ha, &body_b).is_err());
    }
}
