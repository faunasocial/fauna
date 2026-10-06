//! Test fixtures — a synthetic registry and signed content policies, never a
//! real region (`region-blocking.md` § What the build owes in tests). Compiled
//! only for this crate's tests and a downstream test build that turns on
//! `test-fixtures`.

use std::collections::BTreeMap;

use ed25519_dalek::SigningKey;
use fauna_core::region_authority::{
    AuthorityKey, PAYLOAD_KIND_CONTENT_POLICY, PolicyArtifact, RegionCode, RegionEntry,
    RegionRegistry, sign_artifact,
};
use fauna_core::region_policy::{
    BundledScorer, ContentPolicyDocument, ContentRule, ContentVerdict, GRAMMAR_VERSION,
    REASON_DEFAULT_KEY, ScorerKind, scorer_factor,
};
use fauna_protocol::region::RegionArtifactGetReply;

use crate::{DeclaredRegion, RegionPlane, RegionSource};

/// The synthetic region every fixture uses.
pub const REGION: &str = "XZ";
/// The synthetic authority's registry name.
pub const AUTHORITY: &str = "Synthetic Authority";
const SEED: u8 = 42;

pub fn region() -> RegionCode {
    RegionCode::parse(REGION).unwrap()
}

fn key() -> SigningKey {
    SigningKey::from_bytes(&[SEED; 32])
}

/// A registry enrolling the synthetic authority for [`REGION`].
pub fn registry() -> RegionRegistry {
    RegionRegistry {
        version: 1,
        regions: vec![RegionEntry {
            region: region(),
            authority_name: AUTHORITY.into(),
            official_domain: "authority.invalid".into(),
            parent: None,
            keys: vec![AuthorityKey {
                key_id: "k1".into(),
                public_key: key().verifying_key().to_bytes().to_vec(),
                enrolled_at: 0,
                retired_at: None,
            }],
        }],
    }
}

/// One rule firing at 500‰ on `factor`, with `reason` as its default text.
pub fn rule(factor: &str, verdict: ContentVerdict, reason: &str) -> ContentRule {
    ContentRule {
        factor: factor.into(),
        min_permille: 500,
        verdict,
        reason_code: "SA-1".into(),
        reason: BTreeMap::from([(REASON_DEFAULT_KEY.to_string(), reason.to_string())]),
        extra: Default::default(),
    }
}

/// A rule on a bundled `list` scorer naming exactly `ids` — the returned
/// scorer goes in the document beside it.
pub fn listed_rule(
    name: &str,
    ids: &[[u8; 32]],
    verdict: ContentVerdict,
    reason: &str,
) -> (ContentRule, BundledScorer) {
    let mut entries: Vec<([u8; 32], i64)> = ids.iter().map(|id| (*id, 1000)).collect();
    entries.sort_by_key(|a| a.0);
    let scorer = BundledScorer {
        name: name.into(),
        kind: ScorerKind::List,
        bytes: fauna_core::scoring::build_list_artifact(None, entries).unwrap(),
        extra: Default::default(),
    };
    (
        rule(&scorer_factor(&region(), name), verdict, reason),
        scorer,
    )
}

/// A document at this build's grammar version.
pub fn document(rules: Vec<ContentRule>, scorers: Vec<BundledScorer>) -> ContentPolicyDocument {
    ContentPolicyDocument {
        version: GRAMMAR_VERSION,
        rules,
        scorers,
        extra: Default::default(),
    }
}

/// `doc` signed by the synthetic authority at `sequence`.
pub fn envelope(sequence: u64, issued_at: u64, doc: &ContentPolicyDocument) -> PolicyArtifact {
    sign_artifact(
        PolicyArtifact {
            region: region(),
            key_id: "k1".into(),
            sequence,
            issued_at,
            payload_kind: PAYLOAD_KIND_CONTENT_POLICY.to_string(),
            payload: fauna_protocol::encode_canonical(doc).unwrap().to_vec(),
            sig: Vec::new(),
        },
        &key(),
    )
    .unwrap()
}

/// A plane declared in [`REGION`] holding `doc` — what a device looks like
/// after one successful relay fetch.
pub fn plane_holding(doc: &ContentPolicyDocument, now: u64) -> RegionPlane {
    let mut plane = RegionPlane::new(
        Some(DeclaredRegion {
            code: region(),
            source: RegionSource::SystemLocale,
        }),
        registry(),
    );
    plane.apply_reply(
        &region(),
        RegionArtifactGetReply {
            envelope: Some(envelope(1, now.saturating_sub(60), doc)),
            last_checked_at: Some(now),
            ..Default::default()
        },
        now,
    );
    plane
}
