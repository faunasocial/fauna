//! On-device per-user mail spam scorer (WASM) — the browser twin of the Go MDA's
//! `scoreSelectedInbox` scoring (`bins/fauna-bridges/internal/mda/imap/spam_score.go`).
//!
//! Exposes the shared `fauna_mail::spam` scorer to the web SPA so a web-primary
//! user's INBOX can be per-user-scored on-device (post-decrypt), realizing the
//! third scoring position — the Fauna app — of the "one shared-Rust scorer
//! compiles to nest/MDA/clients" shape (`docs/goal/architecture/content-scoring.md`
//! § The shared-scorer shape; `docs/goal/behavior/mail-spam.md` § Scoring
//! placement). The surface mirrors the Go binding the MDA consumes
//! (`mailfauna.WeightedBayesianMilliForModel` + `CombinedSpamScoreMilli`) exactly,
//! so the score is byte-identical to the nest-native and MDA positions (priority
//! #2/#3). The model + body **unwrap** (decrypt of the sealed-to-actor model and
//! bodies) is a separate concern the caller does first — this module scores
//! already-decrypted plaintext with an already-decrypted model, exactly as the Go
//! `score(modelBytes, string(pt))` call does.

use fauna_mail::spam::{
    BayesianKnobs, combined_spam_score_milli as core_combined_spam_score_milli,
    default_bayesian_knobs as core_default_bayesian_knobs,
    weighted_bayesian_milli_for_model as core_weighted_bayesian_milli_for_model,
};
use serde::Serialize;
use wasm_bindgen::prelude::*;

/// `fauna_mail::spam::weighted_bayesian_milli_for_model` → the already-weighted
/// per-user Bayesian contribution (milli-units of the 0–15 scale) for a decrypted
/// message, ready to pass to [`combined_spam_score_milli`].
///
/// `model_bytes` is the opaque `SpamModel::to_bytes` serde_json the caller fetched
/// (`fetch_spam_model`) and **unwrapped** under the session capability (an
/// unreadable/empty model ⇒ a fresh empty model ⇒ 0, the cold-start clamp, so an
/// untrained actor contributes nothing). The three knob scalars are the
/// admin-effective `mail.spam.bayesian_*` overrides (or [`defaultBayesianKnobs`]
/// when the SPA has no policy snapshot) — the wasm mirror of the MDA's
/// `s.bayesianKnobs`, so an admin's confidence-ramp/weight override reaches the
/// on-device scorer identically (`docs/goal/behavior/mail-policy-config.md` § Spam).
#[wasm_bindgen(js_name = weightedBayesianMilliForModel)]
pub fn weighted_bayesian_milli_for_model(
    model_bytes: &[u8],
    text: &str,
    bayesian_weight_milli: u32,
    min_samples: u32,
    full_confidence_samples: u32,
) -> i32 {
    core_weighted_bayesian_milli_for_model(
        model_bytes,
        text,
        BayesianKnobs {
            bayesian_weight_milli,
            min_samples,
            full_confidence_samples,
        },
    )
}

/// `fauna_mail::spam::combined_spam_score_milli` → `max(rspamd_scaled, weighted)`
/// floored at 0 (`docs/goal/behavior/mail-spam.md` § Combined-score formula).
///
/// For the on-device INBOX pass `rspamd_scaled_milli` is `0` — rspamd already
/// scored at ingest and routed anything over a tier to Junk (so it isn't in
/// INBOX); this pass adds only the per-user Bayesian term, exactly as the MDA's
/// `CombinedSpamScoreMilli(0, weighted)` does. Compare the result against the
/// admin-effective `spam_folder` threshold scaled to milli (`points * 1000`) to
/// decide the INBOX→Junk re-file.
#[wasm_bindgen(js_name = combinedSpamScoreMilli)]
pub fn combined_spam_score_milli(rspamd_scaled_milli: i32, weighted_bayesian_milli: i32) -> i32 {
    core_combined_spam_score_milli(rspamd_scaled_milli, weighted_bayesian_milli)
}

/// The § Combined-score formula catalog default knobs (`bayesian_weight = 0.7`,
/// `min_samples = 50`, `full_confidence_samples = 200`) as `{ bayesianWeightMilli,
/// minSamples, fullConfidenceSamples }` — the wasm mirror of
/// `fauna_mail::spam::default_bayesian_knobs`. The SPA passes these to
/// [`weightedBayesianMilliForModel`] as the cold-start fallback when it has no
/// admin `SpamPolicyThresholds` snapshot yet, so the defaults are single-sourced
/// in Rust and never hard-coded (and thus never drift) in JS.
#[wasm_bindgen(js_name = defaultBayesianKnobs)]
pub fn default_bayesian_knobs() -> Result<JsValue, JsValue> {
    let k = core_default_bayesian_knobs();
    crate::rpc::to_js(&JsBayesianKnobs {
        bayesian_weight_milli: k.bayesian_weight_milli,
        min_samples: k.min_samples,
        full_confidence_samples: k.full_confidence_samples,
    })
}

/// camelCase serde mirror of `fauna_mail::spam::BayesianKnobs` for the JS
/// boundary (the core type derives neither `Serialize` nor a wasm binding — it
/// crosses as three scalars in / this object out).
#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct JsBayesianKnobs {
    bayesian_weight_milli: u32,
    min_samples: u32,
    full_confidence_samples: u32,
}
