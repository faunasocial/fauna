//! The nest-side enforcement floor for controversial-class features
//! (`docs/goal/architecture/dynamic-features.md` § Evaluation points — W2 (account-data-plane.md § Workstreams)
//! slice 2).
//!
//! § Evaluation points states the split this module implements: *"Nest-side
//! enforcement is the floor … the gate holds against non-conforming and
//! version-skewed clients (an old client that predates gating calls the old
//! kinds and receives the typed refusal — no bypass)."* Client-side rendering is
//! the courtesy layer above it; nothing here depends on a client having asked.
//!
//! **One call per gate surface.** A handler gating an operation calls
//! [`gate`] and either proceeds or returns the [`RpcError`] it produced. The
//! verdict itself is not this module's — it is
//! `fauna_core::feature_gate::feature_verdict`, the one shared pure function
//! every surface and every app composes rather than reimplements (priority #2).
//!
//! ⚠ **The superset edge is discharged here, structurally, once.**
//! `effective_policy(zaps, …)` inherits a `payments` **deny** only if the caller
//! passes payments' authored policies as `superset_authored`; an empty slice
//! legitimately means *"the superset allows"*, so nothing inside `fauna-core`
//! can force the correct call — it is a caller obligation by construction. This
//! module is the nest's **only** caller of `effective_policy`, and
//! [`resolve_effective_policy`] always loads the superset the registry declares.
//! That is why the obligation is met by a surface that has not been written yet
//! as much as by the one below, and why the pin
//! (`zaps_are_refused_under_a_payments_deny`) tests the resolver rather than any
//! single handler.
//!
//! Note the asymmetry the edge deliberately keeps: it carries **availability
//! only, never bounds** (§ Charter members), so a `payments` *quota* must not
//! consume `zaps`' budget — which falls out of passing the superset's documents
//! only to `effective_policy`, whose subset arm reads nothing but availability.

use std::collections::BTreeMap;

use fauna_core::feature_gate::{
    Availability, EffectivePolicy, FeatureVerdict, GateOp, GatedFeature, QuotaDimension, RuleTier,
    Window, entry,
};
use fauna_protocol::features::{
    AuthoredPolicyItem, FeaturePolicyReadReply, FeaturePolicyReadRequest, FeaturePolicyUpdateReply,
    FeaturePolicyUpdateRequest, FeatureStatusItem, FeaturesStatusReply, FeaturesStatusRequest,
};
use fauna_protocol::{RpcError, Value};

use crate::db::feature_gate::StoredPolicy;
use crate::routes::AppState;
use crate::rpc_errors::internal;

/// The day bucket a gate spends into.
///
/// § Usage accounting binds this to one shared rule — the nest's own clock plus
/// the client's *clamped* UTC offset (`fauna_core::day_bucket`) — so the feature
/// plane and family safety's screen time can never disagree about which bucket
/// an instant lands in.
///
/// The gated kinds do not carry an offset on the wire today, so the gate stamps
/// at UTC, which is the degrade `day_bucket` documents for a client that reports
/// none. The consequence is bounded and worth stating: it moves only where a
/// **day** boundary falls, by at most one bucket, and the week/month windows are
/// trailing sums over 7 and 30 buckets, so they are unaffected in substance.
/// When a gated kind does carry an offset, pass it through here rather than
/// re-deriving a second day rule.
fn today(utc_offset_minutes: i32) -> i64 {
    fauna_core::day_bucket::local_day_bucket(crate::db::now_epoch_secs(), utc_offset_minutes)
}

/// Which tiers a composed meet takes in.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TierScope {
    /// Every applicable tier — the meet the gate enforces and
    /// `fauna.features.status` reports.
    All,
    /// Only the tiers strictly *outside* this one (tier numbers below it) — the
    /// **ceiling** an authored-document read carries for the tier it reads
    /// (`dynamic-features.md` § Authoring surfaces, *Tighten-only is rendered*).
    OutsideOf(RuleTier),
}

impl TierScope {
    fn includes(self, tier: RuleTier) -> bool {
        match self {
            TierScope::All => true,
            TierScope::OutsideOf(read) => tier < read,
        }
    }
}

/// Compose the effective policy for one (account, feature) pair — the meet of
/// every tier in `scope` that authored a document, with tier 1's constants
/// folded in by the shared function.
///
/// The **only** place in the nest that calls `effective_policy`; see the module
/// note on why that matters for the subset edge. A ceiling is a meet too, so it
/// is composed here with a narrower `scope` rather than beside this function —
/// which is what makes the subset edge ride it.
pub async fn resolve_effective_policy(
    state: &AppState,
    actor_id: &[u8; 32],
    feature: GatedFeature,
    scope: TierScope,
) -> anyhow::Result<EffectivePolicy> {
    let in_scope = |mut authored: Vec<(RuleTier, fauna_core::feature_gate::FeaturePolicy)>| {
        authored.retain(|(tier, _)| scope.includes(*tier));
        authored
    };
    let authored = in_scope(state.db.feature_policies_for(actor_id, feature).await?);
    // Always load the superset the registry declares. Not "when the caller
    // thinks it matters": an empty slice is indistinguishable from "the superset
    // allows", so a caller that forgets produces a *silently permissive* gate.
    // The superset's documents are cut to the same scope: a ceiling must not
    // inherit a deny from the very tier being read.
    let superset_authored = match entry(feature).superset {
        Some(superset) => in_scope(state.db.feature_policies_for(actor_id, superset).await?),
        None => Vec::new(),
    };
    Ok(fauna_core::feature_gate::effective_policy(
        feature,
        &authored,
        &superset_authored,
    ))
}

/// **The gate.** Evaluate `op` for `actor_id` and, if it is allowed, spend its
/// quota atomically (§ Usage accounting — spend-on-commit).
///
/// `Ok(())` means the caller may proceed and the operation's quota is already
/// spent. `Err` is the typed Dim-4 refusal, ready to return from the handler.
///
/// Call this **before** performing the operation. The spend is deliberately
/// fail-tight in that order — see `db::feature_gate`'s module note.
pub async fn gate(state: &AppState, actor_id: &[u8; 32], op: &GateOp) -> Result<(), RpcError> {
    let entry = entry(op.feature);
    let policy = resolve_effective_policy(state, actor_id, op.feature, TierScope::All)
        .await
        .map_err(internal)?;
    let verdict = state
        .db
        .try_spend_feature_usage(actor_id, entry, &policy, op, today(0))
        .await
        // § Fail posture: "Usage counters unavailable … → the gate fails closed
        // for the operation, never open." An unreadable counter is an internal
        // error, and an internal error refuses the operation.
        .map_err(internal)?;
    match verdict {
        FeatureVerdict::Allow => Ok(()),
        other => Err(refusal(op, &other)),
    }
}

/// `fauna.features.status` — the transparency read (§ Transparency &
/// auditability, boundary 4: *"there is no restriction you cannot see"*).
///
/// Answers for the **bearer**, about **every** registry member, including the
/// ones they are not limited on: "unrestricted" is an answer, and a client that
/// had to infer it from an absence could not tell it from a nest that never
/// heard of the feature.
///
/// ⚠ **It reads through [`resolve_effective_policy`], and that is the whole
/// point of that function having exactly one caller.** A status read that
/// composed its own meet would omit the subset edge and cheerfully report `zaps`
/// as available under a `payments` deny the gate actually refuses — a *silent
/// gate* wearing the opposite costume, which boundary 4 forbids just as much as
/// an unexplained refusal. Pinned by `status_shows_zaps_denied_when_payments_is_denied`.
pub fn register_features_handlers(b: &mut crate::rpc_router::RpcRouterBuilder) {
    b.add(
        "fauna.features.status",
        crate::rpc_router::RpcKindMeta {
            // A pure read: it writes no policy, spends no quota, and touches no
            // bucket. Mirror any change in `KindRegistry::register_features_kinds`.
            forbid_replay: false,
            default_deadline: std::time::Duration::from_secs(5),
            handler: status_handler(),
        },
    );
    // The write half (§ Wire & data shape). Both are idempotent whole-document
    // replaces at one (tier, subject, feature) key, so `forbid_replay: false`;
    // mirror any change in `KindRegistry::register_features_kinds`.
    b.add(
        KIND_POLICY_UPDATE,
        crate::rpc_router::RpcKindMeta {
            forbid_replay: false,
            default_deadline: std::time::Duration::from_secs(5),
            handler: policy_update_handler(KIND_POLICY_UPDATE, RuleTier::Admin),
        },
    );
    b.add(
        KIND_SELF_LIMITS_UPDATE,
        crate::rpc_router::RpcKindMeta {
            forbid_replay: false,
            default_deadline: std::time::Duration::from_secs(5),
            handler: policy_update_handler(KIND_SELF_LIMITS_UPDATE, RuleTier::SelfImposed),
        },
    );
    // The authored-document reads (§ Wire & data shape). Pure reads, so
    // `forbid_replay: false`; mirror any change in
    // `KindRegistry::register_features_kinds`.
    b.add(
        KIND_POLICY_GET,
        crate::rpc_router::RpcKindMeta {
            forbid_replay: false,
            default_deadline: std::time::Duration::from_secs(5),
            handler: authored_read_handler(KIND_POLICY_GET, RuleTier::Admin),
        },
    );
    b.add(
        KIND_SELF_LIMITS_GET,
        crate::rpc_router::RpcKindMeta {
            forbid_replay: false,
            default_deadline: std::time::Duration::from_secs(5),
            handler: authored_read_handler(KIND_SELF_LIMITS_GET, RuleTier::SelfImposed),
        },
    );
}

/// The two policy-update kinds. Named constants because each is used **twice** —
/// at registration and as the subject of that handler's permission check — and
/// those two uses must never drift apart (see [`policy_update_handler`]).
pub const KIND_POLICY_UPDATE: &str = "fauna.features.policy.update";
pub const KIND_SELF_LIMITS_UPDATE: &str = "fauna.features.self_limits.update";
/// The two authored-document reads — named constants for the same reason as the
/// writes: registration and the permission subject must never drift apart.
pub const KIND_POLICY_GET: &str = "fauna.features.policy.get";
pub const KIND_SELF_LIMITS_GET: &str = "fauna.features.self_limits.get";

/// The audit `action` for a policy write, per tier (§ Transparency &
/// auditability: *"every policy write (admin, guardian, self) writes an audit
/// row"*, on the `family:*` precedent).
///
/// The guardian tier is absent because its write is not this handler's: it rides
/// `fauna.family.policy.update` and audits on that kind's own row, which is the
/// same reason it mints no kind here.
fn audit_action(tier: RuleTier) -> &'static str {
    match tier {
        RuleTier::Admin => "features:policy.update",
        RuleTier::SelfImposed => "features:self_limits.update",
        // Unreachable by construction — `register_features_handlers` builds
        // exactly the two above — but a total match keeps a future tier from
        // silently inheriting one of their audit names.
        _ => "features:policy.update.unknown_tier",
    }
}

/// Both policy-update kinds, parameterised by the tier the kind implies.
///
/// **The tier is never on the wire.** It is baked in here at registration, so a
/// self-limits caller cannot name the admin tier — the dispatch is the
/// authorization, and a tier field would be a second, forgeable answer to a
/// question it already settles.
///
/// ⚠ **`kind` is passed in rather than derived from `tier`, and that is a
/// security property, not tidiness.** The first draft reconstructed it
/// (`match tier { Admin => "…policy.update", _ => "…self_limits.update" }`),
/// which made the *authorization subject* a function of the tier constant — so
/// registering this handler with the wrong tier silently moved the permission
/// check to the other kind's class too, and an ordinary user could write the
/// nest-wide admin document. Not hypothetical: mutating the tier at the
/// registration site above produced exactly that, and only the refusal pin
/// caught it. Declaring the (kind, tier) pair once at the registration site is
/// what keeps the check against the kind the caller actually dispatched.
///
/// ⚠ **No "may this tier relax past another?" check, deliberately.** Every tier
/// can only tighten, so `dynamic-features.md:111` makes relaxing
/// **unrepresentable rather than forbidden-by-rule**: a permissive document
/// simply loses the MIN. A validation rule here would be a second answer to what
/// the composition already answers, free to drift from it.
fn policy_update_handler(kind: &'static str, tier: RuleTier) -> crate::rpc_router::RpcHandler {
    Box::new(move |state, actor_id, payload| {
        Box::pin(async move {
            // The admin kind is admin-class; the self kind is User-class and
            // binds only the caller. `is_permitted` grants Admin ⊇ User, so an
            // admin may also set their own self-limits.
            crate::bridge_method_allowlist::require_permission(
                &state.db, &actor_id, kind, internal,
            )
            .await?;

            let req: FeaturePolicyUpdateRequest = fauna_protocol::decode_strict(&payload)
                .map_err(|e| crate::rpc_errors::malformed_ns("features", e))?;

            // The open arms (`transport.md` § Rule 3 in full) let a newer
            // writer's request decode; this nest still authors nothing it
            // cannot evaluate. A feature it does not know, or an availability
            // it cannot read, is refused typed — for this one request.
            if !req.feature.is_known() {
                return Err(crate::rpc_errors::invalid_params_ns(
                    "features",
                    "this nest does not know that feature".to_string(),
                ));
            }
            if let Some(Availability::Other(value)) = req.policy.as_ref().map(|p| &p.availability) {
                return Err(crate::rpc_errors::invalid_params_ns(
                    "features",
                    format!("this nest cannot read the availability `{value}`"),
                ));
            }

            match &req.policy {
                Some(policy) => {
                    // § The quota grammar: a bound on a dimension the registry
                    // does not declare for this feature is unrepresentable, so
                    // it is refused at the door rather than stored and ignored.
                    policy.validate(entry(req.feature)).map_err(|e| {
                        crate::rpc_errors::invalid_params_ns("features", e.to_string())
                    })?;
                    state
                        .db
                        .put_feature_policy(tier, &actor_id, req.feature, policy)
                        .await
                        .map_err(internal)?;
                }
                // Absent = clear this tier's opinion. Idempotent: clearing a
                // document that is already absent is a no-op, not a not_found —
                // the caller's intent ("this tier says nothing") holds either way.
                None => {
                    state
                        .db
                        .clear_feature_policy(tier, &actor_id, req.feature)
                        .await
                        .map_err(internal)?;
                }
            }

            let _ = state
                .db
                .audit(
                    Some(&actor_id[..]),
                    audit_action(tier),
                    Some(req.feature.as_str()),
                    // Names the feature and whether a document was authored or
                    // cleared, never the bounds themselves: the row records that
                    // a write happened, and the bounds are readable by the person
                    // they bind through `fauna.features.status`.
                    Some(match &req.policy {
                        Some(_) => "authored",
                        None => "cleared",
                    }),
                )
                .await;

            crate::rpc_errors::encode_reply(&FeaturePolicyUpdateReply {
                ok: true,
                extra: Default::default(),
            })
        })
    })
}

/// Both authored-document reads, parameterised by the tier the kind implies —
/// the read twin of [`policy_update_handler`], with `kind` passed in for the
/// same security reason.
///
/// Per registry member it returns the tier's stored document **as stored**
/// (absent / authored / unreadable — never the enforcement fold, which turns an
/// unreadable document into a deny) and the ceiling: the meet of only the tiers
/// outside `tier`, composed by [`resolve_effective_policy`] so the subset edge
/// rides it. It never answers the effective meet under a new name — that is
/// `fauna.features.status`.
fn authored_read_handler(kind: &'static str, tier: RuleTier) -> crate::rpc_router::RpcHandler {
    Box::new(move |state, actor_id, payload| {
        Box::pin(async move {
            crate::bridge_method_allowlist::require_permission(
                &state.db, &actor_id, kind, internal,
            )
            .await?;

            let _req: FeaturePolicyReadRequest = fauna_protocol::decode_strict(&payload)
                .map_err(|e| crate::rpc_errors::malformed_ns("features", e))?;

            let mut features = Vec::with_capacity(fauna_core::feature_gate::registry().len());
            for entry in fauna_core::feature_gate::registry() {
                let stored = state
                    .db
                    .stored_feature_policy(tier, &actor_id, entry.feature)
                    .await
                    .map_err(internal)?;
                let (policy, unreadable) = match stored {
                    StoredPolicy::Absent => (None, false),
                    StoredPolicy::Authored(policy) => (Some(policy), false),
                    StoredPolicy::Unreadable => (None, true),
                };
                let ceiling = resolve_effective_policy(
                    &state,
                    &actor_id,
                    entry.feature,
                    TierScope::OutsideOf(tier),
                )
                .await
                .map_err(internal)?;
                features.push(AuthoredPolicyItem {
                    feature: entry.feature,
                    policy,
                    unreadable,
                    ceiling,
                    extra: Default::default(),
                });
            }

            crate::rpc_errors::encode_reply(&FeaturePolicyReadReply {
                features,
                extra: Default::default(),
            })
        })
    })
}

fn status_handler() -> crate::rpc_router::RpcHandler {
    Box::new(|state, actor_id, payload| {
        Box::pin(async move {
            let _req: FeaturesStatusRequest = fauna_protocol::decode_strict(&payload)
                .map_err(|e| crate::rpc_errors::malformed_ns("features", e))?;

            let today = today(0);
            let mut features = Vec::with_capacity(fauna_core::feature_gate::registry().len());
            for entry in fauna_core::feature_gate::registry() {
                let policy =
                    resolve_effective_policy(&state, &actor_id, entry.feature, TierScope::All)
                        .await
                        .map_err(internal)?;
                let usage = state
                    .db
                    .feature_usage_counters(&actor_id, entry.feature, today)
                    .await
                    .map_err(internal)?;
                features.push(FeatureStatusItem {
                    feature: entry.feature,
                    policy,
                    usage,
                    extra: Default::default(),
                });
            }

            // The region document in force, if any (§ Transparency &
            // auditability: *"the active region document's identity + version is
            // part of the transparency read"*). Absent means no region claims
            // this deployment — the ratified fresh-subject state, not an error,
            // and deliberately distinct from a region tier that allows
            // everything.
            let region = crate::region_tier::active_document(&state)
                .await
                .map_err(internal)?;

            crate::rpc_errors::encode_reply(&FeaturesStatusReply {
                features,
                region,
                extra: Default::default(),
            })
        })
    })
}

/// How often the usage buckets are pruned past the largest window.
///
/// Daily: the horizon it enforces is measured in days, so a shorter cadence
/// reclaims nothing sooner, and the rows are invisible to every user-facing
/// surface. A missed tick costs a few stale rows that no window sums, never a
/// wrong verdict.
const PRUNE_INTERVAL: std::time::Duration = std::time::Duration::from_secs(24 * 60 * 60);

/// Spawn the periodic bucket prune (§ Usage accounting — "pruned past the
/// largest window").
///
/// The horizon is account-agnostic on purpose: it drops any bucket older than
/// the largest window, whoever wrote it, so the table's size is bounded by
/// *active accounts × features × dimensions × 30* and buckets belonging to an
/// account that no longer exists age out on their own.
pub fn spawn_usage_pruner(state: std::sync::Arc<AppState>) {
    let scope = state.clone();
    scope.scope_handle(crate::sweeper::spawn_periodic_sweeper(
        PRUNE_INTERVAL,
        false,
        move || {
            let state = state.clone();
            async move {
                match state.db.prune_feature_usage(today(0)).await {
                    Ok(n) if n > 0 => {
                        tracing::info!(pruned = n, "feature-gate usage prune: dropped aged buckets")
                    }
                    Ok(_) => {}
                    Err(e) => tracing::error!("feature-gate usage prune error: {e}"),
                }
            }
        },
    ));
}

/// The stable wire code for a tier-denied feature.
pub const CODE_FEATURE_DENIED: &str = "fauna.features.denied";
/// The stable wire code for a quota-exceeded operation.
pub const CODE_FEATURE_OVER_QUOTA: &str = "fauna.features.over_quota";

/// Build the typed Dim-4 refusal (§ Evaluation points: *"a distinct `RpcError`
/// code (localized, client-actionable, naming the binding tier), never a generic
/// internal error"*).
///
/// The details carry everything a client needs to render an honest *"limited by
/// …"* surface without a second round trip — which feature, which gate surface,
/// and for a quota refusal the dimension, window, limit and observed total. They
/// carry **nothing else**: no identity, no item, no content (boundary 2).
fn refusal(op: &GateOp, verdict: &FeatureVerdict) -> RpcError {
    let mut details: BTreeMap<String, Value> = BTreeMap::new();
    details.insert(
        "feature".into(),
        Value::String(op.feature.as_str().to_string()),
    );
    details.insert("surface".into(), Value::String(op.surface.to_string()));
    if let Some(tier) = verdict.binding_tier() {
        details.insert("tier".into(), Value::String(tier_label(tier).to_string()));
    }

    let code = match verdict {
        FeatureVerdict::Allow => {
            // Unreachable: `gate` returns before calling this on an allow. Still
            // typed rather than panicking — this stands between an operation and
            // its limits, and the safe answer for an impossible state is refusal.
            CODE_FEATURE_DENIED
        }
        FeatureVerdict::Deny { .. } => CODE_FEATURE_DENIED,
        FeatureVerdict::OverQuota {
            dimension,
            window,
            limit,
            observed,
            ..
        } => {
            details.insert(
                "dimension".into(),
                Value::String(dimension_label(*dimension).to_string()),
            );
            details.insert(
                "window".into(),
                match window {
                    Some(w) => Value::String(window_label(*w).to_string()),
                    // The per-operation magnitude cap is not windowed.
                    None => Value::String("per_operation".into()),
                },
            );
            details.insert("limit".into(), Value::Integer(i128::from(*limit)));
            details.insert("observed".into(), Value::Integer(i128::from(*observed)));
            CODE_FEATURE_OVER_QUOTA
        }
    };

    let message_key = match code {
        CODE_FEATURE_OVER_QUOTA => "error.features.over_quota",
        _ => "error.features.denied",
    };
    let mut error = RpcError::new(code, message_key);
    error.details = Some(Box::new(Value::Map(details)));
    error
}

/// The at-rest/wire spelling of a tier — the vocabulary a client renders
/// *"limited by your admin"* from, and therefore [`RuleTier::as_str`]'s, not
/// a second copy of it. (It was a second copy until 2026-08-23, byte-identical
/// to the client's own `fauna_client_features::row::tier_key` that reads what
/// this writes.)
fn tier_label(tier: RuleTier) -> &'static str {
    tier.as_str()
}

fn dimension_label(dimension: QuotaDimension) -> &'static str {
    dimension.as_str()
}

fn window_label(window: Window) -> &'static str {
    window.as_str()
}

#[cfg(test)]
mod tests {
    use super::*;
    use fauna_core::feature_gate::{Availability, FeaturePolicy};

    fn over_quota() -> FeatureVerdict {
        FeatureVerdict::OverQuota {
            dimension: QuotaDimension::Counterparties,
            window: Some(Window::Week),
            limit: 25,
            observed: 25,
            tier: RuleTier::Region,
        }
    }

    fn op() -> GateOp {
        GateOp {
            feature: GatedFeature::P2pShare,
            surface: fauna_core::feature_gate::SURFACE_P2P_SHARE_MEMBER_ADMIT,
            new_counterparties: 1,
            magnitude: 0,
        }
    }

    fn detail(error: &RpcError, key: &str) -> Value {
        let Some(boxed) = &error.details else {
            panic!("refusal carries no details");
        };
        let Value::Map(map) = boxed.as_ref() else {
            panic!("refusal details are not a map");
        };
        map.get(key)
            .unwrap_or_else(|| panic!("refusal details carry no {key}"))
            .clone()
    }

    /// Boundary 4 — "no silent gates": the refusal must name which feature,
    /// which bound, and **which tier set it**, or the person it binds cannot see
    /// what binds them.
    #[test]
    fn a_quota_refusal_names_the_binding_tier_and_the_bound() {
        let error = refusal(&op(), &over_quota());
        assert_eq!(error.code, CODE_FEATURE_OVER_QUOTA);
        assert_eq!(detail(&error, "feature"), Value::String("p2p-share".into()));
        assert_eq!(detail(&error, "tier"), Value::String("region".into()));
        assert_eq!(
            detail(&error, "dimension"),
            Value::String("counterparties".into())
        );
        assert_eq!(detail(&error, "window"), Value::String("week".into()));
        assert_eq!(detail(&error, "limit"), Value::Integer(25));
        assert_eq!(detail(&error, "observed"), Value::Integer(25));
    }

    #[test]
    fn a_denial_is_a_distinct_code_from_a_quota_refusal() {
        let denied = refusal(
            &op(),
            &FeatureVerdict::Deny {
                tier: RuleTier::Admin,
            },
        );
        assert_eq!(denied.code, CODE_FEATURE_DENIED);
        assert_eq!(detail(&denied, "tier"), Value::String("admin".into()));
        assert_ne!(CODE_FEATURE_DENIED, CODE_FEATURE_OVER_QUOTA);
    }

    /// The per-operation magnitude cap is the one bound with no window, and the
    /// refusal must say so rather than omitting the field and leaving a client
    /// to guess a window.
    #[test]
    fn the_unwindowed_cap_labels_itself() {
        let error = refusal(
            &op(),
            &FeatureVerdict::OverQuota {
                dimension: QuotaDimension::Volume,
                window: None,
                limit: 10,
                observed: 11,
                tier: RuleTier::SelfImposed,
            },
        );
        assert_eq!(
            detail(&error, "window"),
            Value::String("per_operation".into())
        );
    }

    /// The subset-edge pin: a `payments` deny must refuse `zaps` too. Row 62 proved `zaps`
    /// cannot be *built* without `payments`; this is the runtime twin — it
    /// cannot be *exercised* under a payments deny.
    ///
    /// ⚠ It must be driven through [`resolve_effective_policy`], never through
    /// `effective_policy` with slices the test itself assembled. The defect this
    /// pin exists to catch is a caller that does not pass `superset_authored` at
    /// all — a test that assembles the arguments correctly *is that caller*, and
    /// passes no matter what the resolver does. The first draft of this test did
    /// exactly that and survived the mutation that deletes the superset load;
    /// re-graded after the fix, it reds.
    #[tokio::test]
    async fn zaps_are_refused_under_a_payments_deny() {
        let db = std::sync::Arc::new(crate::db::CacheDb::open_in_memory().unwrap());
        let state = AppState::for_test(db.clone());
        let actor = [7u8; 32];
        let deny = FeaturePolicy {
            availability: Availability::Deny,
            ..FeaturePolicy::NO_OPINION
        };
        db.put_feature_policy(RuleTier::Admin, &actor, GatedFeature::Payments, &deny)
            .await
            .unwrap();

        // No zaps document anywhere — the deny must arrive along the edge.
        assert!(
            db.feature_policies_for(&actor, GatedFeature::Zaps)
                .await
                .unwrap()
                .is_empty(),
            "zaps has no document of its own"
        );

        let policy = resolve_effective_policy(&state, &actor, GatedFeature::Zaps, TierScope::All)
            .await
            .unwrap();
        assert_eq!(
            policy.availability(),
            Availability::Deny,
            "a payments deny must reach zaps along the subset edge"
        );

        // …and the edge carries availability ONLY. A payments *quota* must not
        // consume zaps' budget (§ Charter members' asymmetry).
        let mut bounded = FeaturePolicy::NO_OPINION;
        bounded.availability = Availability::Limit;
        bounded.operations = bounded.operations.tightened_with(Window::Day, 1);
        db.put_feature_policy(RuleTier::Admin, &actor, GatedFeature::Payments, &bounded)
            .await
            .unwrap();
        let policy = resolve_effective_policy(&state, &actor, GatedFeature::Zaps, TierScope::All)
            .await
            .unwrap();
        assert_ne!(
            policy.availability(),
            Availability::Deny,
            "a payments quota is not a payments deny"
        );
        assert_eq!(
            policy.bounds(QuotaDimension::Operations).get(Window::Day),
            fauna_core::feature_gate::entry(GatedFeature::Zaps)
                .tier1
                .operations
                .get(Window::Day)
                .map(|limit| fauna_core::feature_gate::BoundSource {
                    limit,
                    tier: RuleTier::Structural,
                }),
            "zaps' day bound is still its own tier-1 constant, not payments' 1/day"
        );
    }
}
