//! `fauna.admin.region.*` — the deployment's **declared** region, and what the
//! region tier is currently doing with it.
//!
//! Owner docs: `docs/goal/behavior/region-blocking.md` § The region/authority
//! plumbing → Region determination (*declared, never detected*) and
//! `docs/goal/architecture/dynamic-features.md` § The region tier → Attachment
//! (*"nest-side enforcement binds the accounts a deployment hosts at the
//! deployment's admin-declared region — its legal situs, declared in admin UI,
//! nest state, never IP-derived"*).
//!
//! **The admin declares which region claims this deployment. That is the whole
//! of the admin's authority here** — the region's *policy* is the authority's,
//! published through the sanctioned channel, and region-blocking.md invariant 5
//! puts it explicitly out of the admin's reach. There is deliberately no kind
//! that submits, edits, or overrides a policy artifact: a submit surface would
//! be an admin lever over the content of region policy even though the signature
//! binds it to the authority.
//!
//! **Withdrawal is a first-class operation** (`region: None`), because a
//! deployment can move its legal situs or discover it declared the wrong one.
//! Withdrawing drops the tier entirely — the deployment returns to *"no region
//! claims it"*, which § Fail posture ratifies as running at tier-1 constants,
//! and is **not** the same as an authority that allows everything.

use std::collections::BTreeMap;

use fauna_core::region_authority::{InclusionEvidence, PolicyArtifact, RegionCode};
use serde::{Deserialize, Serialize};

use crate::Value;

/// The active region document's identity and version — the field § Transparency
/// & auditability asks the transparency read to carry.
///
/// `authority_name` is read from the **curated registry**, never from the
/// artifact: an authority does not get to name itself on a screen that tells a
/// user who is restricting them.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct RegionDocumentRef {
    pub region: RegionCode,
    pub authority_name: String,
    /// Which of the region's enrolled keys signed the document in force.
    pub key_id: String,
    /// The envelope's monotonic sequence — the document's *version*.
    pub sequence: u64,
    /// When the authority issued it, seconds since the Unix epoch.
    pub issued_at: u64,
    /// Forward-compat catch-all: unknown keys from a newer peer are
    /// preserved here and re-emitted on encode (transport.md § Schema
    /// and forward-compat discipline, rule 4).
    #[serde(flatten, default)]
    pub extra: BTreeMap<String, Value>,
}

/// `fauna.admin.region.set` — declare, re-declare, or withdraw.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, Default)]
pub struct AdminRegionSetRequest {
    /// The region to declare, or absent/null to **withdraw** the declaration.
    ///
    /// A whole-value replace, like the feature-policy update kinds: there is no
    /// third "leave it unchanged" state, so no `Option<Option<T>>` — the shape
    /// that does not round-trip on DAG-CBOR.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub region: Option<RegionCode>,
    /// Forward-compat catch-all: unknown keys from a newer peer are
    /// preserved here and re-emitted on encode (transport.md § Schema
    /// and forward-compat discipline, rule 4).
    #[serde(flatten, default)]
    pub extra: BTreeMap<String, Value>,
}

/// `fauna.admin.region.get` — no parameters; the answer is about this
/// deployment.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, Default)]
pub struct AdminRegionStatusRequest {
    /// Forward-compat catch-all: unknown keys from a newer peer are
    /// preserved here and re-emitted on encode (transport.md § Schema
    /// and forward-compat discipline, rule 4).
    #[serde(flatten, default)]
    pub extra: BTreeMap<String, Value>,
}

/// The region tier's state, as the admin screen shows it.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, Default)]
pub struct AdminRegionStatusReply {
    /// What the admin declared, absent if they never have. Absent is the
    /// ratified fresh-install state, not an error.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub declared: Option<RegionCode>,
    /// A declaration row exists but its text cannot be read — corruption, not
    /// absence. `declared` is then absent (an unreadable situs enforces as "no
    /// declaration" — `dynamic-features.md` § Fail posture), while
    /// `feature_policy` may still name the last-accepted document, whose folded
    /// bounds keep binding. Re-declaring or withdrawing from this same screen is
    /// the in-app recovery (a whole-value replace clears the corrupt row).
    #[serde(default)]
    pub declaration_unreadable: bool,
    /// Whether the declared region is one the compiled-in registry enrols. A
    /// declaration naming a region with no enrolled authority is **allowed and
    /// inert** — a legal situs is a fact about the deployment, and most of the
    /// world has no Fauna-enrolled authority. The screen says so rather than
    /// refusing the declaration.
    #[serde(default)]
    pub enrolled: bool,
    /// The feature-policy document in force, if one has ever been accepted.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub feature_policy: Option<RegionDocumentRef>,
    /// Unix seconds of the last time the authority channel **answered** — an
    /// accepted or a refused artifact (a refusal is a reached channel; a
    /// failed fetch is not). Absent until the channel has been reached,
    /// matching the relay's field of the same name
    /// (`RegionArtifactGetReply::last_checked_at`, below).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_checked_at: Option<u64>,
    /// Why the last attempt failed, absent when it succeeded or none has run.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_error: Option<String>,
    /// Whether the channel has gone unreached for longer than the refresh
    /// cadence allows.
    ///
    /// § Fail posture is explicit that this is *"a warning, not an outage"*: a
    /// stale nest keeps enforcing the last-known-good document — it never
    /// relaxes — and this flag exists so the admin can act, not so anything
    /// stops working.
    #[serde(default)]
    pub stale: bool,
    /// Forward-compat catch-all: unknown keys from a newer peer are
    /// preserved here and re-emitted on encode (transport.md § Schema
    /// and forward-compat discipline, rule 4).
    #[serde(flatten, default)]
    pub extra: BTreeMap<String, Value>,
}

/// Reply to `fauna.admin.region.set`.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, Default)]
pub struct AdminRegionSetReply {
    pub ok: bool,
    /// Forward-compat catch-all: unknown keys from a newer peer are
    /// preserved here and re-emitted on encode (transport.md § Schema
    /// and forward-compat discipline, rule 4).
    #[serde(flatten, default)]
    pub extra: BTreeMap<String, Value>,
}

// ── fauna.region.artifact.get — the app's relay ────────────────────────

/// An **app** asks its own nest for one region's published artifact of one
/// payload kind (`region-blocking.md` § The content plane → *How an app obtains
/// its region's policy*).
///
/// The nest is a **relay, not a trust point**: it answers from its per-(region,
/// kind) cache, refilled from the transparency log on the region tier's own
/// cadence, and returns the envelope exactly as verified — the app verifies the
/// signature (and, when served, inclusion) itself, in shared Rust. The asked
/// region only: resolving a region's parent chain is the app's, which holds the
/// same registry.
///
/// There is deliberately **no kind that submits an artifact** and no admin
/// lever over this one — the ingress is the compiled-in log URL
/// (region-blocking.md invariant 5; `region_tier.rs` rule 1).
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct RegionArtifactGetRequest {
    /// The region the app declares (or a region on its declared chain).
    pub region: RegionCode,
    /// Which plane's artifact — `fauna_core::region_authority::PAYLOAD_KIND_*`.
    pub payload_kind: String,
    #[serde(flatten, default)]
    pub extra: BTreeMap<String, Value>,
}

/// The relay's answer.
///
/// `envelope: None` is **"no document"** — the fresh-subject arm of § Fail
/// posture, not an error: nothing is enrolled for the region, or this nest has
/// not reached the log for it yet. Either way the app runs on its own
/// last-known-good, or on no regional policy at all if it has never held one.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, Default)]
pub struct RegionArtifactGetReply {
    /// The artifact as the nest verified and stored it. Its payload is opaque
    /// here — the nest never decodes the document inside, so an app newer than
    /// its nest can read a document version the nest's build does not know.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub envelope: Option<PolicyArtifact>,
    /// The inclusion evidence the log served beside the envelope, when it
    /// served any (`None` in the pre-log era).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub evidence: Option<InclusionEvidence>,
    /// Unix seconds of the last time the log **answered** this nest for the
    /// pair — an accepted or a refused artifact. `None` until it first has.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_checked_at: Option<u64>,
    /// The nest has been trying and the log has not answered for longer than
    /// the region tier's `STALE_AFTER`. A warning, never an outage: the
    /// envelope above is still the last one verified.
    #[serde(default)]
    pub stale: bool,
    #[serde(flatten, default)]
    pub extra: BTreeMap<String, Value>,
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{decode_strict, encode_canonical};

    #[test]
    fn a_declaration_and_a_withdrawal_round_trip() {
        let declare = AdminRegionSetRequest {
            region: Some(RegionCode::parse("NO").unwrap()),
            extra: Default::default(),
        };
        let bytes = encode_canonical(&declare).expect("encode");
        assert_eq!(
            decode_strict::<AdminRegionSetRequest>(&bytes).expect("decode"),
            declare
        );

        // Withdrawal is the absent case, not a sentinel value.
        let withdraw = AdminRegionSetRequest::default();
        let bytes = encode_canonical(&withdraw).expect("encode");
        assert_eq!(
            decode_strict::<AdminRegionSetRequest>(&bytes).expect("decode"),
            withdraw
        );
    }

    /// A hostile region code must not survive the wire — the validation is on
    /// `RegionCode`'s own decode, and this is what pins that it reaches this
    /// request shape.
    #[test]
    fn a_malformed_region_is_refused_at_decode() {
        #[derive(Serialize)]
        struct Loose {
            region: String,
        }
        let bytes = encode_canonical(&Loose {
            region: "not a region".into(),
        })
        .expect("encode");
        assert!(decode_strict::<AdminRegionSetRequest>(&bytes).is_err());
    }

    #[test]
    fn the_status_reply_round_trips_with_a_document_in_force() {
        let reply = AdminRegionStatusReply {
            declared: Some(RegionCode::parse("NO").unwrap()),
            enrolled: true,
            feature_policy: Some(RegionDocumentRef {
                region: RegionCode::parse("NO").unwrap(),
                authority_name: "Test Authority".into(),
                key_id: "k1".into(),
                sequence: 12,
                issued_at: 1_800_000_000,
                extra: Default::default(),
            }),
            last_checked_at: Some(1_800_000_100),
            ..Default::default()
        };
        let bytes = encode_canonical(&reply).expect("encode");
        assert_eq!(
            decode_strict::<AdminRegionStatusReply>(&bytes).expect("decode"),
            reply
        );
    }

    /// The fresh-install answer: nothing declared, nothing in force, not stale.
    #[test]
    fn the_undeclared_reply_round_trips() {
        let reply = AdminRegionStatusReply::default();
        let bytes = encode_canonical(&reply).expect("encode");
        assert_eq!(
            decode_strict::<AdminRegionStatusReply>(&bytes).expect("decode"),
            reply
        );
    }
}
