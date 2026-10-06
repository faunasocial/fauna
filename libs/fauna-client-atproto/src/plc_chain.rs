//! Authenticating a PLC audit log — the check that licenses a signature.
//!
//! [`crate::genesis_verify::verify_audit_log`] answers a *custody* question
//! (does every standing op carry a held key at `rotationKeys[0]`?) and answers
//! it from membership alone. That is the right shape for an **alarm**: it is
//! deliberately sensitive, and a false positive costs the user a card, not an
//! act.
//!
//! It is the wrong shape for the **contest**, which signs. The recovery fork
//! carries the fork point's published JSON forward verbatim (decision 3 of
//! `docs/goal/behavior/atproto-pds-bridge.md` § *The 72 h recovery-fork
//! contest*) and signs the result with the user's genuine senior rotation key —
//! so whatever the directory said the fork point contained becomes bytes the
//! user attested to. Two harms follow if the log is never authenticated, and
//! this module exists to close both:
//!
//! 1. **Signature theft.** A tampered fork point puts attacker-chosen
//!    `verificationMethods` (who may author the repo) and `services` (where the
//!    world reads it) inside the signed bytes. PLC's whole integrity property
//!    is that the directory *cannot forge the user's signature*, so this
//!    converts "the directory lied" — catchable by any third party — into "the
//!    user authorized it", which survives independent audit.
//! 2. **Induced rollback.** A *fabricated violation* appended to the user's
//!    real ops makes the client offer a contest whose fork point is genuine.
//!    The real directory accepts that fork, and the user's own legitimate later
//!    operations are nullified — with the bridge's junior key pruned out of the
//!    new head, so the deployment's automation stops too. The attacker needs no
//!    compromise at all: the alarm the user is answering is itself the lure.
//!
//! The remedy is the property PLC actually offers: a did:plc **is** the hash of
//! its own genesis operation, and every later operation is signed by a rotation
//! key the operation it chains to listed. A client that knows the DID can
//! therefore verify the whole standing chain against nobody's testimony —
//! which is the same reason [`crate::genesis_verify`] fetches the log over the
//! client's own HTTPS rather than through the nest.
//!
//! ⚠ **Detection stays permissive; only the act is gated.** Nothing here is
//! wired into `verify_audit_log`. A hostile log may still raise the alarm and
//! render the ceremony — the finding this module closes says so in as many
//! words ("the plan is allowed to see the hostile log; what must not happen is
//! a signature over it"), and it is also what decision 6 asks for: detect on a
//! clock, act only on the user's gesture over evidence.

use base64::Engine as _;
use base64::engine::general_purpose::URL_SAFE_NO_PAD as B64URL;
use fauna_protocol::atproto::{DidKeyCurve, decode_did_key};

/// RFC 4648 base32, lowercase, no padding — the alphabet both a CIDv1 string
/// and a did:plc identifier are written in.
fn base32_lower_nopad(bytes: &[u8]) -> String {
    const ALPHABET: &[u8; 32] = b"abcdefghijklmnopqrstuvwxyz234567";
    let mut out = String::with_capacity(bytes.len().div_ceil(5) * 8);
    for chunk in bytes.chunks(5) {
        let mut buf = [0u8; 5];
        buf[..chunk.len()].copy_from_slice(chunk);
        let n = u64::from_be_bytes([0, 0, 0, buf[0], buf[1], buf[2], buf[3], buf[4]]);
        // 8 output chars per 5 input bytes; a short final chunk emits only the
        // characters its own bits reach — the no-padding form.
        let chars = match chunk.len() {
            1 => 2,
            2 => 4,
            3 => 5,
            4 => 7,
            _ => 8,
        };
        for i in 0..chars {
            out.push(ALPHABET[((n >> (35 - i * 5)) & 0x1f) as usize] as char);
        }
    }
    out
}

/// The CIDv1 string of a dag-cbor block: multibase-base32 of
/// `<v1><dag-cbor><sha2-256 multihash>`.
///
/// This is what a PLC audit-log entry's `cid` carries and what a following op's
/// `prev` chains on, computed over the **signed** operation (the `sig` field
/// included) — the same derivation the bridge mints with
/// (`bins/fauna-bridges/internal/atprotoid/plc.go`, `SignedCBOR` +
/// `DerivePlcDid`).
///
/// Written out rather than taken from the `cid` crate on purpose: the callers'
/// whole job is binding bytes to an identifier, so a dozen lines of framing
/// with independently-pinned vectors is a smaller trusted base than a
/// dependency — and this crate compiles to wasm for the web SPA.
pub fn dag_cbor_cid_v1(block: &[u8]) -> String {
    use sha2::{Digest, Sha256};
    let digest = Sha256::digest(block);
    // 0x01 CIDv1, 0x71 dag-cbor, 0x12 sha2-256, 0x20 digest length.
    let mut bytes = Vec::with_capacity(4 + digest.len());
    bytes.extend_from_slice(&[0x01, 0x71, 0x12, 0x20]);
    bytes.extend_from_slice(&digest);
    format!("b{}", base32_lower_nopad(&bytes))
}

/// Derive the `did:plc:` identifier from a **signed genesis op's** dag-cbor
/// bytes: `"did:plc:" + lowercase-base32(sha256(bytes))[..24]`.
///
/// The root of trust for a did:plc log: the DID *is* the hash of its own
/// genesis operation, so a client that knows the DID can tell a real log from a
/// fabricated one without asking anyone. Byte-for-byte the derivation the
/// bridge mints with (`plc.go`'s `DerivePlcDid`).
pub fn derive_plc_did(signed_op_cbor: &[u8]) -> String {
    use sha2::{Digest, Sha256};
    let digest = Sha256::digest(signed_op_cbor);
    format!("did:plc:{}", &base32_lower_nopad(&digest)[..24])
}

/// Verify one operation signature against a single `did:key`.
///
/// ECDSA-SHA256 over the op's **unsigned** canonical dag-cbor, signature as the
/// fixed-size `r‖s` pair, base64url-no-pad — the form
/// [`crate::tombstone::sign_tombstone`] and [`crate::recovery_fork::sign_fork_op`]
/// produce and the directory validates. Both did:plc rotation-key curves are
/// covered: the user's senior key is P-256 and the bridge signs its ops with a
/// K-256 key (`plc.go`), so a P-256-only verifier would silently fail to
/// authenticate every op the box ever wrote.
///
/// High-S signatures are accepted on *verification* even though our own signers
/// normalize to low-S: this authenticates what a directory already published,
/// and refusing a malleable-but-valid encoding would reject real history rather
/// than catch an attacker.
pub fn verify_op_sig(unsigned_cbor: &[u8], sig_b64: &str, did_key: &str) -> bool {
    let Ok(sig_bytes) = B64URL.decode(sig_b64) else {
        return false;
    };
    let Ok((curve, point)) = decode_did_key(did_key) else {
        return false;
    };
    match curve {
        DidKeyCurve::P256 => {
            use p256::ecdsa::signature::Verifier as _;
            let (Ok(sig), Ok(vk)) = (
                p256::ecdsa::Signature::from_slice(&sig_bytes),
                p256::ecdsa::VerifyingKey::from_sec1_bytes(&point),
            ) else {
                return false;
            };
            vk.verify(unsigned_cbor, &sig).is_ok()
        }
        DidKeyCurve::K256 => {
            use k256::ecdsa::signature::Verifier as _;
            let (Ok(sig), Ok(vk)) = (
                k256::ecdsa::Signature::from_slice(&sig_bytes),
                k256::ecdsa::VerifyingKey::from_sec1_bytes(&point),
            ) else {
                return false;
            };
            vk.verify(unsigned_cbor, &sig).is_ok()
        }
    }
}

/// Why a standing chain failed to authenticate.
///
/// Every arm is a refusal to *sign*, never a claim that the user is under
/// attack — an unauthenticatable log is a log this client will not act on,
/// which is a different statement from the custody alarm's.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum ChainFailure {
    /// An entry's `operation` could not be canonically encoded.
    #[error("standing op {index} could not be re-encoded as dag-cbor: {detail}")]
    Encode { index: usize, detail: String },
    /// `cid` is not the CID of `operation` — the entry's content and its
    /// identifier disagree, which is exactly the tamper this check exists for.
    #[error("standing op {index}: cid does not match its operation")]
    CidMismatch { index: usize },
    /// The DID is not the hash of this log's genesis operation, so the log
    /// belongs to some other identity (or to none).
    #[error("the genesis operation does not derive the DID being contested")]
    GenesisNotThisDid,
    /// A non-genesis op's `prev` does not name the op before it.
    #[error("standing op {index}: prev does not chain to the previous standing op")]
    BrokenChain { index: usize },
    /// No listed rotation key of the op this one chains to signed it.
    #[error("standing op {index}: signature verifies against no rotation key in force")]
    BadSignature { index: usize },
}

/// One standing entry, as the chain verifier needs it.
pub struct ChainEntry<'a> {
    /// The entry's published `cid`.
    pub cid: &'a str,
    /// The entry's `operation`, exactly as the directory served it.
    pub raw_op: &'a serde_json::Value,
    /// `rotationKeys` as the typed view read them — the keys this op puts *in
    /// force* for the op that chains to it.
    pub rotation_keys: &'a [String],
}

/// Verify the standing chain from genesis through `through_index` inclusive.
///
/// The three assertions, and why each is load-bearing:
///
/// - **`cid` is the CID of `operation`.** Binds content to the identifier the
///   fork's `prev` will name. Without it a tampered op can keep a real `cid`,
///   so the fork names a `prev` the real directory knows while the signed bytes
///   are the attacker's.
/// - **`prev` chains to the previous standing entry.** Makes the sequence a
///   chain rather than a set, so an op cannot be spliced in beside history.
/// - **`sig` verifies against a rotation key the previous op listed** (the
///   genesis against its own list). This is what a fabricator cannot produce:
///   a violation must have been signed by a key genuinely in force, so a real
///   compromise still contests — the box's junior key is listed, which is the
///   whole premise of the custody split — while an invented one cannot.
///
/// Plus the anchor: the genesis operation must hash to the DID being contested.
///
/// Only up to `through_index` — the contested op — is checked. Ops *after* the
/// violation chain through it under the attacker's authority and are displaced
/// by the fork anyway; requiring them to verify would let a malformed tail deny
/// a user their remedy.
pub fn verify_standing_chain(
    entries: &[ChainEntry<'_>],
    did: &str,
    through_index: usize,
) -> Result<(), ChainFailure> {
    for (index, entry) in entries.iter().enumerate().take(through_index + 1) {
        let signed_cbor =
            fauna_protocol::encode_canonical(entry.raw_op).map_err(|e| ChainFailure::Encode {
                index,
                detail: e.to_string(),
            })?;
        if dag_cbor_cid_v1(&signed_cbor) != entry.cid {
            return Err(ChainFailure::CidMismatch { index });
        }

        // The signed bytes are the op WITHOUT its `sig` — the same asymmetry
        // `build_fork_op` relies on when it strips the inherited signature.
        let mut unsigned = entry.raw_op.clone();
        let sig = unsigned
            .as_object_mut()
            .and_then(|o| o.remove("sig"))
            .and_then(|s| s.as_str().map(str::to_string))
            .unwrap_or_default();
        let unsigned_cbor =
            fauna_protocol::encode_canonical(&unsigned).map_err(|e| ChainFailure::Encode {
                index,
                detail: e.to_string(),
            })?;

        let keys_in_force: &[String] = match index.checked_sub(1) {
            // The genesis is self-signed by one of its own listed keys, and it
            // is the only op whose authenticity rests on the DID rather than on
            // a predecessor.
            None => {
                if derive_plc_did(&signed_cbor) != did {
                    return Err(ChainFailure::GenesisNotThisDid);
                }
                entry.rotation_keys
            }
            Some(prev_index) => {
                let prev = &entries[prev_index];
                let names_prev = entry
                    .raw_op
                    .get("prev")
                    .and_then(serde_json::Value::as_str)
                    .is_some_and(|p| p == prev.cid);
                if !names_prev {
                    return Err(ChainFailure::BrokenChain { index });
                }
                prev.rotation_keys
            }
        };

        if !keys_in_force
            .iter()
            .any(|k| verify_op_sig(&unsigned_cbor, &sig, k))
        {
            return Err(ChainFailure::BadSignature { index });
        }
    }
    Ok(())
}

/// Mint **real** PLC audit logs for tests — correctly chained, correctly
/// CID'd, and genuinely signed by keys the chain actually lists.
///
/// This exists because the fixtures it replaces did not. Every op in
/// `recovery_fork`'s original tests carried `"sig": "AAAA"` and every plan
/// succeeded, which is the second proof the finding offered: a
/// signature that is not even base64 of a P-256 signature was accepted,
/// because no code path looked. A fixture that cannot produce a valid
/// signature also cannot produce an *invalid* one on purpose — so it can test
/// neither half of the property.
///
/// The keys mirror production roles: the user's senior rotation key is P-256
/// (what `rotation_key::generate_rotation_key` mints) and the box's junior key
/// is K-256 (what the bridge signs with, `plc.go`). Getting that pairing wrong
/// in a fixture would hide a whole-curve verification gap.
#[cfg(test)]
pub(crate) mod test_log {
    use super::*;
    use std::collections::BTreeMap;

    /// Deterministic scalars — reproducible fixtures, and a value that is
    /// obviously not real key material.
    fn p256_key(seed: u8) -> p256::ecdsa::SigningKey {
        p256::ecdsa::SigningKey::from_slice(&[seed; 32]).expect("valid P-256 scalar")
    }

    fn k256_key(seed: u8) -> k256::ecdsa::SigningKey {
        k256::ecdsa::SigningKey::from_slice(&[seed; 32]).expect("valid K-256 scalar")
    }

    /// A signing key plus the `did:key` string the log lists it under.
    enum Secret {
        P256(Box<p256::ecdsa::SigningKey>),
        K256(Box<k256::ecdsa::SigningKey>),
    }

    impl Secret {
        fn did_key(&self) -> String {
            match self {
                Secret::P256(k) => fauna_protocol::atproto::encode_did_key(
                    DidKeyCurve::P256,
                    k.verifying_key().to_encoded_point(true).as_bytes(),
                ),
                Secret::K256(k) => fauna_protocol::atproto::encode_did_key(
                    DidKeyCurve::K256,
                    k.verifying_key().to_encoded_point(true).as_bytes(),
                ),
            }
            .expect("compressed SEC1 point")
        }

        fn sign(&self, msg: &[u8]) -> String {
            match self {
                Secret::P256(k) => {
                    use p256::ecdsa::signature::Signer as _;
                    let sig: p256::ecdsa::Signature = k.sign(msg);
                    B64URL.encode(sig.normalize_s().unwrap_or(sig).to_bytes())
                }
                Secret::K256(k) => {
                    use k256::ecdsa::signature::Signer as _;
                    let sig: k256::ecdsa::Signature = k.sign(msg);
                    B64URL.encode(sig.normalize_s().unwrap_or(sig).to_bytes())
                }
            }
        }
    }

    fn secrets() -> Vec<Secret> {
        vec![
            Secret::P256(Box::new(p256_key(7))),  // user senior
            Secret::K256(Box::new(k256_key(9))),  // box junior
            Secret::P256(Box::new(p256_key(11))), // some other held key
            Secret::P256(Box::new(p256_key(13))), // the attacker's key
        ]
    }

    /// The user's senior rotation key — P-256, as `rotation_key` mints.
    pub(crate) fn user_key() -> String {
        secrets()[0].did_key()
    }
    /// The bridge's junior rotation key — K-256, as the bridge mints.
    pub(crate) fn box_key() -> String {
        secrets()[1].did_key()
    }
    /// A second user-held key (a sibling device's fresh re-mint key).
    pub(crate) fn other_key() -> String {
        secrets()[2].did_key()
    }
    /// A key nobody in the ring holds — what a seizure installs.
    pub(crate) fn evil_key() -> String {
        secrets()[3].did_key()
    }

    /// One row to mint. `rotation_keys: None` renders a `plc_tombstone`.
    pub(crate) struct RowSpec {
        name: String,
        rotation_keys: Option<Vec<String>>,
        created_at: String,
        nullified: bool,
        forged: bool,
        signed_by: Option<String>,
        extra: Vec<(String, serde_json::Value)>,
    }

    /// A row of the log to build. `name` is a nickname the test refers to the
    /// entry by — the real CID is only knowable after signing, so tests ask
    /// the built chain for it ([`Chain::cid`]).
    pub(crate) fn row(name: &str, rotation_keys: Option<&[String]>, created_at: &str) -> RowSpec {
        RowSpec {
            name: name.to_string(),
            rotation_keys: rotation_keys.map(<[String]>::to_vec),
            created_at: created_at.to_string(),
            nullified: false,
            forged: false,
            signed_by: None,
            extra: Vec::new(),
        }
    }

    impl RowSpec {
        /// Sign this row with a named key rather than the default author.
        ///
        /// Worth being explicit about on any row that models an *act*: a
        /// seizure is the box signing with its own junior key, and a fixture
        /// that let the user's key sign it would be testing a shape that
        /// cannot occur.
        pub(crate) fn signed_by(mut self, did_key: String) -> Self {
            self.signed_by = Some(did_key);
            self
        }

        /// Mark this row nullified — the directory's record of a displaced op.
        pub(crate) fn nullified(mut self) -> Self {
            self.nullified = true;
            self
        }

        /// Add a top-level field to this op before it is signed and CID'd.
        ///
        /// The point is a field no version of this crate models: it must ride
        /// into the *signed bytes* and out again through the fork's verbatim
        /// carry-forward. Injecting it after minting would only prove the test
        /// can edit JSON — and would now break the op's own CID.
        pub(crate) fn extra_field(mut self, name: &str, value: serde_json::Value) -> Self {
            self.extra.push((name.to_string(), value));
            self
        }

        /// Sign this row with a key that is **not** in force for it — the
        /// fabricated operation an attacker splices into a served log. Chains
        /// and CIDs stay honest, so only signature verification catches it.
        pub(crate) fn forged(mut self) -> Self {
            self.forged = true;
            self
        }
    }

    /// A minted log: the body a caller would have fetched, plus the facts only
    /// the minting knows (the DID its genesis derives, and each nickname's CID).
    pub(crate) struct Chain {
        rows: Vec<serde_json::Value>,
        did: String,
        cids: BTreeMap<String, String>,
    }

    impl Chain {
        /// The raw `/{did}/log/audit` body.
        pub(crate) fn body(&self) -> Vec<u8> {
            serde_json::to_vec(&self.rows).expect("rows serialize")
        }

        /// The DID this log's genesis operation derives — the real one.
        pub(crate) fn did(&self) -> &str {
            &self.did
        }

        /// The real CID minted for a nicknamed row.
        pub(crate) fn cid(&self, name: &str) -> String {
            self.cids
                .get(name)
                .unwrap_or_else(|| panic!("no row named {name}"))
                .clone()
        }

        /// Tamper a row's *content* while leaving its published `cid` alone —
        /// the exact shape describes: a real entry whose `operation`
        /// object was rewritten in flight, so the fork would name a `prev` the
        /// real directory knows while the signed bytes are the attacker's.
        pub(crate) fn tamper(&mut self, name: &str, f: impl FnOnce(&mut serde_json::Value)) {
            let cid = self.cid(name);
            let row = self
                .rows
                .iter_mut()
                .find(|r| r["cid"] == serde_json::Value::String(cid.clone()))
                .expect("row exists");
            f(row.get_mut("operation").expect("operation"));
        }
    }

    /// Mint a real, chained, signed log from the row specs.
    ///
    /// Each op is signed by the first key **in force for it** that these
    /// fixtures hold a secret for — the genesis by one of its own listed keys,
    /// every later op by one the op it chains to listed, exactly as PLC
    /// requires. `forged()` rows deliberately break that rule instead.
    pub(crate) fn mint(specs: Vec<RowSpec>) -> Chain {
        let secrets = secrets();
        let mut rows = Vec::new();
        let mut cids = BTreeMap::new();
        let mut did = String::new();
        let mut prev_cid: Option<String> = None;
        // The keys the NEXT op must be signed by, per the last standing op.
        let mut in_force: Vec<String> = Vec::new();

        for spec in specs {
            let mut unsigned = match &spec.rotation_keys {
                Some(keys) => serde_json::json!({
                    "type": "plc_operation",
                    "prev": prev_cid.clone().map(serde_json::Value::String)
                        .unwrap_or(serde_json::Value::Null),
                    "rotationKeys": keys,
                    "verificationMethods": {"atproto": "did:key:zBoxSigningKey"},
                    "alsoKnownAs": ["at://alice.example.com"],
                    "services": {
                        "atproto_pds": {
                            "type": "AtprotoPersonalDataServer",
                            "endpoint": "https://pds.example.com"
                        }
                    },
                }),
                None => serde_json::json!({
                    "type": "plc_tombstone",
                    "prev": prev_cid.clone().map(serde_json::Value::String)
                        .unwrap_or(serde_json::Value::Null),
                }),
            };

            for (name, value) in &spec.extra {
                unsigned
                    .as_object_mut()
                    .expect("object")
                    .insert(name.clone(), value.clone());
            }

            // Genesis signs with its own list; every later op with the list the
            // op it chains to put in force.
            let candidates = if prev_cid.is_none() {
                spec.rotation_keys.clone().unwrap_or_default()
            } else {
                in_force.clone()
            };
            let signer = if spec.forged {
                // A key that is provably not in force — the fabrication.
                secrets
                    .iter()
                    .find(|s| !candidates.contains(&s.did_key()))
                    .expect("a key outside the in-force set")
            } else if let Some(want) = &spec.signed_by {
                secrets
                    .iter()
                    .find(|s| &s.did_key() == want)
                    .expect("signed_by names a key these fixtures hold")
            } else {
                // Default author is the BOX, matching production: the bridge
                // mints the genesis and signs subsequent updates, so a fixture
                // that defaulted to the user's key would model a world where
                // the user routinely authors ops they never author.
                let preferred = box_key();
                secrets
                    .iter()
                    .find(|s| s.did_key() == preferred && candidates.contains(&preferred))
                    .or_else(|| secrets.iter().find(|s| candidates.contains(&s.did_key())))
                    .expect("fixtures must hold a key that is actually in force")
            };

            let unsigned_cbor = fauna_protocol::encode_canonical(&unsigned).expect("encode");
            let sig = signer.sign(&unsigned_cbor);
            let mut signed = unsigned;
            signed
                .as_object_mut()
                .expect("object")
                .insert("sig".into(), serde_json::Value::String(sig));

            let signed_cbor = fauna_protocol::encode_canonical(&signed).expect("encode");
            let cid = dag_cbor_cid_v1(&signed_cbor);
            if prev_cid.is_none() {
                did = derive_plc_did(&signed_cbor);
            }
            if !spec.nullified {
                prev_cid = Some(cid.clone());
                if let Some(keys) = &spec.rotation_keys {
                    in_force = keys.clone();
                }
            }
            cids.insert(spec.name.clone(), cid.clone());
            rows.push(serde_json::json!({
                "cid": cid,
                "nullified": spec.nullified,
                "createdAt": spec.created_at,
                "operation": signed,
            }));
        }

        Chain { rows, did, cids }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // ── CID + DID derivation ────────────────────────────────────────────────
    //
    // Vectors computed with Python's stdlib (`hashlib` + `base64.b32encode`),
    // deliberately NOT with this module's own encoder: the risk being guarded
    // is a bug in our framing, which a self-referential fixture cannot catch.
    // The algorithm itself is pinned twice over by the Go twin in
    // `bins/fauna-bridges/internal/atprotoid/plc.go`.

    #[test]
    fn cid_and_did_match_independently_computed_vectors() {
        for (block, cid, did) in [
            (
                &b""[..],
                "bafyreihdwdcefgh4dqkjv67uzcmw7ojee6xedzdetojuzjevtenxquvyku",
                "did:plc:4oymiquy7qobjgx36tejs35z",
            ),
            (
                &b"hello"[..],
                "bafyreibm6jg3ux5qumhcn2b3flc3tyu6dmlb4xa7u5bf44yegnrjhc4yeq",
                "did:plc:ftze3os7wcrq4jxihmvmlopc",
            ),
            (
                // An arbitrary byte string — this pins the CID *framing*
                // (multihash + multicodec + base32), not any CBOR shape.
                &b"\xa1aki1"[..],
                "bafyreigrt5jbotz7b5o3hgfel3r52ea2sgbbyx7v6foiixk6batx4ezz5i",
                "did:plc:2gpvef2ph4hv3m4yurpohxiq",
            ),
        ] {
            assert_eq!(dag_cbor_cid_v1(block), cid, "cid of {block:?}");
            assert_eq!(derive_plc_did(block), did, "did of {block:?}");
        }
    }

    // ── the chain verifier ──────────────────────────────────────────────────

    use crate::genesis_verify::parse_audit_log_with_raw;
    use test_log::{box_key, evil_key, mint, row, user_key};

    /// Re-read a minted body the way the contest path does, and verify it.
    fn verify(body: &[u8], did: &str, through: usize) -> Result<(), ChainFailure> {
        let parsed = parse_audit_log_with_raw(body).expect("parses");
        let standing: Vec<_> = parsed.into_iter().filter(|(e, _)| !e.nullified).collect();
        let entries: Vec<ChainEntry<'_>> = standing
            .iter()
            .map(|(e, raw)| ChainEntry {
                cid: &e.cid,
                raw_op: raw,
                rotation_keys: &e.operation.rotation_keys,
            })
            .collect();
        verify_standing_chain(&entries, did, through)
    }

    fn seizure_log() -> test_log::Chain {
        let (u, b, e) = (user_key(), box_key(), evil_key());
        mint(vec![
            row(
                "genesis",
                Some(&[u.clone(), b.clone()]),
                "2026-08-02T00:00:00Z",
            ),
            row("good", Some(&[u, b.clone()]), "2026-08-02T01:00:00Z"),
            // The box signs away the user's seniority with its own listed key:
            // a REAL seizure, which must remain contestable.
            row("seizure", Some(&[e, b.clone()]), "2026-08-02T02:00:00Z").signed_by(b),
        ])
    }

    #[test]
    fn a_genuinely_signed_chain_verifies_end_to_end() {
        let c = seizure_log();
        assert_eq!(verify(&c.body(), c.did(), 2), Ok(()));
    }

    #[test]
    fn a_log_belonging_to_another_did_is_refused() {
        // The anchor: a did:plc IS the hash of its own genesis, so a chain that
        // is internally perfect still cannot speak for a DID it does not hash to.
        let c = seizure_log();
        assert_eq!(
            verify(&c.body(), "did:plc:someothersubjectentirely", 2),
            Err(ChainFailure::GenesisNotThisDid),
        );
    }

    #[test]
    fn tampering_an_ops_content_breaks_its_cid() {
        // H1 shape: rewrite the fork point in flight but keep its
        // real `cid`, so the fork would name a `prev` the real directory knows.
        let mut c = seizure_log();
        c.tamper("good", |op| {
            op["verificationMethods"]["atproto"] = "did:key:zATTACKER_SIGNING_KEY".into();
            op["services"]["atproto_pds"]["endpoint"] = "https://attacker.example".into();
        });
        assert_eq!(
            verify(&c.body(), c.did(), 2),
            Err(ChainFailure::CidMismatch { index: 1 }),
        );
    }

    #[test]
    fn a_fabricated_violation_is_refused_though_it_chains_and_cids_honestly() {
        // H2 shape, and the reason this verifies the WHOLE chain
        // rather than stopping at the fork point: the attacker appends a
        // violation to the user's real ops. It chains correctly and its CID is
        // honest — only the signature betrays it, because no key in force
        // signed it.
        let (u, b, e) = (user_key(), box_key(), evil_key());
        let c = mint(vec![
            row(
                "genesis",
                Some(&[u.clone(), b.clone()]),
                "2026-08-02T00:00:00Z",
            ),
            row("good", Some(&[u, b]), "2026-08-02T01:00:00Z"),
            row("fabricated", Some(&[e]), "2026-08-02T02:00:00Z").forged(),
        ]);
        assert_eq!(
            verify(&c.body(), c.did(), 2),
            Err(ChainFailure::BadSignature { index: 2 }),
        );
        // ...and it is genuinely only the signature: through the fork point the
        // very same log is impeccable, which is what makes the fabrication
        // invisible to a fork-point-only check.
        assert_eq!(verify(&c.body(), c.did(), 1), Ok(()));
    }

    #[test]
    fn the_box_key_is_verified_on_its_own_curve() {
        // The bridge signs with K-256 while the user's key is P-256. A
        // P-256-only verifier would reject every op the box ever wrote, so the
        // seizure above would look fabricated and the real remedy would be
        // refused. Assert the box's op verifies on its own terms.
        let c = seizure_log();
        let parsed = parse_audit_log_with_raw(&c.body()).expect("parses");
        let (entry, raw) = &parsed[2];
        let mut unsigned = raw.clone();
        let sig = unsigned
            .as_object_mut()
            .and_then(|o| o.remove("sig"))
            .and_then(|s| s.as_str().map(str::to_string))
            .expect("sig");
        let msg = fauna_protocol::encode_canonical(&unsigned).expect("encode");
        assert!(verify_op_sig(&msg, &sig, &box_key()));
        assert!(!verify_op_sig(&msg, &sig, &user_key()));
        assert_eq!(entry.operation.rotation_keys[0], evil_key());
    }

    /// Cross-implementation pin: a realistic PLC operation, whose CID and
    /// derived DID must match what the e2e fake directory computes in Python
    /// (`tests/e2e-unified/helpers/atproto_fakes.py`, asserted there against
    /// these same literals) and what the bridge computes in Go.
    ///
    /// Three encoders now have to agree on canonical dag-cbor for the contest
    /// to work at all — the client verifies a CID the directory minted — so the
    /// agreement is pinned rather than assumed. A drift in map-key ordering is
    /// the classic way this breaks, and it is invisible in any single-language
    /// test.
    #[test]
    fn a_realistic_operations_cid_matches_the_other_implementations() {
        let op = serde_json::json!({
            "type": "plc_operation",
            "prev": serde_json::Value::Null,
            "rotationKeys": ["did:key:zA", "did:key:zB"],
            "verificationMethods": {"atproto": "did:key:zC"},
            "alsoKnownAs": ["at://alice.example.com"],
            "services": {
                "atproto_pds": {
                    "type": "AtprotoPersonalDataServer",
                    "endpoint": "https://pds.example.com"
                }
            },
            "sig": "AAAA"
        });
        let cbor = fauna_protocol::encode_canonical(&op).expect("encode");
        assert_eq!(
            dag_cbor_cid_v1(&cbor),
            "bafyreifracj42ynhwtl2qp2ed6x3zemr76g23tdmahn4gphc3xtdn3bp2u"
        );
        assert_eq!(derive_plc_did(&cbor), "did:plc:weajhtlbu62npkb7iqp27per");
    }

    #[test]
    fn a_signed_chain_matches_the_python_fakes_acceptance_vector() {
        // The twin of `tests/e2e-unified/tests/test_plc_acceptance_parity.py`:
        // the fake directory VALIDATES submitted signatures, so the two implementations must agree
        // on the signed bytes themselves, not just the CID framing. RFC6979
        // makes p256/k256 signatures deterministic, which is what lets a
        // signature be pinned as a literal at all. If either side drifts —
        // map-key order, b64 engine, S normalization — one of the two tests
        // reds with the drift named at its source.
        let chain = test_log::mint(vec![
            test_log::row(
                "genesis",
                Some(&[test_log::user_key(), test_log::box_key()]),
                "2026-08-01T00:00:00Z",
            ),
            test_log::row(
                "rename",
                Some(&[test_log::user_key(), test_log::box_key()]),
                "2026-08-02T00:00:00Z",
            ),
        ]);
        // The fork leg: the SAME genesis (deterministic, so the two mints
        // agree byte-for-byte) followed by a user-signed op chaining to it —
        // the shape a contest submits: `prev` names a non-head standing entry,
        // signed with the user's *P-256* senior key. The linear rows above are
        // box-signed K-256, so the pair covers both production curves.
        let fork_chain = test_log::mint(vec![
            test_log::row(
                "genesis",
                Some(&[test_log::user_key(), test_log::box_key()]),
                "2026-08-01T00:00:00Z",
            ),
            test_log::row(
                "contest",
                Some(&[test_log::user_key(), test_log::box_key()]),
                "2026-08-03T00:00:00Z",
            )
            .signed_by(test_log::user_key()),
        ]);
        assert_eq!(
            fork_chain.did(),
            chain.did(),
            "the shared genesis must derive one DID"
        );
        let rows: serde_json::Value = serde_json::from_slice(&chain.body()).expect("rows");
        let fork_rows: serde_json::Value =
            serde_json::from_slice(&fork_chain.body()).expect("rows");

        // The Python file carries these three ops as full dict literals; the
        // CIDs pin every byte of them transitively (a CID is the hash of the
        // signed op), and the sig pins localize a signature-encoding drift to
        // this test rather than a cross-suite acceptance failure.
        assert_eq!(chain.did(), "did:plc:nzz6j2ywvty3hhzbbtdqohd4");
        assert_eq!(
            chain.cid("genesis"),
            "bafyreidoopsowfvm6gzz6iimy4dry7fvmbilkzthsulpm546aes24y46n4"
        );
        assert_eq!(
            chain.cid("rename"),
            "bafyreihe2dtt67pqwwsvif45vvhs2ebdurytfcqvjn3bed7bvuubmecmrm"
        );
        assert_eq!(
            fork_chain.cid("contest"),
            "bafyreiam3bwoktmvoxgqpchwvzuxzwjlcmlg5vpvbcwgmhsye7lsp5dwzm"
        );
        assert_eq!(
            rows[0]["operation"]["sig"],
            "vcWWWr4p-DlD5R1LF0wWaeCnqflKzfOSRExZgKSLnyFKuWExlMyGkLOviMIZlxUeWxT9VhvDgqiZ1S0-xn7saQ"
        );
        assert_eq!(
            rows[1]["operation"]["sig"],
            "kaEgv46zHf9KarsZLOXUMAjdItM-oUo-_ZpMpbJnwh8VkLRSluj64Y3McBoQH_pqVQtCNjPkWu1vHMCQR09F0w"
        );
        assert_eq!(
            fork_rows[1]["operation"]["sig"],
            "ufxIoIvzfaESgV8vTwAtQ6YIP7vn0QWw1KqTdEb-E1t88XC8AwF20n3MySLqGJKlwK64nLZjN_Gor5wcnfFKVg"
        );
    }

    #[test]
    fn a_cid_is_the_real_directorys_shape() {
        // Every did:plc op CID a directory serves starts `bafyrei` — the
        // base32 of the v1/dag-cbor/sha2-256 prefix. A framing byte in the
        // wrong order still produces a plausible-looking string, so the prefix
        // is asserted rather than eyeballed.
        assert!(dag_cbor_cid_v1(b"anything").starts_with("bafyrei"));
        assert_eq!(dag_cbor_cid_v1(b"anything").len(), 59);
    }
}
