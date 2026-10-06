//! The **sealed half** of a feed's effective composition — the scorers the nest
//! cannot read, loaded and evaluated client-side
//! (`docs/goal/behavior/topic-factors.md` § Scoring).
//!
//! [`crate::sealed_compose`] owns the arithmetic (what a contribution *is*);
//! this module owns the *state* it runs on: which sealed factors a feed
//! composes, the trained models behind them, and the text each scorer reads
//! from a post. [`crate::manager`] owns the I/O that fills it.
//!
//! # Which factors are sealed
//!
//! Two, from day one (§ Scoring):
//!
//! * **`topic:<hex>`** — a user-trained [`TopicModel`], sealed under the
//!   BackupKey. Its weight is whatever the user gave it in the feed's
//!   composition or their global factor set.
//! * **`muted-keywords`** — the deterministic −1000 penalty over the user's
//!   sealed keyword list. It is **implicit**: the user mutes *words*, never a
//!   composition entry, and the frame requires the penalty to apply globally
//!   ("a global muted keyword mutes it everywhere",
//!   `content-moderation-and-ranking.md` § Composition). So a scored feed
//!   composes it at unit weight (1.0×) whether or not it appears in any stored
//!   composition — with an explicit entry, if the user ever writes one, taking
//!   precedence over the implicit default rather than double-counting.
//!
//! Keeping it implicit is also what keeps it *honest*: the factor's nest-side
//! term is 0 either way (the zero-term seam), so a nest-side global-factor row
//! would buy nothing functionally while telling the nest that this user mutes
//! something — and a mute that depends on a row having been written is a mute
//! that can silently be missing. Nothing to write means nothing to fail.
//!
//! # Where a mute sinks, and where it only collapses
//!
//! Sinking is an *ordering* effect, so it needs an ordering: a sealed
//! contribution is added to `FeedPostItem.score`, which only exists for a feed
//! the manager fetched `order=score`. A **chronological** feed (the local feed;
//! any feed with no composition at all) has no key to adjust, and the manager
//! deliberately does not switch such a feed to score order just because the user
//! mutes a word — that would silently re-sort their chronological timeline by
//! engagement, a far larger change than they asked for. There, the mute surfaces
//! as the **collapse-to-placeholder render treatment** (the frame's ratified
//! verb), which is client-side and applies everywhere. Hence [`SealedScorers::is_muted`],
//! which is a render signal, not an ordering one.

use fauna_core::data::MutedKeyword;
use fauna_core::scoring::{factor, muted_keywords_collapse, muted_keywords_penalty_entry};
use fauna_text_model::publish::PublishedTextModel;
use fauna_text_model::topic::{ExampleLabel, TopicModel, TrainOutcome};

use crate::snapshot::PostSummary;

/// The per-post training gesture: **more like this** / **less like this**
/// (`feed-post-more-like-this` / `feed-post-less-like-this`).
///
/// A `fauna-feed` type, not a re-export of [`ExampleLabel`], because
/// `fauna-text-model` is deliberately **uniffi-free by construction** (its whole
/// point: it compiles unchanged to the nest, the Go MDA, WASM, and every native
/// app). The manager is the client-facing surface, so the FFI vocabulary
/// belongs here — the same split by which the snapshot types, not `fauna-core`'s
/// internals, carry the `uniffi::Record` derives.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[cfg_attr(feature = "uniffi", derive(uniffi::Enum))]
pub enum TrainVerb {
    MoreLikeThis,
    LessLikeThis,
}

/// One scored public post from the loaded window — a candidate exemplar for the
/// trained-factor publish sheet's review-prune list
/// (`topic-factors.md` § Publishing a trained factor).
///
/// `preview` is the same body text the card renders and the factor scored, so
/// the user prunes against what they can actually read. `score` is per-mille ∈
/// [0,1000] — already the `ListEntry` range, so publishing is a straight
/// mapping with no rescale.
#[derive(Debug, Clone, PartialEq, Eq)]
#[cfg_attr(feature = "uniffi", derive(uniffi::Record))]
pub struct ScoredExemplar {
    /// Hex-encoded 32-byte content id — the `ListEntry.content_id` raw digest
    /// after `hex::decode`.
    pub post_id: String,
    /// The post's body preview, as rendered on the card.
    pub preview: String,
    /// The factor's per-mille score for this post ∈ [0,1000].
    pub score: i64,
}

/// One surviving n-gram of a scrubbed vocabulary, as the publish sheet's Model
/// review renders it (`topic-factors.md` § Publishing a trained factor: "text,
/// class direction, distinct-doc count, default-checked include checkbox").
///
/// Unlike the List's [`ScoredExemplar`] rows — which bound *endorsement* of
/// already-public ids — these rows **are** the disclosure, which is why every
/// survivor is listed rather than a top-N of them.
#[derive(Debug, Clone, PartialEq, Eq)]
#[cfg_attr(feature = "uniffi", derive(uniffi::Record))]
pub struct ReviewNgram {
    /// The n-gram itself: 1–3 tokenizer tokens joined by single spaces.
    pub ngram: String,
    /// Distinct *more like this* example documents it occurred in.
    pub more: u32,
    /// Distinct *less like this* example documents it occurred in.
    pub less: u32,
}

/// What the Model half of the publish sheet reviews: the scrubbed vocabulary
/// plus the corpus-size facts its copy states.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
#[cfg_attr(feature = "uniffi", derive(uniffi::Record))]
pub struct TrainedModelReview {
    /// *More like this* documents the vocabulary was built from.
    pub more_docs: u32,
    /// *Less like this* documents the vocabulary was built from.
    pub less_docs: u32,
    /// Marked posts that were **included** — the N of the sheet's "built from N
    /// public examples of your M marked posts" line.
    pub included_examples: u32,
    /// Marked posts in the factor at all — the M of that line. `M - N` is what
    /// the rebuild dropped as restricted, deleted, or unfetchable, and showing
    /// both is what makes that drop visible to the publisher rather than silent.
    pub marked_examples: u32,
    /// Every surviving n-gram, ascending — the full disclosure, reviewable.
    pub ngrams: Vec<ReviewNgram>,
}

impl From<TrainVerb> for ExampleLabel {
    fn from(v: TrainVerb) -> Self {
        match v {
            TrainVerb::MoreLikeThis => ExampleLabel::MoreLikeThis,
            TrainVerb::LessLikeThis => ExampleLabel::LessLikeThis,
        }
    }
}

impl TrainVerb {
    /// The toggle a stored marker renders as — `None` for a label this build
    /// does not name ([`ExampleLabel::Other`]), which renders unchecked: the
    /// marker is kept, never shown as a gesture the user did not make here.
    pub fn from_label(l: ExampleLabel) -> Option<Self> {
        match l {
            ExampleLabel::MoreLikeThis => Some(TrainVerb::MoreLikeThis),
            ExampleLabel::LessLikeThis => Some(TrainVerb::LessLikeThis),
            ExampleLabel::Other(_) => None,
        }
    }
}

/// What a training gesture actually did to the model — the signal a client needs
/// to render the toggle honestly.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[cfg_attr(feature = "uniffi", derive(uniffi::Enum))]
pub enum TrainResult {
    /// A new example. The model changed and was re-sealed.
    Trained,
    /// The post already carried **this** verb: nothing changed, nothing was
    /// written (the model is the guard, not the UI).
    DuplicateSignal,
    /// The post carried the *other* verb: the old delta was inverted and the new
    /// one applied — a flip, never a double-count.
    Flipped,
}

impl From<TrainOutcome> for TrainResult {
    fn from(o: TrainOutcome) -> Self {
        match o {
            TrainOutcome::Trained => TrainResult::Trained,
            TrainOutcome::DuplicateSignal => TrainResult::DuplicateSignal,
            TrainOutcome::Flipped => TrainResult::Flipped,
        }
    }
}

/// One sealed tier-1 scorer, loaded and ready to score.
pub(crate) enum SealedScorer {
    /// A trained topic model. Scores per-mille `[0, 1000]`, damped toward the
    /// neutral 500 until it has seen enough examples — so an untrained model
    /// returns a flat 500 and has *zero* ordering effect (§ The model).
    Topic(Box<TopicModel>),
    /// The user's muted-keyword list. Yields the strongest matching keyword's
    /// weight (`fauna_core::scoring::muted_keywords_penalty_entry`) on a match,
    /// nothing otherwise.
    MutedKeywords(Vec<MutedKeyword>),
    /// A **subscribed** tier-3 `text-model` labeler
    /// (`content-moderation-and-ranking.md` § Tier-3 artifact kinds). Scores
    /// through the same Bernoulli posterior and the same cold-start damp a
    /// sealed tier-1 topic factor does — it *is* the same math over a published
    /// count table — so a thin published model has bounded ordering effect.
    ///
    /// It rides this seam rather than the nest's scoring bus deliberately: the
    /// placement matrix forbids nest-side content evaluation, so the
    /// subscriber's own client is the one position where post plaintext meets
    /// the artifact. The nest term for this factor is 0 by the zero-term seam.
    SubscribedModel(Box<PublishedTextModel>),
}

impl SealedScorer {
    /// This scorer's factor value for `text`, in per-mille — the `v` of
    /// [`crate::sealed_compose::sealed_contribution_micro`]. `None` = no
    /// contribution at all (an unmatched keyword list), which is *not* the same
    /// as a zero value.
    fn value_permille(&self, text: &str) -> Option<i64> {
        match self {
            SealedScorer::Topic(model) => {
                Some(model.damped_score(text, fauna_core::scoring::cues::CUE_ENGAGEMENT_WEIGHT_PM))
            }
            // Ride the shared scorer, never a local re-match: it wraps
            // `fauna_core::keyword::body_excludes_matches`, the ONE evaluation
            // the nest and every app share (`moderation-local-flags`: do not
            // fork it). This is that scorer's first production caller.
            SealedScorer::MutedKeywords(words) => {
                muted_keywords_penalty_entry(words, text).map(|entry| entry.score)
            }
            SealedScorer::SubscribedModel(model) => Some(model.damped_score(text)),
        }
    }
}

/// One sealed factor as it participates in a feed: its key, the weight it
/// carries in the effective composition, and the loaded scorer behind it.
pub(crate) struct SealedEntry {
    /// The composition key — `topic:<hex>` or `muted-keywords`. Kept so a fresh
    /// train can swap its model in place (the gesture already holds the trained
    /// model; refetching it from the nest would be a round-trip to read back
    /// what we just wrote).
    pub(crate) factor: String,
    pub(crate) weight_permille: i64,
    pub(crate) scorer: SealedScorer,
}

/// The sealed scorers a feed's effective composition resolves to.
///
/// Empty ⇒ the seam is inert and the manager leaves the nest's order untouched.
#[derive(Default)]
pub(crate) struct SealedScorers {
    entries: Vec<SealedEntry>,
}

impl SealedScorers {
    pub(crate) fn new(entries: Vec<SealedEntry>) -> Self {
        Self { entries }
    }

    pub(crate) fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    /// The `(weight_permille, factor_value_permille)` terms this post
    /// contributes, for [`crate::sealed_compose::adjusted_score_micro`].
    ///
    /// Scored text is the post's **preview body + tags** — what the snapshot
    /// already holds, so scoring a 50-post window costs no fetches (§ Scoring:
    /// training uses the full text, scoring must stay cheap).
    pub(crate) fn terms_for(&self, post: &PostSummary) -> Vec<(i64, i64)> {
        if self.entries.is_empty() {
            return Vec::new();
        }
        let text = model_text(None, &post.body, &post.tags);
        self.entries
            .iter()
            .filter_map(|e| {
                e.scorer
                    .value_permille(&text)
                    .map(|v| (e.weight_permille, v))
            })
            .collect()
    }

    /// Does this post collapse behind the user's muted keywords? The **render**
    /// signal behind the collapse-to-placeholder treatment — true regardless of
    /// whether the feed is score-ordered, because a mute collapses everywhere
    /// even where it cannot sink (module docs). Only a keyword muted at the full
    /// penalty collapses (`fauna_core::scoring::muted_keywords_collapse`); a
    /// softer weight only demotes.
    pub(crate) fn is_muted(&self, post: &PostSummary) -> bool {
        let text = model_text(None, &post.body, &post.tags);
        self.entries.iter().any(|e| match &e.scorer {
            SealedScorer::MutedKeywords(muted) => muted_keywords_collapse(muted, &text),
            _ => false,
        })
    }

    /// Swap in a just-trained model for `factor`, so the loaded window re-ranks
    /// against it immediately. A no-op when the factor is not part of *this*
    /// feed's composition — training a topic the current feed does not compose
    /// must not reorder that feed.
    pub(crate) fn set_topic(&mut self, factor: &str, model: TopicModel) {
        if let Some(e) = self.entries.iter_mut().find(|e| e.factor == factor) {
            e.scorer = SealedScorer::Topic(Box::new(model));
        }
    }

    /// The trained model loaded for `factor`, if this feed composes it — the
    /// read behind the per-post toggle state (`ExampleLabel`) the gesture
    /// renders.
    pub(crate) fn topic(&self, factor: &str) -> Option<&TopicModel> {
        self.entries
            .iter()
            .find(|e| e.factor == factor)
            .and_then(|e| match &e.scorer {
                SealedScorer::Topic(m) => Some(m.as_ref()),
                // A subscribed model is somebody ELSE's published artifact, not
                // a model of the user's this device may train — the toggle-state
                // read this backs has nothing to show for one.
                SealedScorer::MutedKeywords(_) | SealedScorer::SubscribedModel(_) => None,
            })
    }
}

/// The text a sealed scorer reads for a post: its title/summary, its textual
/// body, and its tags, newline-joined.
///
/// **One function, both paths, on purpose.** Scoring passes the 500-char preview
/// already in the snapshot; training passes the full body fetched at gesture
/// time (§ Training signals). The *shape* the tokenizer sees must be identical
/// across the two or a post would train under one feature set and score under
/// another — only the body's length may differ, and that asymmetry is the
/// declared accepted one.
pub(crate) fn model_text(title: Option<&str>, body: &str, tags: &[String]) -> String {
    let mut parts: Vec<&str> = Vec::with_capacity(2 + tags.len());
    if let Some(t) = title.filter(|t| !t.is_empty()) {
        parts.push(t);
    }
    if !body.is_empty() {
        parts.push(body);
    }
    parts.extend(tags.iter().map(|t| t.as_str()));
    parts.join("\n")
}

/// Is `key` the muted-keywords factor?
pub(crate) fn is_muted_keywords_factor(key: &str) -> bool {
    key == factor::MUTED_KEYWORDS
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::{TestPostSpec, cats_model};
    use fauna_core::scoring::MUTED_KEYWORDS_PENALTY;
    use fauna_text_model::topic::ExampleLabel;

    fn post(body: &str, tags: &[&str]) -> PostSummary {
        TestPostSpec {
            post_id: "aa".into(),
            body: body.to_string(),
            tags: tags.iter().map(|t| t.to_string()).collect(),
            ..Default::default()
        }
        .into_summary()
    }

    const CATS: &str = "topic:aabbccddeeff00112233445566778899";

    fn topic_entry(factor: &str, weight: i64, model: TopicModel) -> SealedEntry {
        SealedEntry {
            factor: factor.to_string(),
            weight_permille: weight,
            scorer: SealedScorer::Topic(Box::new(model)),
        }
    }

    fn muted_entry(weight: i64, words: &[&str]) -> SealedEntry {
        SealedEntry {
            factor: factor::MUTED_KEYWORDS.to_string(),
            weight_permille: weight,
            scorer: SealedScorer::MutedKeywords(
                words.iter().map(|w| MutedKeyword::from(*w)).collect(),
            ),
        }
    }

    #[test]
    fn a_topic_model_scores_an_on_topic_post_above_an_off_topic_one() {
        let scorers = SealedScorers::new(vec![topic_entry(CATS, 1000, cats_model())]);
        let cat = scorers.terms_for(&post("a fluffy cat purring", &[]));
        let fin = scorers.terms_for(&post("shareholder dividends guidance", &[]));
        assert_eq!(cat.len(), 1);
        assert_eq!(fin.len(), 1);
        assert!(
            cat[0].1 > fin[0].1,
            "on-topic {} should outscore off-topic {}",
            cat[0].1,
            fin[0].1,
        );
    }

    /// Tags are part of the scored text, not decoration: a post whose *body*
    /// says nothing about cats but is tagged `cats` still reads as on-topic.
    #[test]
    fn tags_participate_in_the_scored_text() {
        let scorers = SealedScorers::new(vec![topic_entry(CATS, 1000, cats_model())]);
        let untagged = scorers.terms_for(&post("look at this", &[]));
        let tagged = scorers.terms_for(&post("look at this", &["cat", "purring"]));
        assert!(
            tagged[0].1 > untagged[0].1,
            "the tag should carry topic signal: {} vs {}",
            tagged[0].1,
            untagged[0].1,
        );
    }

    #[test]
    fn an_unmatched_keyword_list_contributes_no_term_at_all() {
        let scorers = SealedScorers::new(vec![muted_entry(1000, &["spoiler"])]);
        assert!(
            scorers
                .terms_for(&post("a perfectly ordinary post", &[]))
                .is_empty(),
            "no match ⇒ no term (not a zero-valued one)",
        );
        let matched = scorers.terms_for(&post("here comes a SPOILER for you", &[]));
        assert_eq!(matched, vec![(1000, MUTED_KEYWORDS_PENALTY)]);
    }

    /// A softly weighted keyword contributes its own weight — the post is
    /// demoted in a ranked feed — but does not collapse: only the full penalty
    /// hides (`content-moderation-and-ranking.md` § Composition).
    #[test]
    fn a_soft_weighted_keyword_demotes_without_collapsing() {
        let scorers = SealedScorers::new(vec![SealedEntry {
            factor: factor::MUTED_KEYWORDS.to_string(),
            weight_permille: 1000,
            scorer: SealedScorer::MutedKeywords(vec![MutedKeyword {
                keyword: "politics".into(),
                weight: -200,
            }]),
        }]);
        let p = post("more politics today", &[]);
        assert_eq!(scorers.terms_for(&p), vec![(1000, -200)]);
        assert!(!scorers.is_muted(&p));
    }

    #[test]
    fn a_muted_match_is_a_render_signal_even_though_it_is_also_a_term() {
        let scorers = SealedScorers::new(vec![muted_entry(1000, &["spoiler"])]);
        assert!(scorers.is_muted(&post("SPOILER: the butler did it", &[])));
        assert!(!scorers.is_muted(&post("nothing to see here", &[])));
    }

    /// A muted word in a *tag* mutes the post — the scored text is body + tags,
    /// so muting "politics" catches `#politics` even when the body never says it.
    #[test]
    fn a_muted_word_in_a_tag_mutes_the_post() {
        let scorers = SealedScorers::new(vec![muted_entry(1000, &["politics"])]);
        assert!(scorers.is_muted(&post("read this thread", &["politics"])));
    }

    /// A fresh train swaps the model in place, so the loaded window re-ranks
    /// against it with no refetch of the blob we just wrote.
    #[test]
    fn set_topic_swaps_the_model_for_a_composed_factor() {
        let mut scorers = SealedScorers::new(vec![topic_entry(CATS, 1000, TopicModel::new())]);
        // An untrained model is a flat 500 — no signal.
        let before = scorers.terms_for(&post("a fluffy cat purring", &[]))[0].1;
        assert_eq!(before, 500, "an untrained model is neutral");

        scorers.set_topic(CATS, cats_model());
        let after = scorers.terms_for(&post("a fluffy cat purring", &[]))[0].1;
        assert!(
            after > before,
            "the trained model now carries signal: {after}"
        );
    }

    /// Training a topic the current feed does **not** compose must not reorder
    /// that feed — the swap is scoped to the composition.
    #[test]
    fn set_topic_ignores_a_factor_this_feed_does_not_compose() {
        let mut scorers = SealedScorers::new(vec![topic_entry(CATS, 1000, TopicModel::new())]);
        scorers.set_topic("topic:99887766554433221100ffeeddccbbaa", cats_model());
        assert_eq!(
            scorers.terms_for(&post("a fluffy cat purring", &[]))[0].1,
            500,
            "an uncomposed factor's training must not touch this feed",
        );
    }

    #[test]
    fn topic_reads_back_the_loaded_model_for_the_toggle_state() {
        let mut m = TopicModel::new();
        m.train("post-1", "cats", ExampleLabel::MoreLikeThis);
        let scorers = SealedScorers::new(vec![topic_entry(CATS, 1000, m)]);
        assert_eq!(
            scorers.topic(CATS).unwrap().example_label("post-1"),
            Some(ExampleLabel::MoreLikeThis),
        );
        assert!(scorers.topic(factor::MUTED_KEYWORDS).is_none());
    }

    #[test]
    fn model_text_joins_title_body_and_tags_and_skips_empties() {
        assert_eq!(
            model_text(Some("Heads up"), "the body", &["a".into(), "b".into()]),
            "Heads up\nthe body\na\nb",
        );
        assert_eq!(model_text(None, "just body", &[]), "just body");
        // A media-only post has no text body; its tags still score.
        assert_eq!(model_text(None, "", &["cats".into()]), "cats");
        assert_eq!(model_text(None, "", &[]), "");
    }

    #[test]
    fn no_scorers_means_no_terms() {
        let scorers = SealedScorers::default();
        assert!(scorers.is_empty());
        assert!(scorers.terms_for(&post("anything", &[])).is_empty());
    }
}
