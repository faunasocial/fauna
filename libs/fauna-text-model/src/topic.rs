//! The trainable **topic factor** model — the second instance of the crate's
//! Bernoulli n-gram Naive-Bayes primitive (`docs/goal/behavior/topic-factors.md`
//! § The model + § Training signals).
//!
//! [`TopicModel`] is [`crate::classifier::SpamModel`]'s exact structure with
//! topic semantics: per-n-gram `{more, less}` message counts, two document
//! counters, the same shared tokenizer + 1/2/3-gram windows, the same
//! reversible forward/inverse deltas (a training event is exactly undoable),
//! and the same count-based byte-cap eviction. On top of that it carries the
//! topic-specific **example markers** (`examples`): content-id → label + seq,
//! which render the per-post *more/less like this* toggle state across
//! sessions, guard against double-training ([`TrainOutcome::DuplicateSignal`]),
//! and back a future "examples list" view. Markers are ids only — no deltas,
//! no text.
//!
//! **At-rest compat surface (new with this type):** the serde_json field names
//! (`version` / `ngrams` / `more` / `less` / `more_examples` / `less_examples`
//! / `examples` / `label` / `seq`, plus the additive v2 engagement twins
//! `more_engagement` / `less_engagement` on each n-gram and
//! `more_engagement_examples` / `less_engagement_examples` on the model) are
//! what sealed at-rest topic-model blobs decode by and MUST NOT change — pinned
//! by the golden-bytes test below, exactly like `SpamModel`'s. The engagement
//! twins are `#[serde(default, skip_serializing_if)]`, so a model with zero
//! engagement (and every blob sealed before v2) serializes byte-for-byte as
//! before.
//!
//! **Score convention:** integer per-mille `[0, 1000]` of `P(more | text)`
//! (`tier = TIER_USER` on the bus; dag-cbor forbids floats). Promote-vs-demote
//! direction comes from the composition *weight*, not the score. Cold-model
//! damping ([`TopicModel::damped_score`]) pins an untrained model to a
//! constant 500 so it has **zero ordering effect** on a feed.

use crate::classifier::{bernoulli_posterior, evict_lowest_count_pass, message_ngrams};
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet};

/// Current `TopicModel` schema version. Bump only on a breaking field change;
/// additive fields stay forward-compatible (older readers ignore unknown keys,
/// `from_bytes` returns `None` only on a genuine decode failure). v2's
/// engagement ingestion is required to be additive-only
/// (`topic-factors.md` § Training signals).
const TOPIC_MODEL_VERSION: u16 = 1;

/// Self-cap on the serialized topic model (256 KiB;
/// `topic-factors.md` § The model). Enforced by the S5 orchestration layer via
/// [`TopicModel::cap_to_bytes`] before sealing; the nest's independent cap on
/// the *sealed* blob is 512 KiB (§ At rest — seal + home).
pub const TOPIC_MODEL_MAX_BYTES: usize = 262_144;

/// Maximum example markers kept in-blob (`topic-factors.md` § The model).
/// Above this the **oldest** (lowest-`seq`) markers are evicted — marker only;
/// the statistical counts they trained stay (the documented graceful degrade:
/// the toggle state forgets, the model does not).
pub const TOPIC_EXAMPLE_MARKERS_MAX: usize = 4096;

/// Example count at which the cold-model damp reaches full confidence
/// (`topic-factors.md` § The model: `damp = min(1, samples / 30)`).
pub const TOPIC_FULL_CONFIDENCE_SAMPLES: u32 = 30;

/// The neutral per-mille score (P = 0.5) an empty/untrained model returns —
/// and the fixed point the damp contracts toward.
pub(crate) const NEUTRAL_SCORE_PERMILLE: i64 = 500;

/// The cold-model damp in its exact integer form: `500 + damp · (raw − 500)`
/// with `damp = min(1, samples / TOPIC_FULL_CONFIDENCE_SAMPLES)`, half-units
/// rounding **away from** the neutral 500 so damping is sign-symmetric (a +25
/// and a −25 deviation damp identically, and a half-unit never rounds *toward*
/// a spurious ordering effect at 0 samples — 0 · anything is exactly 0 ⇒
/// exactly 500).
///
/// Shared by the two scorers that run it: [`TopicModel::damped_score`] on its
/// zero-engagement path (the bit-identical-to-pre-v2 invariant), and
/// [`crate::publish::PublishedTextModel::damped_score`], which has no
/// engagement half at all — a published artifact carries only what the
/// publisher's *explicit* examples taught. One implementation, so a subscribed
/// model and a sealed factor can never drift on how confident "thin" is.
pub(crate) fn integer_damp(raw: i64, samples: u32) -> i64 {
    let dev = raw - NEUTRAL_SCORE_PERMILLE;
    let full = i64::from(TOPIC_FULL_CONFIDENCE_SAMPLES);
    let samples = i64::from(samples.min(TOPIC_FULL_CONFIDENCE_SAMPLES));
    let scaled = dev * samples;
    let half = full / 2;
    let rounded = if scaled >= 0 {
        (scaled + half) / full
    } else {
        (scaled - half) / full
    };
    NEUTRAL_SCORE_PERMILLE + rounded
}

/// Training label for one example post: the user's *more like this* / *less
/// like this* gesture (`topic-factors.md` § Training signals).
///
/// The serialized variant names are part of the at-rest compat surface
/// (markers store them verbatim) — see the module doc.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum ExampleLabel {
    MoreLikeThis,
    LessLikeThis,
    /// A label a newer build trains under and this one does not name — the
    /// exact string read, carried so this build's re-seal of the model keeps
    /// the marker (`transport.md` § Schema and forward-compat discipline →
    /// *Rule 3 in full*). Its delta cannot be inverted here, so the marker is
    /// kept untouched: training, untraining or un-marking that content is a
    /// no-op, it renders no toggle state, it contributes to no count and to no
    /// published corpus. No build trains under one.
    #[serde(untagged)]
    Other(String),
}

impl ExampleLabel {
    /// Whether this build names the label — every delta, toggle and corpus
    /// read acts on a known label only.
    pub fn is_known(&self) -> bool {
        !matches!(self, Self::Other(_))
    }
}

/// What one [`TopicModel::train`] call did.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TrainOutcome {
    /// No marker existed: the forward delta was applied and a marker inserted.
    Trained,
    /// A same-label marker already existed: **no mutation** (the double-tap
    /// guard — spam's `DuplicateSignal` equivalent).
    DuplicateSignal,
    /// An opposite-label marker existed: the old label's exact inverse delta
    /// was applied, then the forward delta under the new label (the toggle
    /// gesture: *more* on an already-*less* post, or vice versa).
    Flipped,
}

/// True for a zero `u32` — the `skip_serializing_if` predicate that keeps the
/// v2 engagement counters off the wire until they are actually used, so a model
/// that has never been engagement-trained (and every sealed blob written before
/// v2) serializes byte-for-byte as before (the `golden_bytes` guard).
fn is_zero_u32(v: &u32) -> bool {
    *v == 0
}

/// Per-n-gram occurrence counts: in how many distinct *more*/*less* example
/// documents the n-gram appeared (Bernoulli, deduped per document).
///
/// The `_engagement` twins (v2, `topic-factors.md` § Training signals) are the
/// same Bernoulli occurrence counts derived from *implicit* cue verdicts
/// (`watch-complete` / `skip`) rather than explicit taps. They are kept separate
/// (not folded into `more`/`less`) so a verdict flip reverses exactly, a
/// published factor can strip the private half trivially, and the score-time
/// weak weight applies to them alone. Additive + zero-defaulted: absent on the
/// wire until non-zero.
#[derive(Debug, Clone, Copy, Default, PartialEq, Serialize, Deserialize)]
pub struct TopicNgramCount {
    pub more: u32,
    pub less: u32,
    #[serde(default, skip_serializing_if = "is_zero_u32")]
    pub more_engagement: u32,
    #[serde(default, skip_serializing_if = "is_zero_u32")]
    pub less_engagement: u32,
}

/// One example marker: which label a content id was trained under, and when
/// (`seq` is a per-model monotonic ordinal driving oldest-first eviction).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ExampleMarker {
    pub label: ExampleLabel,
    pub seq: u64,
}

/// A per-user trainable topic model (`topic-factors.md` § The model).
///
/// Serialized like [`crate::classifier::SpamModel`]: deterministic,
/// version-tagged serde_json over sorted-key `BTreeMap`s
/// ([`to_bytes`](Self::to_bytes) / [`from_bytes`](Self::from_bytes)).
///
/// There is deliberately **no persisted `next_seq` field**: a fresh marker's
/// `seq` is derived as `max(existing seqs) + 1`, which (a) keeps the exact-
/// inverse property honest — `train` then `untrain` restores the model
/// **byte-for-byte** to its never-trained serialization — and (b) only ever
/// needs to order the markers *currently present*, which the derivation
/// preserves.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct TopicModel {
    /// Schema version for forward-compat (older readers ignore unknown fields).
    version: u16,
    /// n-gram (1/2/3) -> (more_count, less_count).
    ngrams: BTreeMap<String, TopicNgramCount>,
    /// Number of *more like this* training documents (the damp's sample count,
    /// together with `less_examples`).
    more_examples: u32,
    /// Number of *less like this* training documents.
    less_examples: u32,
    /// content_id_hex → marker (toggle state + eviction ordinal). Capped at
    /// [`TOPIC_EXAMPLE_MARKERS_MAX`], oldest evicted.
    examples: BTreeMap<String, ExampleMarker>,
    /// Number of engagement-*positive* (`watch-complete`) training documents —
    /// the weak-weighted v2 twin of `more_examples` (`topic-factors.md`
    /// § Training signals). Declared after the v1 fields (additive-only, zero-
    /// defaulted) so a never-engagement-trained model serializes byte-identically
    /// to a v1 blob. **No** per-item markers accompany these: the sealed cue
    /// rollup ([`engagement-cues.md`] § At rest) owns per-item verdict state, so
    /// the model holds only the aggregate engagement counts.
    #[serde(default, skip_serializing_if = "is_zero_u32")]
    more_engagement_examples: u32,
    /// Number of engagement-*negative* (`skip`) training documents — the weak-
    /// weighted v2 twin of `less_examples`.
    #[serde(default, skip_serializing_if = "is_zero_u32")]
    less_engagement_examples: u32,
}

impl Default for TopicModel {
    fn default() -> Self {
        Self::new()
    }
}

impl TopicModel {
    /// A fresh, empty model at the current schema version.
    pub fn new() -> Self {
        Self {
            version: TOPIC_MODEL_VERSION,
            ngrams: BTreeMap::new(),
            more_examples: 0,
            less_examples: 0,
            examples: BTreeMap::new(),
            more_engagement_examples: 0,
            less_engagement_examples: 0,
        }
    }

    /// Train on one example post (`topic-factors.md` § Training signals):
    /// `text` is the post's full text (title + body + tags, fetched at gesture
    /// time), `content_id_hex` the marker key.
    ///
    /// Marker semantics:
    /// - same-label marker present ⇒ [`TrainOutcome::DuplicateSignal`], **no
    ///   mutation**;
    /// - opposite-label marker present ⇒ exact inverse of the old delta
    ///   (recomputed from `text` — the caller supplies the same content), then
    ///   the forward delta under the new label ⇒ [`TrainOutcome::Flipped`];
    /// - no marker ⇒ forward delta ⇒ [`TrainOutcome::Trained`].
    ///
    /// Every mutating train upserts the marker with a fresh `seq` and enforces
    /// [`TOPIC_EXAMPLE_MARKERS_MAX`] (evicting the lowest-`seq` markers —
    /// marker only, counts stay).
    ///
    /// A marker under a label this build does not name ([`ExampleLabel::Other`])
    /// is kept untouched — its delta cannot be inverted here — so training that
    /// content answers [`TrainOutcome::DuplicateSignal`] with no mutation, as
    /// does training under such a label.
    pub fn train(&mut self, content_id_hex: &str, text: &str, label: ExampleLabel) -> TrainOutcome {
        let old = self.examples.get(content_id_hex).map(|m| m.label.clone());
        if old.as_ref() == Some(&label)
            || !label.is_known()
            || old.as_ref().is_some_and(|l| !l.is_known())
        {
            return TrainOutcome::DuplicateSignal;
        }
        let delta = message_ngrams(text);
        if let Some(old_label) = &old {
            self.apply_inverse(&delta, old_label);
        }
        self.apply_forward(&delta, &label);
        self.upsert_marker(content_id_hex, label);
        if old.is_some() {
            TrainOutcome::Flipped
        } else {
            TrainOutcome::Trained
        }
    }

    /// Insert/update `content_id_hex`'s marker with a fresh `seq`
    /// (`max(existing) + 1` — see the struct doc for why `seq` is derived, not
    /// persisted as a counter), then enforce [`TOPIC_EXAMPLE_MARKERS_MAX`] by
    /// evicting the lowest-`seq` markers. Eviction removes ONLY the marker —
    /// the statistical counts and document counters stay (the documented
    /// graceful degrade; the fresh marker has the highest `seq`, so it is
    /// never the one evicted).
    fn upsert_marker(&mut self, content_id_hex: &str, label: ExampleLabel) {
        let seq = self
            .examples
            .values()
            .map(|m| m.seq)
            .max()
            .map_or(0, |s| s.saturating_add(1));
        self.examples
            .insert(content_id_hex.to_string(), ExampleMarker { label, seq });
        while self.examples.len() > TOPIC_EXAMPLE_MARKERS_MAX {
            if !self.evict_oldest_marker() {
                break;
            }
        }
    }

    /// Remove the lowest-`seq` marker (marker only — counts stay). Returns
    /// whether one was removed.
    fn evict_oldest_marker(&mut self) -> bool {
        let oldest = self
            .examples
            .iter()
            .min_by_key(|(_, m)| m.seq)
            .map(|(id, _)| id.clone());
        match oldest {
            Some(id) => self.examples.remove(&id).is_some(),
            None => false,
        }
    }

    /// Un-mark one example (the toggle-off gesture): marker present ⇒ apply
    /// the exact inverse delta recomputed from `text`, remove the marker,
    /// decrement the label's document counter, return `true`; absent ⇒ `false`.
    ///
    /// A marker under a label this build does not name is kept: its delta
    /// cannot be inverted here (`false`).
    pub fn untrain(&mut self, content_id_hex: &str, text: &str) -> bool {
        if !self
            .examples
            .get(content_id_hex)
            .is_some_and(|m| m.label.is_known())
        {
            return false;
        }
        let Some(marker) = self.examples.remove(content_id_hex) else {
            return false;
        };
        self.apply_inverse(&message_ngrams(text), &marker.label);
        true
    }

    /// Marker-only removal, for content whose body is **no longer fetchable**
    /// at undo time (`topic-factors.md` § Training signals, the declared undo
    /// limitation): the n-gram counts and the `more_examples`/`less_examples`
    /// document counters are left untouched — this example *did* train, so its
    /// statistical evidence (and its contribution to the damp's sample count)
    /// legitimately remains as the documented "statistical ghost". Returns
    /// whether a marker was removed.
    pub fn remove_marker(&mut self, content_id_hex: &str) -> bool {
        // Marker only: `ngrams` and `more_examples`/`less_examples` are NOT
        // touched. The counters are the damp's sample count, and this example
        // DID train — its evidence is still in `ngrams` (the "statistical
        // ghost"), so decrementing the sample count here would misrepresent
        // the evidence the model actually holds. A marker under a label this
        // build does not name is kept untouched, like `untrain` keeps it.
        if !self
            .examples
            .get(content_id_hex)
            .is_some_and(|m| m.label.is_known())
        {
            return false;
        }
        self.examples.remove(content_id_hex).is_some()
    }

    /// Train on one **implicit engagement** transition (v2, `topic-factors.md`
    /// § Training signals): the item's derived cue verdict moved from `old` to
    /// `new` (`watch-complete` ⇒ [`ExampleLabel::MoreLikeThis`], `skip` ⇒
    /// [`ExampleLabel::LessLikeThis`], no verdict ⇒ `None`). The mapping from the
    /// cue vocabulary is the caller's (the model crate is verdict-agnostic and
    /// stays free of a `fauna-feed` dependency).
    ///
    /// Transition-driven, mirroring the explicit `train` flip but on the
    /// **`_engagement`** counters only and with **no example marker** — the
    /// sealed cue rollup ([`engagement-cues.md`] § At rest) owns per-item verdict
    /// state (capped 4096), so the model keeps only the aggregate counts:
    ///
    /// - `old == new` (including both `None`) ⇒ **no mutation** (a re-watch does
    ///   not double-count — the rollup's last-wins verdict is unchanged);
    /// - otherwise apply the exact inverse of `old`'s delta (where `old` is
    ///   `Some`), then the forward of `new` (where `new` is `Some`), on the
    ///   engagement counters — so a `watch-complete` later flipped to `skip`
    ///   reverses exactly, and clearing a verdict removes its contribution.
    ///
    /// `text` must be the same item text the forward delta was (or would be)
    /// computed from — the caller recomputes the n-grams at transition time, the
    /// documented "text still fetchable" clause (an item whose text is gone is
    /// skipped by the caller, never trained here with mismatched text).
    pub fn train_engagement(
        &mut self,
        text: &str,
        old: Option<ExampleLabel>,
        new: Option<ExampleLabel>,
    ) {
        if old == new {
            return;
        }
        let delta = message_ngrams(text);
        if let Some(old_label) = &old {
            self.apply_inverse_engagement(&delta, old_label);
        }
        if let Some(new_label) = &new {
            self.apply_forward_engagement(&delta, new_label);
        }
    }

    /// The label `content_id_hex` is currently marked with, if any — renders
    /// the per-post toggle state across sessions.
    pub fn example_label(&self, content_id_hex: &str) -> Option<ExampleLabel> {
        self.examples.get(content_id_hex).map(|m| m.label.clone())
    }

    /// Every example marker, as `(content_id_hex, label)`, in ascending id order.
    ///
    /// The **publish corpus read** walks this (`topic-factors.md` § Publishing a
    /// trained factor, v2): the client fetches each marked post, drops everything
    /// restricted, deleted, or fetch-failed, and rebuilds the published
    /// vocabulary from what is left. Ascending order is the `BTreeMap`'s and is
    /// load-bearing only for reproducibility of the walk — the scrub itself is
    /// order-independent.
    ///
    /// ⚠ Markers are **ids**, and ids never enter a published artifact. This
    /// accessor exists so the publish path can decide *which posts to fetch*;
    /// anything downstream that puts a marker id into an artifact is a leak, not
    /// a feature.
    pub fn example_markers(&self) -> impl Iterator<Item = (&str, ExampleLabel)> + '_ {
        self.examples
            .iter()
            .map(|(id, m)| (id.as_str(), m.label.clone()))
    }

    /// Total **explicit** training documents (`more_examples + less_examples`) —
    /// the advisory `sample_count` sent to the nest and the base of the damp's
    /// confidence count (to which [`damped_score`](Self::damped_score) adds the
    /// weak-weighted engagement documents). Explicit-only by design, so this
    /// value — and the nest advisory — is unchanged by v2. Markers evicted over
    /// the cap still count (they trained).
    pub fn example_count(&self) -> u32 {
        self.more_examples.saturating_add(self.less_examples)
    }

    /// Raw NB posterior `P(more | text)` as integer per-mille `[0, 1000]`.
    /// An empty/untrained model returns the neutral `500`; the cold-start gate
    /// is the damp ([`damped_score`](Self::damped_score)), not this raw score.
    ///
    /// **v2 engagement fold-in** (`topic-factors.md` § Training signals): the
    /// posterior reads **effective** counts `explicit + engagement · weight`
    /// (`weight = engagement_weight_pm / 1000`, the caller supplies
    /// `fauna_core::scoring::cues::CUE_ENGAGEMENT_WEIGHT_PM`; the model crate
    /// stays dependency-light) for both the document priors and each n-gram
    /// likelihood. **Invariant:** a model with zero engagement counts scores
    /// **bit-identically** to the pre-v2 model — effective == explicit, and the
    /// f64 arithmetic reproduces the same values integer casts did — regardless
    /// of `engagement_weight_pm`.
    pub fn raw_score(&self, text: &str, engagement_weight_pm: u32) -> i64 {
        let w = f64::from(engagement_weight_pm) / 1000.0;
        let pos_docs = f64::from(self.more_examples) + f64::from(self.more_engagement_examples) * w;
        let neg_docs = f64::from(self.less_examples) + f64::from(self.less_engagement_examples) * w;
        // No evidence at all (explicit *or* engagement) ⇒ the exact neutral 500,
        // with no float round-trip. When engagement is zero this reduces to the
        // pre-v2 `more_examples == 0 && less_examples == 0` guard.
        if pos_docs == 0.0 && neg_docs == 0.0 {
            return NEUTRAL_SCORE_PERMILLE;
        }
        let prob = bernoulli_posterior(text, pos_docs, neg_docs, |gram| {
            self.ngrams.get(gram).map(|c| {
                (
                    f64::from(c.more) + f64::from(c.more_engagement) * w,
                    f64::from(c.less) + f64::from(c.less_engagement) * w,
                )
            })
        });
        ((prob * 1000.0).round() as i64).clamp(0, 1000)
    }

    /// The damped per-mille score the feed seam composes with
    /// (`topic-factors.md` § The model): `500 + damp · (raw − 500)` with
    /// `damp = min(1, samples / TOPIC_FULL_CONFIDENCE_SAMPLES)` (half-units round
    /// **away from the neutral 500**, so damping is sign-symmetric). A constant
    /// 500 shifts every item equally, so an untrained model has zero ordering
    /// effect.
    ///
    /// **v2 engagement fold-in:** `samples` is the *effective* confidence count
    /// `example_count + (engagement docs) · weight` — engagement contributes to
    /// the damp at the same weak weight the posterior uses, so a factor learned
    /// **only** from engagement progressively un-damps (the feed composes
    /// `damped_score`, so a purely-explicit damp would pin such a factor at the
    /// neutral 500 forever and Layer A would never shift the feed — the whole
    /// point of `learn_from_engagement`). This is the implementation choice the
    /// § Training signals ratification delegates, bounded by the invariant below;
    /// the damp interaction is recorded in that doc's § Implementation status.
    /// **Invariant:** with zero engagement counts the *exact* pre-v2 integer damp
    /// runs, so a zero-engagement model's damped score is bit-identical to today.
    pub fn damped_score(&self, text: &str, engagement_weight_pm: u32) -> i64 {
        let raw = self.raw_score(text, engagement_weight_pm);
        let dev = raw - NEUTRAL_SCORE_PERMILLE;
        let w = f64::from(engagement_weight_pm) / 1000.0;
        let eff_engagement = (f64::from(self.more_engagement_examples)
            + f64::from(self.less_engagement_examples))
            * w;
        if eff_engagement == 0.0 {
            // Zero engagement ⇒ the EXACT pre-v2 integer damp, bit-identical to
            // today (the test-guarded invariant) — now the shared
            // [`integer_damp`], which a published model's scorer runs too.
            return integer_damp(raw, self.example_count());
        }
        // Engagement present ⇒ f64 effective sample count, clamped to full
        // confidence. `f64::round` is round-half-away-from-zero, matching the
        // integer path's rounding intent.
        let full_f64 = f64::from(TOPIC_FULL_CONFIDENCE_SAMPLES);
        let samples = (f64::from(self.example_count()) + eff_engagement).min(full_f64);
        let scaled = (samples / full_f64) * dev as f64;
        NEUTRAL_SCORE_PERMILLE + scaled.round() as i64
    }

    /// Bound the serialized model to `max_bytes`, mirroring
    /// [`crate::classifier::SpamModel::cap_to_bytes`]: evict the
    /// least-informative n-grams (lowest `more + less` total, ties by n-gram
    /// string) until under the cap. If the n-grams are exhausted and the model
    /// is still over (a marker map can outweigh a small n-gram table), evict
    /// the **oldest markers** as the secondary valve — marker only, counts
    /// stay, same rule as the count cap. The document counters are never
    /// evicted, so capping never changes [`example_count`](Self::example_count).
    /// Returns the number of entries (n-grams + markers) evicted.
    pub fn cap_to_bytes(&mut self, max_bytes: usize) -> usize {
        let mut evicted = 0;
        loop {
            let serialized = self.to_bytes().len();
            if serialized <= max_bytes {
                return evicted;
            }
            if !self.ngrams.is_empty() {
                evicted += evict_lowest_count_pass(&mut self.ngrams, serialized, max_bytes, |c| {
                    // Total occurrence across explicit AND engagement documents:
                    // an n-gram trained only via engagement is still informative
                    // for scoring, so it is not evicted ahead of an equally-seen
                    // explicit one.
                    u64::from(c.more)
                        + u64::from(c.less)
                        + u64::from(c.more_engagement)
                        + u64::from(c.less_engagement)
                });
            } else if self.evict_oldest_marker() {
                // Secondary valve: no n-grams left to shed, the marker map is
                // what's over the cap.
                evicted += 1;
            } else {
                // Nothing evictable remains (bare counters); the caller keeps
                // the model as-is, same terminal behavior as SpamModel's.
                return evicted;
            }
        }
    }

    /// Deterministic, version-tagged serialization (`serde_json` over the
    /// sorted-key `BTreeMap`s) — the canonical bytes the S5 layer seals.
    pub fn to_bytes(&self) -> Vec<u8> {
        serde_json::to_vec(self).unwrap_or_default()
    }

    /// Decode a model previously produced by `to_bytes`. Returns `None` on any
    /// decode failure so the caller can treat unreadable bytes as absent
    /// (forward-compat; same convention as `SpamModel::from_bytes`).
    pub fn from_bytes(b: &[u8]) -> Option<Self> {
        serde_json::from_slice(b).ok()
    }

    /// Apply one example's forward delta: +1 to each distinct n-gram's class
    /// count, +1 to the class document counter (saturating).
    fn apply_forward(&mut self, ngrams: &BTreeSet<String>, label: &ExampleLabel) {
        // A label this build does not name moves no count.
        if !label.is_known() {
            return;
        }
        for gram in ngrams {
            let entry = self.ngrams.entry(gram.clone()).or_default();
            match label {
                ExampleLabel::MoreLikeThis => entry.more = entry.more.saturating_add(1),
                ExampleLabel::LessLikeThis => entry.less = entry.less.saturating_add(1),
                ExampleLabel::Other(_) => {}
            }
        }
        match label {
            ExampleLabel::MoreLikeThis => self.more_examples = self.more_examples.saturating_add(1),
            ExampleLabel::LessLikeThis => self.less_examples = self.less_examples.saturating_add(1),
            ExampleLabel::Other(_) => {}
        }
    }

    /// Exact inverse of [`apply_forward`](Self::apply_forward): −1 (saturating
    /// at 0) per listed n-gram, removing an n-gram whose counts both reach 0;
    /// −1 to the class document counter.
    fn apply_inverse(&mut self, ngrams: &BTreeSet<String>, label: &ExampleLabel) {
        // A label this build does not name moves no count.
        if !label.is_known() {
            return;
        }
        for gram in ngrams {
            if let Some(entry) = self.ngrams.get_mut(gram) {
                match label {
                    ExampleLabel::MoreLikeThis => entry.more = entry.more.saturating_sub(1),
                    ExampleLabel::LessLikeThis => entry.less = entry.less.saturating_sub(1),
                    ExampleLabel::Other(_) => {}
                }
                // Remove only when the n-gram carries no evidence at all — its
                // engagement twins (v2) count too, so an n-gram still holding an
                // engagement count survives an explicit untrain (and vice versa).
                if entry.more == 0
                    && entry.less == 0
                    && entry.more_engagement == 0
                    && entry.less_engagement == 0
                {
                    self.ngrams.remove(gram);
                }
            }
        }
        match label {
            ExampleLabel::MoreLikeThis => self.more_examples = self.more_examples.saturating_sub(1),
            ExampleLabel::LessLikeThis => self.less_examples = self.less_examples.saturating_sub(1),
            ExampleLabel::Other(_) => {}
        }
    }

    /// Engagement twin of [`apply_forward`](Self::apply_forward): +1 to each
    /// distinct n-gram's `_engagement` class count and +1 to the class engagement
    /// document counter (saturating). An n-gram seen for the first time only via
    /// engagement is created with zero explicit counts (its `more`/`less` stay 0),
    /// which the byte-cap eviction still weighs by total occurrence.
    fn apply_forward_engagement(&mut self, ngrams: &BTreeSet<String>, label: &ExampleLabel) {
        // A label this build does not name moves no count.
        if !label.is_known() {
            return;
        }
        for gram in ngrams {
            let entry = self.ngrams.entry(gram.clone()).or_default();
            match label {
                ExampleLabel::MoreLikeThis => {
                    entry.more_engagement = entry.more_engagement.saturating_add(1)
                }
                ExampleLabel::LessLikeThis => {
                    entry.less_engagement = entry.less_engagement.saturating_add(1)
                }
                ExampleLabel::Other(_) => {}
            }
        }
        match label {
            ExampleLabel::MoreLikeThis => {
                self.more_engagement_examples = self.more_engagement_examples.saturating_add(1)
            }
            ExampleLabel::LessLikeThis => {
                self.less_engagement_examples = self.less_engagement_examples.saturating_add(1)
            }
            ExampleLabel::Other(_) => {}
        }
    }

    /// Exact inverse of [`apply_forward_engagement`](Self::apply_forward_engagement):
    /// −1 (saturating at 0) per listed n-gram's `_engagement` count, removing an
    /// n-gram whose **all four** counts reach 0; −1 to the class engagement
    /// document counter.
    fn apply_inverse_engagement(&mut self, ngrams: &BTreeSet<String>, label: &ExampleLabel) {
        // A label this build does not name moves no count.
        if !label.is_known() {
            return;
        }
        for gram in ngrams {
            if let Some(entry) = self.ngrams.get_mut(gram) {
                match label {
                    ExampleLabel::MoreLikeThis => {
                        entry.more_engagement = entry.more_engagement.saturating_sub(1)
                    }
                    ExampleLabel::LessLikeThis => {
                        entry.less_engagement = entry.less_engagement.saturating_sub(1)
                    }
                    ExampleLabel::Other(_) => {}
                }
                if entry.more == 0
                    && entry.less == 0
                    && entry.more_engagement == 0
                    && entry.less_engagement == 0
                {
                    self.ngrams.remove(gram);
                }
            }
        }
        match label {
            ExampleLabel::MoreLikeThis => {
                self.more_engagement_examples = self.more_engagement_examples.saturating_sub(1)
            }
            ExampleLabel::LessLikeThis => {
                self.less_engagement_examples = self.less_engagement_examples.saturating_sub(1)
            }
            ExampleLabel::Other(_) => {}
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The production weak-engagement weight (mirrors
    /// `fauna_core::scoring::cues::CUE_ENGAGEMENT_WEIGHT_PM`, which this
    /// dependency-light crate cannot import). Every score assertion passes it;
    /// on a zero-engagement model the value is irrelevant to the outcome (the
    /// bit-identical invariant), which the dedicated engagement tests exercise.
    const WEIGHT_PM: u32 = 200;

    /// A label a newer build trains under rides the sealed model into an
    /// older build, which decodes the model, keeps the marker untouched — no
    /// train, untrain or un-mark moves it, since its delta cannot be inverted
    /// here — and re-seals byte-for-byte (`transport.md` § Schema and
    /// forward-compat discipline → *Rule 3 in full*). The newer writer is the
    /// same model with the marker's label rewritten, as its encoder would
    /// write it.
    #[test]
    fn an_unknown_label_marker_is_kept_untouched_across_a_re_seal() {
        let mut m = TopicModel::new();
        m.train("aa", "fluffy cat", ExampleLabel::MoreLikeThis);
        m.train("bb", "tax forms", ExampleLabel::LessLikeThis);
        let mut json: serde_json::Value = serde_json::from_slice(&m.to_bytes()).unwrap();
        json["examples"]["aa"]["label"] = serde_json::Value::String("NotForMe".into());
        let newer = serde_json::to_vec(&json).unwrap();

        let mut older = TopicModel::from_bytes(&newer).expect("an older build decodes the model");
        assert_eq!(
            older.example_label("aa"),
            Some(ExampleLabel::Other("NotForMe".into()))
        );
        assert_eq!(
            older.to_bytes(),
            newer,
            "the re-seal keeps it byte-for-byte"
        );

        assert_eq!(
            older.train("aa", "fluffy cat", ExampleLabel::LessLikeThis),
            TrainOutcome::DuplicateSignal
        );
        assert!(!older.untrain("aa", "fluffy cat"));
        assert!(!older.remove_marker("aa"));
        assert_eq!(
            older.to_bytes(),
            newer,
            "no gesture moved the unknown marker"
        );

        // A known marker beside it still trains as before.
        assert!(older.untrain("bb", "tax forms"));

        // It teaches a published corpus nothing.
        let vocab = crate::publish::scrub_corpus(
            &[(
                "fluffy cat".to_string(),
                ExampleLabel::Other("NotForMe".into()),
            )],
            1,
            100,
        );
        assert_eq!((vocab.more_docs, vocab.less_docs), (0, 0));
        assert!(vocab.ngrams.is_empty());
    }

    #[test]
    fn golden_bytes_pin_at_rest_field_names() {
        // Pins the exact serde_json at-rest encoding — field names `version` /
        // `ngrams` / `more` / `less` / `more_examples` / `less_examples` /
        // `examples` / `label` / `seq` (and the `MoreLikeThis` /
        // `LessLikeThis` variant strings) are the NEW compat surface sealed
        // topic-model blobs will decode by.
        // A rename/reorder fails HERE before it can strand at-rest data.
        let mut m = TopicModel::new();
        assert_eq!(
            m.train("aa", "fluffy cat", ExampleLabel::MoreLikeThis),
            TrainOutcome::Trained
        );
        assert_eq!(
            m.train("bb", "loud dog", ExampleLabel::LessLikeThis),
            TrainOutcome::Trained
        );
        let json = String::from_utf8(m.to_bytes()).expect("utf8 json");
        assert_eq!(
            json,
            "{\"version\":1,\"ngrams\":{\
             \"cat\":{\"more\":1,\"less\":0},\
             \"dog\":{\"more\":0,\"less\":1},\
             \"fluffy\":{\"more\":1,\"less\":0},\
             \"fluffy cat\":{\"more\":1,\"less\":0},\
             \"loud\":{\"more\":0,\"less\":1},\
             \"loud dog\":{\"more\":0,\"less\":1}\
             },\"more_examples\":1,\"less_examples\":1,\"examples\":{\
             \"aa\":{\"label\":\"MoreLikeThis\",\"seq\":0},\
             \"bb\":{\"label\":\"LessLikeThis\",\"seq\":1}\
             }}"
        );
        // And the pinned bytes decode back to the identical model.
        assert_eq!(TopicModel::from_bytes(json.as_bytes()), Some(m));
    }

    #[test]
    fn train_duplicate_flip_untrain_round_trip_restores_pristine() {
        // The exact-inverse property, end to end through the marker gestures:
        // train → duplicate (no-op) → flip → untrain leaves the model
        // BYTE-IDENTICAL to never-trained (mirrors the classifier's
        // inverse_delta_restores, plus the marker/seq machinery).
        let mut m = TopicModel::new();
        let pristine = m.to_bytes();

        assert_eq!(
            m.train(
                "post1",
                "fluffy cat purring softly",
                ExampleLabel::MoreLikeThis
            ),
            TrainOutcome::Trained
        );
        assert_eq!(m.example_label("post1"), Some(ExampleLabel::MoreLikeThis));
        assert_eq!(m.example_count(), 1);
        let after_first = m.to_bytes();

        // Same-label re-train: DuplicateSignal, no mutation at all.
        assert_eq!(
            m.train(
                "post1",
                "fluffy cat purring softly",
                ExampleLabel::MoreLikeThis
            ),
            TrainOutcome::DuplicateSignal
        );
        assert_eq!(m.to_bytes(), after_first, "duplicate must not mutate");

        // Opposite label: exact inverse of the old delta + forward under new.
        assert_eq!(
            m.train(
                "post1",
                "fluffy cat purring softly",
                ExampleLabel::LessLikeThis
            ),
            TrainOutcome::Flipped
        );
        assert_eq!(m.example_label("post1"), Some(ExampleLabel::LessLikeThis));
        assert_eq!(
            m.example_count(),
            1,
            "a flip moves the document, not adds one"
        );

        // Untrain: inverse delta + marker removal + counter decrement.
        assert!(m.untrain("post1", "fluffy cat purring softly"));
        assert_eq!(m.example_label("post1"), None);
        assert_eq!(m.example_count(), 0);
        assert_eq!(
            m.to_bytes(),
            pristine,
            "train/flip/untrain must restore the never-trained bytes"
        );
        assert_eq!(m, TopicModel::new());

        // Untrain of an unmarked id is a no-op `false`.
        assert!(!m.untrain("post1", "fluffy cat purring softly"));
        assert_eq!(m.to_bytes(), pristine);
    }

    #[test]
    fn remove_marker_leaves_statistical_ghost() {
        // The documented no-longer-fetchable path: the marker goes, the
        // trained counts and the document counters stay (§ Training signals,
        // undo limitation).
        let mut m = TopicModel::new();
        m.train("gone", "fluffy cat", ExampleLabel::MoreLikeThis);
        assert!(m.remove_marker("gone"));
        assert_eq!(m.example_label("gone"), None);
        assert_eq!(m.example_count(), 1, "document counter stays (it trained)");
        assert!(
            m.raw_score("fluffy cat", WEIGHT_PM) > 500,
            "the statistical evidence remains"
        );
        // Absent marker: false, still no mutation.
        assert!(!m.remove_marker("gone"));
        assert!(!m.remove_marker("never-seen"));
    }

    #[test]
    fn marker_eviction_at_cap_keeps_counts() {
        let mut m = TopicModel::new();
        for i in 0..(TOPIC_EXAMPLE_MARKERS_MAX + 1) {
            m.train(
                &format!("id{i}"),
                &format!("token{i}"),
                ExampleLabel::MoreLikeThis,
            );
        }
        // The oldest (lowest-seq) marker was evicted; the map sits at the cap.
        assert_eq!(m.example_label("id0"), None, "lowest-seq marker evicted");
        assert_eq!(m.example_label("id1"), Some(ExampleLabel::MoreLikeThis));
        assert_eq!(
            m.example_label(&format!("id{TOPIC_EXAMPLE_MARKERS_MAX}")),
            Some(ExampleLabel::MoreLikeThis)
        );
        // Counts untouched: the evicted example still trained.
        assert_eq!(m.example_count(), TOPIC_EXAMPLE_MARKERS_MAX as u32 + 1);
        assert!(
            m.raw_score("token0", WEIGHT_PM) > 500,
            "evicted marker's statistical counts stay"
        );
    }

    #[test]
    fn raw_score_separates_trained_classes() {
        let mut m = TopicModel::new();
        m.train(
            "c1",
            "fluffy cat purring on the sofa",
            ExampleLabel::MoreLikeThis,
        );
        m.train("c2", "tiny cat chasing a laser", ExampleLabel::MoreLikeThis);
        m.train(
            "c3",
            "cat photos every caturday",
            ExampleLabel::MoreLikeThis,
        );
        m.train(
            "d1",
            "loud dog barking at the mailman",
            ExampleLabel::LessLikeThis,
        );
        m.train(
            "d2",
            "dog fetching sticks in the park",
            ExampleLabel::LessLikeThis,
        );
        let cat = m.raw_score("a cat purring", WEIGHT_PM);
        let dog = m.raw_score("a dog barking", WEIGHT_PM);
        assert!(cat > 500, "cat={cat}");
        assert!(dog < 500, "dog={dog}");
        assert!(cat > dog);
        // Scores stay in the per-mille domain.
        assert!((0..=1000).contains(&cat) && (0..=1000).contains(&dog));
    }

    #[test]
    fn damp_curve_untrained_neutral_full_confidence_and_midpoint() {
        // 0 examples ⇒ every text scores exactly 500 (zero ordering effect).
        let fresh = TopicModel::new();
        assert_eq!(fresh.raw_score("anything at all", WEIGHT_PM), 500);
        assert_eq!(fresh.damped_score("anything at all", WEIGHT_PM), 500);
        assert_eq!(fresh.damped_score("fluffy cat", WEIGHT_PM), 500);

        // Midpoint: 15 examples ⇒ damp = 1/2, half the raw deviation
        // (rounded away from the neutral 500).
        let mut m = TopicModel::new();
        for i in 0..15 {
            m.train(
                &format!("id{i}"),
                "fluffy cat purring",
                ExampleLabel::MoreLikeThis,
            );
        }
        assert_eq!(m.example_count(), 15);
        let raw = m.raw_score("fluffy cat purring", WEIGHT_PM);
        assert!(raw > 500);
        let dev = raw - 500;
        let expected = 500 + (dev * 15 + 15) / 30; // round half away from zero
        assert_eq!(m.damped_score("fluffy cat purring", WEIGHT_PM), expected);

        // Full confidence at TOPIC_FULL_CONFIDENCE_SAMPLES: damped == raw,
        // and beyond it the damp stays clamped at 1.
        for i in 15..30 {
            m.train(
                &format!("id{i}"),
                "fluffy cat purring",
                ExampleLabel::MoreLikeThis,
            );
        }
        assert_eq!(m.example_count(), TOPIC_FULL_CONFIDENCE_SAMPLES);
        assert_eq!(
            m.damped_score("fluffy cat purring", WEIGHT_PM),
            m.raw_score("fluffy cat purring", WEIGHT_PM)
        );
        m.train("id30", "fluffy cat purring", ExampleLabel::MoreLikeThis);
        assert_eq!(
            m.damped_score("fluffy cat purring", WEIGHT_PM),
            m.raw_score("fluffy cat purring", WEIGHT_PM)
        );
    }

    #[test]
    fn cap_to_bytes_shrinks_below_cap_and_stays_decodable() {
        let mut m = TopicModel::new();
        // High-count distinctive tokens the cap should keep.
        for i in 0..50 {
            m.train(
                &format!("keep{i}"),
                "zzkeepmore",
                ExampleLabel::MoreLikeThis,
            );
            m.train(
                &format!("keepl{i}"),
                "zzkeepless",
                ExampleLabel::LessLikeThis,
            );
        }
        // A long tail of one-off n-grams to shed.
        for i in 0..2000 {
            m.train(
                &format!("rare{i}"),
                &format!("zzrare{i}"),
                ExampleLabel::MoreLikeThis,
            );
        }
        let count_before = m.example_count();
        let full = m.to_bytes().len();
        let cap = full / 4;
        let evicted = m.cap_to_bytes(cap);
        assert!(evicted > 0);
        assert!(m.to_bytes().len() <= cap, "{} > {cap}", m.to_bytes().len());
        assert_eq!(
            m.example_count(),
            count_before,
            "capping never changes example_count"
        );
        let back = TopicModel::from_bytes(&m.to_bytes()).expect("capped model decodes");
        assert_eq!(back, m);

        // Under-cap is a no-op.
        let mut small = TopicModel::new();
        small.train("a", "fluffy cat", ExampleLabel::MoreLikeThis);
        let before = small.clone();
        assert_eq!(small.cap_to_bytes(TOPIC_MODEL_MAX_BYTES), 0);
        assert_eq!(small, before);
    }

    #[test]
    fn from_bytes_garbage_is_none() {
        assert_eq!(TopicModel::from_bytes(b"garbage"), None);
        assert_eq!(TopicModel::from_bytes(b""), None);
    }

    // ── v2 engagement (topic-factors.md § Training signals) ───────────────────

    #[test]
    fn train_engagement_shifts_score_and_flip_reverses() {
        // A watch-complete (weak positive) lifts the raw score; a flip to skip
        // (weak negative) reverses it past neutral; clearing restores neutral.
        let mut m = TopicModel::new();
        assert_eq!(
            m.raw_score("fluffy cat", WEIGHT_PM),
            500,
            "fresh is neutral"
        );

        m.train_engagement("fluffy cat", None, Some(ExampleLabel::MoreLikeThis));
        let after_watch = m.raw_score("fluffy cat", WEIGHT_PM);
        assert!(
            after_watch > 500,
            "watch-complete lifts the score: {after_watch}"
        );

        m.train_engagement(
            "fluffy cat",
            Some(ExampleLabel::MoreLikeThis),
            Some(ExampleLabel::LessLikeThis),
        );
        let after_skip = m.raw_score("fluffy cat", WEIGHT_PM);
        assert!(after_skip < 500, "a flip to skip reverses: {after_skip}");

        m.train_engagement("fluffy cat", Some(ExampleLabel::LessLikeThis), None);
        assert_eq!(
            m.raw_score("fluffy cat", WEIGHT_PM),
            500,
            "clearing the verdict restores neutral (no evidence left)"
        );
    }

    #[test]
    fn train_engagement_round_trip_restores_pristine_bytes() {
        // The exact-inverse property for the implicit path: watch → flip → clear
        // leaves the model BYTE-IDENTICAL to never-engagement-trained (mirrors
        // the explicit train/flip/untrain round trip). No markers are involved.
        let mut m = TopicModel::new();
        let pristine = m.to_bytes();

        m.train_engagement("fluffy cat purring", None, Some(ExampleLabel::MoreLikeThis));
        assert_ne!(m.to_bytes(), pristine, "engagement training mutates");
        assert_eq!(
            m.example_label("fluffy cat purring"),
            None,
            "engagement training leaves NO example marker"
        );

        m.train_engagement(
            "fluffy cat purring",
            Some(ExampleLabel::MoreLikeThis),
            Some(ExampleLabel::LessLikeThis),
        );
        m.train_engagement("fluffy cat purring", Some(ExampleLabel::LessLikeThis), None);
        assert_eq!(
            m.to_bytes(),
            pristine,
            "fully reversed engagement restores pristine bytes"
        );
        assert_eq!(m, TopicModel::new());
    }

    #[test]
    fn train_engagement_same_verdict_is_noop() {
        // A re-watch (old == new) must not double-count — the rollup's last-wins
        // verdict is unchanged, so the model is untouched. Both-None is likewise
        // a no-op.
        let mut m = TopicModel::new();
        m.train_engagement("fluffy cat", None, Some(ExampleLabel::MoreLikeThis));
        let once = m.to_bytes();
        m.train_engagement(
            "fluffy cat",
            Some(ExampleLabel::MoreLikeThis),
            Some(ExampleLabel::MoreLikeThis),
        );
        assert_eq!(m.to_bytes(), once, "same-verdict re-watch is a no-op");

        let mut fresh = TopicModel::new();
        fresh.train_engagement("anything", None, None);
        assert_eq!(fresh, TopicModel::new(), "both-None is a no-op");
    }

    #[test]
    fn engagement_coexists_with_explicit_without_dropping_ngrams() {
        // An n-gram carrying both an explicit and an engagement count survives an
        // explicit untrain (its engagement evidence remains) and vice versa — the
        // removal guard checks all four counts.
        let mut m = TopicModel::new();
        m.train("p1", "fluffy cat", ExampleLabel::MoreLikeThis);
        m.train_engagement("fluffy cat", None, Some(ExampleLabel::MoreLikeThis));

        // Untrain the explicit example: the engagement counts keep the n-gram.
        assert!(m.untrain("p1", "fluffy cat"));
        assert!(
            m.raw_score("fluffy cat", WEIGHT_PM) > 500,
            "engagement evidence survives the explicit untrain"
        );

        // Now clear the engagement too: no evidence remains ⇒ pristine.
        m.train_engagement("fluffy cat", Some(ExampleLabel::MoreLikeThis), None);
        assert_eq!(m, TopicModel::new(), "clearing both leaves nothing behind");
    }

    #[test]
    fn zero_engagement_scores_are_weight_invariant() {
        // Bit-identical invariant, from the other side: a model with only
        // explicit training scores identically for ANY weight (engagement counts
        // are zero, so effective == explicit and the exact integer damp runs).
        let mut m = TopicModel::new();
        for i in 0..10 {
            m.train(
                &format!("c{i}"),
                "fluffy cat purring",
                ExampleLabel::MoreLikeThis,
            );
            m.train(
                &format!("d{i}"),
                "loud dog barking",
                ExampleLabel::LessLikeThis,
            );
        }
        for text in ["fluffy cat", "loud dog", "a neutral sentence"] {
            let raw0 = m.raw_score(text, 0);
            let damp0 = m.damped_score(text, 0);
            for w in [1_u32, 200, 500, 1000, 9999] {
                assert_eq!(
                    m.raw_score(text, w),
                    raw0,
                    "raw weight-invariant: {text}@{w}"
                );
                assert_eq!(
                    m.damped_score(text, w),
                    damp0,
                    "damped weight-invariant: {text}@{w}"
                );
            }
        }
    }

    #[test]
    fn engagement_shifts_the_damped_composed_score() {
        // The Layer-A payoff: a factor learned ONLY from engagement (no explicit
        // taps) un-damps and shifts the DAMPED score the feed composes — else
        // learn_from_engagement would be inert (a purely-explicit damp would pin
        // it at 500 forever). 100 distinct cat posts watched to completion, 100
        // distinct dog posts skipped.
        let mut m = TopicModel::new();
        assert_eq!(
            m.damped_score("fluffy cat purring", WEIGHT_PM),
            500,
            "fresh factor is inert on the composed feed"
        );
        for i in 0..100 {
            m.train_engagement(
                &format!("fluffy cat number {i}"),
                None,
                Some(ExampleLabel::MoreLikeThis),
            );
            m.train_engagement(
                &format!("loud dog number {i}"),
                None,
                Some(ExampleLabel::LessLikeThis),
            );
        }
        let cat = m.damped_score("fluffy cat", WEIGHT_PM);
        let dog = m.damped_score("loud dog", WEIGHT_PM);
        assert!(
            cat > 500,
            "engagement-learned cat preference shifts up: {cat}"
        );
        assert!(
            dog < 500,
            "engagement-learned dog aversion shifts down: {dog}"
        );
        assert!(cat > dog);
    }
}
