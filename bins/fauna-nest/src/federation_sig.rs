//! Sign-over-CID primitive for the nest↔nest federation channel handshake
//! (`fauna.federation.hello`). The originating nest signs the canonical dag-cbor
//! CID of a payload with its long-lived Ed25519 key and the peer verifies against
//! the `nest_id` (which **is** that key) — see
//! `docs/goal/architecture/federation.md` § Peer-auth model.
//!
//! Both the signer and the verifier encode the **same** struct with
//! `encode_canonical` (canonical IPLD dag-cbor), so the 36-byte CID is
//! byte-identical on both ends. The Ed25519 signature is **domain-separated**:
//! it covers `FEDERATION_HELLO_V1 ‖ cid` rather than the bare `cid`. The
//! deployment key is the nest's single identity (`box-recovery.md`
//! § Single-identity unification) and signs in several protocol contexts; the
//! constant `fauna.federation.hello.v1\0` prefix makes a federation signature
//! structurally un-reinterpretable in any other context
//! (`key-material-hierarchy.md` § Architectural rules #8, the
//! invariant — the registry of tags lives in `fauna_protocol::sig_domain`).
//!
//! Verification keeps the same two independent checks as the generic
//! `SignedEnvelope::verify_permissive` recipe — (1) `blake3(bytes) == cid` and (2) an
//! Ed25519 verify — but step (2) runs over the **tagged** message
//! (`FEDERATION_HELLO_V1 ‖ cid`), so it cannot delegate to
//! `SignedEnvelope::verify_permissive` (which signs the untagged CID, the generic
//! app-layer sign-over-CID used by posts/etc. with *actor* keys). No encoder
//! sits in the verify path beyond re-deriving the canonical bytes. Federation
//! is **not live in alpha** (no external peers), so there is no legacy untagged
//! federation signature to accept — unlike the additive/versioned cert-binding
//! compat shim, the tag here is enforced with no fallback.
//!
//! The bespoke per-request signing payloads for the HTTP federation twins
//! (key-package fetch, Welcome delivery, namespace/MLS sync) were retired with
//! the HTTP interim in Spec Y2 slice 5: the channel handshake binds the session
//! once (its `FederationHelloSig` lives in `federation_channel.rs`), and every
//! in-session request is attributed to the verified peer with no per-request
//! signature. The canonical form here is byte-identical to the e2e harness's
//! `cbor2.dumps(payload, canonical=True)` (`tests/common/envelope.py`), and the
//! harness prepends the **same** `FEDERATION_HELLO_V1` tag before signing
//! (`tests/common/helpers.py` `sign_as_nest` → `sign_dagcbor_envelope`), so the
//! cross-language signature stays byte-identical to `sign_payload` below.

use ed25519_dalek::{Signer, SigningKey, VerifyingKey};
use fauna_cbor::{Cid, EncodeError, SignedEnvelope, encode_canonical};
use fauna_protocol::sig_domain::{FEDERATION_HELLO_V1, domain_separated};
use serde::Serialize;

// ── Envelope wire codec + sign / verify ───────────────────────────────────────

/// Encode a sign-over-CID envelope as 100-byte hex (36-byte CID || 64-byte sig).
pub fn encode_envelope_hex(env: &SignedEnvelope) -> String {
    let mut raw = [0u8; 100];
    raw[0..36].copy_from_slice(env.cid().as_bytes());
    raw[36..100].copy_from_slice(env.sig());
    hex::encode(raw)
}

/// Decode a hex-encoded 100-byte sign-over-CID envelope (36-byte CID || 64-byte
/// sig). Returns `None` on any shape error; the caller verifies separately.
pub fn decode_envelope_hex(hex_str: &str) -> Option<SignedEnvelope> {
    let raw = hex::decode(hex_str).ok()?;
    if raw.len() != 100 {
        return None;
    }
    let mut cid_arr = [0u8; 36];
    cid_arr.copy_from_slice(&raw[0..36]);
    let cid = Cid::from_bytes(cid_arr).ok()?;
    let mut sig_arr = [0u8; 64];
    sig_arr.copy_from_slice(&raw[36..100]);
    Some(SignedEnvelope::from_parts(cid, sig_arr))
}

/// Canonical-encode `payload`, build the sign-over-CID envelope with `sk`, and
/// return its 100-byte hex form. The wire CID is the untagged payload CID, but
/// the signature covers the **domain-separated** `FEDERATION_HELLO_V1 ‖ cid`
/// (see module docs).
pub fn sign_payload<T: Serialize>(payload: &T, sk: &SigningKey) -> Result<String, EncodeError> {
    let bytes = encode_canonical(payload)?;
    let cid = Cid::of_dag_cbor(&bytes);
    let sig = sk
        .sign(&domain_separated(FEDERATION_HELLO_V1, cid.as_bytes()))
        .to_bytes();
    Ok(encode_envelope_hex(&SignedEnvelope::from_parts(cid, sig)))
}

/// Verify `envelope_hex` against the canonical dag-cbor of `payload` and the
/// signer's public key `pk`. `false` on any decode / hash / signature failure.
///
/// Runs the same two independent checks as `SignedEnvelope::verify_permissive` —
/// `blake3(bytes) == cid` then an Ed25519 verify — but the signature is verified
/// over the **domain-tagged** `FEDERATION_HELLO_V1 ‖ cid`, so it cannot delegate
/// to `SignedEnvelope::verify_permissive` (which checks the untagged CID). The tag is
/// enforced with no legacy fallback (federation is not live in alpha).
pub fn verify_payload<T: Serialize>(envelope_hex: &str, payload: &T, pk: &VerifyingKey) -> bool {
    let Some(env) = decode_envelope_hex(envelope_hex) else {
        return false;
    };
    let Ok(bytes) = encode_canonical(payload) else {
        return false;
    };
    // (1) blake3(bytes) == cid — the CID half, no encoder beyond re-deriving
    // the canonical bytes.
    if !env.cid().matches(&bytes) {
        return false;
    }
    // (2) Ed25519 verify over the domain-tagged CID. Through the one primitive:
    // a federation peer supplies its own key at hello, so this IS the
    // attacker-chosen-key shape — inert today only because federation
    // is not live in alpha, which is not a property to depend on.
    let msg = domain_separated(FEDERATION_HELLO_V1, env.cid().as_bytes());
    fauna_core::identity::verify_detached(&pk.to_bytes(), &msg, env.sig())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A stand-in payload for the generic sign/verify primitive (the federation
    /// channel handshake supplies its own `FederationHelloSig`).
    #[derive(Serialize)]
    struct TestSig<'a> {
        nest_id: &'a str,
        nonce: &'a str,
        seq: i64,
    }

    fn keypair() -> SigningKey {
        let mut secret = [0u8; 32];
        getrandom::fill(&mut secret).unwrap();
        SigningKey::from_bytes(&secret)
    }

    #[test]
    fn sign_verify_round_trips() {
        let sk = keypair();
        let pk = sk.verifying_key();
        let payload = TestSig {
            nest_id: "aa",
            nonce: "bb",
            seq: 7,
        };
        let env = sign_payload(&payload, &sk).unwrap();
        assert!(verify_payload(&env, &payload, &pk));
    }

    #[test]
    fn verify_rejects_wrong_field() {
        let sk = keypair();
        let pk = sk.verifying_key();
        let signed = TestSig {
            nest_id: "aa",
            nonce: "bb",
            seq: 7,
        };
        let env = sign_payload(&signed, &sk).unwrap();
        let tampered = TestSig {
            nest_id: "aa",
            nonce: "bb",
            seq: 8,
        };
        assert!(!verify_payload(&env, &tampered, &pk));
    }

    #[test]
    fn verify_rejects_wrong_signer() {
        let sk = keypair();
        let other = keypair();
        let payload = TestSig {
            nest_id: "aa",
            nonce: "bb",
            seq: 7,
        };
        let env = sign_payload(&payload, &sk).unwrap();
        assert!(!verify_payload(&env, &payload, &other.verifying_key()));
    }

    #[test]
    fn verify_rejects_untagged_federation_sig() {
        // The older federation format signed the *bare* CID. After
        // the domain-separation flip a verifier must reject it — proving the tag
        // is genuinely enforced (no legacy fallback; federation is not live in
        // alpha, so unlike cert-binding there is no compat shim).
        let sk = keypair();
        let pk = sk.verifying_key();
        let payload = TestSig {
            nest_id: "aa",
            nonce: "bb",
            seq: 7,
        };
        let bytes = encode_canonical(&payload).unwrap();
        let cid = Cid::of_dag_cbor(&bytes);
        let untagged_sig = sk.sign(cid.as_bytes()).to_bytes();
        let untagged = encode_envelope_hex(&SignedEnvelope::from_parts(cid, untagged_sig));
        assert!(!verify_payload(&untagged, &payload, &pk));
        // And the correctly-tagged signature over the same payload still verifies.
        let tagged = sign_payload(&payload, &sk).unwrap();
        assert!(verify_payload(&tagged, &payload, &pk));
    }

    #[test]
    fn verify_rejects_cross_context_tag() {
        // A signature made under a *different* deployment-key context
        // (`CERT_BINDING_V1`) over the very same CID must not verify as a
        // federation hello — the structural cross-context guarantee of rule #8.
        use fauna_protocol::sig_domain::CERT_BINDING_V1;
        let sk = keypair();
        let pk = sk.verifying_key();
        let payload = TestSig {
            nest_id: "aa",
            nonce: "bb",
            seq: 7,
        };
        let bytes = encode_canonical(&payload).unwrap();
        let cid = Cid::of_dag_cbor(&bytes);
        let wrong_ctx_sig = sk
            .sign(&domain_separated(CERT_BINDING_V1, cid.as_bytes()))
            .to_bytes();
        let wrong_ctx = encode_envelope_hex(&SignedEnvelope::from_parts(cid, wrong_ctx_sig));
        assert!(!verify_payload(&wrong_ctx, &payload, &pk));
    }

    #[test]
    fn verify_rejects_legacy_json_envelope() {
        // An envelope built the OLD way (sign over `serde_json::to_vec`) must no
        // longer verify against the canonical-dag-cbor payload — proving the
        // wire encoding genuinely changed, not merely the codec byte.
        let sk = keypair();
        let pk = sk.verifying_key();
        let payload = TestSig {
            nest_id: "aa",
            nonce: "bb",
            seq: 7,
        };
        let json = serde_json::json!({ "nest_id": "aa", "nonce": "bb", "seq": 7 });
        let json_bytes = serde_json::to_vec(&json).unwrap();
        let cid = Cid::of_dag_cbor(&json_bytes);
        let sig = sk.sign(cid.as_bytes()).to_bytes();
        let legacy = encode_envelope_hex(&SignedEnvelope::from_parts(cid, sig));
        assert!(!verify_payload(&legacy, &payload, &pk));
    }
}
