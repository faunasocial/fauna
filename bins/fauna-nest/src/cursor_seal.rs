//! Sealing the nest's **opaque pagination cursors** under a nest-held key.
//!
//! A keyset cursor is a *position* in a server-side order, minted by this nest
//! and consumed by this nest; a client only ever round-trips it as an opaque
//! string. That makes its contents pure nest-internal state — and the moment any
//! component of that state is something the reply itself withholds from the
//! caller, handing the cursor over in the clear gives it back.
//!
//! That is finding **leg (a)** (path-sealing S5e). `fauna.media.list`'s
//! v2 cursor carries the page-boundary item's `path_hash`, which S5d withholds
//! from a non-audience (Q5 `AdminDiscovery`) reader on the `MediaItem` itself
//! (`encryption-at-rest.md` § Carve-outs — `path_hash` "is projected on the wire
//! only to a label's audience"). The cursor is computed for sort/skip
//! correctness *before* any audience gate and must stay the true hash, so the
//! fix is not to weaken the key: it is to stop disclosing the cursor's innards
//! to whoever holds it. Sealed, the cursor keeps its exact ordering semantics
//! and discloses nothing to anybody.
//!
//! ## The key, and why no new key material is minted
//!
//! Two prior slices recorded that closing this needed "a persistent nest secret
//! `bins/fauna-nest` does not have" (path-sealing S5c-1's cursor re-key note,
//! and the residual S5d declared). **That premise was wrong.** The nest holds a
//! durable **deployment signing key** — the `nest_keypair` DB row reconciled at
//! every boot from the durable `nest_deployment.key`, which `NestIdentity` is a
//! view over (`crate::deployment_key`, `crate::nest_identity`). It already
//! survives restarts, re-claims and backup/restore, so keying the cursor with a
//! domain-separated subkey of it adds **no** key material, no new KAT
//! obligation and no new backup/restore obligation.
//!
//! The subkey is `BLAKE3::derive_key` over the deployment seed under this
//! module's own context string, so it is cryptographically independent of the
//! Ed25519 signing use of the same seed (and of every other derivation).
//!
//! ## Properties, and the ones deliberately *not* claimed
//!
//! - **Confidential + authenticated.** ChaCha20-Poly1305 with a random nonce
//!   per mint. A cursor is never compared for equality, so a random nonce is
//!   correct here (the `path_crypto` convergent/random rule: derive a nonce only
//!   where the salt determines the plaintext).
//! - **Fails closed, loudly.** A cursor this nest did not seal — a foreign
//!   nest's, one from before the deployment key rotated, a truncated or tampered
//!   string, or a pre-S5e plaintext cursor — does not open, and the caller gets a
//!   typed `invalid_cursor` telling them to restart the listing. Never a silent
//!   mis-page.
//! - **Not a capability.** The seal hides the cursor's *contents*; it is not an
//!   authorization token. Every request carrying a cursor is authorized on its
//!   own actor exactly as before, and a cursor replayed by a *different* caller
//!   is still answered under that caller's own grants. Do not start trusting a
//!   cursor's contents as proof of anything.
//! - **Not stable across a deployment-key rotation.** In-flight cursors refuse
//!   after a re-claim. Acceptable: a cursor is ephemeral within one listing
//!   session, and the refusal is the loud kind.

use anyhow::{Result, bail};
use chacha20poly1305::{
    ChaCha20Poly1305, Nonce,
    aead::{Aead, AeadCore, KeyInit, OsRng},
};

/// The BLAKE3 `derive_key` context — domain-separates the cursor-sealing subkey
/// from the deployment seed's signing use and from every other derivation in the
/// tree. Bump the version suffix only alongside a deliberate cursor-format
/// change; every in-flight cursor refuses at the bump.
const CURSOR_KEY_CONTEXT: &str = "fauna.nest.cursor.v1";

/// ChaCha20-Poly1305 nonce length.
const NONCE_LEN: usize = 12;

/// The nest's cursor-sealing key, derived from its durable deployment seed.
///
/// Callers pass `AppState.nest_identity.signing_key.to_bytes()` — the reconciled
/// deployment seed (`NestIdentity::from_seed`). Stable across restarts by
/// construction, which is what lets a cursor survive one.
pub fn derive_cursor_key(deployment_seed: &[u8; 32]) -> [u8; 32] {
    blake3::derive_key(CURSOR_KEY_CONTEXT, deployment_seed)
}

/// Seal an already-encoded cursor body. Output is `nonce ‖ ciphertext`.
pub fn seal(key: &[u8; 32], plaintext: &[u8]) -> Result<Vec<u8>> {
    let cipher = ChaCha20Poly1305::new_from_slice(key).expect("32-byte key is valid");
    let nonce = ChaCha20Poly1305::generate_nonce(&mut OsRng);
    let ct = cipher
        .encrypt(&nonce, plaintext)
        .map_err(|e| anyhow::anyhow!("cursor sealing failed: {e}"))?;
    let mut out = Vec::with_capacity(NONCE_LEN + ct.len());
    out.extend_from_slice(nonce.as_slice());
    out.extend_from_slice(&ct);
    Ok(out)
}

/// Open a sealed cursor body. Fails closed on anything this key did not seal.
pub fn open(key: &[u8; 32], sealed: &[u8]) -> Result<Vec<u8>> {
    if sealed.len() <= NONCE_LEN {
        bail!("cursor is shorter than the nonce it must carry");
    }
    let (nonce, ct) = sealed.split_at(NONCE_LEN);
    let cipher = ChaCha20Poly1305::new_from_slice(key).expect("32-byte key is valid");
    cipher
        .decrypt(Nonce::from_slice(nonce), ct)
        .map_err(|_| anyhow::anyhow!("cursor was not sealed by this nest (or is corrupt)"))
}

#[cfg(test)]
mod tests {
    use super::*;

    const SEED: [u8; 32] = [7u8; 32];

    #[test]
    fn a_sealed_cursor_round_trips_under_the_same_key() {
        let key = derive_cursor_key(&SEED);
        let body = b"the boundary position".to_vec();
        let sealed = seal(&key, &body).unwrap();
        assert_eq!(open(&key, &sealed).unwrap(), body);
    }

    /// The whole point: the sealed form must not contain the plaintext it
    /// protects. The production plaintext is a `path_hash` the reply withheld.
    #[test]
    fn the_sealed_form_does_not_contain_its_plaintext() {
        let key = derive_cursor_key(&SEED);
        let secret = blake3::hash(b"eviction_notice.pdf").as_bytes().to_vec();
        let sealed = seal(&key, &secret).unwrap();
        assert!(
            !sealed.windows(secret.len()).any(|w| w == secret.as_slice()),
            "the digest must not survive into the sealed bytes"
        );
    }

    /// Two mints of the same body differ — the random nonce — so a cursor is not
    /// itself an equality oracle over the position it encodes.
    #[test]
    fn two_mints_of_one_body_differ() {
        let key = derive_cursor_key(&SEED);
        let a = seal(&key, b"same").unwrap();
        let b = seal(&key, b"same").unwrap();
        assert_ne!(a, b);
    }

    #[test]
    fn another_nests_key_does_not_open_it() {
        let sealed = seal(&derive_cursor_key(&SEED), b"position").unwrap();
        assert!(open(&derive_cursor_key(&[8u8; 32]), &sealed).is_err());
    }

    #[test]
    fn a_tampered_or_truncated_cursor_fails_closed() {
        let key = derive_cursor_key(&SEED);
        let sealed = seal(&key, b"position").unwrap();
        let mut flipped = sealed.clone();
        *flipped.last_mut().unwrap() ^= 0x01;
        assert!(open(&key, &flipped).is_err(), "tag must catch a bit flip");
        assert!(open(&key, &sealed[..NONCE_LEN]).is_err(), "nonce-only");
        assert!(open(&key, b"").is_err(), "empty");
    }

    /// The subkey is not the seed — a fumbled derivation that returned the seed
    /// itself would hand the deployment signing key to anything that logs a key.
    #[test]
    fn the_subkey_is_domain_separated_from_the_seed() {
        assert_ne!(derive_cursor_key(&SEED), SEED);
        assert_ne!(derive_cursor_key(&SEED), derive_cursor_key(&[8u8; 32]));
    }
}
