//! Feed-rule codec — the create-feed form's `(type, value, required)` UI triple
//! ⇄ the typed [`FilterRule`] the `fauna.feed.*` wire carries (`rules:
//! Vec<FilterRule>`, externally tagged, native dag-cbor).
//!
//! Shared home for every native app (Windows / Apple / Android / Linux): the
//! Rust-native Linux app calls [`encode_filter_rules`] / [`decode_filter_rules`]
//! directly, while the UniFFI apps reach the identical logic through the thin
//! `#[uniffi::export]` wrappers in `libs/fauna-ffi/src/feed_client.rs`. Building
//! the real [`fauna_core::scoring::FilterRule`] (rather than hand-rolling a
//! shape per client) means a newly-added variant can't silently mis(de)code on
//! one app, and an unknown `rule_type` is an `Err`, not a `{}`.
//! [`decode_filter_rule`] is the inverse of [`encode_filter_rule`] (the
//! create-feed form round-trips through `fauna.feed.get`'s `rules` back into the
//! editable triples).
//!
//! Semantics pinned to `docs/goal/ui/feed.md` § Filter rule types (and the web
//! reference builder, which emits the identical shape inline): `CreatedAfter`
//! input is **hours** (`age_microseconds = hours × 3.6e9`); the label thresholds
//! ride a **0–10** scale (`× 100` → the per-mille `u16` the dag-cbor wire carries).

use fauna_core::localized::LocalizedText;
use fauna_core::scoring::FilterRule;
use serde::Serialize;

/// Split a comma-separated rule value into its trimmed, non-empty terms — the
/// list-variant input convention (`HasHashtag` / `Source` / `BodyContains` /
/// `BodyExcludes`). Shared by [`build_filter_rule`] and [`rule_summary_label`]
/// so the chip can't render terms the encoder wouldn't send.
fn split_csv(value: &str) -> Vec<String> {
    value
        .split(',')
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
        .collect()
}

/// Encode a single feed-rule `(rule_type, value, required)` UI triple into the
/// typed [`FilterRule`] the `fauna.feed.create` / `fauna.feed.update` `rules`
/// list carries. An unrecognized `rule_type`
/// returns `Err` (never a silent empty object), so a client offering a variant
/// the shared encoder doesn't know surfaces the mismatch.
///
/// `value` is the raw user input: comma-separated for the list variants
/// (`HasHashtag`/`Source`/`BodyContains`/`BodyExcludes`), an integer string for
/// `MinReplies`/`MinReposts` (`count`) and `CreatedAfter` (`age_microseconds`),
/// `"category:threshold"` (threshold on a `0`–`10` scale) for
/// `LabelBelow`/`LabelAbove`, and ignored for the toggles (`HasMedia`/`IsReply`,
/// which read `required`). Unparseable integers fall back to `1`; a missing
/// label threshold defaults to `5` (the midpoint, 500 ‰).
pub fn encode_filter_rule(
    rule_type: &str,
    value: &str,
    required: bool,
) -> Result<FilterRule, String> {
    build_filter_rule(rule_type, value.trim(), required)
        .ok_or_else(|| format!("unknown feed filter rule type: {rule_type}"))
}

/// Encode a create-feed form's whole rule list — [`encode_filter_rule`] over
/// each triple, in order — into the wire `rules` list. The first unknown
/// `rule_type` is the `Err` (the form never sends a partial list).
pub fn encode_filter_rules(rules: &[DecodedFilterRule]) -> Result<Vec<FilterRule>, String> {
    rules
        .iter()
        .map(|r| encode_filter_rule(&r.rule_type, &r.value, r.required))
        .collect()
}

/// Construct the `FilterRule` for a `(rule_type, value, required)` UI triple, or
/// `None` for an unrecognized `rule_type`. `value` is already trimmed.
///
/// Input conventions (`docs/goal/ui/feed.md` § Filter rule types, mirrored by the
/// web rule builder): comma-separated lists; integer `count` for the numeric
/// rules (defaulting to `1`, matching web/linux); `CreatedAfter` input is in
/// **hours**; the label rules pack `"category:threshold"` with `threshold` on a
/// **0–10** scale (see [`parse_label`]).
fn build_filter_rule(rule_type: &str, value: &str, required: bool) -> Option<FilterRule> {
    let csv = || split_csv(value);
    // Numeric value → count, defaulting to 1 (matches the web / linux builders).
    let count = || value.parse::<u64>().unwrap_or(1);
    Some(match rule_type {
        "HasHashtag" => FilterRule::HasHashtag { tags: csv() },
        "Source" => FilterRule::Source { protocols: csv() },
        "BodyContains" => FilterRule::BodyContains { terms: csv() },
        "BodyExcludes" => FilterRule::BodyExcludes { terms: csv() },
        "HasMedia" => FilterRule::HasMedia { required },
        "IsReply" => FilterRule::IsReply { required },
        "MinReplies" => FilterRule::MinReplies {
            count: count() as u32,
        },
        "MinReposts" => FilterRule::MinReposts {
            count: count() as u32,
        },
        "CreatedAfter" => FilterRule::CreatedAfter {
            age_microseconds: count().saturating_mul(3_600_000_000),
        },
        "LabelBelow" => {
            let (category, permille) = parse_label(value);
            FilterRule::LabelBelow {
                category,
                max_confidence_permille: permille,
            }
        }
        "LabelAbove" => {
            let (category, permille) = parse_label(value);
            FilterRule::LabelAbove {
                category,
                min_confidence_permille: permille,
            }
        }
        _ => return None,
    })
}

/// Split the native single-field `"category:threshold"` packing for the label
/// rules. `threshold` rides a **0–10** scale (`feed.md` § Filter rule types) and
/// converts to the float-free per-mille `u16` the dag-cbor wire carries (`× 100`,
/// clamped to `0..=1000`). A blank category defaults to `spam`; a missing or
/// unparseable threshold defaults to `5` (the midpoint, 500 ‰).
fn parse_label(value: &str) -> (String, u16) {
    let (category, threshold_str) = match value.split_once(':') {
        Some((c, t)) => (c.trim(), t.trim()),
        None => (value.trim(), ""),
    };
    let category = if category.is_empty() {
        "spam".to_string()
    } else {
        category.to_string()
    };
    let threshold: f64 = threshold_str.parse().unwrap_or(5.0);
    let permille = (threshold * 100.0).round().clamp(0.0, 1000.0) as u16;
    (category, permille)
}

/// How a rule's value is entered in the create-feed form — which widget the
/// `feed-rule-value-input` / `feed-rule-required-toggle` row shows for the
/// selected type (`docs/goal/ui/feed.md:171-183` § Feed-rule types, the *Input
/// shape* column). Mirrors the arms [`build_filter_rule`] actually reads, so the
/// form can't offer an input the encoder ignores.
///
/// Shared because every app re-derived it from ad-hoc predicates: windows
/// inlined `isToggle = sel == "HasMedia" || sel == "IsReply"` twice, linux
/// computed only `is_label` (and so showed the value entry **and** the required
/// toggle for all 11 types, including the numerics where the encoder reads
/// neither), android only `isLabelRule`, web a 7-branch `{#if}` ladder. Apple
/// alone modelled it as data — this is apple's `RuleInputKind`, lifted.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize)]
pub enum RuleInputKind {
    /// Comma-separated terms — `HasHashtag` / `Source` / `BodyContains` /
    /// `BodyExcludes` (the [`split_csv`] arms).
    Text,
    /// A single integer — `MinReplies` / `MinReposts` (`count`) and
    /// `CreatedAfter` (**hours**). Unparseable input falls back to `1`.
    Number,
    /// A required/excluded toggle — `HasMedia` / `IsReply`. These read `required`
    /// and **ignore** `value`, so the value entry has nothing to bind to.
    Toggle,
    /// A category plus a 0–10 threshold, packed `"category:threshold"` —
    /// `LabelBelow` / `LabelAbove` (the [`parse_label`] arms).
    TextAndNumber,
}

/// The `feed-rule-threshold-input` prefill for the [`RuleInputKind::TextAndNumber`]
/// rules — the midpoint of the 0–10 confidence scale.
///
/// Shared because every app independently hardcoded the same `"5"` (linux's
/// `set_text`, windows' reset, android's `mutableStateOf`), which is exactly the
/// per-app literal priority #2 exists to collapse. A prefill rather than an empty
/// field on purpose: `feed-add-rule-button` gates on a *parseable* threshold, so
/// an empty default would present the add button as disabled the moment a user
/// picks LabelBelow.
pub const DEFAULT_RULE_THRESHOLD: &str = "5";

/// Whether the staged `(input_kind, value, threshold)` create-feed rule inputs
/// are complete enough to enable `feed-add-rule-button` — apple's
/// `FeedCreateForm.canAddRule`, lifted (`docs/goal/ui/feed.md` § Add-rule
/// gating). Apple was the only client that gated this button; the other six
/// let a user stage an empty/unparseable rule, which [`build_filter_rule`]
/// then silently papers over (a blank category defaults to `"spam"`, an
/// unparseable count/threshold defaults to `1`/the midpoint) rather than
/// refusing — so a junk Add used to stage a real, wrong rule instead of doing
/// nothing.
///
/// `value` is what `feed-rule-value-input` holds; `threshold` is
/// `feed-rule-threshold-input`, read only for [`RuleInputKind::TextAndNumber`]
/// — the other three kinds ignore it, mirroring the per-kind reads in
/// [`build_filter_rule`].
pub fn can_add_rule(input_kind: RuleInputKind, value: &str, threshold: &str) -> bool {
    match input_kind {
        RuleInputKind::Text => !value.trim().is_empty(),
        RuleInputKind::Number => value.trim().parse::<i64>().is_ok(),
        RuleInputKind::Toggle => true,
        RuleInputKind::TextAndNumber => {
            !value.trim().is_empty() && threshold.trim().parse::<f64>().is_ok()
        }
    }
}

/// One `feed-rule-type-select` option: the canonical wire value, its localized
/// picker label, and the input widget the form shows for it. The
/// `ConflictPolicyOption` shape.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct RuleTypeOption {
    /// The canonical wire value — the [`FilterRule`] variant name
    /// (`"HasHashtag"` …). What the select writes, what [`encode_filter_rule`]
    /// takes, and what the cross-app `select(id, value)` e2e contract drives
    /// (`ui.yaml:5303-5308` — "the action layer passes the variant name directly
    /// as the select value"). Never localized; [`RuleTypeOption::label`] is what
    /// the user reads.
    pub value: String,
    /// The picker label (`feed.rule_types.*` — the catalog already shipping in
    /// every app bundle, `i18n/strings/en.yaml:675-686`).
    pub label: LocalizedText,
    /// Which input widget the `(value, required)` row shows for this type.
    pub input_kind: RuleInputKind,
}

/// The canonical `feed-rule-type-select` catalog: the 11 types ui.yaml pins
/// (`:5303-5308`), in `feed.md`'s table order (`:171-183`), each with its
/// `feed.rule_types.*` label key and the input widget its row shows.
///
/// The wire-only [`FilterRule`] variants (`BodyHint` / `AuthorInSet` /
/// `AuthorNotInSet` / `HasLabel`) are deliberately absent — no
/// [`build_filter_rule`] arm authors them, so the form must not offer them
/// (`rule_type_options_all_encode_and_round_trip` pins that).
const RULE_TYPES: [(&str, &str, RuleInputKind); 11] = [
    (
        "HasHashtag",
        "feed.rule_types.has_hashtag",
        RuleInputKind::Text,
    ),
    ("Source", "feed.rule_types.source", RuleInputKind::Text),
    (
        "HasMedia",
        "feed.rule_types.has_media",
        RuleInputKind::Toggle,
    ),
    ("IsReply", "feed.rule_types.is_reply", RuleInputKind::Toggle),
    (
        "MinReplies",
        "feed.rule_types.min_replies",
        RuleInputKind::Number,
    ),
    (
        "MinReposts",
        "feed.rule_types.min_reposts",
        RuleInputKind::Number,
    ),
    (
        "CreatedAfter",
        "feed.rule_types.created_after",
        RuleInputKind::Number,
    ),
    (
        "BodyContains",
        "feed.rule_types.body_contains",
        RuleInputKind::Text,
    ),
    (
        "BodyExcludes",
        "feed.rule_types.body_excludes",
        RuleInputKind::Text,
    ),
    (
        "LabelBelow",
        "feed.rule_types.label_below",
        RuleInputKind::TextAndNumber,
    ),
    (
        "LabelAbove",
        "feed.rule_types.label_above",
        RuleInputKind::TextAndNumber,
    ),
];

/// The canonical rule-type picker catalog — wire value + localized label + input
/// kind for each of the 11 types the create-feed form offers. Every app binds
/// `feed-rule-type-select` to this and picks its value/toggle widgets off
/// [`RuleTypeOption::input_kind`], instead of re-deriving a type map and an
/// ad-hoc `isToggle`/`isLabelRule` predicate.
pub fn rule_type_options() -> Vec<RuleTypeOption> {
    RULE_TYPES
        .iter()
        .map(|(value, label_key, input_kind)| RuleTypeOption {
            value: (*value).to_string(),
            label: LocalizedText::key(*label_key),
            input_kind: *input_kind,
        })
        .collect()
}

/// The `feed-rule-required-toggle` label — "Required" when the boolean rule
/// demands the trait, "Excluded" when it forbids it.
///
/// **The toggle flips a real exclusion, not a "don't care".** `HasMedia {
/// required: false }` compiles to `cm.has_media = 0`
/// (`bins/fauna-nest/src/db/feeds.rs:562-566`) — "the post must **not** have
/// media" — so the static "Required" four apps render reads as the exact
/// opposite of the rule the user just built. ui.yaml:5333-5334 names it a
/// "Required/excluded toggle"; both keys already ship (`en.yaml:623-624`), and
/// only apple used the second one. The [`fauna_core::format::follow_toggle_label`]
/// shape: only the bool→label map is shared, the toggle *style* stays an
/// idiomatic per-app render.
pub fn rule_required_label(required: bool) -> LocalizedText {
    if required {
        LocalizedText::key("feed.create.rule_required")
    } else {
        LocalizedText::key("feed.create.rule_excluded")
    }
}

/// The added-rule summary chip for one `(rule_type, value, required)` triple —
/// `feed.md:171-183`'s *Example display* column (`#rust, #fauna`, `source: …`,
/// `media: yes`, `replies >= 5`, `age < 24h`, `contains: …`).
///
/// Shared because four of five apps showed the **raw wire key** here
/// (`HasHashtag`, `LabelBelow` — internal enum identifiers) and the fifth, web,
/// had the canonical prose hard-coded in English. The goal doc's Example-display
/// column *is* web's output — it was the reference builder (see the module doc) —
/// so this lifts web's prose into `LocalizedText` rather than the raw-key
/// majority (priority #4: the richest existing pattern, not the simplest).
///
/// The toggles carry two keys rather than one key plus a `yes`/`no` argument:
/// [`LocalizedText`] args are plain strings, so a nested translatable arm would
/// need a resolved key inside an arg — which the type deliberately can't express.
/// An unknown type degrades to the raw key (the `conflict_badge_label`
/// convention — `LocalizedText::resolve` falls back to the raw string), matching
/// the web builder's `default: return r.rule_type`.
pub fn rule_summary_label(rule_type: &str, value: &str, required: bool) -> LocalizedText {
    let value = value.trim();
    match rule_type {
        "HasHashtag" => {
            let tags = split_csv(value);
            let rendered = if tags.is_empty() {
                String::new()
            } else {
                format!("#{}", tags.join(", #"))
            };
            LocalizedText::key_arg("feed.rule_chip.has_hashtag", "tags", rendered)
        }
        "Source" => LocalizedText::key_arg("feed.rule_chip.source", "value", value),
        "HasMedia" if required => LocalizedText::key("feed.rule_chip.has_media_yes"),
        "HasMedia" => LocalizedText::key("feed.rule_chip.has_media_no"),
        "IsReply" if required => LocalizedText::key("feed.rule_chip.is_reply_yes"),
        "IsReply" => LocalizedText::key("feed.rule_chip.is_reply_no"),
        "MinReplies" => LocalizedText::key_arg("feed.rule_chip.min_replies", "count", value),
        "MinReposts" => LocalizedText::key_arg("feed.rule_chip.min_reposts", "count", value),
        "CreatedAfter" => LocalizedText::key_arg("feed.rule_chip.created_after", "hours", value),
        "BodyContains" => LocalizedText::key_arg("feed.rule_chip.body_contains", "value", value),
        "BodyExcludes" => LocalizedText::key_arg("feed.rule_chip.body_excludes", "value", value),
        "LabelBelow" => LocalizedText::key_arg("feed.rule_chip.label_below", "value", value),
        "LabelAbove" => LocalizedText::key_arg("feed.rule_chip.label_above", "value", value),
        other => LocalizedText::key(other),
    }
}

/// The create-feed form's `(rule_type, value, required)` UI triple — the inverse
/// of [`encode_filter_rule`]'s input. Decoding a wire `FilterRule` back to this
/// triple lets a client populate the feed-edit form from `fauna.feed.get`'s
/// `rules` (and re-encoding the triple round-trips to the same wire shape).
/// The same struct is [`encode_filter_rules`]'s input.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DecodedFilterRule {
    pub rule_type: String,
    pub value: String,
    pub required: bool,
}

/// Decode `fauna.feed.get`'s typed `rules` list into the `(rule_type, value,
/// required)` UI triples a feed-edit form binds to — [`decode_filter_rule`] over
/// each rule, the inverse of [`encode_filter_rules`]. Infallible: the wire
/// decoder already refused any rule of unknown shape.
pub fn decode_filter_rules(rules: &[FilterRule]) -> Vec<DecodedFilterRule> {
    rules.iter().map(decode_filter_rule).collect()
}

/// Decode one wire [`FilterRule`] back to the `(rule_type, value, required)` UI
/// triple. The exact inverse of [`build_filter_rule`]: list variants re-join with
/// `", "`, `CreatedAfter` converts µs → hours, and the label rules re-pack
/// `"category:threshold"` on the 0–10 scale (per-mille ÷ 100, via [`format_label`]).
/// Variants the create-feed form can't author (no [`build_filter_rule`] arm)
/// decode best-effort so a feed carrying one still round-trips its *other* rules.
pub fn decode_filter_rule(rule: &FilterRule) -> DecodedFilterRule {
    let triple = |rule_type: &str, value: String, required: bool| DecodedFilterRule {
        rule_type: rule_type.to_string(),
        value,
        required,
    };
    match rule {
        FilterRule::HasHashtag { tags } => triple("HasHashtag", tags.join(", "), false),
        FilterRule::Source { protocols } => triple("Source", protocols.join(", "), false),
        FilterRule::BodyContains { terms } => triple("BodyContains", terms.join(", "), false),
        FilterRule::BodyExcludes { terms } => triple("BodyExcludes", terms.join(", "), false),
        FilterRule::HasMedia { required } => triple("HasMedia", String::new(), *required),
        FilterRule::IsReply { required } => triple("IsReply", String::new(), *required),
        FilterRule::MinReplies { count } => triple("MinReplies", count.to_string(), false),
        FilterRule::MinReposts { count } => triple("MinReposts", count.to_string(), false),
        FilterRule::CreatedAfter { age_microseconds } => triple(
            "CreatedAfter",
            (age_microseconds / 3_600_000_000).to_string(),
            false,
        ),
        FilterRule::LabelBelow {
            category,
            max_confidence_permille,
        } => triple(
            "LabelBelow",
            format_label(category, *max_confidence_permille),
            false,
        ),
        FilterRule::LabelAbove {
            category,
            min_confidence_permille,
        } => triple(
            "LabelAbove",
            format_label(category, *min_confidence_permille),
            false,
        ),
        // No `build_filter_rule` arm offers these — decode best-effort.
        FilterRule::HasLabel { category } => triple("HasLabel", category.clone(), false),
        FilterRule::BodyHint { .. } => triple("BodyHint", String::new(), false),
        FilterRule::AuthorInSet { .. } => triple("AuthorInSet", String::new(), false),
        FilterRule::AuthorNotInSet { .. } => triple("AuthorNotInSet", String::new(), false),
        // A rule a newer writer added (`transport.md` § Rule 3 in full): no
        // `build_filter_rule` arm authors it either, so re-encoding the form
        // refuses rather than dropping it — the carried rule stays on the nest.
        FilterRule::Unknown(_) => triple("Unknown", String::new(), false),
    }
}

/// Re-pack a label rule's `(category, permille)` into the form's
/// `"category:threshold"` value. `threshold` rides the 0–10 scale ([`parse_label`]
/// is the inverse: `× 100` → per-mille), rendered float-free as the minimal
/// decimal so it round-trips: `500 → "5"`, `550 → "5.5"`, `525 → "5.25"`.
fn format_label(category: &str, permille: u16) -> String {
    let whole = permille / 100;
    let frac = permille % 100;
    if frac == 0 {
        format!("{category}:{whole}")
    } else if frac.is_multiple_of(10) {
        format!("{category}:{whole}.{}", frac / 10)
    } else {
        format!("{category}:{whole}.{frac:02}")
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::{Value, json};

    // Semantics pinned to `docs/goal/ui/feed.md` § Filter rule types and the web
    // reference (`apps/fauna-web/src/routes/feed/+page.svelte`): `CreatedAfter`
    // input is **hours** (`age_microseconds = hours × 3.6e9`), and label
    // thresholds ride a **0–10** scale converting to per-mille `u16` (`× 100`).
    // The typed rule is rendered to a `serde_json::Value` so the assertions pin
    // the externally-tagged wire shape `fauna.feed.create` deserializes into a
    // `Vec<FilterRule>`, independent of serde's formatting.
    fn enc(rule_type: &str, value: &str, required: bool) -> Value {
        let rule = encode_filter_rule(rule_type, value, required)
            .expect("encode should succeed for a known rule type");
        serde_json::to_value(rule).expect("a FilterRule renders to JSON")
    }

    #[test]
    fn body_contains_splits_csv_into_terms() {
        assert_eq!(
            enc("BodyContains", "rust, svelte", false),
            json!({ "BodyContains": { "terms": ["rust", "svelte"] } })
        );
    }

    #[test]
    fn body_excludes_splits_csv_into_terms() {
        assert_eq!(
            enc("BodyExcludes", "spam", false),
            json!({ "BodyExcludes": { "terms": ["spam"] } })
        );
    }

    #[test]
    fn has_hashtag_emits_tags_and_trims_empty_tokens() {
        // Whitespace-only and trailing-comma tokens are dropped.
        assert_eq!(
            enc("HasHashtag", "photography, , travel,", false),
            json!({ "HasHashtag": { "tags": ["photography", "travel"] } })
        );
    }

    #[test]
    fn source_splits_comma_separated_protocols() {
        assert_eq!(
            enc("Source", "fauna, bluesky", false),
            json!({ "Source": { "protocols": ["fauna", "bluesky"] } })
        );
    }

    #[test]
    fn has_media_carries_required_bool() {
        assert_eq!(
            enc("HasMedia", "", true),
            json!({ "HasMedia": { "required": true } })
        );
    }

    #[test]
    fn is_reply_carries_required_bool() {
        assert_eq!(
            enc("IsReply", "", false),
            json!({ "IsReply": { "required": false } })
        );
    }

    #[test]
    fn min_replies_parses_count() {
        assert_eq!(
            enc("MinReplies", "5", false),
            json!({ "MinReplies": { "count": 5 } })
        );
    }

    #[test]
    fn min_reposts_parses_count() {
        assert_eq!(
            enc("MinReposts", "12", false),
            json!({ "MinReposts": { "count": 12 } })
        );
    }

    #[test]
    fn unparseable_count_falls_back_to_one() {
        // Matches the web / linux rule builders (`unwrap_or(1)`).
        assert_eq!(
            enc("MinReplies", "not-a-number", false),
            json!({ "MinReplies": { "count": 1 } })
        );
    }

    #[test]
    fn created_after_converts_hours_to_microseconds() {
        // feed.md: input is hours; 24 h → 24 × 3.6e9 µs = 86_400_000_000.
        assert_eq!(
            enc("CreatedAfter", "24", false),
            json!({ "CreatedAfter": { "age_microseconds": 86_400_000_000_u64 } })
        );
    }

    #[test]
    fn label_below_splits_category_and_converts_threshold_to_permille() {
        // 0–10 scale: threshold 5 → 5 × 100 = 500 ‰.
        assert_eq!(
            enc("LabelBelow", "spam:5", false),
            json!({ "LabelBelow": { "category": "spam", "max_confidence_permille": 500 } })
        );
    }

    #[test]
    fn label_above_splits_category_and_converts_threshold_to_permille() {
        // Threshold 8 → 800 ‰.
        assert_eq!(
            enc("LabelAbove", "nsfw:8", false),
            json!({ "LabelAbove": { "category": "nsfw", "min_confidence_permille": 800 } })
        );
    }

    #[test]
    fn label_without_threshold_defaults_to_midpoint() {
        // No `:threshold` → defaults to 5 (500 ‰), matching linux's hardcoded 500.
        assert_eq!(
            enc("LabelBelow", "spam", false),
            json!({ "LabelBelow": { "category": "spam", "max_confidence_permille": 500 } })
        );
    }

    #[test]
    fn label_empty_value_defaults_category_to_spam() {
        assert_eq!(
            enc("LabelBelow", "", false),
            json!({ "LabelBelow": { "category": "spam", "max_confidence_permille": 500 } })
        );
    }

    #[test]
    fn label_threshold_clamps_above_ten() {
        // Out-of-range input clamps to the 1000 ‰ ceiling (no misencode).
        assert_eq!(
            enc("LabelAbove", "nsfw:17", false),
            json!({ "LabelAbove": { "category": "nsfw", "min_confidence_permille": 1000 } })
        );
    }

    #[test]
    fn unknown_rule_type_is_err_not_silent_empty_object() {
        assert!(encode_filter_rule("Bogus", "x", false).is_err());
    }

    // ── Decode (the inverse of encode) ──────────────────────────────────────
    //
    // The strongest guard is round-trip: a form triple → `encode_filter_rule` →
    // wire JSON → `decode_filter_rule` must return the *same* triple, so a feed
    // saved on one app edits back identically on another. (CSV spacing,
    // CreatedAfter hours, and the label 0–10 scale all have to invert exactly.)

    /// Encode a triple to wire JSON, decode the single rule back to a triple.
    fn round_trip(rule_type: &str, value: &str, required: bool) -> DecodedFilterRule {
        let rule = encode_filter_rule(rule_type, value, required).expect("encode");
        decode_filter_rule(&rule)
    }

    fn triple(rule_type: &str, value: &str, required: bool) -> DecodedFilterRule {
        DecodedFilterRule {
            rule_type: rule_type.to_string(),
            value: value.to_string(),
            required,
        }
    }

    #[test]
    fn round_trip_csv_variants_normalize_spacing() {
        // Encode trims/drops empty tokens; decode re-joins with ", ".
        assert_eq!(
            round_trip("BodyContains", "rust,svelte", false),
            triple("BodyContains", "rust, svelte", false)
        );
        assert_eq!(
            round_trip("HasHashtag", "photography, , travel,", false),
            triple("HasHashtag", "photography, travel", false)
        );
        assert_eq!(
            round_trip("Source", "fauna, bluesky", false),
            triple("Source", "fauna, bluesky", false)
        );
    }

    #[test]
    fn round_trip_toggle_variants_carry_required() {
        assert_eq!(
            round_trip("HasMedia", "", true),
            triple("HasMedia", "", true)
        );
        assert_eq!(
            round_trip("IsReply", "", false),
            triple("IsReply", "", false)
        );
    }

    #[test]
    fn round_trip_count_variants() {
        assert_eq!(
            round_trip("MinReplies", "5", false),
            triple("MinReplies", "5", false)
        );
        assert_eq!(
            round_trip("MinReposts", "12", false),
            triple("MinReposts", "12", false)
        );
    }

    #[test]
    fn round_trip_created_after_hours() {
        // 24 h → 86_400_000_000 µs → back to "24".
        assert_eq!(
            round_trip("CreatedAfter", "24", false),
            triple("CreatedAfter", "24", false)
        );
    }

    #[test]
    fn round_trip_label_thresholds_on_zero_to_ten_scale() {
        assert_eq!(
            round_trip("LabelBelow", "spam:5", false),
            triple("LabelBelow", "spam:5", false)
        );
        assert_eq!(
            round_trip("LabelAbove", "nsfw:8", false),
            triple("LabelAbove", "nsfw:8", false)
        );
    }

    #[test]
    fn decode_label_formats_minimal_decimal() {
        // 500 ‰ → "5", 550 ‰ → "5.5", 525 ‰ → "5.25", 1000 ‰ → "10", 0 ‰ → "0".
        let fmt = |permille: u16| {
            decode_filter_rule(&FilterRule::LabelBelow {
                category: "spam".into(),
                max_confidence_permille: permille,
            })
            .value
        };
        assert_eq!(fmt(500), "spam:5");
        assert_eq!(fmt(550), "spam:5.5");
        assert_eq!(fmt(525), "spam:5.25");
        assert_eq!(fmt(505), "spam:5.05");
        assert_eq!(fmt(1000), "spam:10");
        assert_eq!(fmt(0), "spam:0");
    }

    #[test]
    fn decode_filter_rules_handles_the_list() {
        let rules = [
            FilterRule::BodyContains {
                terms: vec!["rust".into()],
            },
            FilterRule::HasMedia { required: true },
        ];
        assert_eq!(
            decode_filter_rules(&rules),
            vec![
                triple("BodyContains", "rust", false),
                triple("HasMedia", "", true),
            ]
        );
    }

    #[test]
    fn decode_filter_rules_empty_list() {
        assert_eq!(decode_filter_rules(&[]), vec![]);
    }

    #[test]
    fn encode_filter_rules_round_trips_the_form_list() {
        let form = vec![
            triple("BodyContains", "rust, svelte", false),
            triple("IsReply", "", true),
            triple("LabelAbove", "nsfw:8", false),
        ];
        let rules = encode_filter_rules(&form).expect("known rule types");
        assert_eq!(rules.len(), 3);
        assert_eq!(decode_filter_rules(&rules), form);
    }

    #[test]
    fn encode_filter_rules_refuses_an_unknown_type() {
        let form = [triple("HasMedia", "", true), triple("Bogus", "x", false)];
        assert!(encode_filter_rules(&form).is_err());
    }

    #[test]
    fn decode_has_label_variant_not_offered_by_the_form() {
        // `HasLabel` has no encoder arm; decode best-effort to category value.
        assert_eq!(
            decode_filter_rule(&FilterRule::HasLabel {
                category: "nsfw".into(),
            }),
            triple("HasLabel", "nsfw", false)
        );
    }

    /// The catalog is the 11 rule types ui.yaml pins as the `feed-rule-type-select`
    /// values (`ui.yaml:5303-5308`), in `feed.md`'s table order (`:171-183`).
    /// Pinned as a literal so a reorder/rename fails here rather than silently
    /// re-labelling every app's picker.
    #[test]
    fn rule_type_options_match_the_ui_yaml_catalog() {
        let values: Vec<String> = rule_type_options().into_iter().map(|o| o.value).collect();
        assert_eq!(
            values,
            [
                "HasHashtag",
                "Source",
                "HasMedia",
                "IsReply",
                "MinReplies",
                "MinReposts",
                "CreatedAfter",
                "BodyContains",
                "BodyExcludes",
                "LabelBelow",
                "LabelAbove",
            ]
        );
    }

    /// The catalog can't drift from the encoder: every offered type must encode
    /// (catalog ⊆ `build_filter_rule`), and its wire key must survive the
    /// `decode_filter_rule` round-trip (so the edit form re-selects the same row).
    #[test]
    fn rule_type_options_all_encode_and_round_trip() {
        for opt in rule_type_options() {
            let rule = build_filter_rule(&opt.value, "spam:5", true).unwrap_or_else(|| {
                panic!(
                    "catalog offers {} but build_filter_rule has no arm",
                    opt.value
                )
            });
            assert_eq!(
                decode_filter_rule(&rule).rule_type,
                opt.value,
                "{} does not round-trip through decode_filter_rule",
                opt.value
            );
        }
    }

    /// The input kind must agree with the shape the encoder actually reads for
    /// that variant (`feed.md:171-183` Input shape; `encoder.rs:27-33`): the
    /// toggles read `required` and ignore `value`; the numerics parse an integer;
    /// the label rules pack `"category:threshold"`; the rest are comma-separated.
    /// This is the anti-drift coupling — a variant whose encoder arm changes shape
    /// without a catalog update fails here.
    #[test]
    fn rule_input_kind_matches_the_encoder_arm_shape() {
        for opt in rule_type_options() {
            let rule = build_filter_rule(&opt.value, "spam:5", true).unwrap();
            let expected = match rule {
                FilterRule::HasMedia { .. } | FilterRule::IsReply { .. } => RuleInputKind::Toggle,
                FilterRule::MinReplies { .. }
                | FilterRule::MinReposts { .. }
                | FilterRule::CreatedAfter { .. } => RuleInputKind::Number,
                FilterRule::LabelBelow { .. } | FilterRule::LabelAbove { .. } => {
                    RuleInputKind::TextAndNumber
                }
                _ => RuleInputKind::Text,
            };
            assert_eq!(opt.input_kind, expected, "input kind for {}", opt.value);
        }
    }

    /// Every catalog label is a `feed.rule_types.*` key — the catalog that already
    /// ships in all five app bundles (`i18n/strings/en.yaml:675-686`).
    #[test]
    fn rule_type_options_label_from_the_shipped_i18n_catalog() {
        for opt in rule_type_options() {
            assert!(
                opt.label.key.starts_with("feed.rule_types."),
                "{} labelled with {}",
                opt.value,
                opt.label.key
            );
            assert!(opt.label.args.is_empty());
        }
    }

    /// The `feed-rule-required-toggle` label flips — ui.yaml:5333-5334 calls it a
    /// "Required/excluded toggle", and the nest evaluates `required:false` as a
    /// genuine exclusion (`cm.has_media = 0`, `feeds.rs:562-566`), so a static
    /// "Required" reads as the opposite of the rule the user built. The
    /// `follow_toggle_label` shape (`fauna_core::format:1003`); both keys already
    /// ship (`en.yaml:623-624`).
    #[test]
    fn rule_required_label_flips_on_state() {
        assert_eq!(rule_required_label(true).key, "feed.create.rule_required");
        assert_eq!(rule_required_label(false).key, "feed.create.rule_excluded");
    }

    /// The added-rule chip renders `feed.md:173-181`'s Example-display column —
    /// the canonical prose the web reference builder already emits — never the raw
    /// `rule_type` wire key the four natives show today.
    #[test]
    fn rule_summary_label_renders_the_goal_doc_example_display() {
        // `#rust, #fauna` — the tags are re-prefixed and joined, not raw csv.
        let hashtag = rule_summary_label("HasHashtag", "rust, fauna", false);
        assert_eq!(hashtag.key, "feed.rule_chip.has_hashtag");
        assert_eq!(
            hashtag.args.get("tags").map(String::as_str),
            Some("#rust, #fauna")
        );

        // `source: fauna, bluesky`
        let source = rule_summary_label("Source", "fauna, bluesky", false);
        assert_eq!(source.key, "feed.rule_chip.source");
        assert_eq!(
            source.args.get("value").map(String::as_str),
            Some("fauna, bluesky")
        );

        // `media: yes` / `reply: no` — the value is ignored; `required` picks the
        // key (two keys, not a nested LocalizedText — args are plain strings).
        assert_eq!(
            rule_summary_label("HasMedia", "", true).key,
            "feed.rule_chip.has_media_yes"
        );
        assert_eq!(
            rule_summary_label("HasMedia", "", false).key,
            "feed.rule_chip.has_media_no"
        );
        assert_eq!(
            rule_summary_label("IsReply", "", false).key,
            "feed.rule_chip.is_reply_no"
        );

        // `replies >= 5`, `reposts >= 3`, `age < 24h`
        let replies = rule_summary_label("MinReplies", "5", false);
        assert_eq!(replies.key, "feed.rule_chip.min_replies");
        assert_eq!(replies.args.get("count").map(String::as_str), Some("5"));
        assert_eq!(
            rule_summary_label("MinReposts", "3", false).key,
            "feed.rule_chip.min_reposts"
        );
        let age = rule_summary_label("CreatedAfter", "24", false);
        assert_eq!(age.key, "feed.rule_chip.created_after");
        assert_eq!(age.args.get("hours").map(String::as_str), Some("24"));

        // `contains: fauna, mls` / `excludes: spam`
        assert_eq!(
            rule_summary_label("BodyContains", "fauna, mls", false).key,
            "feed.rule_chip.body_contains"
        );
        assert_eq!(
            rule_summary_label("BodyExcludes", "spam", false).key,
            "feed.rule_chip.body_excludes"
        );

        // The label rules keep the `"category:threshold"` packing in the chip.
        let below = rule_summary_label("LabelBelow", "spam:5", false);
        assert_eq!(below.key, "feed.rule_chip.label_below");
        assert_eq!(below.args.get("value").map(String::as_str), Some("spam:5"));
        assert_eq!(
            rule_summary_label("LabelAbove", "spam:5", false).key,
            "feed.rule_chip.label_above"
        );
    }

    // ── can_add_rule (the lifted `FeedCreateForm.canAddRule` gate) ──────────
    //
    // Mirrors `FeedCreateFormTests.swift`'s `canAddRule*` cases (the apple
    // predicate this lifts), so the two stay behaviorally identical.

    #[test]
    fn can_add_rule_text_requires_non_empty_value() {
        assert!(!can_add_rule(RuleInputKind::Text, "", ""));
        assert!(!can_add_rule(RuleInputKind::Text, "   ", ""));
        assert!(can_add_rule(RuleInputKind::Text, "rust", ""));
    }

    #[test]
    fn can_add_rule_number_requires_parseable_int() {
        assert!(!can_add_rule(RuleInputKind::Number, "not-a-number", ""));
        assert!(can_add_rule(RuleInputKind::Number, "5", ""));
    }

    #[test]
    fn can_add_rule_toggle_is_always_addable() {
        assert!(can_add_rule(RuleInputKind::Toggle, "", ""));
    }

    #[test]
    fn can_add_rule_text_and_number_requires_both_value_and_threshold() {
        assert!(!can_add_rule(RuleInputKind::TextAndNumber, "spam", ""));
        assert!(!can_add_rule(
            RuleInputKind::TextAndNumber,
            "spam",
            "not-a-number"
        ));
        assert!(can_add_rule(RuleInputKind::TextAndNumber, "spam", "5"));
    }

    /// An unknown rule type degrades to the raw key rather than rendering blank —
    /// the `conflict_badge_label` convention: `LocalizedText::resolve` falls
    /// back to the raw string when no such i18n key exists, matching the web
    /// reference builder's `default: return r.rule_type` (`+page.svelte:643`).
    #[test]
    fn rule_summary_label_degrades_unknown_type_to_the_raw_key() {
        let unknown = rule_summary_label("SomeFutureRule", "x", false);
        assert_eq!(unknown.key, "SomeFutureRule");
        assert!(unknown.args.is_empty());
    }
}
