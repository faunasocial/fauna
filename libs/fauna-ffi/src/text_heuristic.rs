//! UniFFI façade for the shared built-in text-heuristic classifier
//! (`fauna_core::text_heuristic`).
//!
//! The spam/phishing heuristic — caps ratio, URL count, spam phrases, urgency /
//! credential words, suspicious-URL patterns — is the canonical shared classifier
//! used by nest server-side ingest and `fauna_wasm` client-side post-decryption
//! scanning (`docs/goal/behavior/moderation.md` § Where logic lives: "Classifier
//! (spam / phishing) … must be shared Rust"). This façade lets the native apps
//! call it over UniFFI so they stop re-implementing it: android previously
//! hand-rolled the identical algorithm in `TextHeuristic.kt`, kept in lockstep with
//! the Rust impl only by a parallel parity-test pair — exactly the silent-drift risk
//! priority #1/#4 removes by collapsing onto one definition.
//!
//! It routes through `fauna_client_core::scan::classify_text` — the designated
//! client-side scanning entry point web already consumes over WASM
//! (`fauna_wasm::classifyText`) — so web and the native apps enter the shared
//! heuristic at one identical surface (it is a thin remap over
//! `fauna_core::text_heuristic`, so the scores are byte-identical).
//!
//! Like `src/feed.rs`/`src/markdown.rs`, the `#[uniffi::export]` fn returns a
//! **fauna-ffi-local** `uniffi::Record` (built-in `String`/`f64` fields, not a bare
//! library type) so uniffi-bindgen-go emits a self-contained Go binding — no
//! cross-namespace import, hence no feature gate.

use fauna_client_core::scan::classify_text as classify;

/// One text-classification label: a `category` (`"spam"` | `"phishing"`) and a
/// `confidence` in `0.0..=1.0`. Mirror of [`fauna_client_core::scan::ClassificationResult`].
#[derive(Debug, uniffi::Record)]
pub struct FfiClassifyLabel {
    pub category: String,
    pub confidence: f64,
}

/// Run the built-in spam/phishing text heuristic on `text`, returning one label per
/// category whose confidence is `> 0.0` (an empty list when the text trips no signal).
/// Pure string computation; the canonical implementation lives in
/// `fauna_core::text_heuristic` and is consumed identically by web (WASM) and nest.
#[uniffi::export]
pub fn classify_text(text: String) -> Vec<FfiClassifyLabel> {
    classify(&text)
        .into_iter()
        .map(|r| FfiClassifyLabel {
            category: r.category,
            confidence: r.confidence,
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn spammy_text_maps_to_spam_label() {
        let labels = classify_text(
            "BUY NOW!!! CLICK HERE for FREE MONEY!!! https://a.com https://b.com https://c.com https://d.com"
                .to_string(),
        );
        let spam = labels.iter().find(|l| l.category == "spam");
        assert!(spam.is_some(), "expected a spam label, got {labels:?}");
        assert!(spam.unwrap().confidence > 0.0);
    }

    #[test]
    fn clean_text_yields_no_labels() {
        assert!(classify_text("Just a normal post about my day.".to_string()).is_empty());
    }
}
