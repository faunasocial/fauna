//! Create-feed rule-builder presentation FFI — the picker catalog, the
//! added-rule summary chip, the required/excluded toggle label, and the
//! built-in ranking-factor options.
//!
//! The *encoding* half of the rule builder lives ungated in
//! [`crate::feed_client`] (`encode_filter_rule` / `normalize_filter_rules`, which
//! cross to Go). This module is the *presentation* half: every export here
//! returns a `fauna_core` [`LocalizedText`], so it must stay gated out of the Go
//! mail-bridge `--no-default-features` build — see the `feed-rules` note in
//! `lib.rs`.
//!
//! Mirrors follow [`crate::feed_client::FfiFilterRule`]'s precedent:
//! `fauna-client-feed` carries no `uniffi` feature, so its types cross the
//! boundary through `Ffi*` mirrors declared here rather than through derives on
//! the shared crate.

use fauna_core::localized::LocalizedText;

/// FFI mirror of [`fauna_client_feed::RuleInputKind`] — which input widget the
/// `feed-rule-value-input` / `feed-rule-required-toggle` row shows for the
/// selected rule type (`docs/goal/ui/feed.md:171-183`, the *Input shape*
/// column).
#[derive(uniffi::Enum, Clone, Copy, Debug, PartialEq, Eq)]
pub enum FfiRuleInputKind {
    /// Comma-separated terms — `HasHashtag` / `Source` / `BodyContains` /
    /// `BodyExcludes`.
    Text,
    /// A single integer — `MinReplies` / `MinReposts` / `CreatedAfter` (hours).
    Number,
    /// A required/excluded toggle — `HasMedia` / `IsReply`. The value entry has
    /// nothing to bind to: the encoder reads `required` and ignores `value`.
    Toggle,
    /// A category plus a 0–10 threshold, packed `"category:threshold"` —
    /// `LabelBelow` / `LabelAbove`.
    TextAndNumber,
}

impl From<fauna_client_feed::RuleInputKind> for FfiRuleInputKind {
    fn from(k: fauna_client_feed::RuleInputKind) -> Self {
        match k {
            fauna_client_feed::RuleInputKind::Text => FfiRuleInputKind::Text,
            fauna_client_feed::RuleInputKind::Number => FfiRuleInputKind::Number,
            fauna_client_feed::RuleInputKind::Toggle => FfiRuleInputKind::Toggle,
            fauna_client_feed::RuleInputKind::TextAndNumber => FfiRuleInputKind::TextAndNumber,
        }
    }
}

impl From<FfiRuleInputKind> for fauna_client_feed::RuleInputKind {
    fn from(k: FfiRuleInputKind) -> Self {
        match k {
            FfiRuleInputKind::Text => fauna_client_feed::RuleInputKind::Text,
            FfiRuleInputKind::Number => fauna_client_feed::RuleInputKind::Number,
            FfiRuleInputKind::Toggle => fauna_client_feed::RuleInputKind::Toggle,
            FfiRuleInputKind::TextAndNumber => fauna_client_feed::RuleInputKind::TextAndNumber,
        }
    }
}

/// FFI mirror of [`fauna_client_feed::RuleTypeOption`] — one
/// `feed-rule-type-select` option.
#[derive(uniffi::Record, Clone, Debug, PartialEq)]
pub struct FfiRuleTypeOption {
    /// The canonical wire value — the `FilterRule` variant name. What the select
    /// writes, what [`crate::feed_client::encode_filter_rule`] takes, and what
    /// the cross-app `select(id, value)` e2e contract drives
    /// (`ui.yaml:5303-5308`). Never localized.
    pub value: String,
    /// The picker label (`feed.rule_types.*`), for the client to resolve.
    pub label: LocalizedText,
    /// Which input widget this type's row shows.
    pub input_kind: FfiRuleInputKind,
}

impl From<fauna_client_feed::RuleTypeOption> for FfiRuleTypeOption {
    fn from(o: fauna_client_feed::RuleTypeOption) -> Self {
        FfiRuleTypeOption {
            value: o.value,
            label: o.label,
            input_kind: o.input_kind.into(),
        }
    }
}

/// UniFFI façade for [`fauna_client_feed::rule_type_options`] — the 11-type
/// `feed-rule-type-select` catalog (wire value + localized label + input kind).
/// Lets the Apple / Windows / Android forms drop their hand-rolled rule-type maps
/// and their ad-hoc `isToggle` / `isLabelRule` predicates.
#[uniffi::export]
pub fn rule_type_options() -> Vec<FfiRuleTypeOption> {
    fauna_client_feed::rule_type_options()
        .into_iter()
        .map(FfiRuleTypeOption::from)
        .collect()
}

/// FFI mirror of [`fauna_client_feed::FactorOption`] — one built-in
/// `feed-factor-select` option.
#[derive(uniffi::Record, Clone, Debug, PartialEq)]
pub struct FfiFactorOption {
    /// The factor key the select writes and `FactorWeightInput.factor`
    /// carries (`"engagement"`, `"trending"`). Never localized.
    pub value: String,
    /// The picker label (`feed.create.factor_*`), for the client to resolve.
    pub label: LocalizedText,
}

impl From<fauna_client_feed::FactorOption> for FfiFactorOption {
    fn from(o: fauna_client_feed::FactorOption) -> Self {
        FfiFactorOption {
            value: o.value,
            label: o.label,
        }
    }
}

/// UniFFI façade for [`fauna_client_feed::builtin_factor_options`] — the
/// built-in head of the `feed-factor-select` list (`engagement`, `trending`),
/// which an app follows with its subscribed labeler and trained-topic factors.
/// Replaces the `"engagement"` literal each native picker starts from.
#[uniffi::export]
pub fn builtin_factor_options() -> Vec<FfiFactorOption> {
    fauna_client_feed::builtin_factor_options()
        .into_iter()
        .map(FfiFactorOption::from)
        .collect()
}

/// UniFFI façade for [`fauna_client_feed::rule_required_label`] — the
/// `feed-rule-required-toggle` label, "Required" when the boolean rule demands
/// the trait and "Excluded" when it forbids it (`ui.yaml:5333-5334`). The nest
/// evaluates `required:false` as a genuine exclusion, so the static "Required"
/// the natives render today reads as the opposite of the rule the user built.
#[uniffi::export]
pub fn rule_required_label(required: bool) -> LocalizedText {
    fauna_client_feed::rule_required_label(required)
}

/// UniFFI façade for [`fauna_client_feed::rule_summary_label`] — the added-rule
/// summary chip for one `(rule_type, value, required)` triple, rendering
/// `feed.md:171-183`'s *Example display* prose (`#rust, #fauna`, `media: yes`,
/// `replies >= 5`) instead of the raw `rule_type` wire key the natives show.
#[uniffi::export]
pub fn rule_summary_label(rule_type: String, value: String, required: bool) -> LocalizedText {
    fauna_client_feed::rule_summary_label(&rule_type, &value, required)
}

/// UniFFI façade for [`fauna_client_feed::can_add_rule`] — whether the staged
/// `(input_kind, value, threshold)` create-feed rule inputs are complete enough
/// to enable `feed-add-rule-button` (`docs/goal/ui/feed.md` § Add-rule gating).
/// Apple's own `FeedCreateForm.canAddRule` is the lift source; android/windows
/// consume this so the button stops accepting an empty/unparseable rule.
#[uniffi::export]
pub fn can_add_rule(input_kind: FfiRuleInputKind, value: String, threshold: String) -> bool {
    fauna_client_feed::can_add_rule(input_kind.into(), &value, &threshold)
}

/// UniFFI façade for [`fauna_client_feed::DEFAULT_RULE_THRESHOLD`] — the
/// `feed-rule-threshold-input` prefill for the [`FfiRuleInputKind::TextAndNumber`]
/// rules (the `default_lapse_tier()` precedent, `value_format.rs`, for exposing a
/// shared constant to the UniFFI apps). android and windows hardcoded their
/// own `"5"` literal until this export existed.
#[uniffi::export]
pub fn default_rule_threshold() -> String {
    fauna_client_feed::DEFAULT_RULE_THRESHOLD.to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The mirror carries the shared catalog through unchanged — same order,
    /// same wire values, same input kinds.
    #[test]
    fn rule_type_options_mirror_the_shared_catalog() {
        let shared = fauna_client_feed::rule_type_options();
        let ffi = rule_type_options();
        assert_eq!(ffi.len(), shared.len());
        for (a, b) in ffi.iter().zip(shared.iter()) {
            assert_eq!(a.value, b.value);
            assert_eq!(a.label, b.label);
            assert_eq!(a.input_kind, b.input_kind.into());
        }
    }

    /// The mirror carries the built-in factor options through unchanged.
    #[test]
    fn builtin_factor_options_mirror_the_shared_catalog() {
        let shared = fauna_client_feed::builtin_factor_options();
        let ffi = builtin_factor_options();
        assert_eq!(ffi.len(), shared.len());
        for (a, b) in ffi.iter().zip(shared.iter()) {
            assert_eq!(a.value, b.value);
            assert_eq!(a.label, b.label);
        }
        assert!(ffi.iter().any(|o| o.value == "trending"));
    }

    /// The toggles are the only `Toggle`-kind rows — the pair ui.yaml:5334 names.
    #[test]
    fn only_the_boolean_rules_take_the_required_toggle() {
        let toggles: Vec<String> = rule_type_options()
            .into_iter()
            .filter(|o| o.input_kind == FfiRuleInputKind::Toggle)
            .map(|o| o.value)
            .collect();
        assert_eq!(toggles, ["HasMedia", "IsReply"]);
    }

    /// The FFI wrapper carries `can_add_rule`'s verdict through unchanged for
    /// every kind — a thin pass-through, but the `FfiRuleInputKind` ->
    /// `RuleInputKind` conversion is exactly what a reversed match arm could
    /// silently mismap.
    #[test]
    fn can_add_rule_mirrors_the_shared_predicate_for_every_kind() {
        assert!(!can_add_rule(FfiRuleInputKind::Text, "".into(), "".into()));
        assert!(can_add_rule(
            FfiRuleInputKind::Text,
            "rust".into(),
            "".into()
        ));
        assert!(!can_add_rule(
            FfiRuleInputKind::Number,
            "not-a-number".into(),
            "".into()
        ));
        assert!(can_add_rule(
            FfiRuleInputKind::Number,
            "5".into(),
            "".into()
        ));
        assert!(can_add_rule(FfiRuleInputKind::Toggle, "".into(), "".into()));
        assert!(!can_add_rule(
            FfiRuleInputKind::TextAndNumber,
            "spam".into(),
            "".into()
        ));
        assert!(can_add_rule(
            FfiRuleInputKind::TextAndNumber,
            "spam".into(),
            "5".into()
        ));
    }

    #[test]
    fn default_rule_threshold_mirrors_the_shared_constant() {
        assert_eq!(
            default_rule_threshold(),
            fauna_client_feed::DEFAULT_RULE_THRESHOLD
        );
    }
}
