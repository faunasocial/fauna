//! Per-user Naive Bayes spam classifier over shared-tokenizer n-grams, plus the
//! confidence-weighting formula that turns its raw score into the
//! `weighted_bayesian_milli` term `combined_spam_score_milli` consumes.
//!
//! **One shared-Rust scorer, every position** (`docs/goal/behavior/mail-spam.md`
//! § Scoring placement + § Combined-score formula): this pure module compiles to
//! nest (native), the MDA (Go via UniFFI/cgo), and the apps (WASM/UniFFI), so
//! a given plaintext input yields a byte-identical score everywhere. No I/O, no
//! floats on the wire — the score is carried in milli-units of the 0–15 scale.
//!
//! The **pure model core** (`SpamModel` + `NgramCount` + `SpamLabel` + the
//! n-gram feature extraction) lives in the mail-independent `fauna-text-model`
//! crate — the trainable topic factor (`docs/goal/behavior/topic-factors.md`
//! § The model) shares the same primitive — and is re-exported here so every
//! existing `fauna_mail::spam::*` path resolves unchanged. What *stays* in this
//! module is the spam-formula-specific surface: the confidence-weighting
//! formula ([`BayesianKnobs`] + [`weighted_bayesian_milli`]) and the
//! UniFFI-exported byte-oriented entry points the apps / Go MDA call.

// The pure model core (SpamModel, NgramCount, SpamLabel, MODEL_MAX_BYTES_DEFAULT,
// BASELINE_MIN_CONTRIBUTORS, …) — re-exported at its historical paths so no call
// site (or UniFFI-exported symbol) moves. See `fauna_text_model::classifier` for
// the model itself, including the at-rest serde_json compat pin.
pub use fauna_text_model::classifier::*;

/// Confidence-ramp + weight knobs for `weighted_bayesian_milli`. The defaults
/// are the § Combined-score formula values (`bayesian_weight = 0.7`,
/// `bayesian_min_samples = 50`, `bayesian_full_confidence_samples = 200`); the
/// Tier-2 `mail.spam.bayesian_*` admin overrides are projected on the
/// `SpamPolicyThresholds` wire sub-struct and seeded into the off-nest scorer
/// at the search-equivalent position (`mail-policy-config.md` § Spam). The
/// weight is carried as milli (`700`) to avoid a float knob.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[cfg_attr(feature = "uniffi", derive(uniffi::Record))]
pub struct BayesianKnobs {
    pub bayesian_weight_milli: u32,
    pub min_samples: u32,
    pub full_confidence_samples: u32,
}

impl Default for BayesianKnobs {
    fn default() -> Self {
        Self {
            bayesian_weight_milli: 700,
            min_samples: 50,
            full_confidence_samples: 200,
        }
    }
}

/// Apply the confidence ramp + Bayesian weight to a raw per-user score.
///
/// `confidence = clamp((sample_count - min_samples) / (full_confidence_samples -
/// min_samples), 0, 1)`; below/at `min_samples` ⇒ confidence 0 ⇒ result 0 (the
/// cold-start clamp). Returns the already-weighted Bayesian contribution in
/// milli-units of the 0–15 scale, ready to pass as `weighted_bayesian_milli` to
/// `combined_spam_score_milli` (`mail-spam.md` § Combined-score formula).
pub fn weighted_bayesian_milli(
    raw_score_milli: i32,
    sample_count: u32,
    knobs: &BayesianKnobs,
) -> i32 {
    // Cold-start clamp: at or below the floor the per-user term is ignored.
    if sample_count <= knobs.min_samples {
        return 0;
    }
    let span = knobs
        .full_confidence_samples
        .saturating_sub(knobs.min_samples) as f64;
    // A non-positive span means full_confidence <= min (misconfigured); treat
    // anything past the floor as full confidence rather than dividing by zero.
    let confidence = if span <= 0.0 {
        1.0
    } else {
        (((sample_count - knobs.min_samples) as f64) / span).clamp(0.0, 1.0)
    };
    let weighted =
        raw_score_milli as f64 * confidence * (knobs.bayesian_weight_milli as f64 / 1000.0);
    weighted.round() as i32
}

// ── On-device scoring surface (UniFFI/WASM) ─────────────────────────────────────
//
// The apps (WASM/UniFFI) and the MDA (Go via UniFFI/cgo) run the shared
// scorer at the search-equivalent position (`mail-spam.md` § Scoring placement):
// they hold the decrypted message text and fetch the actor's model from nest as
// opaque bytes. `SpamModel`'s `BTreeMap` field is not a natural UniFFI/WASM
// value, so the model crosses the boundary as its `to_bytes` serde_json and the
// entry points below take it as `&[u8]` — one byte-oriented surface for every
// off-nest position, so the score is byte-identical to the nest-native path.

/// On-device per-user spam score from a serialized model.
///
/// `model_bytes` is the opaque `SpamModel::to_bytes` serde_json a client / the
/// MDA fetches from nest (the `spam_models` row). Decodes it (unreadable/empty ⇒
/// a fresh empty model), scores `text`, applies the confidence ramp + Bayesian
/// weight, and returns the `weighted_bayesian_milli` term ready to pass to
/// [`combined_spam_score_milli`](super::combined_spam_score_milli)
/// (`mail-spam.md` § Combined-score formula). A fresh or sub-`min_samples` model
/// returns 0 (the cold-start clamp), so an untrained user contributes nothing.
#[cfg_attr(feature = "uniffi", uniffi::export)]
pub fn weighted_bayesian_milli_for_model(
    model_bytes: &[u8],
    text: &str,
    knobs: BayesianKnobs,
) -> i32 {
    let model = SpamModel::from_bytes(model_bytes).unwrap_or_default();
    weighted_bayesian_milli(model.score(text), model.sample_count(), &knobs)
}

/// Fold the published deployment baseline into a just-unwrapped per-user model
/// at the **scoring agent** — the client / AUTH'd MDA session leg of the
/// read-time faded prior (`mail-spam.md` § Cold start Path 2 step 4). The
/// baseline arrives on the `fetch_spam_model` reply's additive `baseline`
/// field **only for a client-sealed stored model** (the nest folds a
/// plaintext-stored one itself — the no-double-fold rule, `mail-spam.md`
/// § Encrypted-mode interaction); the caller passes it through here after
/// unwrapping the model. Delegates to [`SpamModel::fold_baseline_faded`], the
/// one fade implementation every position shares, so scores stay
/// byte-identical. Tolerant: an empty or unparseable baseline (or an
/// unparseable model, or a model at/above `full_confidence_samples`) returns
/// `model_bytes` unchanged.
#[cfg_attr(feature = "uniffi", uniffi::export)]
pub fn fold_spam_model_baseline(
    model_bytes: Vec<u8>,
    baseline_bytes: Vec<u8>,
    full_confidence_samples: u32,
) -> Vec<u8> {
    if baseline_bytes.is_empty() {
        return model_bytes;
    }
    let Some(mut model) = SpamModel::from_bytes(&model_bytes) else {
        return model_bytes;
    };
    if full_confidence_samples == 0 || model.sample_count() >= full_confidence_samples {
        return model_bytes;
    }
    let Some(baseline) = SpamModel::from_bytes(&baseline_bytes) else {
        return model_bytes;
    };
    model.fold_baseline_faded(&baseline, full_confidence_samples);
    model.to_bytes()
}

/// The § Combined-score formula default knobs (`bayesian_weight = 0.7`,
/// `min_samples = 50`, `full_confidence_samples = 200`). The catalog defaults an
/// off-nest caller uses when it has no admin-effective `SpamPolicyThresholds`
/// snapshot yet (the MDA seeds the projected Tier-2 `mail.spam.bayesian_*`
/// overrides via `mailfauna.BayesianKnobsFromSnapshot`).
#[cfg_attr(feature = "uniffi", uniffi::export)]
pub fn default_bayesian_knobs() -> BayesianKnobs {
    BayesianKnobs::default()
}

/// The result of [`apply_spam_training`]: the mutated model to re-seal + the
/// forward n-gram delta to seal into the training-history row.
#[cfg_attr(feature = "uniffi", derive(uniffi::Record))]
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SpamTrainingMutation {
    /// The mutated model re-serialized ([`SpamModel::to_bytes`]) — the MDA re-seals
    /// these to the actor's **own** recipient key and writes them back opaque via
    /// `fauna.bridges.put_spam_model` (leg 2 write-back).
    pub new_model_bytes: Vec<u8>,
    /// `serde_json` of the distinct forward n-gram set this event touched — the same
    /// bytes a *server-written* `spam_training_history.model_delta_applied` stores
    /// plaintext; the MDA seals these into the history row so a later client-side
    /// undo can replay the exact inverse (`ModelWriteOp::Undo`). An empty set ⇒ `[]`.
    pub delta_json: Vec<u8>,
}

/// Apply one agent-side `\Junk`-train event to a **decoded (plaintext)** per-user
/// spam model — the mutate step of the Go MDA training a *sealed* model (leg 2 of
/// the tier-1 at-rest sealing end-game, `mail-spam.md`).
///
/// Once the model is sealed at rest the nest can no longer read-mutate-write it, so
/// the MDA does it agent-side: it opens the sealed model under its session MLS
/// capability, calls this to apply the training delta, then **re-seals**
/// `new_model_bytes` to the actor's own recipient key and **seals** `delta_json`
/// into a `spam_training_history` row (via `put_spam_model`'s `history_op`).
///
/// Pure — no crypto, no I/O (the open/re-seal live Go-side). Byte-for-byte
/// equivalent to the nest's `train_spam`/`train_ham` and the client
/// `ModelWriteOp::Train` (same `SpamModel` primitives): decode
/// (unreadable/empty ⇒ a fresh model, matching `load_or_create`), apply the forward
/// delta, size-cap ([`MODEL_MAX_BYTES_DEFAULT`] — the client/agent is the only
/// place the cap fires since the sealed blob is nest-opaque), re-serialize.
/// `is_spam` selects the label ([`SpamLabel::Spam`] vs [`SpamLabel::Ham`] — a
/// `\Junk`-move-in vs -out).
#[cfg_attr(feature = "uniffi", uniffi::export)]
pub fn apply_spam_training(model_bytes: &[u8], text: &str, is_spam: bool) -> SpamTrainingMutation {
    // Empty ⇒ a fresh current-version model (untrained actor), mirroring the client
    // `apply_and_reseal`; a non-empty-but-unreadable blob decodes to `default()`
    // (the `load_or_create` forward-compat path). Using `new()` for the empty case
    // keeps the version tag current so an untrained→trained→undo round-trips clean.
    let mut model = if model_bytes.is_empty() {
        SpamModel::new()
    } else {
        SpamModel::from_bytes(model_bytes).unwrap_or_default()
    };
    let label = if is_spam {
        SpamLabel::Spam
    } else {
        SpamLabel::Ham
    };
    let delta = SpamModel::delta_ngrams(text);
    model.apply_forward_delta_ngrams(&delta, label);
    model.cap_to_bytes(MODEL_MAX_BYTES_DEFAULT);
    // `[]` on the vanishingly unlikely serialize failure — a lost delta only
    // degrades a *later* client-side undo of this one event, never the model.
    let delta_json = serde_json::to_vec(&delta).unwrap_or_else(|_| b"[]".to_vec());
    SpamTrainingMutation {
        new_model_bytes: model.to_bytes(),
        delta_json,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::BTreeSet;

    #[test]
    fn fold_spam_model_baseline_is_tolerant_and_folds() {
        let mut baseline = SpamModel::new();
        for _ in 0..3 {
            baseline.train_spam("qzbasetokwx urgent offer");
            baseline.train_ham("ham note");
        }
        let mut own = SpamModel::new();
        own.train_spam("cheap pills");
        own.train_ham("team lunch");
        let own_bytes = own.to_bytes();

        // The fold == the shared fade applied to the decoded pair.
        let folded = fold_spam_model_baseline(own_bytes.clone(), baseline.to_bytes(), 200);
        let mut expected = own.clone();
        expected.fold_baseline_faded(&baseline, 200);
        assert_eq!(
            folded,
            expected.to_bytes(),
            "fold == SpamModel::fold_baseline_faded"
        );

        // Tolerance: empty baseline, unparseable baseline, unparseable model,
        // and a 0 horizon all return the model bytes unchanged.
        assert_eq!(
            fold_spam_model_baseline(own_bytes.clone(), Vec::new(), 200),
            own_bytes,
            "empty baseline ⇒ verbatim"
        );
        assert_eq!(
            fold_spam_model_baseline(own_bytes.clone(), vec![0xEE; 32], 200),
            own_bytes,
            "unparseable baseline ⇒ verbatim"
        );
        assert_eq!(
            fold_spam_model_baseline(vec![0xEE; 32], baseline.to_bytes(), 200),
            vec![0xEE; 32],
            "unparseable (still-sealed) model ⇒ verbatim"
        );
        assert_eq!(
            fold_spam_model_baseline(own_bytes.clone(), baseline.to_bytes(), 0),
            own_bytes,
            "0 horizon ⇒ guarded no-op"
        );
    }

    #[test]
    fn weighted_bayesian_ramp() {
        let k = BayesianKnobs::default();
        let raw = 15000;
        assert_eq!(weighted_bayesian_milli(raw, 49, &k), 0);
        assert_eq!(weighted_bayesian_milli(raw, 50, &k), 0);
        assert_eq!(
            weighted_bayesian_milli(raw, 125, &k),
            (raw as f64 * 0.5 * 0.7).round() as i32
        );
        assert_eq!(
            weighted_bayesian_milli(raw, 200, &k),
            (raw as f64 * 0.7).round() as i32
        );
        assert_eq!(
            weighted_bayesian_milli(raw, 300, &k),
            weighted_bayesian_milli(raw, 200, &k)
        );
    }

    #[test]
    fn apply_spam_training_mutates_and_returns_the_exact_forward_delta() {
        // Empty bytes ⇒ a fresh model (the untrained forward-compat path).
        let out = apply_spam_training(b"", "buy cheap pills now", true);
        let model = SpamModel::from_bytes(&out.new_model_bytes).expect("decode mutated model");
        assert_eq!(model.spam_messages, 1);
        assert_eq!(model.ham_messages, 0);
        // The returned delta_json is exactly the forward n-gram set the event
        // touched — so the MDA seals it into the history row for an exact undo.
        let delta: BTreeSet<String> = serde_json::from_slice(&out.delta_json).expect("delta json");
        assert_eq!(delta, SpamModel::delta_ngrams("buy cheap pills now"));
        assert!(!delta.is_empty());
    }

    #[test]
    fn apply_spam_training_delta_is_the_exact_inverse() {
        // The returned delta inverts the event byte-for-byte — the guarantee the
        // client-side undo relies on (leg 1c reads the sealed model_delta_applied).
        let out = apply_spam_training(b"", "limited offer act now", true);
        let mut mutated = SpamModel::from_bytes(&out.new_model_bytes).expect("decode");
        let delta: BTreeSet<String> = serde_json::from_slice(&out.delta_json).unwrap();
        mutated.apply_inverse_delta_ngrams(&delta, SpamLabel::Spam);
        assert_eq!(mutated, SpamModel::new());
    }

    #[test]
    fn apply_spam_training_ham_label_trains_ham() {
        // `\Junk`-move-OUT ⇒ a ham train (is_spam = false).
        let out = apply_spam_training(b"", "weekly team sync agenda", false);
        let model = SpamModel::from_bytes(&out.new_model_bytes).expect("decode");
        assert_eq!(model.ham_messages, 1);
        assert_eq!(model.spam_messages, 0);
    }

    #[test]
    fn cold_start_neutral() {
        let m = SpamModel::new();
        assert_eq!(m.score("anything at all here"), 7500);
        let k = BayesianKnobs::default();
        assert_eq!(weighted_bayesian_milli(7500, m.sample_count(), &k), 0);
        assert_eq!(m.sample_count(), 0);
    }

    #[test]
    fn merge_scaled_cold_start_fade_compensates_confidence_ramp() {
        // The cold-start property the faded read-time merge buys: a fresh actor
        // (0 own samples) inheriting the full baseline scores its distinctive
        // spam token ABOVE the spam_folder threshold, with no cliff. We model
        // `fetch_spam_model`'s fade policy here (the handler computes the same
        // fraction) and check the scored, weighted, combined value.
        use crate::spam::combined_spam_score_milli;
        let knobs = BayesianKnobs::default();
        let full = knobs.full_confidence_samples; // 200

        // A baseline that maps a distinctive token strongly to spam at balanced
        // priors (so the prior is neutral and the token drives the score),
        // mirroring the e2e fixture calibration.
        let mut baseline = SpamModel::new();
        for _ in 0..full {
            baseline.train_spam("qzbaselinespamwx urgent offer");
            baseline.train_ham("qzhamtoken routine note");
        }

        // Fade fraction for a fresh actor: (full - 0)/full == 1/1 == full merge.
        let mut fresh = SpamModel::new();
        let own_samples = fresh.sample_count();
        let numerator = full.saturating_sub(own_samples.min(full));
        fresh.merge_scaled(&baseline, numerator, full);

        let raw = fresh.score("qzbaselinespamwx is a great deal");
        let weighted = weighted_bayesian_milli(raw, fresh.sample_count(), &knobs);
        let combined = combined_spam_score_milli(0, weighted);
        // spam_folder default threshold is 5 on the 0–15 scale (5000 milli).
        assert!(
            combined > 5000,
            "a cold-start actor seeded with the full baseline must score its \
             spam token above the spam_folder threshold; got {combined} milli"
        );

        // A mature actor (own samples >= full confidence) merges 0 baseline:
        // its own model fully governs (the user-controls-their-filter property).
        let mut mature = SpamModel::new();
        for _ in 0..full {
            mature.train_ham("qzbaselinespamwx is actually wanted mail");
        }
        let mature_num = full.saturating_sub(mature.sample_count().min(full)); // 0
        assert_eq!(
            mature_num, 0,
            "a full-confidence actor fades the baseline to 0"
        );
        let before = mature.clone();
        mature.merge_scaled(&baseline, mature_num, full);
        assert_eq!(
            mature, before,
            "a mature actor's read-time model is its own model only — the \
             baseline never overrides a user who has trained to full confidence"
        );
    }

    #[test]
    fn weighted_bayesian_milli_for_model_matches_two_step() {
        // Train past full confidence so the weight is non-zero, serialize, and
        // score through the byte-oriented on-device entry point.
        let mut m = SpamModel::new();
        for _ in 0..120 {
            m.train_spam("buy cheap pills now claim your prize");
            m.train_ham("lunch meeting agenda notes for tomorrow");
        }
        let bytes = m.to_bytes();
        let k = BayesianKnobs::default();

        let spammy = weighted_bayesian_milli_for_model(&bytes, "cheap pills prize", k);
        let hammy = weighted_bayesian_milli_for_model(&bytes, "meeting agenda tomorrow", k);
        assert!(spammy > hammy, "spammy={spammy} hammy={hammy}");
        assert!(
            spammy > 0,
            "trained-past-floor model must contribute: {spammy}"
        );

        // The byte entry point is exactly the two-step (score → weight) path.
        assert_eq!(
            spammy,
            weighted_bayesian_milli(m.score("cheap pills prize"), m.sample_count(), &k),
        );
    }

    #[test]
    fn weighted_bayesian_milli_for_model_unreadable_is_cold_start_zero() {
        // Unreadable/empty bytes decode to a fresh model ⇒ sample_count 0 ⇒ the
        // cold-start clamp returns 0 (an untrained user contributes nothing).
        let k = BayesianKnobs::default();
        assert_eq!(
            weighted_bayesian_milli_for_model(b"not a model", "anything here", k),
            0
        );
        assert_eq!(
            weighted_bayesian_milli_for_model(&[], "anything here", k),
            0
        );
    }

    #[test]
    fn e2e_seeded_model_scores_token_above_and_below_default_spam_folder() {
        // Pins the tier_3 fixture's scoring calibration (tests/e2e-unified
        // conftest `_seed_spam_model` / `mda_spam_scoring`, consumed by
        // test_mail_bridge_mda.py's spam-scoring tests) to the shared scorer, so
        // a formula/tokenizer/knob change fails HERE — fast — rather than in the
        // opt-in tier_3. The fixture seeds a per-user model that maps one nonce
        // unigram to spam and another to ham over balanced class counts
        // (full confidence); a message bearing the spam unigram must cross the
        // default `spam_folder` tier (5 → 5000 milli — fauna_protocol
        // `SpamPolicyThresholds::default().max_score_before_spam_folder`, the Go
        // bridge's `DefaultSpamPolicyThresholds()`), and one bearing the ham
        // unigram must stay well below it.
        const SPAM_FOLDER_MILLI: i32 = 5000;
        let spam_token = "qzspamtokenwx";
        let ham_token = "qzhamtokenwx";

        let mut model = SpamModel::new();
        model
            .ngrams
            .insert(spam_token.to_string(), NgramCount { spam: 110, ham: 0 });
        model
            .ngrams
            .insert(ham_token.to_string(), NgramCount { spam: 0, ham: 110 });
        model.spam_messages = 110;
        model.ham_messages = 110;
        assert_eq!(model.sample_count(), 220, "balanced, past full_confidence");

        let bytes = model.to_bytes();
        let k = BayesianKnobs::default();
        // The exact message bodies the e2e APPENDs (only the distinctive unigram
        // is in the model; every other token is unseen ⇒ skipped ⇒ neutral).
        let spam_score = weighted_bayesian_milli_for_model(
            &bytes,
            &format!("Act now: the {spam_token} offer expires today."),
            k,
        );
        let ham_score = weighted_bayesian_milli_for_model(
            &bytes,
            &format!("Thanks for the {ham_token} notes from the meeting."),
            k,
        );
        assert!(
            spam_score >= SPAM_FOLDER_MILLI,
            "spam-token message must cross the default spam_folder threshold; got {spam_score}"
        );
        assert!(
            ham_score < SPAM_FOLDER_MILLI,
            "ham-token message must stay below the default spam_folder threshold; got {ham_score}"
        );
    }

    #[test]
    fn default_bayesian_knobs_are_the_formula_defaults() {
        assert_eq!(default_bayesian_knobs(), BayesianKnobs::default());
        assert_eq!(default_bayesian_knobs().bayesian_weight_milli, 700);
        assert_eq!(default_bayesian_knobs().min_samples, 50);
        assert_eq!(default_bayesian_knobs().full_confidence_samples, 200);
    }
}
