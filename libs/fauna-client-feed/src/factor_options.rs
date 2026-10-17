//! The create-feed form's built-in ranking-factor options — the head of the
//! `feed-factor-select` list, before the caller's subscribed `labeler:<hex>`
//! factors and trained `topic:<hex>` factors (`docs/goal/ui/feed.md` § Where
//! logic lives → Feed factor-picker built-ins).
//!
//! A built-in is a factor the nest computes for every post without any
//! subscription or training, so every user can weight it from a fresh account:
//! `engagement` (the cumulative-engagement scalar), `trending` (the decayed
//! trend velocity, `docs/goal/behavior/trending.md` § The Trending feed — "offered
//! in `feed-factor-select` like any bus factor"), then the two Layer-B signals
//! `signal:watch-complete` and `signal:skip` (the k-anonymized aggregates of who
//! watched a public post to the end or skipped it,
//! `docs/goal/behavior/engagement-cues.md` § Layer B). The value each option carries
//! is the factor key the nest composes on, taken from
//! [`fauna_core::scoring::factor`] so it cannot drift from the scorer's own name.

use fauna_core::localized::LocalizedText;
use fauna_core::scoring::factor;
use serde::Serialize;

/// One built-in `feed-factor-select` option: the factor key and its picker
/// label. The `RuleTypeOption` shape, minus the input kind (every factor takes
/// the same weight input).
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct FactorOption {
    /// The factor key — what the select writes, what `FactorWeightInput.factor`
    /// carries to the nest, and what the cross-app `select(id, value)` e2e
    /// contract drives. Never localized; [`FactorOption::label`] is what the
    /// user reads.
    pub value: String,
    /// The picker label (`feed.create.factor_*`).
    pub label: LocalizedText,
}

/// The built-ins, in picker order. `engagement` leads, as it has since the
/// picker shipped; the two Layer-B signals follow — the k-anonymized
/// watch-to-the-end and skip aggregates the nest writes for public posts,
/// which compose through each user's own weights
/// (`docs/goal/behavior/engagement-cues.md` § Layer B → *Composition*).
const BUILTIN_FACTORS: [(&str, &str); 4] = [
    (factor::ENGAGEMENT, "feed.create.factor_engagement"),
    (factor::TRENDING, "feed.create.factor_trending"),
    (
        factor::SIGNAL_WATCH_COMPLETE,
        "feed.create.factor_watch_complete",
    ),
    (factor::SIGNAL_SKIP, "feed.create.factor_skip"),
];

/// The built-in head of every app's `feed-factor-select` list. An app offers
/// these first, then the caller's subscribed labeler factors, then their
/// trained-topic factors — so a new built-in reaches every picker from here
/// rather than as a literal added to seven option lists.
pub fn builtin_factor_options() -> Vec<FactorOption> {
    BUILTIN_FACTORS
        .iter()
        .map(|(value, label_key)| FactorOption {
            value: (*value).to_string(),
            label: LocalizedText::key(*label_key),
        })
        .collect()
}

/// The built-in option for `factor_key`, or `None` for a labeler/topic key (or
/// anything else), whose display text the app resolves itself.
pub fn builtin_factor_option(factor_key: &str) -> Option<FactorOption> {
    builtin_factor_options()
        .into_iter()
        .find(|o| o.value == factor_key)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Pinned as a literal so a reorder or rename fails here rather than
    /// silently changing every app's picker.
    #[test]
    fn builtin_factor_options_are_engagement_trending_then_the_two_signals() {
        let values: Vec<String> = builtin_factor_options()
            .into_iter()
            .map(|o| o.value)
            .collect();
        assert_eq!(
            values,
            [
                "engagement",
                "trending",
                "signal:watch-complete",
                "signal:skip"
            ]
        );
    }

    /// `engagement-cues.md` § Layer B → *Composition*: the shared
    /// watch-to-the-end and skip aggregates compose through each user's own
    /// weights, so the picker offers them under the nest scorer's own keys.
    #[test]
    fn the_layer_b_signals_are_offered_under_the_scorer_s_keys() {
        let watched = builtin_factor_option(factor::SIGNAL_WATCH_COMPLETE).expect("offered");
        assert_eq!(watched.label.key, "feed.create.factor_watch_complete");
        let skipped = builtin_factor_option(factor::SIGNAL_SKIP).expect("offered");
        assert_eq!(skipped.label.key, "feed.create.factor_skip");
    }

    /// `trending.md` § The Trending feed: the trend factor is offered like any
    /// bus factor, under the nest scorer's own key.
    #[test]
    fn trending_is_offered_under_the_scorer_s_key() {
        let trending = builtin_factor_option(factor::TRENDING).expect("trending offered");
        assert_eq!(trending.value, factor::TRENDING);
        assert_eq!(trending.label.key, "feed.create.factor_trending");
    }

    /// Every label is a `feed.create.factor_*` key the i18n catalog ships.
    #[test]
    fn builtin_factor_labels_resolve_in_the_shipped_catalog() {
        for opt in builtin_factor_options() {
            assert!(opt.label.args.is_empty());
            assert!(
                fauna_i18n::strings::lookup(&opt.label.key).is_some(),
                "{} labelled with unshipped key {}",
                opt.value,
                opt.label.key
            );
        }
    }

    /// A labeler or topic key is not a built-in — the app renders those itself.
    #[test]
    fn labeler_and_topic_keys_are_not_builtins() {
        assert!(builtin_factor_option("labeler:00ff").is_none());
        assert!(builtin_factor_option("topic:00ff").is_none());
    }
}
