//! Content moderation label types.
//!
//! `ContentLabel` is the universal output format for all scanning mechanisms.

use serde::{Deserialize, Serialize};

use crate::data::Timestamp;
use crate::identity::ActorId;

/// What was scanned — a reference to any content type in Fauna.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub enum ContentRef {
    Post {
        #[serde(with = "serde_bytes")]
        post_id: [u8; 32],
    },
    Blob {
        #[serde(with = "serde_bytes")]
        blob_hash: [u8; 32],
    },
    Email {
        inbox_id: u64,
    },
    Channel {
        #[serde(with = "serde_bytes")]
        channel_id: [u8; 32],
        message_index: u64,
    },
    Actor {
        actor_id: ActorId,
    },
}

/// Which scanning mechanism produced the label.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[repr(u8)]
pub enum MechanismType {
    PerceptualHash = 0,
    TextClassifier = 1,
    ImageClassifier = 2,
    DnsBl = 3,
    AuthVerify = 4,
    ThresholdMatch = 5,
    ZkProof = 6,
    Manual = 7,
    ExternalScanner = 8,
}

/// Identifies a specific classifier version.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct MechanismRef {
    pub mechanism_type: MechanismType,
    #[serde(with = "serde_bytes")]
    pub classifier_id: [u8; 32],
    pub version: u64,
}

/// How trustworthy is this label — what proof backs it.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub enum Attestation {
    /// Honor system — scanner signs the label.
    Signed,
    /// Hardware-attested (TEE).
    TeeAttested {
        #[serde(with = "serde_bytes")]
        report: Vec<u8>,
    },
    /// K-of-N threshold perceptual hash match.
    ThresholdRevealed {
        k: u8,
        n: u8,
        #[serde(with = "crate::byte_array::vec_of_bufs")]
        auditor_sigs: Vec<Vec<u8>>,
    },
    /// Zero-knowledge proof of classifier execution (future).
    ZkProof {
        #[serde(with = "serde_bytes")]
        proof: Vec<u8>,
    },
}

/// A label produced by scanning content.
///
/// Every scanning mechanism — DNSBL, WASM classifier, on-device scanner,
/// perceptual hash — produces `ContentLabel` records.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ContentLabel {
    pub content_ref: ContentRef,
    /// Open-ended category string: "spam", "csam", "nsfw", "auth-fail", etc.
    pub category: String,
    /// Probability 0.0–1.0 that content belongs to category.
    pub confidence: f64,
    pub mechanism: MechanismRef,
    pub attestation: Attestation,
    /// Which obligation required this scan (if any).
    pub obligation_id: Option<ActorId>,
    pub created_at: Timestamp,
    #[serde(with = "serde_bytes")]
    pub signature: Vec<u8>,
}

/// Well-known classifier IDs for built-in mechanisms.
pub fn builtin_classifier_id(name: &str) -> [u8; 32] {
    *blake3::hash(name.as_bytes()).as_bytes()
}

/// Well-known built-in classifier names.
pub const CLASSIFIER_AUTH_VERIFY: &str = "fauna.builtin.auth-verify.v1";
pub const CLASSIFIER_DNSBL: &str = "fauna.builtin.dnsbl.v1";
pub const CLASSIFIER_TEXT_HEURISTIC: &str = "fauna.builtin.text-heuristic.v1";
pub const CLASSIFIER_CLAMAV: &str = "fauna.builtin.clamav.v1";
pub const CLASSIFIER_RSPAMD: &str = "fauna.builtin.rspamd.v1";

#[cfg(test)]
mod tests {
    use super::*;

    /// Regression pin: `builtin_classifier_id` is embedded into every
    /// persisted `ContentLabel.mechanism.classifier_id` at rest, so any
    /// change to the hash function or an input name silently reassigns
    /// classifier identity for every already-scanned row on every deployed
    /// nest, with nothing else in the workspace to catch it — no test
    /// anywhere asserted the literal byte output before this. Literal hex
    /// values computed via the real function, not re-derived in the test.
    #[test]
    fn builtin_classifier_id_pins_known_values() {
        assert_eq!(
            hex::encode(builtin_classifier_id(CLASSIFIER_AUTH_VERIFY)),
            "c34971c44763289263cca6d236f1a6124c77e0be733a11ddf5ff0088f516395e"
        );
        assert_eq!(
            hex::encode(builtin_classifier_id(CLASSIFIER_DNSBL)),
            "5385df4ee4d44cf5a9f3bf6a942c2b5644a6d328182c7f38d5682fd8fc845e0c"
        );
        assert_eq!(
            hex::encode(builtin_classifier_id(CLASSIFIER_TEXT_HEURISTIC)),
            "7d202fa2923249192e7cabf19bf6d9a1109c98932c160a983a3e508650618723"
        );
        assert_eq!(
            hex::encode(builtin_classifier_id(CLASSIFIER_CLAMAV)),
            "edf1017cc3b30e6eb9255085c3baa23b9cbfabf515dedb201b7e95e181b0064c"
        );
        assert_eq!(
            hex::encode(builtin_classifier_id(CLASSIFIER_RSPAMD)),
            "dd866574731f5503a5d45cfeed96508bdd699d74431a27b49309eb6528553d18"
        );
    }

    /// A copy-pasted new constant that forgot to change its name string
    /// would silently merge two different scanning mechanisms' labels
    /// under one classifier identity — no collision test existed before.
    #[test]
    fn builtin_classifier_ids_are_pairwise_distinct() {
        let ids = [
            builtin_classifier_id(CLASSIFIER_AUTH_VERIFY),
            builtin_classifier_id(CLASSIFIER_DNSBL),
            builtin_classifier_id(CLASSIFIER_TEXT_HEURISTIC),
            builtin_classifier_id(CLASSIFIER_CLAMAV),
            builtin_classifier_id(CLASSIFIER_RSPAMD),
        ];
        for i in 0..ids.len() {
            for j in (i + 1)..ids.len() {
                assert_ne!(ids[i], ids[j], "classifier ids at {i} and {j} collide");
            }
        }
    }

    #[test]
    fn builtin_classifier_id_is_deterministic() {
        assert_eq!(
            builtin_classifier_id(CLASSIFIER_DNSBL),
            builtin_classifier_id(CLASSIFIER_DNSBL)
        );
    }
}
