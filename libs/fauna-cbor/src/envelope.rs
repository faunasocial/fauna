// verify-ok(permissive door): `SignedEnvelope::verify_permissive` IS the
// permissive primitive, kept for fixture and negative-path tests that hold
// their own key. Its only production callers are `fauna-core`'s envelope
// wrappers, and those verify through `fauna_core::identity::verify_detached`
// (PROBE-381-A pins it). The walk guard reads this marker.
use ed25519_dalek::{Signature, Signer, SigningKey, Verifier, VerifyingKey};
use serde::Serialize;

use crate::cid::Cid;
use crate::codec::encode_canonical;
use crate::error::{EncodeError, VerifyError};

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct SignedEnvelope {
    cid: Cid,
    sig: [u8; 64],
}

impl SignedEnvelope {
    /// Encode `v` to canonical dag-cbor, compute its CID, sign the CID with `sk`.
    /// Returns `(bytes, envelope)`. The bytes ARE the signed canonical encoding.
    pub fn sign<T: Serialize>(
        v: &T,
        sk: &SigningKey,
    ) -> Result<(Vec<u8>, SignedEnvelope), EncodeError> {
        let bytes = encode_canonical(v)?;
        let cid = Cid::of_dag_cbor(&bytes);
        let sig = sk.sign(cid.as_bytes()).to_bytes();
        Ok((bytes, SignedEnvelope { cid, sig }))
    }

    /// Reconstruct an envelope from a CID and a 64-byte signature.
    ///
    /// Use this when reading an envelope off the wire (or a vendored fixture):
    /// split the 100-byte payload into a 36-byte CID and a 64-byte signature,
    /// reconstruct each, then assemble here. No verification runs in this
    /// constructor — call [`SignedEnvelope::verify_permissive`] separately against the
    /// signed bytes and the signer's public key.
    pub fn from_parts(cid: Cid, sig: [u8; 64]) -> Self {
        SignedEnvelope { cid, sig }
    }

    /// Verify in two independent steps:
    /// 1. blake3(bytes) == cid.multihash  — no encoder involved.
    /// 2. ed25519_verify(sig, cid.bytes, pk) — no encoder involved.
    ///
    /// ⚠ **`_permissive` is a warning, not a decoration — production code wants
    /// [`fauna_core::encoding::verify_envelope`] instead.** Step 2 runs
    /// `ed25519_dalek`'s permissive `Verifier`, which is not a binding signature
    /// check when `pk` arrived off the wire beside `sig`: for a small-order `pk`
    /// an all-zero signature satisfies the equation on ~a quarter of messages
    /// (measured 63/256 through this method — `PROBE-381-A`,
    /// `fauna-core/tests/sign_over_cid.rs`), so a payload can claim an identity
    /// nobody holds a key to. Every "prove you hold the key you name" ceremony
    /// therefore owes `verify_strict` plus a weak-key refusal —
    /// `fauna_core::identity::verify_detached` is the tree's single primitive for
    /// that, and `fauna-core`'s envelope wrappers route through it
    /// (`docs/goal/architecture/security.md` § Key material and signature
    /// verification). This method survives for **fixture and negative-path tests
    /// that want the raw two-step contract**, where `pk` is the test's own.
    pub fn verify_permissive(&self, bytes: &[u8], pk: &VerifyingKey) -> Result<(), VerifyError> {
        if !self.cid.matches(bytes) {
            return Err(VerifyError::CidMismatch);
        }
        let sig = Signature::from_bytes(&self.sig);
        pk.verify(self.cid.as_bytes(), &sig)
            .map_err(|_| VerifyError::SignatureInvalid)
    }

    pub fn cid(&self) -> &Cid {
        &self.cid
    }

    pub fn sig(&self) -> &[u8; 64] {
        &self.sig
    }

    /// Test-only mutator for negative tests. Hidden from docs.
    #[doc(hidden)]
    pub fn sig_mut(&mut self) -> &mut [u8; 64] {
        &mut self.sig
    }
}
