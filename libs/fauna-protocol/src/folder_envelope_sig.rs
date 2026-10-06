//! **The owner's signature over a shared set's content-key envelope** —
//! `docs/goal/architecture/writer-signed-change-records.md` ruling (11)(b).
//!
//! The envelope's payload is an AEAD under the group epoch's export secret,
//! which every member at that epoch holds — so a member, with a nest's help,
//! could seal a payload naming a set nonce of its choosing. The blob the nest
//! stores is therefore **signature-plus-ciphertext**: the owner's identity key
//! signs the sealed bytes under [`FOLDER_ENVELOPE_OWNER_SIG_V1`], bound to the
//! set's channel, and a member ingests no envelope whose signature does not
//! verify. Who may sign *what* — the current owner moves a member's nonce, a
//! proven predecessor of the owner may only confirm what the member holds — is
//! the ingest's rule (`fauna_conversations`' custody ingest); this module only
//! frames, signs and verifies.
//!
//! The nest keeps the blob opaque, as it kept the bare ciphertext
//! (`content_key_put_core` checks a non-empty hex under the channel claimant).
//! Every envelope is signed from birth — there is no unsigned shape to accept
//! (ruling (11)(k)).

use fauna_core::identity::{ActorId, ActorKeypair};
use serde::{Deserialize, Serialize};

use crate::sig_domain::{FOLDER_ENVELOPE_OWNER_SIG_V1, domain_separated};

/// Why a stored envelope blob was refused before its open.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum EnvelopeSigError {
    /// Not a signed envelope blob at all.
    #[error("content-key envelope blob is malformed: {0}")]
    Malformed(String),
    /// The signature does not verify under the named signer for this channel.
    #[error("content-key envelope signature does not verify")]
    SignatureInvalid,
}

/// The stored blob's shape — a canonical dag-cbor map, so it stays additive.
#[derive(Serialize, Deserialize)]
struct SignedEnvelopeWire {
    #[serde(with = "serde_bytes")]
    signer: [u8; 32],
    #[serde(with = "serde_bytes")]
    sig: Vec<u8>,
    #[serde(with = "serde_bytes")]
    sealed: Vec<u8>,
    /// Keys a newer build added (rule 4). Nothing here is signed: the
    /// signature covers the channel and `sealed` alone.
    #[serde(flatten, default)]
    extra: std::collections::BTreeMap<String, fauna_cbor::Value>,
}

/// A stored envelope whose signature verified — who signed it, and the sealed
/// bytes to open.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct VerifiedEnvelope {
    /// The identity whose key signed the blob.
    pub signer: ActorId,
    /// The sealed payload, as `MlsEngine::open_content_key_envelope_payload`
    /// takes it.
    pub sealed: Vec<u8>,
}

/// The message the owner signs and every member verifies: the tag, the set's
/// 32-byte channel id, then the sealed bytes to the end — the one builder for
/// both sides.
pub fn signed_message(channel_id: &[u8; 32], sealed: &[u8]) -> Vec<u8> {
    let mut body = Vec::with_capacity(32 + sealed.len());
    body.extend_from_slice(channel_id);
    body.extend_from_slice(sealed);
    domain_separated(FOLDER_ENVELOPE_OWNER_SIG_V1, &body)
}

/// Sign `sealed` for the set at `channel_id` as `owner`, returning the blob the
/// nest stores.
pub fn sign(owner: &ActorKeypair, channel_id: &[u8; 32], sealed: &[u8]) -> Vec<u8> {
    use ed25519_dalek::Signer as _;
    let sig = owner
        .signing_key()
        .sign(&signed_message(channel_id, sealed));
    fauna_core::encoding::canonical_encode(&SignedEnvelopeWire {
        signer: owner.actor_id().0,
        sig: sig.to_bytes().to_vec(),
        sealed: sealed.to_vec(),
        extra: std::collections::BTreeMap::new(),
    })
    .expect("a byte-only map always encodes")
}

/// Verify a stored blob for the set at `channel_id`: the signature must verify
/// under the identity the blob names. Says nothing about whether that identity
/// may move anything — the caller decides that against the owner it trusts.
pub fn verify(blob: &[u8], channel_id: &[u8; 32]) -> Result<VerifiedEnvelope, EnvelopeSigError> {
    let wire: SignedEnvelopeWire = fauna_core::encoding::canonical_decode(blob)
        .map_err(|e| EnvelopeSigError::Malformed(e.to_string()))?;
    let sig: [u8; 64] = wire
        .sig
        .as_slice()
        .try_into()
        .map_err(|_| EnvelopeSigError::Malformed("signature is not 64 bytes".into()))?;
    let key = ed25519_dalek::VerifyingKey::from_bytes(&wire.signer)
        .map_err(|_| EnvelopeSigError::SignatureInvalid)?;
    key.verify_strict(
        &signed_message(channel_id, &wire.sealed),
        &ed25519_dalek::Signature::from_bytes(&sig),
    )
    .map_err(|_| EnvelopeSigError::SignatureInvalid)?;
    Ok(VerifiedEnvelope {
        signer: ActorId(wire.signer),
        sealed: wire.sealed,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    const CHANNEL: [u8; 32] = [3; 32];

    #[test]
    fn a_signed_blob_verifies_and_names_its_signer() {
        let owner = ActorKeypair::from_secret([1; 32]);
        let blob = sign(&owner, &CHANNEL, b"sealed bytes");
        let v = verify(&blob, &CHANNEL).expect("verifies");
        assert_eq!(v.signer, owner.actor_id());
        assert_eq!(v.sealed, b"sealed bytes");
    }

    /// A blob moved to another set, a re-sealed ciphertext under the owner's
    /// signature, and a bare ciphertext are all refused.
    #[test]
    fn a_moved_altered_or_unsigned_blob_is_refused() {
        let owner = ActorKeypair::from_secret([1; 32]);
        let blob = sign(&owner, &CHANNEL, b"sealed bytes");
        assert_eq!(
            verify(&blob, &[4; 32]),
            Err(EnvelopeSigError::SignatureInvalid)
        );
        let mut wire: SignedEnvelopeWire = fauna_core::encoding::canonical_decode(&blob).unwrap();
        wire.sealed = b"other bytes".to_vec();
        let forged = fauna_core::encoding::canonical_encode(&wire).unwrap();
        assert_eq!(
            verify(&forged, &CHANNEL),
            Err(EnvelopeSigError::SignatureInvalid)
        );
        assert!(matches!(
            verify(b"sealed bytes", &CHANNEL),
            Err(EnvelopeSigError::Malformed(_))
        ));
    }

    /// A blob a newer build wrote with one more key still verifies: the
    /// signature covers the channel and the sealed bytes, not the map.
    #[test]
    fn a_blob_with_a_key_this_build_does_not_name_still_verifies() {
        let owner = ActorKeypair::from_secret([1; 32]);
        let blob = sign(&owner, &CHANNEL, b"sealed bytes");
        let mut wire: SignedEnvelopeWire = fauna_core::encoding::canonical_decode(&blob).unwrap();
        wire.extra
            .insert("later".into(), fauna_cbor::Value::Integer(7));
        let newer = fauna_core::encoding::canonical_encode(&wire).unwrap();
        assert_ne!(newer, blob);
        let v = verify(&newer, &CHANNEL).expect("verifies");
        assert_eq!(v.signer, owner.actor_id());
        assert_eq!(v.sealed, b"sealed bytes");
    }
}
