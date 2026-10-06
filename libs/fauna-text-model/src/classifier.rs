//! The pure per-user Naive Bayes model over shared-tokenizer n-grams — the
//! trainable core the spam classifier (first instance) and the trainable topic
//! factor (`docs/goal/behavior/topic-factors.md`, second instance) share.
//!
//! **One shared-Rust scorer, every position** (`docs/goal/behavior/mail-spam.md`
//! § Scoring placement + § Combined-score formula): this pure module compiles to
//! nest (native), the MDA (Go via UniFFI/cgo), and the apps (WASM/UniFFI), so
//! a given plaintext input yields a byte-identical score everywhere. No I/O, no
//! floats on the wire — the score is carried in milli-units of the 0–15 scale.
//! (The spam-specific confidence-weighting formula that turns the raw score into
//! the `weighted_bayesian_milli` term, and the UniFFI-exported byte-oriented
//! entry points, stay in `fauna_mail::spam::classifier`, which re-exports
//! everything here at the original paths.)
//!
//! Features are **n-grams of sizes 1/2/3** drawn from the shared deterministic
//! tokenizer's ordered stream (`tokenizer::tokenize_positional`), so the bridge,
//! nest, and every app extract identical features. The model is Bernoulli-
//! style: each distinct n-gram is counted **once per message**, and the per-
//! n-gram `(spam, ham)` counts are additive, which is what makes per-event
//! **undo** (`apply_inverse_delta`) a clean exact inverse (§ Undo).

use crate::tokenizer::tokenize_positional;
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet};

/// Current `SpamModel` schema version. Bump only on a breaking field change;
/// additive fields stay forward-compatible (older readers ignore unknown keys,
/// `from_bytes` returns `None` only on a genuine decode failure).
const MODEL_VERSION: u16 = 1;

/// n-gram sizes extracted from the token stream (unigram/bigram/trigram). A
/// message with fewer than `k` tokens simply yields no `k`-grams.
const NGRAM_SIZES: [usize; 3] = [1, 2, 3];

/// The neutral score (0.5 × 15 × 1000) returned for an empty/untrained model.
/// The cold-start gate lives in `weighted_bayesian_milli` (the weight clamps to
/// 0 below `min_samples`), so a neutral raw score here is harmless.
const NEUTRAL_SCORE_MILLI: i32 = 7500;

/// Defense-in-depth byte cap on the message body fed into [`message_ngrams`].
/// Every live caller is already bounded by the 2 MiB WS frame, but a future
/// caller (a batch/CLI path, a larger post-size limit) might not be; truncating
/// here bounds the tokenizer's work and the resulting n-gram set regardless of
/// the upstream limit. 1 MiB is far above any real mail text body, so
/// legitimate scores are unaffected.
const MAX_TOKENIZE_BYTES: usize = 1_048_576;

/// Default per-model serialized-size cap (the `mail.spam.model_max_bytes`
/// catalog default, 1 MiB). Bounds classifier inference cost — above ~10 MiB
/// the per-message n-gram lookup at delivery time impacts latency
/// (`mail-spam.md` § Bounded size). [`SpamModel::cap_to_bytes`] enforces it on
/// the per-user model (on persist) and on the deployment baseline (after the
/// publish merge fold). A hard-coded constant today: the allocated Tier-2 admin
/// knob (`bridges-detail-mail-spam-model-max-bytes`, `mail-policy-config.md`) is
/// still "Bucket C" — deferred, not yet projected/writable.
pub const MODEL_MAX_BYTES_DEFAULT: usize = 1_048_576;

/// k-anonymity floor for the admin-opt-in **deployment baseline**
/// (BASELINE-KANON, `mail-spam.md` § Cold start Path 2): the minimum number of
/// opt-in contributing models that must merge into a baseline before it may be
/// published. With fewer contributors the aggregate approximates an
/// individual's model — and an opted-out contributor could still be
/// reconstructable — so `publish_spam_baseline` withholds (publishes an empty
/// baseline, withdrawing any prior one) below this floor, falling back to
/// rspamd-only cold start. A hard-coded **safety** constant, deliberately NOT a
/// client knob: it is a privacy floor, not a preference a deployment expresses.
/// `3` is a conservative minimum that defeats
/// the degenerate single-/two-contributor cases while still permitting a
/// baseline on any non-trivial deployment.
pub const BASELINE_MIN_CONTRIBUTORS: u32 = 3;

/// Training label for a single message. (A sibling `SpamLabel` exists in
/// `fauna-protocol::bridge_routing`, but that crate is not a dependency under the
/// `spam` feature, so the classifier carries its own.)
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SpamLabel {
    Spam,
    Ham,
}

/// Per-n-gram occurrence counts: in how many distinct training **messages** of
/// each class the n-gram appeared (Bernoulli, deduped per message).
#[derive(Debug, Clone, Copy, Default, Serialize, Deserialize, PartialEq)]
pub struct NgramCount {
    pub spam: u32,
    pub ham: u32,
}

/// A per-user Naive Bayes spam model over shared-tokenizer n-grams.
///
/// Serialized as a deterministic, version-tagged binary (`to_bytes`/`from_bytes`)
/// via `serde_json` over the sorted-key `BTreeMap` — no CBOR/postcard codec
/// exists in the workspace, and JSON over a `BTreeMap` is deterministic and
/// forward-compatible (unknown fields ignored).
///
/// The serde_json field names (`version`/`ngrams`/`spam`/`ham`/`spam_messages`/
/// `ham_messages`) are an **at-rest compat surface** — sealed model blobs decode
/// by these names — and MUST NOT change (see the golden-bytes pin test below).
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
pub struct SpamModel {
    /// Schema version for forward-compat (older readers ignore unknown fields).
    pub version: u16,
    /// n-gram (1/2/3) -> (spam_count, ham_count).
    pub ngrams: BTreeMap<String, NgramCount>,
    /// Number of spam training documents.
    pub spam_messages: u32,
    /// Number of ham training documents.
    pub ham_messages: u32,
}

impl SpamModel {
    /// A fresh, empty model at the current schema version.
    pub fn new() -> Self {
        Self {
            version: MODEL_VERSION,
            ngrams: BTreeMap::new(),
            spam_messages: 0,
            ham_messages: 0,
        }
    }

    /// Total training documents seen (`spam_messages + ham_messages`). Feeds the
    /// confidence ramp in `weighted_bayesian_milli`.
    pub fn sample_count(&self) -> u32 {
        self.spam_messages.saturating_add(self.ham_messages)
    }

    /// Train on one spam message: count each distinct n-gram once, bump the spam
    /// message counter.
    pub fn train_spam(&mut self, text: &str) {
        self.apply_delta(text, SpamLabel::Spam);
    }

    /// Train on one ham message: count each distinct n-gram once, bump the ham
    /// message counter.
    pub fn train_ham(&mut self, text: &str) {
        self.apply_delta(text, SpamLabel::Ham);
    }

    /// Apply one training message (forward delta): extract the distinct n-grams
    /// from `text` and hand them to [`apply_forward_delta_ngrams`](Self::apply_forward_delta_ngrams).
    fn apply_delta(&mut self, text: &str, label: SpamLabel) {
        self.apply_forward_delta_ngrams(&message_ngrams(text), label);
    }

    /// Apply one training event from a **precomputed** forward-delta set (the
    /// distinct n-grams of the message, as [`delta_ngrams`](Self::delta_ngrams)
    /// produces): increment each listed n-gram's class count by 1 (saturating)
    /// and the class message counter by 1. Byte-for-byte equivalent to
    /// `train_spam`/`train_ham` on the originating text — so the nest can run
    /// the CPU-bound n-gram extraction off the classifier lock (in
    /// `spawn_blocking`) and apply the cheap count update under a brief lock.
    /// The forward twin of [`apply_inverse_delta_ngrams`](Self::apply_inverse_delta_ngrams).
    pub fn apply_forward_delta_ngrams(&mut self, ngrams: &BTreeSet<String>, label: SpamLabel) {
        for gram in ngrams {
            let entry = self.ngrams.entry(gram.clone()).or_default();
            match label {
                SpamLabel::Spam => entry.spam = entry.spam.saturating_add(1),
                SpamLabel::Ham => entry.ham = entry.ham.saturating_add(1),
            }
        }
        match label {
            SpamLabel::Spam => self.spam_messages = self.spam_messages.saturating_add(1),
            SpamLabel::Ham => self.ham_messages = self.ham_messages.saturating_add(1),
        }
    }

    /// The distinct n-gram set one `train_spam`/`train_ham` on `text` touches —
    /// the **forward delta** of a single training event (each listed n-gram's
    /// class count goes +1, plus +1 to the class message counter). This is the
    /// exact information `undo` needs to invert that event later, after the
    /// original body is gone: the nest persists this set as the
    /// `spam_training_history` row's `model_delta_applied` and feeds it back to
    /// [`apply_inverse_delta_ngrams`](Self::apply_inverse_delta_ngrams)
    /// (`mail-spam.md` § Training-sample retention / § Undo). It is stored via
    /// the model's own `serde_json` codec — the workspace has no DAG-CBOR codec
    /// (the doc names DAG-CBOR generically), and a sorted `BTreeSet<String>`
    /// serializes deterministically, the same rationale as [`to_bytes`](Self::to_bytes).
    pub fn delta_ngrams(text: &str) -> BTreeSet<String> {
        message_ngrams(text)
    }

    /// Exact inverse of one `train_spam`/`train_ham`: decrement each distinct
    /// n-gram's class count by 1 (saturating at 0) and the class message counter
    /// by 1. An n-gram whose counts both reach 0 is removed, so undoing a
    /// training event restores the model byte-for-byte to its prior state — the
    /// clean inverse the per-event **Undo** affordance needs (§ Undo). Re-derives
    /// the delta set from the original `text`; see
    /// [`apply_inverse_delta_ngrams`](Self::apply_inverse_delta_ngrams) for the
    /// undo path that replays a *stored* set.
    pub fn apply_inverse_delta(&mut self, text: &str, label: SpamLabel) {
        self.apply_inverse_delta_ngrams(&message_ngrams(text), label);
    }

    /// Exact inverse of one training event, applied from a **precomputed** delta
    /// set (as produced by [`delta_ngrams`](Self::delta_ngrams) and persisted in
    /// a `spam_training_history` row). Decrements each listed n-gram's class
    /// count by 1 (saturating at 0, removing an n-gram whose counts both reach
    /// 0) and the class message counter by 1. This is the form the client-side undo
    /// uses, because by undo time the original message body is gone — only the
    /// stored delta remains. Applying the stored set is equivalent to re-deriving
    /// it from the text ([`apply_inverse_delta`](Self::apply_inverse_delta)).
    pub fn apply_inverse_delta_ngrams(&mut self, ngrams: &BTreeSet<String>, label: SpamLabel) {
        for gram in ngrams {
            if let Some(entry) = self.ngrams.get_mut(gram) {
                match label {
                    SpamLabel::Spam => entry.spam = entry.spam.saturating_sub(1),
                    SpamLabel::Ham => entry.ham = entry.ham.saturating_sub(1),
                }
                if entry.spam == 0 && entry.ham == 0 {
                    self.ngrams.remove(gram);
                }
            }
        }
        match label {
            SpamLabel::Spam => self.spam_messages = self.spam_messages.saturating_sub(1),
            SpamLabel::Ham => self.ham_messages = self.ham_messages.saturating_sub(1),
        }
    }

    /// Merge another model's counts into this one (additive aggregation).
    ///
    /// Sums each n-gram's `(spam, ham)` counts and the per-class message
    /// counters, saturating. This is the **deployment-baseline aggregator**
    /// primitive (`mail-spam.md` § Cold start Path 2): the nest folds the
    /// `SpamModel`s of all opt-in users into one baseline by repeated `merge`
    /// over a fresh [`SpamModel::new`]. Plain integer addition over the union of
    /// n-grams is commutative + associative, so the fold order does not change
    /// the result; the merged weights are sums that do not identify which user
    /// contributed which n-gram (`mail-spam.md` § Cold start Path 2 — "the
    /// n-gram weights inside don't identify which users contributed").
    ///
    /// Exactly [`merge_scaled`](Self::merge_scaled) at full weight (`1/1`).
    pub fn merge(&mut self, other: &SpamModel) {
        self.merge_scaled(other, 1, 1);
    }

    /// Merge a fraction `numerator / denominator` of `other`'s counts into this
    /// model — a *faded* additive merge. Each `(spam, ham)` count and the
    /// per-class message counter is scaled by the fraction (integer floor)
    /// before being added, saturating; a count that fades to `0` contributes
    /// nothing (no zero n-gram entry is inserted, so the model stays clean and
    /// its serialization stable). `numerator == denominator` is exactly
    /// [`merge`](Self::merge); `numerator == 0` (or a `0` denominator) is a
    /// no-op.
    ///
    /// This is the **deployment-baseline cold-start prior** primitive
    /// (`mail-spam.md` § Cold start, Path 2 step 4). `fetch_spam_model` merges
    /// the published baseline into the actor's returned model at read time,
    /// scaled by `(full_confidence_samples − own_sample_count) /
    /// full_confidence_samples`: a fresh actor (0 own samples) inherits the
    /// **full** baseline, and the baseline then fades to nothing exactly as the
    /// actor's own model ramps to full confidence (§ Combined-score formula).
    /// The fade compensates the confidence ramp at every own-sample count, so
    /// there is **no cold-start cliff** (a conditional all-or-nothing merge
    /// would zero the per-user term right at the `bayesian_min_samples`
    /// boundary), and at/above full confidence the baseline contributes nothing
    /// — the actor's own model **fully governs**, so a user can always train
    /// their filter to override the baseline (the user-controls-their-data
    /// invariant). The merge is read-time only; it is never persisted into the
    /// actor's own `spam_models` row.
    /// Apply the deployment-baseline **faded fold** — the read-time prior of
    /// `mail-spam.md` § Cold start Path 2 step 4 — wherever the model
    /// plaintext lives: the nest for a plaintext-stored model, or the scoring
    /// agent (Fauna app / AUTH'd MDA session) for a client-sealed one,
    /// which receives the aggregate via the `fetch_spam_model` reply's
    /// additive `baseline` field (the no-double-fold rule, `mail-spam.md`
    /// § Encrypted-mode interaction). Scales the baseline by
    /// `(full_confidence − own_sample_count) / full_confidence`, clamped: a
    /// fresh model inherits the full baseline; at/above full confidence (or a
    /// `0` horizon) the fold is a no-op and the own model fully governs. One
    /// implementation for every position keeps scores byte-identical.
    pub fn fold_baseline_faded(&mut self, baseline: &SpamModel, full_confidence: u32) {
        let own = self.sample_count();
        let numerator = full_confidence.saturating_sub(own.min(full_confidence));
        if numerator == 0 || full_confidence == 0 {
            return;
        }
        self.merge_scaled(baseline, numerator, full_confidence);
    }

    pub fn merge_scaled(&mut self, other: &SpamModel, numerator: u32, denominator: u32) {
        if numerator == 0 || denominator == 0 {
            return;
        }
        // Integer-floor scale of a count by `numerator / denominator`, computed
        // in u64 to avoid overflow before the divide (counts are `u32`).
        let scale = |c: u32| -> u32 { ((c as u64 * numerator as u64) / denominator as u64) as u32 };
        for (gram, count) in &other.ngrams {
            let spam = scale(count.spam);
            let ham = scale(count.ham);
            if spam == 0 && ham == 0 {
                continue;
            }
            let entry = self.ngrams.entry(gram.clone()).or_default();
            entry.spam = entry.spam.saturating_add(spam);
            entry.ham = entry.ham.saturating_add(ham);
        }
        self.spam_messages = self
            .spam_messages
            .saturating_add(scale(other.spam_messages));
        self.ham_messages = self.ham_messages.saturating_add(scale(other.ham_messages));
    }

    /// Bound the serialized model to `max_bytes` by evicting the
    /// **least-informative** n-grams — those with the lowest total
    /// `(spam + ham)` count, ties broken by the n-gram string for determinism —
    /// until `to_bytes().len() <= max_bytes`. Returns the number of n-grams
    /// evicted (`0` ⇒ already under cap, the common case — a small model is
    /// untouched). The per-class **message counters are never evicted** (they
    /// are tiny and load-bearing for `sample_count` / the confidence ramp), so
    /// capping never changes `sample_count`.
    ///
    /// This is the `mail.spam.model_max_bytes` bound (`mail-spam.md` § Bounded
    /// size): it caps the per-user model on persist and the deployment baseline
    /// after the publish merge fold (`mail-spam.md` § Cold start Path 2),
    /// so the model that is sealed + scored on every cold-start fetch and every
    /// delivered message stays within the inference-latency budget. Eviction is
    /// **count-based**, not the doc's earlier "least-recently-updated":
    /// `NgramCount` carries no recency, and the lowest-count n-grams are the
    /// least statistically informative (and the most numerous), so dropping them
    /// best preserves classifier accuracy under the cap. (Consequence: an
    /// over-cap eviction makes a later per-event `undo` of an aged-out n-gram a
    /// no-op for that n-gram — only at the >1 MiB extreme, where the model is
    /// already lossy; `mail-spam.md` § Undo.)
    pub fn cap_to_bytes(&mut self, max_bytes: usize) -> usize {
        let mut evicted = 0;
        loop {
            let serialized = self.to_bytes().len();
            if serialized <= max_bytes || self.ngrams.is_empty() {
                return evicted;
            }
            evicted += evict_lowest_count_pass(&mut self.ngrams, serialized, max_bytes, |c| {
                c.spam as u64 + c.ham as u64
            });
        }
    }

    /// Raw per-user spam score for `text`, in milli-units of the 0–15 scale
    /// (`0..=15000`). Naive Bayes with Laplace (add-1) smoothing over the
    /// distinct in-message n-grams ([`bernoulli_posterior`]); the posterior
    /// probability is scaled to 0–15.
    ///
    /// An empty/untrained model returns the neutral `7500`; the cold-start gate
    /// is the **weight** (`weighted_bayesian_milli`), not this raw score.
    pub fn score(&self, text: &str) -> i32 {
        if self.spam_messages == 0 && self.ham_messages == 0 {
            return NEUTRAL_SCORE_MILLI;
        }
        let prob = bernoulli_posterior(
            text,
            f64::from(self.spam_messages),
            f64::from(self.ham_messages),
            |gram| {
                self.ngrams
                    .get(gram)
                    .map(|c| (f64::from(c.spam), f64::from(c.ham)))
            },
        );
        let raw_0_15 = prob * 15.0;
        ((raw_0_15 * 1000.0).round() as i32).clamp(0, 15000)
    }

    /// Deterministic, version-tagged serialization (`serde_json` over the
    /// sorted-key `BTreeMap`).
    pub fn to_bytes(&self) -> Vec<u8> {
        serde_json::to_vec(self).unwrap_or_default()
    }

    /// Decode a model previously produced by `to_bytes`. Returns `None` on any
    /// decode failure so the caller can treat unreadable/legacy bytes as a fresh
    /// model (forward-compat; no data loss — only re-derivable models exist).
    pub fn from_bytes(b: &[u8]) -> Option<Self> {
        serde_json::from_slice(b).ok()
    }
}

/// Largest prefix of `text` that is at most `max_bytes` long and ends on a
/// UTF-8 char boundary (so slicing never panics). Defense-in-depth bound for
/// [`message_ngrams`] — see [`MAX_TOKENIZE_BYTES`].
///
/// **Deliberately a private copy of `fauna_core::encoding::truncate_to_char_boundary`,
/// which is byte-for-byte this function** (adjudicated 2026-08-23 by the
/// near-duplicate sweep; do not "fix" it). This crate is deliberately minimal —
/// four leaf dependencies, "mail-independent, WASM-safe" per its own manifest
/// description — and taking a `fauna-core` dependency to import nine lines would
/// drag ed25519-dalek and the whole core graph in behind it. The trade only ever
/// gets worse; if a third copy appears, move the helper *down* to a leaf crate
/// rather than pulling this one up.
fn truncate_on_char_boundary(text: &str, max_bytes: usize) -> &str {
    if text.len() <= max_bytes {
        return text;
    }
    let mut end = max_bytes;
    while end > 0 && !text.is_char_boundary(end) {
        end -= 1;
    }
    &text[..end]
}

/// Shared Bernoulli-NB posterior `P(positive | text)` over the distinct
/// in-message n-grams — the scoring math both model instances use
/// ([`SpamModel::score`] with positive = spam, and
/// `topic::TopicModel::raw_score` with positive = *more like this*).
///
/// Naive Bayes with Laplace (add-1) smoothing: a smoothed log-prior from the
/// two document counters, plus a per-token log-likelihood ratio for each
/// distinct in-message n-gram the model knows (`lookup` returns the
/// `(positive, negative)` counts, or `None` for an unseen n-gram — skipped:
/// with smoothing its ratio is ≈ 1 ⇒ log ≈ 0 anyway). The summed log-odds map
/// through a logistic to a probability in `(0, 1)`; each caller owns its own
/// integer scaling (floats never reach a wire — dag-cbor forbids them).
///
/// Counts are `f64` so a caller can pass **effective** (weak-weighted) counts:
/// `TopicModel::raw_score` folds its v2 engagement counters in as
/// `explicit + engagement · weight` fractional documents. `SpamModel` (and any
/// integer-count caller) passes whole numbers, for which this is numerically
/// identical to the former `u32` signature — the function always cast to `f64`
/// internally, so whole-number `f64` inputs flow through the same operations and
/// produce bit-identical scores.
pub(crate) fn bernoulli_posterior(
    text: &str,
    positive_docs: f64,
    negative_docs: f64,
    lookup: impl Fn(&str) -> Option<(f64, f64)>,
) -> f64 {
    let p = positive_docs;
    let n = negative_docs;

    // Smoothed log-prior: ln(p_positive_prior / p_negative_prior).
    let p_pos_prior = (p + 1.0) / (p + n + 2.0);
    let p_neg_prior = (n + 1.0) / (p + n + 2.0);
    let mut log_odds = (p_pos_prior / p_neg_prior).ln();

    for gram in message_ngrams(text) {
        if let Some((pos, neg)) = lookup(&gram) {
            let p_token_pos = (pos + 1.0) / (p + 2.0);
            let p_token_neg = (neg + 1.0) / (n + 2.0);
            log_odds += (p_token_pos / p_token_neg).ln();
        }
    }

    1.0 / (1.0 + (-log_odds).exp())
}

/// One pass of the shared count-based byte-cap eviction loop
/// ([`SpamModel::cap_to_bytes`] / `topic::TopicModel::cap_to_bytes`): drop the
/// lowest-total-count n-grams (ties broken by the n-gram string, for
/// determinism), estimating how many to drop this pass from the average
/// serialized bytes/entry — the caller's outer loop re-measures and calls
/// again if the estimate under-shot, so the cap converges without
/// re-serializing per individual entry. The `+1` guarantees forward progress
/// (≥ 1 removed per pass). Returns the number of n-grams removed. `ngrams`
/// must be non-empty and `serialized > max_bytes` (the caller's loop guard).
pub(crate) fn evict_lowest_count_pass<C>(
    ngrams: &mut BTreeMap<String, C>,
    serialized: usize,
    max_bytes: usize,
    total: impl Fn(&C) -> u64,
) -> usize {
    let n = ngrams.len();
    let avg = (serialized / n).max(1);
    let to_remove = ((serialized - max_bytes) / avg + 1).min(n);
    let mut order: Vec<(u64, &String)> = ngrams.iter().map(|(g, c)| (total(c), g)).collect();
    order.sort_unstable();
    let drop_keys: Vec<String> = order
        .into_iter()
        .take(to_remove)
        .map(|(_, g)| g.clone())
        .collect();
    let mut removed = 0;
    for g in drop_keys {
        ngrams.remove(&g);
        removed += 1;
    }
    removed
}

/// Distinct n-grams (sizes 1/2/3) of `text`, deduped (Bernoulli per-message).
/// Built from the shared deterministic tokenizer's ordered stream; a size-`k`
/// n-gram is `k` consecutive tokens joined by a single space. The body is
/// truncated to [`MAX_TOKENIZE_BYTES`] first so an
/// unbounded input from a caller lacking the upstream WS-frame cap can't blow
/// up the tokenizer or the n-gram set. `pub(crate)`: the topic model extracts
/// the identical feature set (one shared definition, so the two instances can
/// never drift on features).
pub(crate) fn message_ngrams(text: &str) -> BTreeSet<String> {
    let tokens: Vec<String> =
        tokenize_positional(truncate_on_char_boundary(text, MAX_TOKENIZE_BYTES))
            .into_iter()
            .map(|t| t.text)
            .collect();
    let mut set = BTreeSet::new();
    for k in NGRAM_SIZES {
        if tokens.len() < k {
            continue;
        }
        for window in tokens.windows(k) {
            set.insert(window.join(" "));
        }
    }
    set
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::BTreeSet;

    /// Standing evidence for `fauna.bridges.put_spam_model`'s
    /// `forbid_replay = true` (71st pass, then on the plaintext relay it replaced). Asserts the **hazard**, not the
    /// flag: training is an ACCUMULATOR, so applying the same message twice
    /// double-weights it rather than being a no-op. That is what makes a
    /// post-reconnect replay of the kind a real double-apply — it skews the
    /// user's personal filter.
    ///
    /// The nest's one-lesson dedup (`mail-spam.md` § 3) does not save it: that
    /// rule keys on the actor's newest recorded lesson for the message, and an
    /// intervening opposite-label lesson re-opens the key, so a stale replay
    /// would still land on this accumulator. (Until 2026-09-25 the guard was a
    /// 1 s in-memory window, expired before any post-reconnect retry.)
    ///
    /// If the accumulator itself is ever made idempotent (a per-message set
    /// rather than counts), change this test first, then the flag, then both
    /// metadata tables.
    #[test]
    fn training_the_same_message_twice_applies_the_delta_twice() {
        let mut once = SpamModel::new();
        once.train_spam("buy cheap pills");

        let mut twice = SpamModel::new();
        twice.train_spam("buy cheap pills");
        twice.train_spam("buy cheap pills");

        assert_eq!(once.spam_messages, 1);
        assert_eq!(
            twice.spam_messages, 2,
            "the replayed training counts as a second document"
        );
        assert_ne!(
            once.to_bytes(),
            twice.to_bytes(),
            "a replay is NOT a no-op: every n-gram's spam count is doubled"
        );

        // The inverse is exact, which is why the harm is describable as
        // "skewed until undone" rather than "corrupt" — but the user is never
        // told a replay happened, so nothing triggers the undo.
        let delta = SpamModel::delta_ngrams("buy cheap pills");
        twice.apply_inverse_delta_ngrams(&delta, SpamLabel::Spam);
        assert_eq!(
            once.to_bytes(),
            twice.to_bytes(),
            "undoing exactly one of the two trainings restores the single-train model"
        );
    }

    #[test]
    fn golden_bytes_pin_at_rest_field_names() {
        // Pins the exact serde_json at-rest encoding — field names `version` /
        // `ngrams` / `spam` / `ham` / `spam_messages` / `ham_messages` are the
        // compat surface sealed model blobs decode by. A rename/reorder fails
        // HERE before it can strand at-rest data.
        let mut m = SpamModel::new();
        m.train_spam("buy pills");
        m.train_spam("cheap pills");
        m.train_ham("meeting notes");
        let json = String::from_utf8(m.to_bytes()).expect("utf8 json");
        assert_eq!(
            json,
            "{\"version\":1,\"ngrams\":{\
             \"buy\":{\"spam\":1,\"ham\":0},\
             \"buy pills\":{\"spam\":1,\"ham\":0},\
             \"cheap\":{\"spam\":1,\"ham\":0},\
             \"cheap pills\":{\"spam\":1,\"ham\":0},\
             \"meeting\":{\"spam\":0,\"ham\":1},\
             \"meeting notes\":{\"spam\":0,\"ham\":1},\
             \"notes\":{\"spam\":0,\"ham\":1},\
             \"pills\":{\"spam\":2,\"ham\":0}\
             },\"spam_messages\":2,\"ham_messages\":1}"
        );
        // And the pinned bytes decode back to the identical model.
        assert_eq!(SpamModel::from_bytes(json.as_bytes()), Some(m));
    }

    #[test]
    fn train_discriminates() {
        let mut m = SpamModel::new();
        for _ in 0..4 {
            m.train_spam("buy cheap pills now claim your prize");
            m.train_ham("lunch meeting agenda for tomorrow");
        }
        let spammy = m.score("cheap pills prize");
        let hammy = m.score("meeting agenda tomorrow");
        assert!(spammy > hammy, "spammy={spammy} hammy={hammy}");
        assert!(spammy > 7500, "spammy={spammy}");
        assert!(hammy < 7500, "hammy={hammy}");
    }

    #[test]
    fn serialization_round_trip() {
        let mut m = SpamModel::new();
        m.train_spam("buy cheap pills now claim your prize");
        m.train_ham("lunch meeting agenda for tomorrow");
        let bytes = m.to_bytes();
        let back = SpamModel::from_bytes(&bytes).expect("round-trips");
        assert_eq!(m, back);
        assert_eq!(SpamModel::from_bytes(b"garbage"), None);
    }

    #[test]
    fn inverse_delta_restores() {
        let mut m = SpamModel::new();
        m.train_spam("buy cheap pills now claim your prize");
        m.train_ham("lunch meeting agenda for tomorrow");
        let snapshot = m.clone();
        m.train_spam("a brand new spammy message about pills");
        assert_ne!(m, snapshot);
        m.apply_inverse_delta("a brand new spammy message about pills", SpamLabel::Spam);
        assert_eq!(m, snapshot);
    }

    #[test]
    fn delta_ngrams_inverse_restores_like_text_form() {
        // The undo path stores the forward delta (the distinct n-gram set) at
        // train time and replays its inverse later, when the original body is
        // gone. That set-based inverse must restore the model byte-for-byte —
        // identical to re-deriving the set from the text.
        let mut m = SpamModel::new();
        m.train_spam("buy cheap pills now claim your prize");
        m.train_ham("lunch meeting agenda for tomorrow");
        let snapshot = m.clone();

        // Capture the forward delta exactly as the nest does at train time…
        let delta = SpamModel::delta_ngrams("a brand new spammy message about pills");
        m.train_spam("a brand new spammy message about pills");
        assert_ne!(m, snapshot);

        // …then undo from the *stored* set restores the prior state.
        m.apply_inverse_delta_ngrams(&delta, SpamLabel::Spam);
        assert_eq!(m, snapshot);
    }

    #[test]
    fn delta_ngrams_captures_features_and_round_trips() {
        let delta = SpamModel::delta_ngrams("buy cheap pills now");
        // Contains the expected unigram + bigram features (1/2/3-grams).
        assert!(delta.contains("buy"));
        assert!(delta.contains("buy cheap"));
        assert!(delta.contains("buy cheap pills"));
        // Serializes deterministically via the model's own serde_json codec —
        // the `spam_training_history.model_delta_applied` column shape.
        let bytes = serde_json::to_vec(&delta).expect("delta serializes");
        let back: std::collections::BTreeSet<String> =
            serde_json::from_slice(&bytes).expect("round-trips");
        assert_eq!(delta, back);
    }

    #[test]
    fn merge_sums_counts_and_is_order_independent() {
        // Two users' models with overlapping + disjoint n-grams.
        let mut a = SpamModel::new();
        a.train_spam("buy cheap pills");
        a.train_ham("lunch meeting agenda");
        let mut b = SpamModel::new();
        b.train_spam("buy cheap watches"); // shares "buy", "cheap", "buy cheap"
        b.train_ham("project status update");

        // Fold a ⊕ b into a fresh baseline (the nest aggregator shape).
        let mut ab = SpamModel::new();
        ab.merge(&a);
        ab.merge(&b);

        // Message counters are the plain sums.
        assert_eq!(ab.spam_messages, a.spam_messages + b.spam_messages);
        assert_eq!(ab.ham_messages, a.ham_messages + b.ham_messages);
        assert_eq!(ab.sample_count(), a.sample_count() + b.sample_count());

        // A shared n-gram's counts add; a disjoint one carries over unchanged.
        let shared = ab.ngrams.get("buy cheap").expect("shared n-gram present");
        assert_eq!(
            shared.spam,
            a.ngrams["buy cheap"].spam + b.ngrams["buy cheap"].spam
        );
        assert_eq!(
            ab.ngrams.get("buy cheap pills"),
            a.ngrams.get("buy cheap pills")
        );
        assert_eq!(
            ab.ngrams.get("project status update"),
            b.ngrams.get("project status update")
        );

        // Commutative: b ⊕ a yields the identical baseline.
        let mut ba = SpamModel::new();
        ba.merge(&b);
        ba.merge(&a);
        assert_eq!(ab, ba, "merge order must not change the aggregate");
    }

    #[test]
    fn merge_empty_is_identity() {
        let mut m = SpamModel::new();
        m.train_spam("buy cheap pills");
        m.train_ham("lunch meeting agenda");
        let before = m.clone();
        m.merge(&SpamModel::new());
        assert_eq!(m, before, "merging an empty model is a no-op");

        // And merging into a fresh model reproduces the source exactly (counts,
        // n-grams, message totals) — so a single-contributor baseline equals
        // that contributor's model.
        let mut fresh = SpamModel::new();
        fresh.merge(&before);
        assert_eq!(fresh.ngrams, before.ngrams);
        assert_eq!(fresh.spam_messages, before.spam_messages);
        assert_eq!(fresh.ham_messages, before.ham_messages);
    }

    #[test]
    fn fold_baseline_faded_matches_the_read_time_fade() {
        // The agent-side fold must be the same math as the nest's server-side
        // read-time fade (`mail-spam.md` § Cold start Path 2 step 4): scale by
        // (full_confidence − own) / full_confidence, clamped.
        let mut baseline = SpamModel::new();
        for _ in 0..3 {
            baseline.train_spam("qzbasetokwx urgent offer");
            baseline.train_ham("ham note");
        }

        // Fresh model (0 own samples) ⇒ inherits the FULL baseline.
        let mut fresh = SpamModel::new();
        fresh.fold_baseline_faded(&baseline, 200);
        assert_eq!(fresh, baseline, "fade fraction 1 at 0 own samples");

        // Partial fade == merge_scaled with the same fraction.
        let mut partial = SpamModel::new();
        for _ in 0..25 {
            partial.train_spam("cheap pills");
            partial.train_ham("team lunch");
        }
        let own = partial.sample_count();
        let mut expected = partial.clone();
        expected.merge_scaled(&baseline, 200 - own, 200);
        partial.fold_baseline_faded(&baseline, 200);
        assert_eq!(partial, expected, "partial fade == merge_scaled fraction");

        // At/above full confidence (and at a 0 horizon) ⇒ no-op: the own
        // model fully governs (user-controls-their-data — no permanent swamp).
        let mut mature = SpamModel::new();
        for _ in 0..120 {
            mature.train_spam("cheap pills");
            mature.train_ham("team lunch");
        }
        assert!(mature.sample_count() >= 200);
        let before = mature.clone();
        mature.fold_baseline_faded(&baseline, 200);
        assert_eq!(mature, before, "mature model ignores the baseline");
        mature.fold_baseline_faded(&baseline, 0);
        assert_eq!(mature, before, "0 horizon is a guarded no-op");
    }

    #[test]
    fn merge_scaled_full_fraction_equals_merge() {
        // numerator == denominator must be byte-for-byte `merge` (the baseline
        // aggregator path), for any non-zero denominator.
        let mut src = SpamModel::new();
        src.train_spam("buy cheap pills now");
        src.train_ham("lunch meeting agenda tomorrow");

        let mut via_merge = SpamModel::new();
        via_merge.merge(&src);
        let mut via_scaled_1_1 = SpamModel::new();
        via_scaled_1_1.merge_scaled(&src, 1, 1);
        let mut via_scaled_n_n = SpamModel::new();
        via_scaled_n_n.merge_scaled(&src, 200, 200);

        assert_eq!(via_scaled_1_1, via_merge, "1/1 scaled-merge == merge");
        assert_eq!(via_scaled_n_n, via_merge, "n/n scaled-merge == merge");
    }

    #[test]
    fn merge_scaled_zero_numerator_or_denominator_is_noop() {
        let mut m = SpamModel::new();
        m.train_spam("buy cheap pills");
        m.train_ham("lunch meeting agenda");
        let before = m.clone();

        let mut other = SpamModel::new();
        other.train_spam("watches and pills galore");

        m.merge_scaled(&other, 0, 200);
        assert_eq!(
            m, before,
            "a 0 numerator merges nothing (fully faded prior)"
        );
        m.merge_scaled(&other, 200, 0);
        assert_eq!(m, before, "a 0 denominator is a guarded no-op, not a panic");
    }

    #[test]
    fn merge_scaled_halves_counts_and_skips_faded_to_zero() {
        // A baseline with a count of 1 fades to 0 at 1/2 (integer floor) and
        // must not appear; a count of 2 halves to 1. The message counters scale
        // the same way. This is what makes the cold-start fade smooth without
        // littering the model with zero entries.
        let mut baseline = SpamModel::new();
        // "rare" appears once (count 1 ⇒ floor(1/2)=0, dropped); the other grams
        // of this single spam message also have count 1 and drop, so build a
        // count of 2 deliberately by training the same distinctive token twice.
        baseline.train_spam("zzonce zztwice");
        baseline.train_spam("zztwice again");
        // Now zztwice has spam count 2; zzonce has spam count 1.
        assert_eq!(baseline.ngrams["zztwice"].spam, 2);
        assert_eq!(baseline.ngrams["zzonce"].spam, 1);

        let mut faded = SpamModel::new();
        faded.merge_scaled(&baseline, 1, 2); // half

        assert_eq!(
            faded.ngrams.get("zztwice").map(|c| c.spam),
            Some(1),
            "count 2 halves to 1"
        );
        assert!(
            !faded.ngrams.contains_key("zzonce"),
            "count 1 fades to 0 and is not inserted"
        );
        // 2 spam messages scaled by 1/2 ⇒ 1.
        assert_eq!(faded.spam_messages, 1);
        assert_eq!(faded.ham_messages, 0);
    }

    #[test]
    fn apply_forward_delta_ngrams_equals_train_on_text() {
        // The off-lock spawn_blocking path (compute delta, apply set) must be
        // byte-identical to `train_spam`/`train_ham` on the originating
        // text.
        let text = "buy cheap pills now claim your prize";
        let mut via_text = SpamModel::new();
        via_text.train_spam(text);
        let mut via_delta = SpamModel::new();
        via_delta.apply_forward_delta_ngrams(&SpamModel::delta_ngrams(text), SpamLabel::Spam);
        assert_eq!(via_text, via_delta, "delta path == train_spam");

        let mut h_text = SpamModel::new();
        h_text.train_ham(text);
        let mut h_delta = SpamModel::new();
        h_delta.apply_forward_delta_ngrams(&SpamModel::delta_ngrams(text), SpamLabel::Ham);
        assert_eq!(h_text, h_delta, "delta path == train_ham");
    }

    #[test]
    fn message_ngrams_byte_cap_truncates_unbounded_input() {
        // A token before the cap is kept; a distinctive token placed PAST the
        // cap is dropped (the body is truncated at a char boundary before
        // tokenizing).
        let filler = "x ".repeat(MAX_TOKENIZE_BYTES); // ~2× the cap in bytes
        let text = format!("zzbeforecap {filler} zzpastcap");
        assert!(text.len() > MAX_TOKENIZE_BYTES);
        let grams = SpamModel::delta_ngrams(&text);
        assert!(grams.contains("zzbeforecap"), "pre-cap token kept");
        assert!(
            !grams.contains("zzpastcap"),
            "post-cap token dropped by the byte cap"
        );
    }

    #[test]
    fn message_ngrams_normal_input_not_truncated() {
        // A realistic body (far below the cap) is tokenized in full.
        let text = "buy cheap pills now and claim your free prize today";
        let grams = SpamModel::delta_ngrams(text);
        assert!(grams.contains("buy"));
        assert!(
            grams.contains("today"),
            "a normal body is tokenized to its last token (no truncation)"
        );
    }

    #[test]
    fn cap_to_bytes_under_cap_is_noop() {
        let mut m = SpamModel::new();
        m.train_spam("buy cheap pills");
        m.train_ham("lunch meeting agenda");
        let before = m.clone();
        let evicted = m.cap_to_bytes(MODEL_MAX_BYTES_DEFAULT);
        assert_eq!(evicted, 0, "a small model under cap is untouched");
        assert_eq!(m, before);
    }

    #[test]
    fn cap_to_bytes_evicts_lowest_count_first_and_keeps_under_cap() {
        let mut m = SpamModel::new();
        // High-count distinctive tokens (one-token messages, trained 50×).
        for _ in 0..50 {
            m.train_spam("zzkeepspam");
            m.train_ham("zzkeepham");
        }
        // Many one-off (count-1) n-grams — the long tail the cap should shed.
        for i in 0..2000 {
            m.train_spam(&format!("zzrare{i}"));
        }
        let sample_before = m.sample_count();
        let full = m.to_bytes().len();
        let cap = full / 4; // force eviction
        let evicted = m.cap_to_bytes(cap);
        assert!(evicted > 0, "eviction must have occurred");
        assert!(
            m.to_bytes().len() <= cap,
            "model brought under cap: {} > {cap}",
            m.to_bytes().len()
        );
        // The high-count tokens survive; the rare ones are shed first.
        assert!(m.ngrams.contains_key("zzkeepspam"), "high-count kept");
        assert!(m.ngrams.contains_key("zzkeepham"), "high-count kept");
        // Message counters (and thus sample_count) are never evicted.
        assert_eq!(
            m.sample_count(),
            sample_before,
            "capping never changes sample_count"
        );
    }

    #[test]
    fn cap_to_bytes_empty_model_is_noop() {
        // A model with only message counters (no n-grams) can't be shrunk
        // below the counters; cap_to_bytes returns without looping forever.
        let mut m = SpamModel::new();
        m.spam_messages = 5;
        let evicted = m.cap_to_bytes(1); // absurdly small cap
        assert_eq!(evicted, 0);
        assert_eq!(m.spam_messages, 5);
    }

    #[test]
    fn ngram_extraction() {
        let got = message_ngrams("buy cheap pills");
        let want: BTreeSet<String> = [
            "buy",
            "cheap",
            "pills",
            "buy cheap",
            "cheap pills",
            "buy cheap pills",
        ]
        .iter()
        .map(|s| s.to_string())
        .collect();
        assert_eq!(got, want);
    }
}
