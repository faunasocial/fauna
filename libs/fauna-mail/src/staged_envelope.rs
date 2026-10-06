//! The staged envelope — one-shot confidentiality for **plaintext** mail bytes
//! crossing the bulk-byte plane.
//!
//! Owner doc: `docs/goal/behavior/smtp-server.md` § Message size limits, *the
//! staged-envelope rule* (ratified 2026-07-18).
//!
//! Two mail legs cannot stage already-sealed bytes: client import
//! (`ImportMessageItem.body` is raw RFC 5322 — the nest seals at ingest) and
//! the outbound queue pair (`raw_message` is plaintext the nest and the MTA
//! must read). The byte plane's open download route is safe *only because
//! everything in the chunk store is ciphertext*, so these legs never stage raw
//! plaintext. Instead the producer seals the whole payload under a **one-shot
//! random 32-byte key** here, splits the resulting *ciphertext* with the same
//! [`crate::body_ref`] chunk rule, and the RPC carries the reference **and the
//! key** — inside the already-confidential authenticated WS-RPC, the very
//! channel that otherwise carries the plaintext itself inline. The chunk store
//! never sees plaintext or a stable content hash (fresh key ⇒ unique
//! ciphertext ⇒ no cross-staging correlation), and the consumer's open
//! authenticates the entire rejoin.
//!
//! Shape: ChaCha20Poly1305 (the in-tree symmetric convention), a random
//! 12-byte nonce **prepended** to the ciphertext, and a fixed AAD domain
//! string. The key is deliberately a plain byte vector, not a zeroizing
//! newtype: it is one-shot, minted per staging, and carried on the wire by
//! design — the wire struct (`fauna_protocol::bridge_routing::StagedBodyRef`,
//! whose `key` field is a zeroizing `SecretByteBuf`) is where its lifetime lives.
//! Debug-*redaction*, however, is a separate and cheaper property that still
//! matters on this intermediate: [`StagedSeal`] carries a hand-written redacted
//! `Debug` so a `tracing::debug!(?seal)` in any consumer never prints the key
//! beside the reference's chunk hashes.
//!
//! One implementation for every leg and every language (priority #2): Rust
//! producers/consumers call [`seal_staged_body`] / [`open_staged_body`] (or the
//! chunk-composed pair below), and the Go bridge reaches the same functions
//! over UniFFI — a second derivation of the nonce layout or the AAD string
//! would corrupt mail silently.

use chacha20poly1305::aead::rand_core::RngCore;
use chacha20poly1305::aead::{Aead, OsRng, Payload};
use chacha20poly1305::{ChaCha20Poly1305, KeyInit, Nonce};

use crate::body_ref::{MailBodyChunk, join_sealed_mail_body_checked, split_sealed_mail_body};

/// Length of the one-shot staging key carried beside the reference.
pub const STAGING_KEY_BYTES: usize = 32;

/// Random nonce prepended to the sealed bytes. A fresh random key per staging
/// already rules out nonce reuse; the random nonce is defense in depth and
/// keeps the layout self-describing.
const NONCE_BYTES: usize = 12;

/// Poly1305 tag appended by the seal.
const TAG_BYTES: usize = 16;

/// Bytes the staged envelope adds to a plaintext body: the nonce prefix plus
/// the AEAD tag. A consumer converts its plaintext ceiling into the sealed
/// ceiling a [`crate::body_ref::check_mail_body_ref_shape`] pre-check takes by
/// adding this.
pub const STAGED_ENVELOPE_OVERHEAD_BYTES: u64 = (NONCE_BYTES + TAG_BYTES) as u64;

/// Domain separation: a staged-envelope ciphertext can never be confused with
/// (or replayed as) any other ChaCha20Poly1305 use in the tree.
const STAGED_BODY_AAD: &[u8] = b"fauna-mail-staged-body-v1";

/// Why a staged envelope could not be opened. All variants are terminal for
/// the carrying request — a reference that cannot be opened must fail the
/// call, never degrade to empty bytes.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum StagedEnvelopeError {
    #[error("staging key must be {STAGING_KEY_BYTES} bytes, got {got}")]
    BadKeyLength { got: usize },
    #[error("staged envelope shorter than its {NONCE_BYTES}-byte nonce prefix ({got} bytes)")]
    TooShort { got: usize },
    #[error(
        "staged envelope failed to open (wrong key, tampered bytes, or a mis-joined chunk list)"
    )]
    OpenFailed,
    #[error(transparent)]
    Join(#[from] crate::body_ref::MailBodyRefError),
}

/// A freshly sealed staging payload: the one-shot key (to ride the RPC beside
/// the reference) and the sealed bytes (nonce-prefixed ciphertext, ready for
/// [`split_sealed_mail_body`] → upload).
#[cfg_attr(feature = "uniffi", derive(uniffi::Record))]
#[derive(Clone)]
pub struct StagedSeal {
    /// The one-shot random key, [`STAGING_KEY_BYTES`] long.
    pub key: Vec<u8>,
    /// Nonce-prefixed ciphertext — what actually gets chunked and staged.
    pub sealed: Vec<u8>,
}

/// Redacted — the one-shot key must never reach a log line. (`uniffi::Record`
/// does not require `Debug`, so dropping the derive is free; the field stays a
/// plain `Vec<u8>` — the wire newtype owns the zeroizing lifetime, per the
/// module docs.)
impl std::fmt::Debug for StagedSeal {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("StagedSeal")
            .field("key", &format_args!("<redacted {} bytes>", self.key.len()))
            .field("sealed", &format_args!("{} bytes", self.sealed.len()))
            .finish()
    }
}

/// Seal `plain` under a fresh one-shot key.
///
/// Infallible by construction: key generation is OS-random and
/// ChaCha20Poly1305 encryption of an in-memory slice cannot fail.
pub fn seal_staged_body(plain: &[u8]) -> StagedSeal {
    let mut key = vec![0u8; STAGING_KEY_BYTES];
    OsRng.fill_bytes(&mut key);
    let mut nonce = [0u8; NONCE_BYTES];
    OsRng.fill_bytes(&mut nonce);

    let cipher = ChaCha20Poly1305::new_from_slice(&key).expect("32-byte key is valid");
    let ct = cipher
        .encrypt(
            Nonce::from_slice(&nonce),
            Payload {
                msg: plain,
                aad: STAGED_BODY_AAD,
            },
        )
        .expect("in-memory ChaCha20Poly1305 seal cannot fail");

    let mut sealed = Vec::with_capacity(NONCE_BYTES + ct.len());
    sealed.extend_from_slice(&nonce);
    sealed.extend_from_slice(&ct);
    StagedSeal { key, sealed }
}

/// Open a rejoined staged envelope with the key that rode the RPC.
///
/// Fails closed: a wrong key, a tampered byte, a dropped/reordered chunk that
/// survived the length check — anything that changes the ciphertext — fails
/// the AEAD tag. The caller then fails its request; it never proceeds with
/// partial bytes.
pub fn open_staged_body(sealed: &[u8], key: &[u8]) -> Result<Vec<u8>, StagedEnvelopeError> {
    if key.len() != STAGING_KEY_BYTES {
        return Err(StagedEnvelopeError::BadKeyLength { got: key.len() });
    }
    if sealed.len() < NONCE_BYTES {
        return Err(StagedEnvelopeError::TooShort { got: sealed.len() });
    }
    let (nonce, ct) = sealed.split_at(NONCE_BYTES);
    let cipher = ChaCha20Poly1305::new_from_slice(key).expect("length checked above");
    cipher
        .decrypt(
            Nonce::from_slice(nonce),
            Payload {
                msg: ct,
                aad: STAGED_BODY_AAD,
            },
        )
        .map_err(|_| StagedEnvelopeError::OpenFailed)
}

/// Producer composition: seal `plain`, split the sealed bytes into
/// content-addressed chunks (the caller uploads each), and return the key +
/// chunks + declared total the reference carries. One call per leg, so no
/// producer can get the seal-then-split order wrong.
pub fn seal_and_split_staged_body(plain: &[u8]) -> (Vec<u8>, Vec<MailBodyChunk>, u64) {
    let StagedSeal { key, sealed } = seal_staged_body(plain);
    let total = sealed.len() as u64;
    let chunks = split_sealed_mail_body(&sealed);
    (key, chunks, total)
}

/// Consumer composition: rejoin fetched chunks (fail-closed on the declared
/// total — the same pin every sealed-bytes leg applies) and open the result.
pub fn join_and_open_staged_body(
    chunks: &[Vec<u8>],
    total_bytes: u64,
    key: &[u8],
) -> Result<Vec<u8>, StagedEnvelopeError> {
    let sealed = join_sealed_mail_body_checked(chunks, total_bytes)?;
    open_staged_body(&sealed, key)
}

#[cfg(feature = "uniffi")]
mod ffi {
    use super::*;

    /// UniFFI: seal a plaintext body for staging (the Go MTA's outbound
    /// enqueue leg; the split into upload chunks rides the existing
    /// `split_sealed_mail_body` export).
    #[uniffi::export]
    pub fn seal_staged_body(plain: Vec<u8>) -> StagedSeal {
        super::seal_staged_body(&plain)
    }

    /// UniFFI-visible open failure. Deliberately one flat variant: every
    /// cause (wrong key, tamper, truncation, bad key length) is equally
    /// terminal for the carrying unit — the Go side logs the detail and
    /// fails the unit, it never branches on the cause.
    #[derive(Debug, thiserror::Error, uniffi::Error)]
    pub enum StagedEnvelopeFfiError {
        #[error("staged envelope open failed: {detail}")]
        Open { detail: String },
    }

    /// UniFFI: open a rejoined staged envelope (the Go MTA's outbound-due
    /// consumer leg).
    #[uniffi::export]
    pub fn open_staged_body(
        sealed: Vec<u8>,
        key: Vec<u8>,
    ) -> Result<Vec<u8>, StagedEnvelopeFfiError> {
        super::open_staged_body(&sealed, &key).map_err(|e| StagedEnvelopeFfiError::Open {
            detail: e.to_string(),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_staged_body_round_trips() {
        let plain = b"From: a@b\r\n\r\nhello world".to_vec();
        let StagedSeal { key, sealed } = seal_staged_body(&plain);
        assert_eq!(key.len(), STAGING_KEY_BYTES);
        assert_ne!(sealed, plain, "sealed bytes are ciphertext");
        assert_eq!(open_staged_body(&sealed, &key).unwrap(), plain);
    }

    #[test]
    fn the_envelope_overhead_is_exactly_what_the_seal_adds() {
        // A consumer's sealed ceiling is its plaintext ceiling plus this; if the
        // seal ever grew, an honest body at the ceiling would be refused.
        for n in [0usize, 1, 4096] {
            let StagedSeal { sealed, .. } = seal_staged_body(&vec![7u8; n]);
            assert_eq!(
                sealed.len() as u64,
                n as u64 + STAGED_ENVELOPE_OVERHEAD_BYTES
            );
        }
    }

    #[test]
    fn debug_redacts_the_one_shot_key() {
        // The one-shot key must never reach a log line
        // via `tracing::debug!(?seal)`. Only its length may appear.
        let seal = seal_staged_body(b"secret plaintext");
        let rendered = format!("{seal:?}");
        assert!(
            rendered.contains("key: <redacted 32 bytes>"),
            "Debug leaked the one-shot key: {rendered}"
        );
        // The ciphertext body is summarized by length too, never dumped.
        assert!(!rendered.contains(&format!("{:?}", seal.sealed)));
    }

    #[test]
    fn an_empty_body_round_trips() {
        let StagedSeal { key, sealed } = seal_staged_body(b"");
        assert_eq!(open_staged_body(&sealed, &key).unwrap(), b"".to_vec());
    }

    #[test]
    fn two_seals_of_identical_bytes_differ() {
        // The whole point: identical plaintext staged twice must not collide
        // in the content-addressed store (no cross-staging correlation).
        let plain = b"identical source mail".to_vec();
        let a = seal_staged_body(&plain);
        let b = seal_staged_body(&plain);
        assert_ne!(a.key, b.key);
        assert_ne!(a.sealed, b.sealed);
    }

    #[test]
    fn a_wrong_key_fails_closed() {
        let StagedSeal { key, sealed } = seal_staged_body(b"secret");
        let mut wrong = key.clone();
        wrong[0] ^= 1;
        assert_eq!(
            open_staged_body(&sealed, &wrong),
            Err(StagedEnvelopeError::OpenFailed)
        );
    }

    #[test]
    fn a_tampered_byte_fails_closed() {
        let StagedSeal { key, mut sealed } = seal_staged_body(b"secret");
        let last = sealed.len() - 1;
        sealed[last] ^= 1;
        assert_eq!(
            open_staged_body(&sealed, &key),
            Err(StagedEnvelopeError::OpenFailed)
        );
    }

    #[test]
    fn a_bad_key_length_is_a_typed_error() {
        let StagedSeal { sealed, .. } = seal_staged_body(b"secret");
        assert_eq!(
            open_staged_body(&sealed, &[0u8; 16]),
            Err(StagedEnvelopeError::BadKeyLength { got: 16 })
        );
    }

    #[test]
    fn a_truncated_envelope_is_a_typed_error() {
        assert_eq!(
            open_staged_body(&[0u8; 4], &[0u8; STAGING_KEY_BYTES]),
            Err(StagedEnvelopeError::TooShort { got: 4 })
        );
    }

    #[test]
    fn the_chunk_composition_round_trips_a_multi_chunk_body() {
        // Over one chunk boundary so the split/join legs genuinely engage.
        let plain: Vec<u8> = (0..5 * 1024 * 1024).map(|i| (i % 251) as u8).collect();
        let (key, chunks, total) = seal_and_split_staged_body(&plain);
        assert!(chunks.len() > 1, "must span chunks to exercise the join");
        let fetched: Vec<Vec<u8>> = chunks.into_iter().map(|c| c.bytes).collect();
        assert_eq!(
            join_and_open_staged_body(&fetched, total, &key).unwrap(),
            plain
        );
    }

    #[test]
    fn a_lying_total_fails_before_the_open() {
        let plain = b"body".to_vec();
        let (key, chunks, total) = seal_and_split_staged_body(&plain);
        let fetched: Vec<Vec<u8>> = chunks.into_iter().map(|c| c.bytes).collect();
        assert!(matches!(
            join_and_open_staged_body(&fetched, total + 1, &key),
            Err(StagedEnvelopeError::Join(_))
        ));
    }

    #[test]
    fn a_dropped_chunk_fails_closed() {
        let plain: Vec<u8> = (0..5 * 1024 * 1024).map(|i| (i % 241) as u8).collect();
        let (key, chunks, total) = seal_and_split_staged_body(&plain);
        let mut fetched: Vec<Vec<u8>> = chunks.into_iter().map(|c| c.bytes).collect();
        fetched.remove(0);
        // Either the total pin or the AEAD tag catches it — never a success.
        assert!(join_and_open_staged_body(&fetched, total, &key).is_err());
    }
}
