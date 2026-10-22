//! The nest's **published rules** on the wire — the signed carriage a member
//! reads in the nest's self-description, and the admin's two kinds that write
//! it (`docs/goal/behavior/nest-rules.md` § The nest content policy).
//!
//! The document is the shared content-policy grammar
//! ([`fauna_core::content_policy`], publisher `nest`); this module adds only how
//! it travels. The carriage holds the document as its **canonical bytes**, not
//! as a decoded value: the signature is over exactly those bytes, and a member
//! whose build predates a newer grammar must still decode the self-description
//! that carries it — the bytes decode version first, inside
//! [`SignedContentPolicy::verify`], to an inert document rather than failing the
//! whole `fauna.nest.info` reply.

use std::collections::BTreeMap;

use fauna_core::content_policy::{ContentPolicyDocument, NestPolicyRejection};
use serde::{Deserialize, Serialize};

use crate::Value;

/// A nest's content policy as the nest signed it with its identity key: each
/// accepted `fauna.admin.content_policy.set` stamps a monotonic `sequence` and
/// `issued_at`, so a member can show what rules were in force when and detect a
/// change (`nest-rules.md` § Acceptance at joining, and a change of rules).
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct SignedContentPolicy {
    /// The canonical dag-cbor of the document
    /// ([`fauna_core::content_policy::encode_document`]).
    #[serde(with = "serde_bytes")]
    pub document: Vec<u8>,
    /// Monotonic per nest, from 1 at the first accepted write.
    pub sequence: u64,
    /// Unix seconds the nest accepted the write.
    pub issued_at: i64,
    /// Ed25519 over [`fauna_core::content_policy::nest_policy_signing_input`],
    /// by the key `NestInfoReply::nest_id` names.
    #[serde(with = "serde_bytes")]
    pub signature: Vec<u8>,
    /// Forward-compat catch-all (transport.md § Schema and forward-compat
    /// discipline, rule 4).
    #[serde(flatten, default)]
    pub extra: BTreeMap<String, Value>,
}

impl SignedContentPolicy {
    /// Sign `document` as the nest — the nest's act on an accepted write.
    ///
    /// # Errors
    ///
    /// The document or the signing input does not encode.
    pub fn sign(
        document: &ContentPolicyDocument,
        sequence: u64,
        issued_at: i64,
        nest_key: &ed25519_dalek::SigningKey,
    ) -> Result<Self, String> {
        let document = fauna_core::content_policy::encode_document(document)?;
        let signature =
            fauna_core::content_policy::sign_nest_policy(nest_key, &document, sequence, issued_at)?
                .to_vec();
        Ok(Self {
            document,
            sequence,
            issued_at,
            signature,
            extra: BTreeMap::new(),
        })
    }

    /// Verify against the nest's identity key (`NestInfoReply::nest_id`) and
    /// decode the document — the one door to a nest's rules.
    ///
    /// # Errors
    ///
    /// See [`fauna_core::content_policy::verify_nest_policy`].
    pub fn verify(
        &self,
        nest_key: &[u8; 32],
    ) -> Result<ContentPolicyDocument, NestPolicyRejection> {
        fauna_core::content_policy::verify_nest_policy(
            nest_key,
            &self.document,
            self.sequence,
            self.issued_at,
            &self.signature,
        )
    }
}

/// `fauna.admin.content_policy.get` — no parameters; the answer is this nest's
/// rules as last accepted.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, Default)]
pub struct AdminContentPolicyGetRequest {
    #[serde(flatten, default)]
    pub extra: BTreeMap<String, Value>,
}

/// Reply to `fauna.admin.content_policy.get`.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Default)]
pub struct AdminContentPolicyGetReply {
    /// The document in force with its signature, absent when the admin has
    /// never written one (the fresh-install state: no rules).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub policy: Option<SignedContentPolicy>,
    #[serde(flatten, default)]
    pub extra: BTreeMap<String, Value>,
}

/// `fauna.admin.content_policy.set` — replace the nest's whole document.
///
/// A whole-value replace: an empty document (no rules) is how the admin
/// withdraws every rule. The nest validates it structurally with the shared
/// module for publisher `nest` before storing and refuses a malformed one with
/// the defect named ([`AdminContentPolicySetReply::defect`]).
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct AdminContentPolicySetRequest {
    pub document: ContentPolicyDocument,
    #[serde(flatten, default)]
    pub extra: BTreeMap<String, Value>,
}

/// Reply to `fauna.admin.content_policy.set`.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Default)]
pub struct AdminContentPolicySetReply {
    /// The document as accepted and signed; absent when refused.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub policy: Option<SignedContentPolicy>,
    /// Why the document was refused, as the shared validator's defect sentence;
    /// absent when accepted. Text rather than the defect type, which stays
    /// local to a build: the editor pre-validates with the same module, so a
    /// refusal is a defence, not the UX.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub defect: Option<String>,
    #[serde(flatten, default)]
    pub extra: BTreeMap<String, Value>,
}

#[cfg(test)]
mod tests {
    use fauna_core::content_policy::{ContentRule, ContentVerdict, REASON_DEFAULT_KEY};

    use super::*;
    use crate::{decode_strict, encode_canonical};

    fn document() -> ContentPolicyDocument {
        let mut d = ContentPolicyDocument::empty();
        d.rules.push(ContentRule {
            factor: "spam".into(),
            min_permille: 500,
            verdict: ContentVerdict::Collapse,
            reason_code: "HOUSE-1".into(),
            reason: BTreeMap::from([(REASON_DEFAULT_KEY.to_string(), "No spam here.".into())]),
            extra: BTreeMap::new(),
        });
        d
    }

    #[test]
    fn the_signed_carriage_verifies_against_the_nest_key_and_refuses_a_tampered_byte() {
        let key = ed25519_dalek::SigningKey::from_bytes(&[5u8; 32]);
        let public = key.verifying_key().to_bytes();
        let signed = SignedContentPolicy::sign(&document(), 1, 1_700_000_000, &key).unwrap();
        let bytes = encode_canonical(&signed).unwrap();
        let back: SignedContentPolicy = decode_strict(&bytes).unwrap();
        assert_eq!(back.verify(&public).unwrap(), document());

        let mut tampered = back.clone();
        tampered.document[3] ^= 0x01;
        assert_eq!(
            tampered.verify(&public),
            Err(NestPolicyRejection::SignatureFailed)
        );
        let mut resequenced = back;
        resequenced.sequence = 2;
        assert_eq!(
            resequenced.verify(&public),
            Err(NestPolicyRejection::SignatureFailed)
        );
    }

    #[test]
    fn the_set_request_and_replies_round_trip() {
        let req = AdminContentPolicySetRequest {
            document: document(),
            extra: BTreeMap::new(),
        };
        let bytes = encode_canonical(&req).unwrap();
        assert_eq!(
            decode_strict::<AdminContentPolicySetRequest>(&bytes).unwrap(),
            req
        );

        let refused = AdminContentPolicySetReply {
            defect: Some("rule 0 carries no reason for the required default key".into()),
            ..Default::default()
        };
        let bytes = encode_canonical(&refused).unwrap();
        assert_eq!(
            decode_strict::<AdminContentPolicySetReply>(&bytes).unwrap(),
            refused
        );
    }
}
