//! Submission tokens. The plaintext payload of a
//! `WrappedSubmissionTokenBlob` is a `SubmissionToken` signed by the
//! user's primary client (Ed25519) with the placeholder pattern from
//! the spec.

use crate::wrapped_blob::format::{UnwrapError, WrapError};
// verify-ok(caller-supplied key): the verifying key arrives as a parameter from
// a caller that already knows whose signature it expects — it never rides the
// wire beside the signature, so the weak-key class does not apply (and the AEAD
// gate authenticates the payload first). A self-describing key would owe
// `fauna_core::identity::verify_detached`.
use ed25519_dalek::{Signature, Signer, SigningKey, Verifier, VerifyingKey};
use serde::{Deserialize, Serialize};
use serde_bytes::ByteBuf;

/// Ed25519 signature length.
pub const SIGNATURE_LEN: usize = 64;

/// Submission-token plaintext. Carries enough authorization data
/// for the MTA to accept a per-message submission as the user.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SubmissionToken {
    #[serde(rename = "actor_id")]
    #[serde(with = "serde_bytes")]
    pub actor_id: Vec<u8>,
    pub credential_id: String,
    pub issued_at: u64,
    pub expires_at: u64,
    pub max_recipients: u32,
    pub max_messages_per_day: u32,
    /// Ed25519 signature by the user's primary client. Computed over
    /// the canonical DAG-CBOR of this struct with `user_sig` set to
    /// 64 zero bytes.
    pub user_sig: ByteBuf,
}

impl SubmissionToken {
    /// Sign with `signing_key`. The struct is encoded with `user_sig`
    /// set to zeros, signed, and the resulting signature replaces the
    /// placeholder.
    ///
    /// # Errors
    ///
    /// Returns `WrapError::CborEncode` if the canonical-CBOR encode
    /// step fails (practically unreachable for this fixed shape).
    pub fn sign(mut self, signing_key: &SigningKey) -> Result<Self, WrapError> {
        self.user_sig = ByteBuf::from(vec![0u8; SIGNATURE_LEN]);
        let bytes = self.to_canonical_bytes()?;
        let sig = signing_key.sign(&bytes);
        self.user_sig = ByteBuf::from(sig.to_bytes().to_vec());
        Ok(self)
    }

    /// Verify the embedded signature against `verifying_key`.
    ///
    /// # Errors
    ///
    /// - `UnwrapError::InvalidFormat` if `user_sig` length is not
    ///   `SIGNATURE_LEN` (64) or the placeholder re-encode fails.
    /// - `UnwrapError::SignatureFailed` if the Ed25519 verify fails.
    pub fn verify(&self, verifying_key: &VerifyingKey) -> Result<(), UnwrapError> {
        if self.user_sig.len() != SIGNATURE_LEN {
            return Err(UnwrapError::InvalidFormat(format!(
                "user_sig must be {SIGNATURE_LEN} bytes"
            )));
        }
        let mut sig_bytes = [0u8; SIGNATURE_LEN];
        sig_bytes.copy_from_slice(&self.user_sig);
        let sig = Signature::from_bytes(&sig_bytes);

        // Build the placeholder version for verification.
        let mut placeholder = self.clone();
        placeholder.user_sig = ByteBuf::from(vec![0u8; SIGNATURE_LEN]);
        let bytes = placeholder
            .to_canonical_bytes()
            .map_err(|e| UnwrapError::InvalidFormat(format!("cbor encode: {e}")))?;

        verifying_key
            .verify(&bytes, &sig)
            .map_err(|_| UnwrapError::SignatureFailed)
    }

    /// Encode to canonical DAG-CBOR bytes.
    ///
    /// # Errors
    ///
    /// Returns `WrapError::CborEncode` on encoding failure.
    pub fn to_canonical_bytes(&self) -> Result<Vec<u8>, WrapError> {
        fauna_cbor::encode_canonical(self).map_err(|e| WrapError::CborEncode(e.to_string()))
    }

    /// Decode from canonical CBOR.
    ///
    /// # Errors
    ///
    /// Returns `UnwrapError::InvalidFormat` for bad CBOR or wrong
    /// `actor_id` length.
    pub fn from_canonical_bytes(bytes: &[u8]) -> Result<Self, UnwrapError> {
        let tok: Self = fauna_cbor::decode_strict(bytes)
            .map_err(|e| UnwrapError::InvalidFormat(format!("cbor decode: {e}")))?;
        if tok.actor_id.len() != 32 {
            return Err(UnwrapError::InvalidFormat(
                "actor_id must be 32 bytes".into(),
            ));
        }
        Ok(tok)
    }
}

/// A fresh, signed test [`SubmissionToken`] for `actor_id`/`credential_id` —
/// the canonical fixture shape shared by every submission-token round-trip
/// test in this crate and in `fauna-ffi` (previously three near-identical
/// private copies).
#[cfg(any(test, feature = "test-helpers"))]
pub fn fresh_signed_token(
    signing_key: &SigningKey,
    actor_id: &[u8; 32],
    credential_id: &str,
) -> SubmissionToken {
    SubmissionToken {
        actor_id: actor_id.to_vec(),
        credential_id: credential_id.to_string(),
        issued_at: 1_700_000_000,
        expires_at: 1_700_000_000 + 86_400,
        max_recipients: 100,
        max_messages_per_day: 1000,
        user_sig: ByteBuf::from(vec![0u8; SIGNATURE_LEN]),
    }
    .sign(signing_key)
    .expect("sign submission token")
}

#[cfg(test)]
mod tests {
    use super::*;
    use rand::RngCore;

    fn fresh_keypair() -> SigningKey {
        let mut secret = [0u8; 32];
        rand::thread_rng().fill_bytes(&mut secret);
        SigningKey::from_bytes(&secret)
    }

    #[test]
    fn sign_then_verify_succeeds() {
        let sk = fresh_keypair();
        let vk = sk.verifying_key();
        let signed = fresh_signed_token(&sk, &[0u8; 32], "cred-1");
        signed.verify(&vk).unwrap();
    }

    #[test]
    fn verify_with_wrong_key_fails() {
        let sk1 = fresh_keypair();
        let sk2 = fresh_keypair();
        let signed = fresh_signed_token(&sk1, &[0u8; 32], "cred-1");
        let err = signed.verify(&sk2.verifying_key()).unwrap_err();
        assert!(matches!(err, UnwrapError::SignatureFailed));
    }

    #[test]
    fn verify_after_tamper_fails() {
        let sk = fresh_keypair();
        let vk = sk.verifying_key();
        let mut signed = fresh_signed_token(&sk, &[0u8; 32], "cred-1");
        signed.expires_at += 1; // tamper
        let err = signed.verify(&vk).unwrap_err();
        assert!(matches!(err, UnwrapError::SignatureFailed));
    }

    #[test]
    fn signature_field_zeroed_during_canonical_bytes_for_signing() {
        // Sanity: the placeholder pattern means signing+verification
        // both encode with zeros in user_sig, so signing twice with
        // the same key yields the same signature (Ed25519 is
        // deterministic).
        let sk = fresh_keypair();
        let s1 = fresh_signed_token(&sk, &[0u8; 32], "cred-1");
        let s2 = fresh_signed_token(&sk, &[0u8; 32], "cred-1");
        assert_eq!(s1.user_sig.as_ref(), s2.user_sig.as_ref());
    }

    #[test]
    fn verify_with_wrong_length_user_sig_returns_invalid_format() {
        let sk = fresh_keypair();
        let mut token = fresh_signed_token(&sk, &[0u8; 32], "cred-1");
        // Truncate user_sig to a wrong length. Verify must return
        // InvalidFormat (decoder-shape error), NOT SignatureFailed
        // (auth signal). Callers distinguish the two so they don't
        // surface format-bug detail through the auth-failure path.
        token.user_sig = ByteBuf::from(vec![0u8; 32]);
        let err = token.verify(&sk.verifying_key()).unwrap_err();
        assert!(matches!(err, UnwrapError::InvalidFormat(_)));
    }
}
