//! `fauna.features.*` — the controversial-class feature plane's transparency
//! read (`docs/goal/architecture/dynamic-features.md` § Transparency &
//! auditability, W2 (account-data-plane.md § Workstreams) slice 3).
//!
//! Boundary 4 of § What this is NOT is the reason this family exists: *"No
//! silent gates. Every active restriction is visible to the person it binds —
//! which feature, what limit, which tier set it."* An account asks about
//! **itself**; there is no parameter naming another account, because there is no
//! such read.
//!
//! **The wire carries the shared types, not twins of them.** `EffectivePolicy`,
//! `UsageCounters` and `GatedFeature` live in `fauna_core::feature_gate` and are
//! what the nest evaluates and what a client's courtesy layer renders — so a
//! parallel wire shape would be two definitions of one thing, free to drift
//! (priority #2). `EffectivePolicy` already carries the attribution this read
//! exists to deliver: a `BoundSource` per (dimension, window) cell naming the
//! tier that set it, plus `denied_by` for an outright denial.
//!
//! **Remaining quota is `limit − observed`, computed by the reader.** The nest
//! sends both halves rather than a pre-subtracted remainder: a remainder alone
//! cannot say *what the bound is*, and § Transparency asks for the limit, the
//! remainder **and** the binding tier. `UsageCounters` is the observed half over
//! the same trailing windows the gate evaluated.
//!
//! **The active region document's identity + version arrived with slice 4**, as
//! the additive `region` field promised — absent while no region claims the
//! deployment (§ Fail posture's ratified fresh-subject state), present and
//! naming the authority once an artifact is in force.

use std::collections::BTreeMap;

use fauna_core::feature_gate::{EffectivePolicy, FeaturePolicy, GatedFeature, UsageCounters};
use serde::{Deserialize, Serialize};

use crate::Value;

/// The guardian tier's `features` sub-document — one authored [`FeaturePolicy`]
/// per gated feature, keyed by the feature's **stable key**
/// ([`GatedFeature::as_str`]).
///
/// § Wire & data shape rules that the guardian tier *"deliberately mints no new
/// kind"*: it rides `fauna.family.policy.update` as an additive sub-document, and
/// its documents live in `guardian_policies` rather than `feature_policies`. This
/// is that sub-document's content grammar — the carriage is family-safety.md's.
///
/// **Keyed by `String`, not by `GatedFeature`, deliberately.** A DAG-CBOR map key
/// must be a string, and more importantly a guardian document written by a *newer*
/// nest may name a feature this build has never heard of. Keying by the stable
/// string lets such an entry round-trip untouched (additive-everywhere) while
/// [`GatedFeature::from_key`] simply declines to resolve it — an unknown feature
/// is not a fault, and dropping it would silently discard a restriction the
/// guardian set.
pub type GuardianFeaturePolicies = BTreeMap<String, FeaturePolicy>;

/// The guardian's authored policy for one feature, if they set one.
///
/// The lookup is by stable key rather than by iterating, so an entry naming a
/// feature this build does not know is skipped without ever being parsed as one.
pub fn guardian_policy_for(
    documents: &GuardianFeaturePolicies,
    feature: GatedFeature,
) -> Option<&FeaturePolicy> {
    documents.get(feature.as_str())
}

/// `fauna.features.status` — the caller's own effective feature policy.
///
/// No parameters: the answer is about the bearer, and covers every registry
/// member, because "which features am I limited on" is not a question you can
/// ask one feature at a time without already knowing the answer.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Default)]
pub struct FeaturesStatusRequest {
    /// Forward-compat catch-all: unknown keys from a newer peer are
    /// preserved here and re-emitted on encode (transport.md § Schema
    /// and forward-compat discipline, rule 4).
    #[serde(flatten, default)]
    pub extra: BTreeMap<String, Value>,
}

/// One registry member's status for the calling account.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct FeatureStatusItem {
    pub feature: GatedFeature,
    /// The meet of every tier that spoke, with tier 1's constants folded in and
    /// **every surviving bound carrying the tier that set it** — the
    /// attribution boundary 4 requires.
    pub policy: EffectivePolicy,
    /// What this account has already spent, over the same trailing windows the
    /// gate evaluates. Remaining is `limit − observed` per cell.
    pub usage: UsageCounters,
    /// Forward-compat catch-all: unknown keys from a newer peer are
    /// preserved here and re-emitted on encode (transport.md § Schema
    /// and forward-compat discipline, rule 4).
    #[serde(flatten, default)]
    pub extra: BTreeMap<String, Value>,
}

/// `fauna.features.policy.update` (admin tier) and
/// `fauna.features.self_limits.update` (self tier) — write one tier's authored
/// document for one feature.
///
/// **One type for both kinds, deliberately.** They differ in exactly one thing —
/// which tier the write lands at — and that is implied by the kind, never sent.
/// A tier field on the wire would be a second, forgeable answer to a question the
/// dispatch already answers: a self-limits caller could name the admin tier.
///
/// **`policy: None` clears this tier's document**, which is the *"no opinion at
/// this tier"* state and is **not** the same as an authored `allow` — an absent
/// tier drops out of the meet entirely, while an authored allow is a tier
/// actively declining to restrict. The request is a whole-document replace, so
/// there is no third "leave it unchanged" state to represent and therefore no
/// `Option<Option<T>>` (the shape that does not round-trip on DAG-CBOR, and the
/// trap the `tiers.update` clear-verb finding records).
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct FeaturePolicyUpdateRequest {
    pub feature: GatedFeature,
    /// The document to author, or absent/null to clear this tier's opinion.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub policy: Option<FeaturePolicy>,
    /// Forward-compat catch-all: unknown keys from a newer peer are
    /// preserved here and re-emitted on encode (transport.md § Schema
    /// and forward-compat discipline, rule 4).
    #[serde(flatten, default)]
    pub extra: BTreeMap<String, Value>,
}

/// Reply to both policy-update kinds.
///
/// **Deliberately does not echo the new effective policy.** Composing a meet is
/// the one thing this plane keeps to a single site (`resolve_effective_policy`
/// nest-side), because the subset edge fires only when the caller passes the
/// superset's documents and an empty slice legitimately means "the superset
/// allows" — so every additional place that assembles one is a place that can
/// silently omit it. A writer that wants to see the result reads
/// `fauna.features.status`, which is the surface built to answer exactly that,
/// through exactly that resolver.
///
/// For the admin tier the echo would be ill-defined anyway: an admin document is
/// nest-wide, so "the new effective policy" is a per-account question the write
/// does not name an account for.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Default)]
pub struct FeaturePolicyUpdateReply {
    pub ok: bool,
    /// Forward-compat catch-all: unknown keys from a newer peer are
    /// preserved here and re-emitted on encode (transport.md § Schema
    /// and forward-compat discipline, rule 4).
    #[serde(flatten, default)]
    pub extra: BTreeMap<String, Value>,
}

/// `fauna.features.policy.get` (admin tier) and `fauna.features.self_limits.get`
/// (self tier) — read one tier's **authored** documents back, for every
/// registry member (`dynamic-features.md` § Wire & data shape, the
/// authored-document reads).
///
/// No parameters, and — as with the writes — **no tier field**: the tier is
/// implied by the kind, so a self-limits caller cannot name the admin tier.
///
/// These exist because a save is a whole-document replace (§ Authoring
/// surfaces): `fauna.features.status` answers the *effective* meet, in which a
/// bound the tier authored that lost the MIN to a tighter tier is invisible, so
/// an editor seeded from it would silently drop that bound on its next save.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Default)]
pub struct FeaturePolicyReadRequest {
    /// Forward-compat catch-all: unknown keys from a newer peer are
    /// preserved here and re-emitted on encode (transport.md § Schema
    /// and forward-compat discipline, rule 4).
    #[serde(flatten, default)]
    pub extra: BTreeMap<String, Value>,
}

/// One registry member's authored document at the tier the kind reads.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct AuthoredPolicyItem {
    pub feature: GatedFeature,
    /// The tier's authored document, verbatim — or absent: the tier has **no
    /// opinion**, which is not the same as an authored allow.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub policy: Option<FeaturePolicy>,
    /// A stored document exists but this nest cannot decode it.
    ///
    /// ⚠ **Never collapsed into absence** (`nest/common.md` § Unreadable stored
    /// values): the gate *enforces* an unreadable document as a deny at its
    /// tier, so a reader that saw only `policy: None` would render *No limit
    /// set* over a feature that is actually off — the silent gate in its third
    /// costume. `policy` is always absent when this is set.
    #[serde(default)]
    pub unreadable: bool,
    /// The **ceiling** — the meet of only the tiers *outside* the one read
    /// (tiers 1–2 for the admin kind; tiers 1–4, for the bearer, for the self
    /// kind), composed nest-side by the one resolver so the subset edge rides
    /// it. What an editor renders its *"No effect: …"* notes from; the client
    /// composes no meet.
    pub ceiling: EffectivePolicy,
    /// Forward-compat catch-all: unknown keys from a newer peer are
    /// preserved here and re-emitted on encode (transport.md § Schema
    /// and forward-compat discipline, rule 4).
    #[serde(flatten, default)]
    pub extra: BTreeMap<String, Value>,
}

/// One registry member's **ceiling** on a guardian-side ward entry
/// ([`crate::family::FamilyWardInfo::features_ceiling`]) — the meet of the tiers
/// outside the guardian's (tiers 1–3), composed nest-side **for the ward**.
///
/// The guardian tier has no authored-document read of its own: the document
/// already rides the ward entry's `policy.features`, so only the half an
/// [`AuthoredPolicyItem`] carries beside it travels here
/// (`family-safety.md` § Wire & data shape — the guardian's feature-limits
/// editor seed).
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct FeatureCeilingItem {
    pub feature: GatedFeature,
    /// What an editor renders its *"No effect: …"* notes from; the client
    /// composes no meet.
    pub ceiling: EffectivePolicy,
    /// Forward-compat catch-all: unknown keys from a newer peer are
    /// preserved here and re-emitted on encode (transport.md § Schema
    /// and forward-compat discipline, rule 4).
    #[serde(flatten, default)]
    pub extra: BTreeMap<String, Value>,
}

/// Reply to both authored-document reads.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Default)]
pub struct FeaturePolicyReadReply {
    /// One entry per registry member, in the registry's canonical order —
    /// always complete, for the same reason as [`FeaturesStatusReply::features`].
    pub features: Vec<AuthoredPolicyItem>,
    /// Forward-compat catch-all: unknown keys from a newer peer are
    /// preserved here and re-emitted on encode (transport.md § Schema
    /// and forward-compat discipline, rule 4).
    #[serde(flatten, default)]
    pub extra: BTreeMap<String, Value>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Default)]
pub struct FeaturesStatusReply {
    /// One entry per registry member, in the registry's canonical order. Always
    /// complete: a feature the caller is *not* limited on still appears, with
    /// its tier-1 bounds — "unrestricted" is an answer, and a client that had to
    /// infer it from an absence could not tell it from a nest that never heard
    /// of the feature.
    pub features: Vec<FeatureStatusItem>,
    /// The region document in force over this deployment, if any — the
    /// *"identity + version"* half of § Transparency & auditability.
    ///
    /// Absent means **no region claims this deployment**, which is the ratified
    /// fresh-install state and not an error. It deliberately does *not* mean
    /// "the region tier allows everything": an absent tier drops out of the meet
    /// entirely, and a reader that conflated the two would attribute a
    /// tier-1 bound to an authority that has never spoken.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub region: Option<crate::region::RegionDocumentRef>,
    /// Forward-compat catch-all: unknown keys from a newer peer are
    /// preserved here and re-emitted on encode (transport.md § Schema
    /// and forward-compat discipline, rule 4).
    #[serde(flatten, default)]
    pub extra: BTreeMap<String, Value>,
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{decode_strict, encode_canonical};
    use fauna_core::feature_gate::{RuleTier, effective_policy, entry};

    /// The wire shape must survive a DAG-CBOR round trip — the attribution is
    /// the payload, and an `Option<BoundSource>` per window cell is exactly the
    /// shape that would break if anything about it were float- or
    /// nested-Option-flavoured.
    #[test]
    fn status_round_trips_through_dag_cbor() {
        let policy = effective_policy(GatedFeature::P2pShare, &[], &[]);
        let reply = FeaturesStatusReply {
            features: vec![FeatureStatusItem {
                feature: GatedFeature::P2pShare,
                policy,
                usage: UsageCounters::default(),
                extra: Default::default(),
            }],
            region: None,
            extra: Default::default(),
        };

        let bytes = encode_canonical(&reply).expect("encode");
        let back: FeaturesStatusReply = decode_strict(&bytes).expect("decode");
        assert_eq!(back, reply);

        // The attribution specifically — a round trip that dropped the tier
        // would still compare equal on the numbers alone if the tier defaulted.
        let cell = back.features[0]
            .policy
            .bounds(fauna_core::feature_gate::QuotaDimension::Counterparties)
            .get(fauna_core::feature_gate::Window::Week)
            .expect("p2p-share bounds counterparties per week at tier 1");
        assert_eq!(cell.tier, RuleTier::Structural);
        assert_eq!(
            cell.limit,
            entry(GatedFeature::P2pShare)
                .tier1
                .counterparties
                .per_week
                .unwrap()
        );
    }

    /// Absent and unreadable must survive the wire as two distinct states.
    #[test]
    fn an_authored_read_round_trips_absent_authored_and_unreadable() {
        let ceiling = effective_policy(GatedFeature::Payments, &[], &[]);
        let item = |policy, unreadable| AuthoredPolicyItem {
            feature: GatedFeature::Payments,
            policy,
            unreadable,
            ceiling,
            extra: Default::default(),
        };
        let reply = FeaturePolicyReadReply {
            features: vec![
                item(None, false),
                item(Some(FeaturePolicy::DENIED), false),
                item(None, true),
            ],
            extra: Default::default(),
        };
        let bytes = encode_canonical(&reply).expect("encode");
        let back: FeaturePolicyReadReply = decode_strict(&bytes).expect("decode");
        assert_eq!(back, reply);
    }

    /// A newer nest's registry member, as an older app meets it.
    ///
    /// The nest lists every member in every status reply, so before the open
    /// arm one new feature failed the older app's whole reply
    /// (`transport.md` § Rule 3 in full). The newer writer is modelled by
    /// re-keying one item of a real reply — its `feature` and its policy's —
    /// to a key this build has never heard of, plus a tier it cannot name.
    #[test]
    fn a_status_reply_naming_an_unknown_feature_still_decodes() {
        let reply = FeaturesStatusReply {
            features: vec![
                FeatureStatusItem {
                    feature: GatedFeature::Payments,
                    policy: effective_policy(GatedFeature::Payments, &[], &[]),
                    usage: UsageCounters::default(),
                    extra: Default::default(),
                },
                FeatureStatusItem {
                    feature: GatedFeature::P2pShare,
                    policy: effective_policy(GatedFeature::P2pShare, &[], &[]),
                    usage: UsageCounters::default(),
                    extra: Default::default(),
                },
            ],
            region: None,
            extra: Default::default(),
        };
        let mut tree: Value = decode_strict(&encode_canonical(&reply).unwrap()).unwrap();
        fn rekey(v: &mut Value) {
            match v {
                Value::String(s) if s == "p2p-share" => *s = "dowsing".into(),
                Value::String(s) if s == "structural" => *s = "council".into(),
                Value::List(items) => items.iter_mut().for_each(rekey),
                Value::Map(map) => map.values_mut().for_each(rekey),
                _ => {}
            }
        }
        let Value::Map(top) = &mut tree else {
            panic!("a reply is a map")
        };
        let Some(Value::List(items)) = top.get_mut("features") else {
            panic!("features is a list")
        };
        rekey(&mut items[1]);
        let newer = encode_canonical(&tree).unwrap();

        let back: FeaturesStatusReply = decode_strict(&newer).expect("the whole reply decodes");
        assert_eq!(
            back.features[0], reply.features[0],
            "the known member is untouched"
        );
        assert_eq!(back.features[1].feature, GatedFeature::Unknown);
        assert_eq!(back.features[1].policy.feature, GatedFeature::Unknown);
        let cell = back.features[1]
            .policy
            .bounds(fauna_core::feature_gate::QuotaDimension::Counterparties)
            .get(fauna_core::feature_gate::Window::Week)
            .expect("the newer member's bound survives");
        assert_eq!(
            cell.tier,
            RuleTier::Unknown,
            "an unnamed tier still carries its bound"
        );
    }

    #[test]
    fn an_empty_request_round_trips() {
        let bytes = encode_canonical(&FeaturesStatusRequest::default()).expect("encode");
        let back: FeaturesStatusRequest = decode_strict(&bytes).expect("decode");
        assert_eq!(back, FeaturesStatusRequest::default());
    }
}
