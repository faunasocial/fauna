//! The pure render derivations behind a feature-limits surface — no clock, no
//! transport, no nest read of its own, so every app folds the same
//! `fauna.features.status` reply into the same rows (priority #2).
//!
//! Three questions an app asks, answered here once:
//!
//! 1. **"What binds me, and how much is left?"** — [`feature_limits`] joins the
//!    [`EffectivePolicy`]'s per-cell bounds with the [`UsageCounters`]' observed
//!    half. *Remaining is `limit − observed`*, computed here because the nest
//!    deliberately sends both halves: a remainder alone cannot say what the
//!    bound is, and § Transparency asks for the limit, the remainder **and**
//!    the binding tier.
//! 2. **"Who did this to me?"** — every surviving cell carries its
//!    [`RuleTier`], and [`tier_label`] / [`restriction_text`] turn that into
//!    i18n keys. Attribution is not decoration: boundary 4 requires *which tier
//!    set it*, so a row that showed a number without a source would not satisfy
//!    the invariant this whole plane exists for.
//! 3. **"Should I even offer this button?"** — [`affordance`], the Dim-3
//!    hide-or-disable rule (`version-compatibility.md` § Dimension 3).
//!
//! # Why a denied feature is DISABLED and never HIDDEN
//!
//! Hiding is the honest answer to *"this build/nest does not have the
//! feature"*; disabling-with-a-reason is the honest answer to *"you have it and
//! something is bounding it"*. Collapsing the two would break boundary 4 in the
//! exact way it names — a restriction the bound person cannot see is a silent
//! gate — so [`Affordance::Hidden`] is reachable **only** through a missing
//! capability token, never through a policy.

use fauna_core::feature_gate::{
    Availability, BoundSource, EffectivePolicy, GatedFeature, MagnitudeUnit, QuotaDimension,
    RuleTier, UsageCounters, Window,
};
use fauna_core::localized::LocalizedText;
use fauna_protocol::discovery::capability;
use fauna_protocol::discovery::capability::supports;
use fauna_protocol::features::FeatureStatusItem;

/// Remaining quota in one cell: `limit − observed`, floored at zero.
///
/// Saturating rather than signed: the gate spends *before* the operation runs
/// and never decrements (§ Usage accounting), so `observed` can legitimately
/// exceed a limit that was lowered after the spend — and "you are 3 over" is
/// not a thing a remaining-quota row can render. Zero is the honest floor.
pub fn remaining(limit: u64, observed: u64) -> u64 {
    limit.saturating_sub(observed)
}

/// One (dimension, window) cell that survived the meet, joined with what this
/// account has already spent against it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct LimitCell {
    pub dimension: QuotaDimension,
    pub window: Window,
    /// The bound in force, in the dimension's unit.
    pub limit: u64,
    /// Spent so far over the same trailing window the gate evaluates.
    pub observed: u64,
    /// `limit − observed`, floored at zero ([`remaining`]).
    pub remaining: u64,
    /// The tier that set this bound — boundary 4's attribution.
    pub tier: RuleTier,
}

impl LimitCell {
    /// No headroom left in this cell.
    pub fn is_exhausted(&self) -> bool {
        self.remaining == 0
    }
}

/// One registry member's row, ready to render.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FeatureLimits {
    pub feature: GatedFeature,
    /// Derived by [`EffectivePolicy::availability`], never stored — so
    /// "limited" can never disagree with the bounds actually enforced.
    pub availability: Availability,
    /// The tier that denied outright, if any.
    pub denied_by: Option<RuleTier>,
    /// Every surviving cell, in a stable order: dimension (operations,
    /// counterparties, volume) major, window (day, week, month) minor. Stable
    /// so a row does not reshuffle between reads.
    pub cells: Vec<LimitCell>,
    /// The per-operation size cap, if one survived. Not a quota — it bounds one
    /// operation's magnitude rather than a window's total, so it never
    /// exhausts.
    pub per_operation_max: Option<BoundSource>,
    /// The unit `volume` cells and [`Self::per_operation_max`] are counted in,
    /// from the registry — so a magnitude can never be rendered in the wrong
    /// unit.
    pub unit: MagnitudeUnit,
}

impl FeatureLimits {
    /// The cell for one (dimension, window) pair, if it survived the meet.
    pub fn cell(&self, dimension: QuotaDimension, window: Window) -> Option<&LimitCell> {
        self.cells
            .iter()
            .find(|c| c.dimension == dimension && c.window == window)
    }

    /// Cells whose headroom is gone.
    pub fn exhausted(&self) -> impl Iterator<Item = &LimitCell> {
        self.cells.iter().filter(|c| c.is_exhausted())
    }
}

/// Fold one status item into its renderable row.
///
/// Reads the [`EffectivePolicy`] the nest sent **verbatim** — see the crate
/// docs' trap: recomposing the meet client-side drops the subset edge and
/// reports `zaps` as available under a `payments` deny the nest refuses.
pub fn feature_limits(item: &FeatureStatusItem) -> FeatureLimits {
    let policy: &EffectivePolicy = &item.policy;
    let usage: &UsageCounters = &item.usage;
    let mut cells = Vec::new();

    for dimension in [
        QuotaDimension::Operations,
        QuotaDimension::Counterparties,
        QuotaDimension::Volume,
    ] {
        let bounds = policy.bounds(dimension);
        let counts = usage.get(dimension);
        for window in [Window::Day, Window::Week, Window::Month] {
            let Some(source) = bounds.get(window) else {
                continue;
            };
            let observed = counts.get(window);
            cells.push(LimitCell {
                dimension,
                window,
                limit: source.limit,
                observed,
                remaining: remaining(source.limit, observed),
                tier: source.tier,
            });
        }
    }

    FeatureLimits {
        feature: item.feature,
        availability: policy.availability(),
        denied_by: policy.denied_by,
        cells,
        per_operation_max: policy.per_operation_max,
        unit: fauna_core::feature_gate::entry(item.feature).unit,
    }
}

/// The counterparty headroom for this window, if the dimension is bounded.
///
/// Split out of [`affordance`] deliberately. A spent `counterparties` cell
/// blocks only operations toward a **new** counterparty — an operation toward
/// someone already in the feature's records contributes a `0` delta and is
/// still admitted (§ The quota grammar's newness-delta rule). Only the calling
/// surface knows which kind of operation its button would start, so folding
/// this into the general affordance would disable buttons the nest would have
/// allowed.
pub fn counterparty_headroom(limits: &FeatureLimits, window: Window) -> Option<u64> {
    limits
        .cell(QuotaDimension::Counterparties, window)
        .map(|c| c.remaining)
}

/// The capability token whose absence means *this nest build does not carry the
/// feature at all* — the only ground on which a row may be hidden.
///
/// `payments` = `subscriptions` (made conditional by the nest-side excision
/// on 2026-08-10 — `fauna-protocol/src/discovery.rs::capability::SUBSCRIPTIONS`).
/// `p2p-share` = its own token (advertised under
/// `cfg!(feature = "p2p-share")` since the nest legs landed 2026-08-17 —
/// wormability rule 7's brake; the same string
/// `ceremony_bind_verdict` consults, so the limits screen and the bind door
/// cannot disagree about what an excised nest looks like).
/// `zaps` has **no token yet** (§ Wire & data shape names the per-feature
/// tokens as target state), and `None` is the honest answer: its presence is
/// unknowable from the capability set, so nothing may be hidden on that
/// basis. When its token lands, this map grows in one place instead of seven.
pub fn capability_token(feature: GatedFeature) -> Option<&'static str> {
    match feature {
        GatedFeature::Payments => Some(capability::SUBSCRIPTIONS),
        GatedFeature::P2pShare => Some(capability::P2P_SHARE),
        GatedFeature::Zaps => None,
        // No surface renders a feature this build does not know.
        GatedFeature::Unknown => None,
    }
}

/// What an app should do with a gated affordance.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Affordance {
    /// Render it, actionable.
    Available,
    /// Render it, not actionable, showing `reason` — the honest surface that
    /// keeps the user from walking into a refusal (§ Evaluation points item 2).
    Disabled { reason: LocalizedText },
    /// Do not render it: this nest build does not carry the feature.
    Hidden,
}

/// The Dim-3 hide-or-disable decision for a feature's affordances.
///
/// Order, and each arm's ground:
///
/// 1. **Hidden** — the feature's capability token is defined and the nest does
///    not advertise it, so this build genuinely lacks the plane. This arm is
///    load-bearing rather than theoretical: `fauna_core::feature_gate::registry`
///    is **not** cfg-gated, so a payments-excised (store-safe) nest still
///    returns a `payments` row in `fauna.features.status` — rendering it would
///    advertise a plane the artifact does not have.
/// 2. **Disabled** — a tier denied, or a cell that blocks *any* operation is
///    spent. Never hidden: boundary 4 requires the bound person to see it.
/// 3. **Available** — otherwise.
///
/// Exhaustion is judged on the `operations` and `volume` dimensions only. Those
/// bound every operation; `counterparties` bounds only operations toward a new
/// counterparty, which is [`counterparty_headroom`]'s question because only the
/// call site knows.
pub fn affordance(item: &FeatureStatusItem, nest_capabilities: &[String]) -> Affordance {
    if let Some(token) = capability_token(item.feature)
        && !supports(nest_capabilities, token)
    {
        return Affordance::Hidden;
    }

    let limits = feature_limits(item);
    match restriction_text(&limits) {
        Some(reason) => Affordance::Disabled { reason },
        None => Affordance::Available,
    }
}

/// The honest "why can't I" line for a row that is blocking something, or
/// `None` when nothing blocks.
///
/// A *bounded but not spent* feature returns `None`: its affordance stays
/// actionable, and its limits are still fully visible in the row
/// [`feature_limits`] produced — boundary 4 is satisfied by the surface, not by
/// disabling a button that works.
pub fn restriction_text(limits: &FeatureLimits) -> Option<LocalizedText> {
    if let Some(tier) = limits.denied_by {
        return Some(LocalizedText::key(denied_by_key(tier)));
    }
    let blocking = limits.exhausted().find(|c| {
        matches!(
            c.dimension,
            QuotaDimension::Operations | QuotaDimension::Volume
        )
    })?;
    Some(LocalizedText::key_arg(
        exhausted_key(blocking.tier),
        "window",
        window_key(blocking.window),
    ))
}

/// A tier's human name — *"your admin"*, *"your guardian"*, …
pub fn tier_label(tier: RuleTier) -> LocalizedText {
    LocalizedText::key(match tier {
        RuleTier::Structural => "features.tier_structural",
        RuleTier::Region => "features.tier_region",
        RuleTier::Admin => "features.tier_admin",
        RuleTier::Guardian => "features.tier_guardian",
        RuleTier::SelfImposed => "features.tier_self",
        RuleTier::Unknown => "features.tier_other",
    })
}

/// A registry member's human name.
pub fn feature_label(feature: GatedFeature) -> LocalizedText {
    LocalizedText::key(match feature {
        GatedFeature::Payments => "features.name_payments",
        GatedFeature::Zaps => "features.name_zaps",
        GatedFeature::P2pShare => "features.name_p2p_share",
        GatedFeature::Unknown => "features.name_other",
    })
}

/// Per-tier keys rather than one template with a `{tier}` argument: a
/// [`LocalizedText`] argument is a finished string, so nesting one localized
/// term inside another template would force every app to resolve twice and
/// would deny translators the grammar agreement most languages need here.
pub(crate) fn denied_by_key(tier: RuleTier) -> &'static str {
    match tier {
        RuleTier::Structural => "features.denied_by_structural",
        RuleTier::Region => "features.denied_by_region",
        RuleTier::Admin => "features.denied_by_admin",
        RuleTier::Guardian => "features.denied_by_guardian",
        RuleTier::SelfImposed => "features.denied_by_self",
        RuleTier::Unknown => "features.denied_by_other",
    }
}

fn exhausted_key(tier: RuleTier) -> &'static str {
    match tier {
        RuleTier::Structural => "features.exhausted_structural",
        RuleTier::Region => "features.exhausted_region",
        RuleTier::Admin => "features.exhausted_admin",
        RuleTier::Guardian => "features.exhausted_guardian",
        RuleTier::SelfImposed => "features.exhausted_self",
        RuleTier::Unknown => "features.exhausted_other",
    }
}

/// The `{window}` substitution — a bare noun the exhaustion templates embed
/// ("… until the week rolls over"). Data, not a sentence, so one argument is
/// right here where a nested tier term was not.
fn window_key(window: Window) -> &'static str {
    match window {
        Window::Day => "features.window_day",
        Window::Week => "features.window_week",
        Window::Month => "features.window_month",
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use fauna_core::feature_gate::{
        FeaturePolicy, RuleTier, UsageCounters, WindowCounts, effective_policy, entry,
    };

    fn item(
        feature: GatedFeature,
        policy: EffectivePolicy,
        usage: UsageCounters,
    ) -> FeatureStatusItem {
        FeatureStatusItem {
            feature,
            policy,
            usage,
            extra: Default::default(),
        }
    }

    /// Tier-1 constants alone, the shape a nest sends an unrestricted account.
    fn tier1(feature: GatedFeature) -> EffectivePolicy {
        effective_policy(feature, &[], &[])
    }

    #[test]
    fn remaining_is_limit_minus_observed_and_floors_at_zero() {
        assert_eq!(remaining(50, 20), 30);
        assert_eq!(remaining(50, 50), 0);
        // A limit lowered after the spend: the gate never decrements, so this
        // is reachable in production, not a hypothetical.
        assert_eq!(remaining(10, 50), 0);
    }

    #[test]
    fn cells_join_the_bound_with_what_was_spent_and_keep_the_tier() {
        let feature = GatedFeature::P2pShare;
        let policy = tier1(feature);
        let week_limit = entry(feature)
            .tier1
            .counterparties
            .per_week
            .expect("p2p-share bounds counterparties per week at tier 1");

        let usage = UsageCounters {
            counterparties: WindowCounts {
                day: 0,
                week: 3,
                month: 3,
            },
            ..Default::default()
        };

        let limits = feature_limits(&item(feature, policy, usage));
        let cell = limits
            .cell(QuotaDimension::Counterparties, Window::Week)
            .expect("the week cell survived");

        assert_eq!(cell.limit, week_limit);
        assert_eq!(cell.observed, 3);
        assert_eq!(cell.remaining, week_limit - 3);
        // Attribution: tier 1's constants are the structural tier.
        assert_eq!(cell.tier, RuleTier::Structural);
    }

    #[test]
    fn cells_come_in_a_stable_dimension_then_window_order() {
        let feature = GatedFeature::P2pShare;
        let limits = feature_limits(&item(feature, tier1(feature), UsageCounters::default()));

        let mut seen: Vec<(QuotaDimension, Window)> = limits
            .cells
            .iter()
            .map(|c| (c.dimension, c.window))
            .collect();
        let expected = seen.clone();
        seen.sort_by_key(|(d, w)| (*d, *w));
        assert_eq!(
            seen, expected,
            "cells must already be in (dimension, window) order so rows don't reshuffle"
        );
    }

    /// The trap, rendered: a `payments` deny reaches `zaps` through the subset
    /// edge, and the nest sends that already-composed. The view model must
    /// carry the deny through rather than reading the (empty) zaps slice as
    /// "allowed".
    #[test]
    fn a_denied_subset_member_renders_as_denied() {
        let denied = FeaturePolicy {
            availability: Availability::Deny,
            ..Default::default()
        };
        // The nest's resolver, called the one correct way: the superset's
        // authored documents are what make the subset edge fire.
        let policy = effective_policy(GatedFeature::Zaps, &[], &[(RuleTier::Admin, denied)]);

        let limits = feature_limits(&item(GatedFeature::Zaps, policy, UsageCounters::default()));

        assert_eq!(limits.availability, Availability::Deny);
        assert_eq!(limits.denied_by, Some(RuleTier::Admin));
        assert_eq!(
            restriction_text(&limits),
            Some(LocalizedText::key("features.denied_by_admin")),
            "the row must name the tier that denied — boundary 4's attribution"
        );
    }

    #[test]
    fn a_denied_feature_is_disabled_with_a_reason_never_hidden() {
        let denied = FeaturePolicy {
            availability: Availability::Deny,
            ..Default::default()
        };
        let policy = effective_policy(GatedFeature::P2pShare, &[(RuleTier::Admin, denied)], &[]);
        let it = item(GatedFeature::P2pShare, policy, UsageCounters::default());

        match affordance(&it, &[capability::P2P_SHARE.to_string()]) {
            Affordance::Disabled { reason } => {
                assert_eq!(reason, LocalizedText::key("features.denied_by_admin"));
            }
            other => panic!("a denied feature must be disabled-with-a-reason, got {other:?}"),
        }
    }

    #[test]
    fn an_unrestricted_feature_is_available_and_has_no_restriction_text() {
        let feature = GatedFeature::P2pShare;
        let it = item(feature, tier1(feature), UsageCounters::default());

        assert_eq!(
            affordance(&it, &[capability::P2P_SHARE.to_string()]),
            Affordance::Available
        );
        assert_eq!(restriction_text(&feature_limits(&it)), None);
    }

    /// A bounded-but-unspent feature keeps its button. Its limits are visible
    /// in the row either way, which is what boundary 4 asks for — disabling a
    /// button the nest would honour is not transparency, it is a worse product.
    #[test]
    fn a_bounded_but_unspent_feature_stays_actionable() {
        let feature = GatedFeature::P2pShare;
        let limits = feature_limits(&item(feature, tier1(feature), UsageCounters::default()));

        assert_eq!(limits.availability, Availability::Limit);
        assert!(!limits.cells.is_empty());
        assert_eq!(restriction_text(&limits), None);
    }

    #[test]
    fn a_spent_operations_quota_disables_and_names_the_window() {
        let feature = GatedFeature::P2pShare;
        // Taken from the registry rather than restated: OQ-4's ratification
        // moved a tier-1 number on 2026-08-11, and a fixture that copies a
        // constant disagrees with the registry the next time one moves.
        let ops_day = entry(feature)
            .tier1
            .operations
            .per_day
            .expect("p2p-share bounds operations per day at tier 1");
        let usage = UsageCounters {
            operations: WindowCounts {
                day: ops_day,
                week: ops_day,
                month: ops_day,
            },
            ..Default::default()
        };
        let it = item(feature, tier1(feature), usage);

        match affordance(&it, &[capability::P2P_SHARE.to_string()]) {
            Affordance::Disabled { reason } => {
                assert_eq!(reason.key, "features.exhausted_structural");
                assert_eq!(
                    reason.args.get("window").map(String::as_str),
                    Some("features.window_day"),
                    "the row must name the window that ran out, not just that one did"
                );
            }
            other => panic!("a spent operations quota must disable, got {other:?}"),
        }
    }

    /// The over-restriction this split exists to prevent: a spent
    /// `counterparties` cell blocks only operations toward someone NEW, so the
    /// general affordance must stay available.
    #[test]
    fn a_spent_counterparty_quota_does_not_disable_the_whole_feature() {
        let feature = GatedFeature::P2pShare;
        let cp_month = entry(feature)
            .tier1
            .counterparties
            .per_month
            .expect("p2p-share bounds counterparties per month at tier 1");
        let usage = UsageCounters {
            counterparties: WindowCounts {
                day: 0,
                week: cp_month,
                month: cp_month,
            },
            ..Default::default()
        };
        let it = item(feature, tier1(feature), usage);

        assert_eq!(
            affordance(&it, &[capability::P2P_SHARE.to_string()]),
            Affordance::Available,
            "sharing with an EXISTING counterparty is still admitted by the nest"
        );
        assert_eq!(
            counterparty_headroom(&feature_limits(&it), Window::Month),
            Some(0),
            "…but the call site can see there is no room for a new one"
        );
    }

    /// `registry()` is not cfg-gated, so a payments-excised nest still reports
    /// a `payments` row. Rendering it would advertise a plane the artifact does
    /// not carry — the one ground for hiding.
    #[test]
    fn payments_is_hidden_when_the_nest_does_not_advertise_subscriptions() {
        let feature = GatedFeature::Payments;
        let it = item(feature, tier1(feature), UsageCounters::default());

        assert_eq!(affordance(&it, &[]), Affordance::Hidden);
        assert_eq!(
            affordance(&it, &[capability::SUBSCRIPTIONS.to_string()]),
            Affordance::Available
        );
    }

    /// `zaps` has no token yet, so absence proves nothing and nothing may be
    /// hidden on that basis; `p2p-share` gained its token with the 2026-08-17
    /// nest legs and hides exactly like payments.
    #[test]
    fn a_member_without_a_capability_token_is_never_hidden() {
        assert_eq!(capability_token(GatedFeature::Zaps), None);
        let it = item(
            GatedFeature::Zaps,
            tier1(GatedFeature::Zaps),
            UsageCounters::default(),
        );
        assert_ne!(affordance(&it, &[]), Affordance::Hidden);

        assert_eq!(
            capability_token(GatedFeature::P2pShare),
            Some(capability::P2P_SHARE)
        );
        let it = item(
            GatedFeature::P2pShare,
            tier1(GatedFeature::P2pShare),
            UsageCounters::default(),
        );
        assert_eq!(affordance(&it, &[]), Affordance::Hidden);
        assert_eq!(
            affordance(&it, &[capability::P2P_SHARE.to_string()]),
            Affordance::Available
        );
    }

    #[test]
    fn every_tier_and_member_has_a_label_key() {
        for tier in [
            RuleTier::Structural,
            RuleTier::Region,
            RuleTier::Admin,
            RuleTier::Guardian,
            RuleTier::SelfImposed,
        ] {
            assert!(tier_label(tier).key.starts_with("features.tier_"));
            assert!(denied_by_key(tier).starts_with("features.denied_by_"));
            assert!(exhausted_key(tier).starts_with("features.exhausted_"));
        }
        for feature in GatedFeature::ALL {
            assert!(feature_label(feature).key.starts_with("features.name_"));
        }
    }

    /// Every key this crate can emit must exist in `i18n/strings/en.yaml`.
    /// A typo compiles fine and renders the raw key on all 7 apps
    /// (`LocalizedText::resolve` falls back to the key), so nothing but a test
    /// catches it. Also pins that `{window}` really is substituted — the
    /// exhaustion templates are the only two-level keys here.
    #[test]
    fn every_emitted_key_resolves() {
        let mut keys: Vec<String> = Vec::new();
        for feature in GatedFeature::ALL {
            keys.push(feature_label(feature).key);
        }
        for tier in [
            RuleTier::Structural,
            RuleTier::Region,
            RuleTier::Admin,
            RuleTier::Guardian,
            RuleTier::SelfImposed,
        ] {
            keys.push(tier_label(tier).key);
            keys.push(denied_by_key(tier).to_string());
            keys.push(exhausted_key(tier).to_string());
        }
        for window in [Window::Day, Window::Week, Window::Month] {
            keys.push(window_key(window).to_string());
        }

        for key in &keys {
            assert!(
                fauna_i18n::strings::lookup(key).is_some(),
                "{key} is not in i18n/strings/en.yaml — it would render as the raw key"
            );
        }

        // The exhaustion template genuinely substitutes its window noun.
        let text = LocalizedText::key_arg(
            exhausted_key(RuleTier::Admin),
            "window",
            fauna_i18n::strings::lookup(window_key(Window::Week)).expect("window key"),
        );
        let rendered = text.resolve(fauna_i18n::strings::lookup);
        assert!(
            rendered.contains("week") && !rendered.contains('{'),
            "the window noun must be substituted, got {rendered:?}"
        );
    }

    /// The crate docs' trap, enforced structurally instead of by a comment: a
    /// client that recomposes the meet from authored documents drops the subset
    /// edge. Test code legitimately calls `effective_policy` to *build* the
    /// fixtures a nest would have sent, so only the shipped modules are checked.
    #[test]
    fn this_crate_never_recomposes_the_meet() {
        for (name, src) in [
            ("lib.rs", include_str!("lib.rs")),
            ("view_model.rs", include_str!("view_model.rs")),
            ("row.rs", include_str!("row.rs")),
            // The authoring editor compares against the nest-composed CEILING;
            // it must never compose one itself.
            ("editor.rs", include_str!("editor.rs")),
        ] {
            let shipped: String = src
                .lines()
                .take_while(|l| !l.trim_start().starts_with("#[cfg(test)]"))
                .filter(|l| {
                    let t = l.trim_start();
                    !t.starts_with("//")
                })
                .collect::<Vec<_>>()
                .join("\n");
            assert!(
                !shipped.contains("effective_policy"),
                "{name} calls effective_policy: render the EffectivePolicy the nest sent, \
                 never recompose the meet (the subset edge fires only for the resolver \
                 that passes the superset's authored documents)"
            );
        }
    }
}
