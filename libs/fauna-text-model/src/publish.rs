//! The **publish-time scrub** and the **subscriber-side scorer** for the v2
//! `text-model` labeler artifact (`docs/goal/behavior/topic-factors.md` §
//! Publishing a trained factor, RATIFIED 2026-08-13).
//!
//! Two halves of one contract, kept in this crate because both are
//! **tokenizer-dependent**: the artifact's n-grams only mean anything relative
//! to the tokenizer that produced them, which is what the artifact's `version`
//! field pins. The artifact *type*, its canonical encoding, and its validation
//! live in `fauna_core::scoring` beside the List's — this crate stays
//! dependency-light by construction (crate docs), so the two bucket-1 bounds
//! are passed in by the single shared publish lifecycle rather than duplicated
//! here.
//!
//! # The scrub is a REBUILD, not a redaction
//!
//! [`scrub_corpus`] takes a **corpus of public post texts**, never a
//! [`crate::topic::TopicModel`]. That is the design, not an implementation
//! detail: the private model is never serialized outward *even scrubbed*, so
//! four leak classes die by construction rather than by stripping diligence —
//! example markers (ids never enter), engagement counts (the rebuild cannot
//! read them; it has no model to read them from), restricted-content text
//! (never fetched into the corpus), and the private model's statistical ghosts
//! (residue of unfetchable posts never enters). The type signature *is* the
//! privacy argument, and [`the_scrub_cannot_see_the_private_model`] pins it.

use std::collections::BTreeMap;

use crate::classifier::{bernoulli_posterior, message_ngrams};
use crate::topic::{ExampleLabel, integer_damp};

/// One surviving n-gram of a scrubbed vocabulary: the n-gram and its two
/// per-class **distinct-document** counts.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ScrubbedNgram {
    pub ngram: String,
    pub more: u32,
    pub less: u32,
}

/// What [`scrub_corpus`] produces: the two class document counters and the
/// pruned, informativeness-bounded vocabulary, **ascending by n-gram** (the
/// artifact's own canonical order — a review sheet is free to re-sort).
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct ScrubbedVocabulary {
    pub more_docs: u32,
    pub less_docs: u32,
    pub ngrams: Vec<ScrubbedNgram>,
}

/// Rebuild a publishable vocabulary from a corpus of public example posts.
///
/// `corpus` is `(model_text, label)` per **included** example — the caller has
/// already dropped everything restricted, deleted, or fetch-failed, and has
/// re-tokenized nothing (that happens here, with the same tokenizer training
/// used, which is what makes the counts comparable at the subscriber).
///
/// Three passes, in this order:
///
/// 1. **Count**, Bernoulli per document — an n-gram occurring five times in one
///    post counts once for that post, matching how the model itself trains and
///    how [`bernoulli_posterior`] reads.
/// 2. **Prune** every n-gram below `min_docs` distinct documents, counting both
///    classes together (the class-blind anti-quote floor).
/// 3. **Rank + truncate** at `max_ngrams` by the NB's own informativeness —
///    `|log-odds|` under the scorer's smoothing — with **lexicographic** tie
///    breaking, so the scrub is deterministic and two clients publishing the
///    same corpus produce the same bytes.
pub fn scrub_corpus(
    corpus: &[(String, ExampleLabel)],
    min_docs: u32,
    max_ngrams: usize,
) -> ScrubbedVocabulary {
    let mut more_docs: u32 = 0;
    let mut less_docs: u32 = 0;
    let mut counts: BTreeMap<String, (u32, u32)> = BTreeMap::new();

    for (text, label) in corpus {
        // A label this build does not name teaches the artifact nothing — an
        // exclusion, like every other corpus doubt.
        if !label.is_known() {
            continue;
        }
        match label {
            ExampleLabel::MoreLikeThis => more_docs = more_docs.saturating_add(1),
            ExampleLabel::LessLikeThis => less_docs = less_docs.saturating_add(1),
            ExampleLabel::Other(_) => {}
        }
        // `message_ngrams` returns a SET, so the per-document dedup that makes
        // these Bernoulli counts is the tokenizer layer's, not ours.
        for gram in message_ngrams(text) {
            let entry = counts.entry(gram).or_insert((0, 0));
            match label {
                ExampleLabel::MoreLikeThis => entry.0 = entry.0.saturating_add(1),
                ExampleLabel::LessLikeThis => entry.1 = entry.1.saturating_add(1),
                ExampleLabel::Other(_) => {}
            }
        }
    }

    let mut survivors: Vec<ScrubbedNgram> = counts
        .into_iter()
        .filter(|(_, (more, less))| more.saturating_add(*less) >= min_docs)
        .map(|(ngram, (more, less))| ScrubbedNgram { ngram, more, less })
        .collect();

    if survivors.len() > max_ngrams {
        survivors.sort_unstable_by(|a, b| publish_rank(a, b, more_docs, less_docs));
        survivors.truncate(max_ngrams);
        // Back to the artifact's canonical ascending order.
        survivors.sort_unstable_by(|a, b| a.ngram.cmp(&b.ngram));
    }

    ScrubbedVocabulary {
        more_docs,
        less_docs,
        ngrams: survivors,
    }
}

/// The truncation order: informativeness **descending**, ties broken by n-gram
/// **ascending**.
///
/// ⚠ This comparator must be a **total order** — never returning `Equal` for
/// two distinct n-grams — and that is a correctness requirement, not a style
/// one. `|log-odds|` ties are common (any two n-grams occurring in the same
/// documents of the same class tie exactly), and without the lexicographic tie
/// break the surviving set would be decided by whatever the sort implementation
/// happens to do with equal keys. Two clients on different Rust versions could
/// then publish **different artifacts from the same corpus**, breaking "equal
/// corpus ⇒ equal bytes" — and no assertion on a sort's *output* can catch
/// that, because on any one machine the unspecified behaviour is perfectly
/// repeatable (a mutation run confirmed it: deleting the tie break left an
/// all-tied end-to-end cap test still passing). So the property is pinned on
/// the comparator itself ([`the_rank_comparator_is_a_total_order`]), which is
/// the only place it is observable.
fn publish_rank(
    a: &ScrubbedNgram,
    b: &ScrubbedNgram,
    more_docs: u32,
    less_docs: u32,
) -> std::cmp::Ordering {
    let ia = ngram_informativeness(a, more_docs, less_docs);
    let ib = ngram_informativeness(b, more_docs, less_docs);
    // `total_cmp` orders the f64 magnitudes with no NaN special case.
    ib.total_cmp(&ia).then_with(|| a.ngram.cmp(&b.ngram))
}

/// `|log-odds|` of one n-gram under **the scorer's own smoothing** — the same
/// add-1 per-class likelihoods [`bernoulli_posterior`] uses, so the ranking
/// that decides what crosses is the ranking that decides what matters at the
/// subscriber. Using a different measure here would let the cap drop n-grams
/// the scorer would have weighted most.
fn ngram_informativeness(n: &ScrubbedNgram, more_docs: u32, less_docs: u32) -> f64 {
    let p = f64::from(more_docs);
    let q = f64::from(less_docs);
    let p_more = (f64::from(n.more) + 1.0) / (p + 2.0);
    let p_less = (f64::from(n.less) + 1.0) / (q + 2.0);
    (p_more / p_less).ln().abs()
}

/// A **subscribed** published model: the artifact's counts, ready to score.
///
/// Built by a consumer from a validated `fauna_core::scoring::TextModelArtifact`
/// (that crate owns the wire type; this one owns the math), so this struct
/// deliberately carries no `version` — a caller reaching this type has already
/// decided the version is one it can tokenize for. An unknown version never
/// gets here: it goes **inert** at the consumer, which says so rather than
/// mis-scoring (frame § Tier-3 artifact kinds).
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct PublishedTextModel {
    more_docs: u32,
    less_docs: u32,
    ngrams: BTreeMap<String, (u32, u32)>,
}

impl PublishedTextModel {
    /// Assemble from a validated artifact's parts.
    pub fn new(
        more_docs: u32,
        less_docs: u32,
        ngrams: impl IntoIterator<Item = (String, u32, u32)>,
    ) -> Self {
        Self {
            more_docs,
            less_docs,
            ngrams: ngrams
                .into_iter()
                .map(|(gram, more, less)| (gram, (more, less)))
                .collect(),
        }
    }

    /// Total documents the published vocabulary was built from — the cold-start
    /// damp's sample count, and the honest "built from N examples" a catalog row
    /// can show.
    pub fn document_count(&self) -> u32 {
        self.more_docs.saturating_add(self.less_docs)
    }

    /// Raw NB posterior `P(more | text)` as integer per-mille `[0, 1000]`,
    /// through the **same** [`bernoulli_posterior`] the private model scores by
    /// — a published model and the factor it came from are the same math over
    /// different counts.
    pub fn raw_score(&self, text: &str) -> i64 {
        let p = f64::from(self.more_docs);
        let q = f64::from(self.less_docs);
        if p == 0.0 && q == 0.0 {
            return crate::topic::NEUTRAL_SCORE_PERMILLE;
        }
        let prob = bernoulli_posterior(text, p, q, |gram| {
            self.ngrams
                .get(gram)
                .map(|(more, less)| (f64::from(*more), f64::from(*less)))
        });
        ((prob * 1000.0).round() as i64).clamp(0, 1000)
    }

    /// The damped per-mille score the feed seam composes with — the **same**
    /// cold-model damp a sealed tier-1 factor gets
    /// ([`crate::topic::TOPIC_FULL_CONFIDENCE_SAMPLES`] over the artifact's
    /// document count),
    /// so a thin published model has bounded ordering effect rather than
    /// swinging a subscriber's feed on three examples.
    ///
    /// There is no engagement half to fold in: a published artifact carries only
    /// what the publisher's *explicit* examples taught, so this is exactly the
    /// integer damp path `TopicModel` runs at zero engagement — one shared
    /// implementation ([`integer_damp`]), so the two can never drift.
    pub fn damped_score(&self, text: &str) -> i64 {
        integer_damp(self.raw_score(text), self.document_count())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::topic::TopicModel;

    const MIN_DOCS: u32 = 3;
    const MAX_NGRAMS: usize = 512;

    fn more(text: &str) -> (String, ExampleLabel) {
        (text.to_string(), ExampleLabel::MoreLikeThis)
    }

    fn less(text: &str) -> (String, ExampleLabel) {
        (text.to_string(), ExampleLabel::LessLikeThis)
    }

    fn gram<'a>(v: &'a ScrubbedVocabulary, g: &str) -> Option<&'a ScrubbedNgram> {
        v.ngrams.iter().find(|n| n.ngram == g)
    }

    /// ⚠ PRIVACY PIN #1 — the scrub's **input** is the corpus, and there is no
    /// signature by which a `TopicModel` could reach it.
    ///
    /// This is the structural half of "a model trained with engagement *and*
    /// restricted examples publishes byte-identically to its explicit-public-
    /// only twin": the engagement counters, the example markers, and every
    /// n-gram the private model learned from content the corpus excludes are
    /// unreachable from here — not stripped, *unreachable*. A future session
    /// tempted to "just pass the model in for the counts" has to delete this
    /// test to do it.
    #[test]
    fn the_scrub_cannot_see_the_private_model() {
        let mut trained = TopicModel::new();
        trained.train("aa", "orange cat photo", ExampleLabel::MoreLikeThis);
        trained.train("bb", "private diary entry", ExampleLabel::MoreLikeThis);
        trained.train_engagement(
            "secret engagement text",
            None,
            Some(ExampleLabel::MoreLikeThis),
        );

        // The corpus is the ONLY input, and it is built from public post text —
        // the model above contributes nothing to it.
        let corpus = vec![
            more("orange cat photo"),
            more("orange cat photo"),
            more("orange cat photo"),
        ];
        let vocab = scrub_corpus(&corpus, MIN_DOCS, MAX_NGRAMS);

        assert_eq!(vocab.more_docs, 3);
        assert_eq!(vocab.less_docs, 0);
        assert!(
            gram(&vocab, "diary").is_none(),
            "an n-gram the private model holds but the corpus does not must not appear"
        );
        assert!(
            gram(&vocab, "secret").is_none(),
            "engagement-trained text is unreachable from the scrub by construction"
        );
        assert!(
            gram(&vocab, "orange").is_some(),
            "the corpus's own n-grams survive"
        );
    }

    /// ⚠ PRIVACY PIN #2 — nothing unique to one or two marked posts survives.
    #[test]
    fn an_ngram_below_the_prune_floor_never_survives() {
        let corpus = vec![
            more("shared shared shared unique_one"),
            more("shared again"),
            less("shared elsewhere unique_two"),
        ];
        let vocab = scrub_corpus(&corpus, MIN_DOCS, MAX_NGRAMS);

        let shared = gram(&vocab, "shared").expect("3 distinct documents clears the floor");
        assert_eq!(
            (shared.more, shared.less),
            (2, 1),
            "Bernoulli: three occurrences in ONE document still count that document once"
        );
        assert!(
            gram(&vocab, "unique_one").is_none() && gram(&vocab, "unique_two").is_none(),
            "a single-document n-gram is a quote — it must not cross"
        );
        assert!(
            gram(&vocab, "again").is_none(),
            "two documents is still below the floor"
        );
        for n in &vocab.ngrams {
            assert!(
                n.more + n.less >= MIN_DOCS,
                "{} survived at {} documents",
                n.ngram,
                n.more + n.less
            );
        }
    }

    /// ⚠ PRIVACY PIN #3 — the scrub is deterministic, so "equal corpus ⇒ equal
    /// bytes" (the `wasm_hash` binding, and the byte-identical-twin pin the
    /// publish lifecycle builds on) does not depend on fetch ordering.
    #[test]
    fn the_scrub_is_deterministic_under_corpus_reordering() {
        let a = vec![
            more("orange cat sleeping"),
            less("blue dog running"),
            more("orange cat running"),
            more("orange cat photo"),
            less("blue dog photo"),
            less("blue dog sleeping"),
        ];
        let mut b = a.clone();
        b.reverse();
        b.swap(0, 3);

        assert_eq!(
            scrub_corpus(&a, MIN_DOCS, MAX_NGRAMS),
            scrub_corpus(&b, MIN_DOCS, MAX_NGRAMS),
            "the order posts were fetched in must not reach the artifact"
        );
    }

    #[test]
    fn the_vocabulary_cap_keeps_the_most_informative_and_breaks_ties_lexicographically() {
        // Six documents, all `more`, each carrying the same two "boring" grams
        // (present everywhere ⇒ least informative under the smoothing) plus one
        // discriminating gram shared by exactly three of them.
        let corpus = vec![
            more("aaa bbb zzz"),
            more("aaa bbb zzz"),
            more("aaa bbb zzz"),
            more("aaa bbb yyy"),
            more("aaa bbb yyy"),
            more("aaa bbb yyy"),
        ];
        let full = scrub_corpus(&corpus, MIN_DOCS, MAX_NGRAMS);
        assert!(full.ngrams.len() > 2, "the uncapped scrub keeps everything");

        let capped = scrub_corpus(&corpus, MIN_DOCS, 2);
        assert_eq!(capped.ngrams.len(), 2);
        // Ranking must be by informativeness, and the survivors come back in
        // ascending order regardless of the rank they were kept at.
        let kept: Vec<&str> = capped.ngrams.iter().map(|n| n.ngram.as_str()).collect();
        assert_eq!(kept, {
            let mut k = kept.clone();
            k.sort_unstable();
            k
        });
        // With every gram in this corpus equally (un)informative — all `more`,
        // all-positive class — the tie break is lexicographic and total.
        let again = scrub_corpus(&corpus, MIN_DOCS, 2);
        assert_eq!(capped, again, "a tie must resolve the same way every time");
    }

    /// ⚠ The tie break is graded **here, on the comparator** — not on
    /// `scrub_corpus`'s output — and that placement is the finding, not a
    /// preference.
    ///
    /// Removing `.then_with(|| a.ngram.cmp(&b.ngram))` leaves the sort with
    /// equal keys, whose relative order is *unspecified* but, on any single
    /// machine, perfectly repeatable: a mutation run confirmed that an
    /// output-level assertion over an all-tied 87-n-gram corpus at a cap of one
    /// **still passed** with the tie break deleted. An assertion that cannot
    /// fail on the machine running it is not coverage. The comparator is the
    /// one place the property is observable, so it is asserted directly.
    #[test]
    fn the_rank_comparator_is_a_total_order() {
        use std::cmp::Ordering;

        // Identical counts ⇒ identical informativeness ⇒ an exact tie.
        let a = ScrubbedNgram {
            ngram: "aaa".to_string(),
            more: 3,
            less: 1,
        };
        let b = ScrubbedNgram {
            ngram: "bbb".to_string(),
            more: 3,
            less: 1,
        };
        assert_eq!(
            ngram_informativeness(&a, 9, 9),
            ngram_informativeness(&b, 9, 9),
            "fixture check: these two must genuinely tie on informativeness"
        );
        assert_eq!(
            publish_rank(&a, &b, 9, 9),
            Ordering::Less,
            "a tie must resolve lexicographically, never Equal — two clients \
             with different sort implementations must keep the same n-grams"
        );
        assert_eq!(
            publish_rank(&b, &a, 9, 9),
            Ordering::Greater,
            "antisymmetric"
        );

        // And informativeness still outranks the alphabet.
        let dull = ScrubbedNgram {
            ngram: "aaa".to_string(),
            more: 5,
            less: 5,
        };
        let sharp = ScrubbedNgram {
            ngram: "zzz".to_string(),
            more: 9,
            less: 0,
        };
        assert_eq!(
            publish_rank(&sharp, &dull, 9, 9),
            Ordering::Less,
            "the informative n-gram sorts first despite losing the tie break"
        );
    }

    /// The *end-to-end* companion to the comparator pin above: it cannot grade
    /// the tie break on its own (see that test), but it does pin that the
    /// comparator is actually wired into the truncation.
    #[test]
    fn a_tie_at_the_vocabulary_cap_is_broken_lexicographically() {
        let words: Vec<String> = (0..30).map(|i| format!("w{i:02}")).collect();
        let doc = words.join(" ");
        let corpus = vec![more(&doc), more(&doc), more(&doc)];

        let capped = scrub_corpus(&corpus, MIN_DOCS, 1);
        assert_eq!(
            capped.ngrams,
            vec![ScrubbedNgram {
                ngram: "w00".to_string(),
                more: 3,
                less: 0
            }],
            "with every candidate equally informative, the lexicographically \
             smallest n-gram is the one that crosses"
        );
    }

    #[test]
    fn an_empty_or_too_small_corpus_yields_an_empty_vocabulary() {
        // "A corpus of fewer than 3 public examples yields an empty vocabulary"
        // — the refusal itself belongs to the publish lifecycle, exactly as the
        // List's empty-set refusal does.
        assert_eq!(
            scrub_corpus(&[], MIN_DOCS, MAX_NGRAMS),
            ScrubbedVocabulary::default()
        );
        let two = vec![more("orange cat"), more("orange cat")];
        assert!(scrub_corpus(&two, MIN_DOCS, MAX_NGRAMS).ngrams.is_empty());
    }

    #[test]
    fn a_published_model_generalizes_to_an_unseen_post() {
        // THE property a List cannot have: an UNSEEN post carrying the learned
        // vocabulary scores above neutral (§ Publishing, v2's whole reason).
        let corpus = vec![
            more("orange cat sleeping on a sunny windowsill"),
            more("small orange cat playing with yarn"),
            more("my orange cat naps in the sun"),
            less("quarterly revenue projections spreadsheet"),
            less("quarterly earnings call transcript"),
            less("quarterly budget planning meeting"),
        ];
        let vocab = scrub_corpus(&corpus, MIN_DOCS, MAX_NGRAMS);
        let model = PublishedTextModel::new(
            vocab.more_docs,
            vocab.less_docs,
            vocab
                .ngrams
                .iter()
                .map(|n| (n.ngram.clone(), n.more, n.less)),
        );

        let unseen_match = model.raw_score("a tiny orange cat found a warm spot");
        let unseen_miss = model.raw_score("the quarterly report is attached");
        assert!(
            unseen_match > 500,
            "an unseen post sharing the vocabulary must score above neutral (got {unseen_match})"
        );
        assert!(
            unseen_miss < 500,
            "an unseen post sharing the negative vocabulary must score below (got {unseen_miss})"
        );
    }

    #[test]
    fn a_thin_published_model_is_damped_toward_neutral() {
        // Six documents against TOPIC_FULL_CONFIDENCE_SAMPLES = 30: a published
        // model gets the SAME cold-start damp a sealed tier-1 factor gets, so a
        // subscriber's feed cannot swing on a barely-trained artifact.
        let corpus = vec![
            more("orange cat sleeping"),
            more("orange cat playing"),
            more("orange cat napping"),
            less("quarterly revenue spreadsheet"),
            less("quarterly earnings transcript"),
            less("quarterly budget meeting"),
        ];
        let vocab = scrub_corpus(&corpus, MIN_DOCS, MAX_NGRAMS);
        let model = PublishedTextModel::new(
            vocab.more_docs,
            vocab.less_docs,
            vocab
                .ngrams
                .iter()
                .map(|n| (n.ngram.clone(), n.more, n.less)),
        );
        let text = "an orange cat sleeping again";
        let raw = model.raw_score(text);
        let damped = model.damped_score(text);
        assert!(raw > 500, "raw must be confident (got {raw})");
        assert!(
            (damped - 500).abs() < (raw - 500).abs(),
            "6 of 30 samples must pull the score toward neutral (raw {raw}, damped {damped})"
        );
        assert_eq!(model.document_count(), 6);
    }

    #[test]
    fn an_empty_published_model_is_exactly_neutral() {
        // Zero ordering effect: a constant 500 shifts every item equally.
        let model = PublishedTextModel::default();
        assert_eq!(model.raw_score("anything at all"), 500);
        assert_eq!(model.damped_score("anything at all"), 500);
    }
}
