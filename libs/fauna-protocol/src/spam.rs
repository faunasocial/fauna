//! User-facing WS-RPC payload types for the spam-classifier preferences
//! surface — `fauna.spam.{get_preferences,set_preferences}`. A faithful
//! transport migration of the former `GET|PUT /api/v1/spam/preferences` HTTP
//! routes; the two thresholds
//! map to the settled `ui.yaml` Settings IDs (`spam-threshold`,
//! `phishing-threshold`). Slice scoped and tracked internally.
//!
//! Kind registry entries live in `kind.rs::register_spam_kinds`.

use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

use fauna_cbor::Value;

// ── fauna.spam.get_preferences ─────────────────────────────────────────

/// Read the calling actor's spam-classifier preferences. Empty request —
/// the WS-RPC connection knows its caller (the HTTP twin's bearer-actor is
/// implicit).
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct SpamGetPreferencesRequest {
    #[serde(flatten, default)]
    pub extra: BTreeMap<String, Value>,
}

/// The spam-preferences object (the two thresholds) — the reply to both
/// `get_preferences` and `set_preferences` (the latter echoes the
/// resulting state, unlike the HTTP twin's `{"status":"updated"}`).
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct SpamPreferences {
    /// Spam score threshold as **per-mille** (0–1000 = probability `[0.0, 1.0]`
    /// × 1000). Integer because the dag-cbor wire forbids floats — see
    /// `docs/goal/architecture/serialization.md` § Floats.
    pub spam_threshold: u16,
    /// Phishing score threshold as **per-mille** (0–1000).
    pub phishing_threshold: u16,
    #[serde(flatten, default)]
    pub extra: BTreeMap<String, Value>,
}

// ── fauna.spam.set_preferences ─────────────────────────────────────────

/// Partial update of the calling actor's spam-classifier preferences —
/// only the `Some(_)` fields change; `None` keeps the stored value
/// (mirrors the HTTP twin's `payload.get(...)`-per-field semantics). The
/// nest clamps the thresholds to `[0, 1000]` per-mille. The reply echoes
/// the resulting full `SpamPreferences`.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct SpamSetPreferencesRequest {
    /// Per-mille (0–1000); `None` keeps the stored value.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub spam_threshold: Option<u16>,
    /// Per-mille (0–1000); `None` keeps the stored value.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub phishing_threshold: Option<u16>,
    #[serde(flatten, default)]
    pub extra: BTreeMap<String, Value>,
}

// ── Presentation contract (shared across all 7 apps) ───────────────
//
// The wire carries `spam_threshold` as per-mille `u16`, but the
// *presentation* of that value — the threshold slider's scale/step and its
// label bands — must be identical on every app (`docs/goal/ui/settings.md` § Spam
// threshold slider labels: "the same range/label mapping applies on every
// app"). This block is that single source of truth, mirroring the
// `email::encode_filter_rule` precedent: the pure logic lives here, with a
// wasm face (`fauna-wasm` `spamThresholdBand`) and a UniFFI face
// (`fauna-ffi`) so web and the native apps render from one definition
// instead of each re-deriving the buckets (priority #2).

/// Inclusive per-mille bounds + step for the spam/phishing threshold sliders.
/// `100` per-mille == `0.1` probability — the step the goal doc mandates so
/// the label bands below have no gaps at slider positions.
pub const SPAM_THRESHOLD_MIN_PER_MILLE: u16 = 0;
pub const SPAM_THRESHOLD_MAX_PER_MILLE: u16 = 1000;
pub const SPAM_THRESHOLD_STEP_PER_MILLE: u16 = 100;

/// The three label bands a spam threshold falls into. The numeric ranges are
/// fixed by `docs/goal/ui/settings.md`; [`spam_threshold_band`] is the canonical
/// mapping and [`SpamThresholdBand::key`] yields the `spam:` i18n key each app
/// localizes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SpamThresholdBand {
    /// `0.0–0.3` — filters aggressively.
    Aggressive,
    /// `0.4–0.7` — the middle ground (and the spec's undefined gaps).
    Moderate,
    /// `0.8–1.0` — permissive; little is flagged.
    Permissive,
}

impl SpamThresholdBand {
    /// The `spam:` i18n string key (`aggressive`/`moderate`/`permissive` in
    /// `i18n/strings/en.yaml`) — the shells that localize outside Rust (the
    /// UniFFI four and web, via `fauna-ffi` / `fauna-wasm`) map this key
    /// themselves.
    pub fn key(self) -> &'static str {
        match self {
            SpamThresholdBand::Aggressive => "aggressive",
            SpamThresholdBand::Moderate => "moderate",
            SpamThresholdBand::Permissive => "permissive",
        }
    }

    /// The localized band label, for the shells that resolve strings **in
    /// Rust** — the one owner of the band → `status.spam.*` map that linux and
    /// tui each hand-rolled beside their own slider.
    ///
    /// It lives here rather than in a client crate because this is where the
    /// band and [`key`](Self::key) already live, and `fauna-i18n` is already an
    /// unconditional dependency of this crate — the tidier-looking home
    /// (`fauna-client-spam`) would have had to grow a new dependency, and one
    /// that compiles to wasm for shells that never want an English constant.
    pub fn label(self) -> &'static str {
        use fauna_i18n::strings::status::spam;
        match self {
            SpamThresholdBand::Aggressive => spam::AGGRESSIVE,
            SpamThresholdBand::Moderate => spam::MODERATE,
            SpamThresholdBand::Permissive => spam::PERMISSIVE,
        }
    }
}

/// The localized threshold-band label for a probability in `[0.0, 1.0]` — the
/// whole slider-value → label path in one call, so a shell renders it without
/// re-deriving either the bucket boundaries or the label map.
///
/// Out-of-range values follow [`spam_threshold_band`]'s defensive reading.
pub fn spam_band_label(probability: f64) -> &'static str {
    spam_threshold_band(probability_to_per_mille(probability)).label()
}

/// Map a per-mille spam threshold (`0–1000`) to its label band. Total over all
/// `u16`: `<=300` Aggressive, `>=800` Permissive, else Moderate — the spec's
/// bands at every step-0.1 point, matching web's settled inline logic. Over-range
/// values (the nest clamps to `1000`) read as Permissive defensively.
pub fn spam_threshold_band(per_mille: u16) -> SpamThresholdBand {
    if per_mille <= 300 {
        SpamThresholdBand::Aggressive
    } else if per_mille >= 800 {
        SpamThresholdBand::Permissive
    } else {
        SpamThresholdBand::Moderate
    }
}

/// Probability `[0.0, 1.0]` → wire per-mille `[0, 1000]`. The dag-cbor wire
/// forbids floats (`docs/goal/architecture/serialization.md` § Floats), so the
/// spam/phishing thresholds ride as integers while every app's slider works
/// in probability. The input is clamped to `[0.0, 1.0]` and rounded **half away
/// from zero**, so the result is always a valid `[0, 1000]` per-mille value AND
/// every app rounds identically — the single definition the nest, the wasm
/// web path, and the native apps all share instead of each hand-rolling
/// `* 1000` (priority #1/#2/#4). Hand-rolled copies risked divergence at exact
/// half-per-mille slider points (C#'s `Math.Round` is banker's-rounding).
pub fn probability_to_per_mille(probability: f64) -> u16 {
    (probability.clamp(0.0, 1.0) * 1000.0).round() as u16
}

/// Wire per-mille `[0, 1000]` → probability `[0.0, 1.0]` — the inverse of
/// [`probability_to_per_mille`]. Over-range per-mille (the nest clamps writes to
/// [`SPAM_THRESHOLD_MAX_PER_MILLE`]) saturates at `1.0` defensively.
pub fn per_mille_to_probability(per_mille: u16) -> f64 {
    f64::from(per_mille.min(SPAM_THRESHOLD_MAX_PER_MILLE)) / 1000.0
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::codec::{decode_strict as decode, encode_canonical};

    fn sample_preferences() -> SpamPreferences {
        SpamPreferences {
            spam_threshold: 800,
            phishing_threshold: 600,
            extra: BTreeMap::new(),
        }
    }

    #[test]
    fn get_request_is_empty_and_round_trips() {
        let req = SpamGetPreferencesRequest {
            extra: BTreeMap::new(),
        };
        let bytes = encode_canonical(&req).unwrap();
        let decoded: SpamGetPreferencesRequest = decode(&bytes).unwrap();
        assert_eq!(req, decoded);
    }

    #[test]
    fn preferences_round_trips() {
        let prefs = sample_preferences();
        let bytes = encode_canonical(&prefs).unwrap();
        let decoded: SpamPreferences = decode(&bytes).unwrap();
        assert_eq!(prefs, decoded);
    }

    #[test]
    fn preferences_canonical_re_encodes_identically() {
        let prefs = sample_preferences();
        let bytes1 = encode_canonical(&prefs).unwrap();
        let decoded: SpamPreferences = decode(&bytes1).unwrap();
        let bytes2 = encode_canonical(&decoded).unwrap();
        assert_eq!(bytes1, bytes2);
    }

    #[test]
    fn set_request_round_trips_with_partial_fields() {
        // Only one of the two fields set — the other stays None (untouched).
        let req = SpamSetPreferencesRequest {
            spam_threshold: Some(420),
            phishing_threshold: None,
            extra: BTreeMap::new(),
        };
        let bytes = encode_canonical(&req).unwrap();
        let decoded: SpamSetPreferencesRequest = decode(&bytes).unwrap();
        assert_eq!(req, decoded);
    }

    #[test]
    fn set_request_full_round_trips() {
        let req = SpamSetPreferencesRequest {
            spam_threshold: Some(100),
            phishing_threshold: Some(200),
            extra: BTreeMap::new(),
        };
        let bytes = encode_canonical(&req).unwrap();
        let decoded: SpamSetPreferencesRequest = decode(&bytes).unwrap();
        assert_eq!(req, decoded);
    }

    // ── Presentation contract: threshold bands ─────────────────────────────
    // Canonical mapping from `docs/goal/ui/settings.md` § Spam threshold
    // slider labels: 0.0–0.3 Aggressive / 0.4–0.7 Moderate / 0.8–1.0
    // Permissive, step 0.1, identical on every app.

    #[test]
    fn threshold_band_boundaries_match_goal_doc() {
        use SpamThresholdBand::*;
        // Aggressive: 0.0–0.3 → per-mille 0–300.
        assert_eq!(spam_threshold_band(0), Aggressive);
        assert_eq!(spam_threshold_band(300), Aggressive);
        // The spec gap 0.3–0.4 (per-mille 301–399) falls to Moderate (no
        // step-0.1 value lands here; the fn is total, so this is the choice
        // that matches web's settled inline logic `<=300 / >=800 / else`).
        assert_eq!(spam_threshold_band(301), Moderate);
        // Moderate: 0.4–0.7 → 400–700.
        assert_eq!(spam_threshold_band(400), Moderate);
        assert_eq!(spam_threshold_band(700), Moderate);
        assert_eq!(spam_threshold_band(799), Moderate);
        // Permissive: 0.8–1.0 → 800–1000 (and any over-range value, defensive).
        assert_eq!(spam_threshold_band(800), Permissive);
        assert_eq!(spam_threshold_band(1000), Permissive);
        assert_eq!(spam_threshold_band(5000), Permissive);
    }

    /// Every variant maps, and to the constant its own key names — the pairing
    /// that lets a shell use either face without them drifting apart.
    #[test]
    fn band_label_maps_every_variant_to_its_keys_string() {
        use fauna_i18n::strings::status::spam;
        for (band, label) in [
            (SpamThresholdBand::Aggressive, spam::AGGRESSIVE),
            (SpamThresholdBand::Moderate, spam::MODERATE),
            (SpamThresholdBand::Permissive, spam::PERMISSIVE),
        ] {
            assert_eq!(band.label(), label);
            assert_eq!(
                fauna_i18n::strings::lookup(&format!("status.spam.{}", band.key())),
                Some(label),
                "key() and label() must name the same string"
            );
        }
    }

    /// The slider-value path linux and tui each used to re-derive, pinned at
    /// the band boundaries `spam_threshold_band` defines.
    #[test]
    fn spam_band_label_spans_the_slider() {
        use fauna_i18n::strings::status::spam;
        assert_eq!(spam_band_label(0.0), spam::AGGRESSIVE);
        assert_eq!(spam_band_label(0.3), spam::AGGRESSIVE);
        assert_eq!(spam_band_label(0.5), spam::MODERATE);
        assert_eq!(spam_band_label(0.8), spam::PERMISSIVE);
        assert_eq!(spam_band_label(1.0), spam::PERMISSIVE);
    }

    #[test]
    fn band_keys_match_i18n_string_keys() {
        // Keys must equal the `spam:` i18n keys in i18n/strings/en.yaml so
        // every app maps band → localized label uniformly.
        assert_eq!(SpamThresholdBand::Aggressive.key(), "aggressive");
        assert_eq!(SpamThresholdBand::Moderate.key(), "moderate");
        assert_eq!(SpamThresholdBand::Permissive.key(), "permissive");
    }

    #[test]
    fn step_const_is_one_tenth() {
        // 0.1 probability == 100 per-mille; the spec's mandated step.
        assert_eq!(SPAM_THRESHOLD_STEP_PER_MILLE, 100);
        assert_eq!(SPAM_THRESHOLD_MAX_PER_MILLE, 1000);
        assert_eq!(SPAM_THRESHOLD_MIN_PER_MILLE, 0);
    }

    // ── probability ↔ per-mille conversion (the slider's wire codec) ──────────

    #[test]
    fn probability_to_per_mille_scales_clamps_and_rounds() {
        // Exact scale at the canonical anchors.
        assert_eq!(probability_to_per_mille(0.0), 0);
        assert_eq!(probability_to_per_mille(0.5), 500);
        assert_eq!(probability_to_per_mille(1.0), 1000);
        // Rounds half away from zero — the uniform rule that fixes the
        // C# banker's-rounding divergence the inline copies risked.
        assert_eq!(probability_to_per_mille(0.0005), 1); // 0.5 per-mille → 1
        assert_eq!(probability_to_per_mille(0.4567), 457);
        // Out-of-range input clamps to the valid per-mille bounds (no overflow).
        assert_eq!(probability_to_per_mille(-0.3), SPAM_THRESHOLD_MIN_PER_MILLE);
        assert_eq!(probability_to_per_mille(1.7), SPAM_THRESHOLD_MAX_PER_MILLE);
    }

    #[test]
    fn per_mille_to_probability_is_the_inverse_and_saturates() {
        assert_eq!(per_mille_to_probability(0), 0.0);
        assert_eq!(per_mille_to_probability(500), 0.5);
        assert_eq!(per_mille_to_probability(1000), 1.0);
        // Over-range per-mille saturates at 1.0 (defensive — nest clamps writes).
        assert_eq!(per_mille_to_probability(5000), 1.0);
    }

    #[test]
    fn every_canonical_step_maps_to_its_exact_per_mille() {
        // Each slider step (0.0, 0.1, … 1.0) lands on its exact per-mille
        // multiple despite f64 rounding, so the shared conversion never drifts a
        // stored preference on re-save.
        for step in 0..=10u16 {
            let p = f64::from(step) / 10.0;
            assert_eq!(
                probability_to_per_mille(p),
                step * SPAM_THRESHOLD_STEP_PER_MILLE
            );
        }
    }
}
