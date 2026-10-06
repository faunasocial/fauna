//! The finished row an app renders — built once here so the UniFFI face, the
//! wasm face and the two direct Rust consumers (tui, linux) all hand their UI
//! the *same* shape with the *same* vocabulary.
//!
//! # Why the row exists rather than each face folding [`crate::view_model`]
//!
//! The enum-ish values a UI shows (`feature`, `availability`, `dimension`,
//! `window`, `tier`) have to cross every boundary as strings eventually. Left
//! to each face, they diverge on contact — a `Debug` print, a serde derive and
//! a hand-written FFI mapping each pick their own spelling, so two apps end up
//! with two vocabularies for one concept (priority #1's exact failure mode).
//! Naming them once here, through the shared `as_str` maps, makes the faces
//! mechanical.
//!
//! These strings are a **client-rendering vocabulary**, deliberately distinct
//! from the wire: `GatedFeature`'s keys are shared with the cargo features and
//! capability tokens and so are reproduced exactly, but the tier/dimension/
//! window names exist only for UI plumbing and carry no wire obligation.

use serde::{Deserialize, Serialize};

use fauna_core::feature_gate::{
    Availability, GatedFeature, MagnitudeUnit, QuotaDimension, RuleTier, Window,
};
use fauna_core::localized::LocalizedText;
use fauna_protocol::features::FeatureStatusItem;

use crate::view_model::{Affordance, FeatureLimits, LimitCell, affordance, feature_limits};

/// `operations` | `counterparties` | `volume`. The spelling itself is
/// [`QuotaDimension::as_str`]'s — at-rest vocabulary the nest writes into
/// quota-bucket columns, so it is one map fleet-wide, not one per side.
pub fn dimension_key(dimension: QuotaDimension) -> &'static str {
    dimension.as_str()
}

/// `day` | `week` | `month`. Trailing windows, not calendar ones.
pub fn window_key(window: Window) -> &'static str {
    window.as_str()
}

/// `structural` | `region` | `admin` | `guardian` | `self` — or `unknown`, a
/// tier a newer nest named that this build cannot.
pub fn tier_key(tier: RuleTier) -> &'static str {
    tier.as_str()
}

/// `allow` | `deny` | `limit`. A value this build cannot read renders as the
/// `deny` it enforces as (`transport.md` § Rule 3 in full).
pub fn availability_key(availability: &Availability) -> &'static str {
    match availability {
        Availability::Allow => "allow",
        Availability::Deny | Availability::Other(_) => "deny",
        Availability::Limit => "limit",
    }
}

/// `bytes` | `millisats`.
pub fn unit_key(unit: MagnitudeUnit) -> &'static str {
    match unit {
        MagnitudeUnit::Bytes => "bytes",
        MagnitudeUnit::Millisats => "millisats",
    }
}

/// `available` | `disabled` | `hidden`.
pub fn affordance_key(affordance: &Affordance) -> &'static str {
    match affordance {
        Affordance::Available => "available",
        Affordance::Disabled { .. } => "disabled",
        Affordance::Hidden => "hidden",
    }
}

/// A dimension's human name — the noun a quota row is counting.
pub fn dimension_label(dimension: QuotaDimension) -> LocalizedText {
    LocalizedText::key(match dimension {
        QuotaDimension::Operations => "features.dimension_operations",
        QuotaDimension::Counterparties => "features.dimension_counterparties",
        QuotaDimension::Volume => "features.dimension_volume",
    })
}

/// A window's human noun — `day` | `week` | `month`, **trailing**.
///
/// The same key [`crate::view_model::restriction_text`] substitutes, exposed
/// here because a quota row names its window too.
pub fn window_label(window: Window) -> LocalizedText {
    LocalizedText::key(match window {
        Window::Day => "features.window_day",
        Window::Week => "features.window_week",
        Window::Month => "features.window_month",
    })
}

/// One bound in force, with what has been spent against it and which tier set
/// it.
///
/// The three `LocalizedText` fields are derived **here** rather than by each
/// app, for the reason the module docs give: left to the faces, the same cell
/// becomes "3 of 10 left" on one app and "7 used, 10 max" on another —
/// priority #1's exact failure mode, one concept in seven vocabularies.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RowCell {
    pub dimension: String,
    pub window: String,
    pub limit: u64,
    pub observed: u64,
    /// `limit − observed`, floored at zero.
    pub remaining: u64,
    pub tier: String,
    /// The tier's human name, for a row's "set by …" half.
    pub tier_label: LocalizedText,
    pub exhausted: bool,
    /// What this cell counts, over which window — *"Uses per week"*. Both
    /// substitutions are themselves keys, so resolve with
    /// [`LocalizedText::resolve_nested`].
    pub label: LocalizedText,
    /// The headroom sentence — *"3 left of 10"*, or the exhausted form when
    /// nothing is left. `{remaining}` / `{limit}` are already finished numbers
    /// **unless** [`Self::magnitudes`] is `Some`, in which case they are the
    /// raw counts and the app substitutes the localized magnitudes instead.
    pub value: LocalizedText,
    /// The two magnitudes in the dimension's own unit, when the cell counts a
    /// unit rather than bare items — `Some` exactly for `volume` cells.
    ///
    /// Two levels for the reason `BackupLastUploadDisplay` is two levels: a
    /// [`LocalizedText`] argument is a flat string, so a magnitude that is
    /// itself localized ("1 TB", "21 000 sats") has to be resolved by the app
    /// first and then substituted into [`Self::value`]. Native Rust callers get
    /// that composition from [`cell_value_text`] rather than writing it twice.
    pub magnitudes: Option<CellMagnitudes>,
}

/// The localized magnitudes of a `volume` cell — see [`RowCell::magnitudes`].
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CellMagnitudes {
    pub remaining: LocalizedText,
    pub limit: LocalizedText,
}

impl RowCell {
    fn from_cell(c: &LimitCell, unit: MagnitudeUnit) -> Self {
        RowCell {
            dimension: dimension_key(c.dimension).to_string(),
            window: window_key(c.window).to_string(),
            limit: c.limit,
            observed: c.observed,
            remaining: c.remaining,
            tier: tier_key(c.tier).to_string(),
            tier_label: crate::view_model::tier_label(c.tier),
            exhausted: c.is_exhausted(),
            label: LocalizedText::key_args(
                "features.quota_label",
                [
                    ("dimension", dimension_label(c.dimension).key),
                    ("window", window_label(c.window).key),
                ],
            ),
            value: LocalizedText::key_args(
                if c.is_exhausted() {
                    "features.quota_value_exhausted"
                } else {
                    "features.quota_value"
                },
                [
                    ("remaining", c.remaining.to_string()),
                    ("limit", c.limit.to_string()),
                ],
            ),
            magnitudes: (c.dimension == QuotaDimension::Volume).then(|| CellMagnitudes {
                remaining: magnitude(c.remaining, unit),
                limit: magnitude(c.limit, unit),
            }),
        }
    }
}

/// A `volume` magnitude in its own unit: the shared 1024-unit byte scale, or
/// **sats** for a millisat amount (the unit a person actually reads — the tier
/// editor already takes prices in sats, `monetization.md` § The asking price).
///
/// Sats round down: a remaining-headroom number that rounded *up* would promise
/// a sat the quota does not have.
pub(crate) fn magnitude(value: u64, unit: MagnitudeUnit) -> LocalizedText {
    match unit {
        MagnitudeUnit::Bytes => fauna_core::format::byte_size(value),
        MagnitudeUnit::Millisats => LocalizedText::key_arg(
            "features.magnitude_sats",
            "value",
            (value / MSAT_PER_SAT).to_string(),
        ),
    }
}

/// Millisats per sat — the wire stores millisats, people read sats. Owned by
/// [`fauna_core::money::MSATS_PER_SAT`].
use fauna_core::money::MSATS_PER_SAT as MSAT_PER_SAT;

/// A cell's headroom sentence, fully composed — the inner magnitudes resolved
/// first, then substituted into the outer template.
///
/// The native-Rust half of [`RowCell::magnitudes`]' two-level contract, written
/// once here so tui and linux cannot compose it differently (the FFI and wasm
/// faces hand the same two levels to Swift/Kotlin/JS, which resolve through
/// their own pipelines exactly as they already do for
/// `BackupLastUploadDisplay`).
pub fn cell_value_text<F, S>(cell: &RowCell, lookup: F) -> String
where
    F: Fn(&str) -> Option<S> + Copy,
    S: AsRef<str>,
{
    let Some(magnitudes) = &cell.magnitudes else {
        return cell.value.resolve(lookup);
    };
    let mut composed = cell.value.clone();
    composed
        .args
        .insert("remaining".into(), magnitudes.remaining.resolve(lookup));
    composed
        .args
        .insert("limit".into(), magnitudes.limit.resolve(lookup));
    composed.resolve(lookup)
}

/// One registry member's row on a feature-limits surface.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct FeatureRow {
    /// The stable key — `payments` | `zaps` | `p2p-share`. Shared with the
    /// cargo feature and the capability token.
    pub feature: String,
    /// The member's human name; the stable key never reaches a screen.
    pub name: LocalizedText,
    pub availability: String,
    pub denied_by: Option<String>,
    /// Every bound in force, in a stable (dimension, window) order.
    pub cells: Vec<RowCell>,
    /// A per-operation magnitude cap. Not a quota — it bounds one operation's
    /// size rather than a window's total, so it never runs out.
    pub per_operation_max: Option<u64>,
    pub per_operation_max_tier: Option<String>,
    pub unit: String,
    pub affordance: String,
    /// Why the affordance is disabled, when it is. Its `{window}` substitution
    /// is a key — resolve with [`LocalizedText::resolve_nested`].
    pub restriction: Option<LocalizedText>,
    /// The row's one-word state, for the surface's status column: *Available*
    /// or *Restricted*. Derived from [`Self::affordance`] rather than from the
    /// availability, so the word can never disagree with whether the button
    /// works.
    pub status: LocalizedText,
}

/// Build the row for one status item against the nest's advertised capability
/// set (`NestInfoReply.capabilities`), which is what decides the `hidden`
/// affordance — see [`crate::view_model::affordance`].
pub fn feature_row(item: &FeatureStatusItem, nest_capabilities: &[String]) -> FeatureRow {
    let limits = feature_limits(item);
    let decision = affordance(item, nest_capabilities);
    row_from(&limits, decision)
}

/// Every registry member's row, in registry order — the whole surface. A
/// feature a newer nest lists that this build does not know has no row: it
/// gates nothing here and no surface offers it (`transport.md` § Rule 3 in
/// full).
pub fn feature_rows(items: &[FeatureStatusItem], nest_capabilities: &[String]) -> Vec<FeatureRow> {
    items
        .iter()
        .filter(|item| item.feature.is_known())
        .map(|item| feature_row(item, nest_capabilities))
        .collect()
}

fn row_from(limits: &FeatureLimits, decision: Affordance) -> FeatureRow {
    let affordance = affordance_key(&decision).to_string();
    let status = LocalizedText::key(match decision {
        Affordance::Available => "features.status_available",
        // A hidden row is not rendered at all, so its word never shows; naming
        // it "restricted" rather than inventing a third one keeps the column
        // total without implying a state the surface can display.
        Affordance::Disabled { .. } | Affordance::Hidden => "features.status_restricted",
    });
    let restriction = match decision {
        Affordance::Disabled { reason } => Some(reason),
        Affordance::Available | Affordance::Hidden => None,
    };
    FeatureRow {
        feature: limits.feature.as_str().to_string(),
        name: crate::view_model::feature_label(limits.feature),
        availability: availability_key(&limits.availability).to_string(),
        denied_by: limits.denied_by.map(|t| tier_key(t).to_string()),
        cells: limits
            .cells
            .iter()
            .map(|c| RowCell::from_cell(c, limits.unit))
            .collect(),
        per_operation_max: limits.per_operation_max.map(|b| b.limit),
        per_operation_max_tier: limits
            .per_operation_max
            .map(|b| tier_key(b.tier).to_string()),
        unit: unit_key(limits.unit).to_string(),
        affordance,
        restriction,
        status,
    }
}

/// Why `feature`'s own UI affordance (a button, an add-action) is disabled,
/// or `None` when it isn't — `available` and "no such row yet" (rows not
/// hydrated, or the key absent) both read as nothing to show. The decision
/// is READ off the row's own `affordance`, never re-derived: `hidden` still
/// disables here exactly like `disabled` does (the *removal* story is the
/// orthogonal compile-time cargo feature, not this render-time courtesy).
pub fn gate_reason<F, S>(rows: &[FeatureRow], feature: &str, lookup: F) -> Option<String>
where
    F: Fn(&str) -> Option<S> + Copy,
    S: AsRef<str>,
{
    let row = rows.iter().find(|r| r.feature == feature)?;
    if row.affordance == "available" {
        return None;
    }
    Some(
        row.restriction
            .as_ref()
            .map(|r| r.resolve_nested(lookup))
            .unwrap_or_else(|| row.status.resolve(lookup)),
    )
}

/// Every registry member's stable key, in registry order — for a surface that
/// must list the gated set before (or without) a nest read.
pub fn gated_feature_keys() -> Vec<String> {
    GatedFeature::ALL
        .iter()
        .map(|f| f.as_str().to_string())
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use fauna_core::feature_gate::{FeaturePolicy, UsageCounters, effective_policy, entry};
    use fauna_protocol::discovery::capability;

    fn item(feature: GatedFeature) -> FeatureStatusItem {
        FeatureStatusItem {
            feature,
            policy: effective_policy(feature, &[], &[]),
            usage: UsageCounters::default(),
            extra: Default::default(),
        }
    }

    /// A nest that carries the share plane — the p2p-share fixtures' default
    /// posture (its token became conditional with the 2026-08-17 nest legs).
    fn p2p() -> Vec<String> {
        vec![capability::P2P_SHARE.to_string()]
    }

    /// A feature a newer nest lists that this build does not know has no row
    /// (`transport.md` § Rule 3 in full — no UI offers it), and an unknown
    /// tier renders as "another rule-setter".
    #[test]
    fn an_unknown_feature_has_no_row_and_an_unknown_tier_has_a_name() {
        let rows = feature_rows(
            &[item(GatedFeature::Unknown), item(GatedFeature::P2pShare)],
            &p2p(),
        );
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].feature, "p2p-share");
        assert_eq!(
            crate::view_model::tier_label(RuleTier::Unknown).key,
            "features.tier_other"
        );
    }

    #[test]
    fn a_row_carries_the_bounds_and_the_tier_that_set_each() {
        let feature = GatedFeature::P2pShare;
        let r = feature_row(&item(feature), &p2p());

        assert_eq!(r.feature, "p2p-share");
        assert_eq!(r.availability, "limit");
        assert_eq!(r.unit, "bytes");
        assert_eq!(r.affordance, "available");
        assert!(r.restriction.is_none());
        assert_eq!(r.name.key, "features.name_p2p_share");

        let day = r
            .cells
            .iter()
            .find(|c| c.dimension == "operations" && c.window == "day")
            .expect("p2p-share bounds operations per day at tier 1");
        // From the registry, not a restated constant.
        assert_eq!(day.limit, entry(feature).tier1.operations.per_day.unwrap());
        assert_eq!(day.tier, "structural");
        assert_eq!(day.tier_label.key, "features.tier_structural");
        assert!(!day.exhausted);
    }

    #[test]
    fn a_denied_member_becomes_a_disabled_row_naming_its_tier() {
        let denied = FeaturePolicy {
            availability: Availability::Deny,
            ..Default::default()
        };
        let feature = GatedFeature::P2pShare;
        let it = FeatureStatusItem {
            feature,
            policy: effective_policy(feature, &[(RuleTier::Admin, denied)], &[]),
            usage: UsageCounters::default(),
            extra: Default::default(),
        };

        let r = feature_row(&it, &p2p());
        assert_eq!(r.affordance, "disabled");
        assert_eq!(r.denied_by.as_deref(), Some("admin"));
        assert_eq!(
            r.restriction.map(|t| t.key).as_deref(),
            Some("features.denied_by_admin")
        );
    }

    #[test]
    fn gate_reason_is_none_when_the_row_is_available() {
        let rows = vec![feature_row(&item(GatedFeature::P2pShare), &p2p())];
        assert_eq!(
            gate_reason(&rows, "p2p-share", |_: &str| None::<&str>),
            None
        );
    }

    #[test]
    fn gate_reason_is_none_for_a_feature_key_absent_from_the_rows() {
        let rows = vec![feature_row(&item(GatedFeature::P2pShare), &p2p())];
        assert_eq!(gate_reason(&rows, "payments", |_: &str| None::<&str>), None);
    }

    #[test]
    fn gate_reason_resolves_the_restriction_when_disabled() {
        let denied = FeaturePolicy {
            availability: Availability::Deny,
            ..Default::default()
        };
        let feature = GatedFeature::P2pShare;
        let it = FeatureStatusItem {
            feature,
            policy: effective_policy(feature, &[(RuleTier::Admin, denied)], &[]),
            usage: UsageCounters::default(),
            extra: Default::default(),
        };
        let rows = vec![feature_row(&it, &p2p())];
        assert_eq!(
            gate_reason(&rows, "p2p-share", |_: &str| None::<&str>).as_deref(),
            Some("features.denied_by_admin")
        );
    }

    #[test]
    fn payments_is_a_hidden_row_without_the_subscriptions_token() {
        let it = item(GatedFeature::Payments);
        assert_eq!(feature_row(&it, &[]).affordance, "hidden");
        assert_eq!(
            feature_row(&it, &[capability::SUBSCRIPTIONS.to_string()]).affordance,
            "available"
        );
    }

    /// The vocabulary is the reason this module exists: the self tier is
    /// `self` — the shared `as_str`, which the wire's serde spelling matches.
    /// One name, or two apps disagree about one concept.
    #[test]
    fn the_rendering_vocabulary_is_stable_and_total() {
        assert_eq!(tier_key(RuleTier::SelfImposed), "self");
        for tier in [
            RuleTier::Structural,
            RuleTier::Region,
            RuleTier::Admin,
            RuleTier::Guardian,
            RuleTier::SelfImposed,
        ] {
            assert!(!tier_key(tier).is_empty());
        }
        for d in [
            QuotaDimension::Operations,
            QuotaDimension::Counterparties,
            QuotaDimension::Volume,
        ] {
            assert!(!dimension_key(d).is_empty());
        }
        for w in [Window::Day, Window::Week, Window::Month] {
            assert!(!window_key(w).is_empty());
        }
        for a in [Availability::Allow, Availability::Deny, Availability::Limit] {
            assert!(!availability_key(&a).is_empty());
        }
        assert_eq!(
            availability_key(&Availability::Other("suspend".into())),
            "deny"
        );
        for u in [MagnitudeUnit::Bytes, MagnitudeUnit::Millisats] {
            assert!(!unit_key(u).is_empty());
        }
    }

    /// The web face serializes this shape straight to JS, so it has to survive
    /// a serde round trip — including the `LocalizedText` args map.
    #[test]
    fn a_row_round_trips_through_serde() {
        let feature = GatedFeature::P2pShare;
        let ops_day = entry(feature).tier1.operations.per_day.unwrap();
        let it = FeatureStatusItem {
            feature,
            policy: effective_policy(feature, &[], &[]),
            usage: fauna_core::feature_gate::UsageCounters {
                operations: fauna_core::feature_gate::WindowCounts {
                    day: ops_day,
                    week: ops_day,
                    month: ops_day,
                },
                ..Default::default()
            },
            extra: Default::default(),
        };

        let row = feature_row(&it, &p2p());
        assert_eq!(row.affordance, "disabled", "the day quota is spent");

        let json = serde_json::to_string(&row).expect("serialize");
        let back: FeatureRow = serde_json::from_str(&json).expect("deserialize");
        assert_eq!(back, row);
        assert_eq!(
            back.restriction.expect("a reason").args.get("window"),
            Some(&"features.window_day".to_string()),
            "the window argument must survive the boundary or the sentence renders with a hole"
        );
    }

    #[test]
    fn the_gated_set_is_listable_without_a_nest_read() {
        assert_eq!(gated_feature_keys(), vec!["payments", "zaps", "p2p-share"]);
    }

    // ── The render vocabulary ────────────────────────────────────────────

    /// Every key the derived fields can emit must exist in en.yaml — a typo
    /// compiles fine and paints the raw key on all 7 apps.
    #[test]
    fn every_derived_key_resolves() {
        let mut keys = vec![
            "features.section_title".to_string(),
            "features.empty".to_string(),
            "features.status_available".to_string(),
            "features.status_restricted".to_string(),
            "features.quota_label".to_string(),
            "features.quota_value".to_string(),
            "features.quota_value_exhausted".to_string(),
            "features.magnitude_sats".to_string(),
        ];
        for d in [
            QuotaDimension::Operations,
            QuotaDimension::Counterparties,
            QuotaDimension::Volume,
        ] {
            keys.push(dimension_label(d).key);
        }
        for w in [Window::Day, Window::Week, Window::Month] {
            keys.push(window_label(w).key);
        }
        for key in &keys {
            assert!(
                fauna_i18n::strings::lookup(key).is_some(),
                "{key} is not in i18n/strings/en.yaml — it would render as the raw key"
            );
        }
    }

    /// A cell's label is two nested keys, so plain `resolve` would paint
    /// "features.dimension_operations per features.window_day" — the exact
    /// leak `resolve_nested` exists for. Pinned end-to-end against the real
    /// string table rather than a stub.
    #[test]
    fn a_cell_label_resolves_both_of_its_nested_nouns() {
        let r = feature_row(&item(GatedFeature::P2pShare), &p2p());
        let day = r
            .cells
            .iter()
            .find(|c| c.dimension == "operations" && c.window == "day")
            .expect("p2p-share bounds operations per day");

        let rendered = day.label.resolve_nested(fauna_i18n::strings::lookup);
        assert_eq!(rendered, "Uses per day");
        assert!(
            !rendered.contains("features."),
            "a raw key reached the screen: {rendered:?}"
        );
    }

    /// The same leak on the row's own why-line, which is the sentence boundary
    /// 4 is actually about — "no silent gates" is not satisfied by a sentence
    /// with an i18n key in the middle of it.
    #[test]
    fn a_restriction_sentence_resolves_its_window_noun() {
        let feature = GatedFeature::P2pShare;
        let ops_day = entry(feature).tier1.operations.per_day.unwrap();
        let it = FeatureStatusItem {
            feature,
            policy: effective_policy(feature, &[], &[]),
            usage: UsageCounters {
                operations: fauna_core::feature_gate::WindowCounts {
                    day: ops_day,
                    week: ops_day,
                    month: ops_day,
                },
                ..Default::default()
            },
            extra: Default::default(),
        };

        let r = feature_row(&it, &p2p());
        let rendered = r
            .restriction
            .expect("a spent day quota blocks")
            .resolve_nested(fauna_i18n::strings::lookup);
        assert_eq!(
            rendered,
            "You've used up Fauna's built-in limit for this day."
        );
    }

    /// A bare-count cell needs no second level: `cell_value_text` must render
    /// it from `value` alone.
    #[test]
    fn a_counted_cell_renders_its_headroom_without_magnitudes() {
        let r = feature_row(&item(GatedFeature::P2pShare), &p2p());
        let day = r
            .cells
            .iter()
            .find(|c| c.dimension == "operations" && c.window == "day")
            .expect("operations/day");

        assert!(day.magnitudes.is_none(), "a count carries no unit");
        assert_eq!(
            cell_value_text(day, fauna_i18n::strings::lookup),
            format!("{} left of {}", day.limit, day.limit)
        );
    }

    /// The two-level path: a byte volume must reach the screen on the shared
    /// 1024-unit scale, never as a raw byte count. p2p-share's tier-1 volume is
    /// 1 TB/day, which is exactly the number a raw render would make unreadable.
    #[test]
    fn a_byte_volume_cell_renders_on_the_shared_size_scale() {
        let r = feature_row(&item(GatedFeature::P2pShare), &p2p());
        let vol = r
            .cells
            .iter()
            .find(|c| c.dimension == "volume" && c.window == "day")
            .expect("p2p-share bounds volume per day");

        assert!(vol.magnitudes.is_some(), "bytes carry a unit");
        // 10^12 bytes on a 1024-unit scale is 931.3 GB, not "1 TB" — the
        // registry constant is decimal and the shared scale is binary.
        let rendered = cell_value_text(vol, fauna_i18n::strings::lookup);
        assert_eq!(rendered, "931.3 GB left of 931.3 GB");
        assert!(
            !rendered.contains("1000000000000"),
            "a raw byte count reached the screen: {rendered:?}"
        );
    }

    /// Millisats are stored on the wire and read by people in sats — a raw
    /// msat number is off by three orders of magnitude to every reader.
    #[test]
    fn a_millisat_volume_cell_renders_in_sats() {
        let feature = GatedFeature::Zaps;
        let r = feature_row(&item(feature), &[capability::SUBSCRIPTIONS.to_string()]);
        let vol = r
            .cells
            .iter()
            .find(|c| c.dimension == "volume" && c.window == "day")
            .expect("zaps bounds volume per day");

        let msats = entry(feature).tier1.volume.per_day.unwrap();
        let rendered = cell_value_text(vol, fauna_i18n::strings::lookup);
        assert!(
            rendered.contains(&(msats / 1_000).to_string()) && rendered.contains("sats"),
            "expected a sat amount, got {rendered:?}"
        );
    }

    /// The status word is derived from the affordance, not the availability:
    /// a *bounded but unspent* feature is Available, and only something that
    /// actually blocks reads Restricted. A word taken from `availability`
    /// would call every limited member "Restricted" and make the column noise.
    #[test]
    fn the_status_word_follows_the_affordance_not_the_availability() {
        let available = feature_row(&item(GatedFeature::P2pShare), &p2p());
        assert_eq!(available.availability, "limit", "it is bounded");
        assert_eq!(available.affordance, "available");
        assert_eq!(available.status.key, "features.status_available");

        let denied = FeaturePolicy {
            availability: Availability::Deny,
            ..Default::default()
        };
        let it = FeatureStatusItem {
            feature: GatedFeature::P2pShare,
            policy: effective_policy(GatedFeature::P2pShare, &[(RuleTier::Admin, denied)], &[]),
            usage: UsageCounters::default(),
            extra: Default::default(),
        };
        assert_eq!(
            feature_row(&it, &p2p()).status.key,
            "features.status_restricted"
        );
    }

    /// The p2p-share twin of the payments hidden test: an excised nest omits
    /// the token (wormability rule 7 / excision criterion 4), and the limits
    /// screen hides the row rather than promising a plane the nest refuses.
    #[test]
    fn p2p_share_is_a_hidden_row_without_its_token() {
        let it = item(GatedFeature::P2pShare);
        assert_eq!(feature_row(&it, &[]).affordance, "hidden");
        assert_eq!(feature_row(&it, &p2p()).affordance, "available");
    }
}
