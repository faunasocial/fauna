//! Built-in text heuristic classifier.
//!
//! Produces labels for spam and phishing based on simple text signals.
//! Shared between fauna-nest (server-side ingest) and fauna-wasm (client-side
//! post-decryption scanning). Pure string computation, WASM-safe.

use crate::label::{CLASSIFIER_TEXT_HEURISTIC, builtin_classifier_id};

/// Result of running the text heuristic classifier.
#[derive(Debug, Clone)]
pub struct HeuristicResult {
    pub category: String,
    pub confidence: f64,
}

/// Well-known classifier ID for the built-in text heuristic.
pub fn classifier_id() -> [u8; 32] {
    builtin_classifier_id(CLASSIFIER_TEXT_HEURISTIC)
}

/// Run text heuristic classification on post content.
///
/// Returns a list of (category, confidence) labels. Only returns labels
/// where confidence > 0.0.
pub fn classify_text(text: &str) -> Vec<HeuristicResult> {
    let mut results = Vec::new();

    let spam_score = spam_confidence(text);
    if spam_score > 0.0 {
        results.push(HeuristicResult {
            category: "spam".into(),
            confidence: spam_score,
        });
    }

    let phishing_score = phishing_confidence(text);
    if phishing_score > 0.0 {
        results.push(HeuristicResult {
            category: "phishing".into(),
            confidence: phishing_score,
        });
    }

    results
}

/// Estimate spam probability from text signals.
fn spam_confidence(text: &str) -> f64 {
    let mut score: f64 = 0.0;
    let lower = text.to_lowercase();
    let len = text.len() as f64;
    if len == 0.0 {
        return 0.0;
    }

    if text.len() >= 20 {
        let caps_ratio = text.chars().filter(|c| c.is_uppercase()).count() as f64
            / text.chars().count().max(1) as f64;
        if caps_ratio > 0.5 {
            score += 0.3;
        }
    }

    let url_count = lower.matches("http://").count() + lower.matches("https://").count();
    if url_count > 3 {
        score += 0.2;
    } else if url_count > 1 && len < 200.0 {
        score += 0.1;
    }

    let spam_phrases = [
        "buy now",
        "act now",
        "limited time",
        "free money",
        "click here",
        "congratulations you",
        "you have been selected",
        "earn money",
        "make money fast",
        "no obligation",
        "risk free",
        "winner",
        "100% free",
        "double your",
        "guaranteed income",
    ];
    let phrase_hits = spam_phrases.iter().filter(|p| lower.contains(**p)).count();
    score += phrase_hits as f64 * 0.15;

    let exclaim_count = text.chars().filter(|c| *c == '!' || *c == '?').count();
    if exclaim_count > 5 {
        score += 0.1;
    }

    let mut max_repeat = 0u32;
    let mut current_repeat = 1u32;
    let mut prev_char = '\0';
    for c in text.chars() {
        if c == prev_char && c.is_alphabetic() {
            current_repeat += 1;
            max_repeat = max_repeat.max(current_repeat);
        } else {
            current_repeat = 1;
        }
        prev_char = c;
    }
    if max_repeat > 4 {
        score += 0.1;
    }

    score.clamp(0.0, 1.0)
}

/// Estimate phishing probability from text signals.
fn phishing_confidence(text: &str) -> f64 {
    let mut score: f64 = 0.0;
    let lower = text.to_lowercase();

    let urgency = ["urgent", "immediately", "verify your", "suspend", "locked"];
    let credentials = [
        "password",
        "account",
        "login",
        "credential",
        "ssn",
        "social security",
    ];

    let has_urgency = urgency.iter().any(|p| lower.contains(p));
    let has_credentials = credentials.iter().any(|p| lower.contains(p));

    if has_urgency && has_credentials {
        score += 0.5;
    } else if has_urgency {
        score += 0.1;
    }

    let suspicious_url_patterns = [
        "bit.ly",
        "tinyurl",
        "t.co",
        "goo.gl",
        "login.",
        "signin.",
        "verify.",
        "secure-",
        "account-verify",
        "update-info",
    ];
    for pattern in &suspicious_url_patterns {
        if lower.contains(pattern) {
            score += 0.15;
        }
    }

    score.clamp(0.0, 1.0)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn clean_text_no_labels() {
        let results = classify_text("Just a normal post about my day. Had coffee and read a book.");
        assert!(results.is_empty());
    }

    #[test]
    fn spammy_text_detected() {
        let results = classify_text(
            "BUY NOW!!! CLICK HERE for FREE MONEY!!! Limited time!!! https://spam.example.com https://spam2.example.com https://spam3.example.com https://spam4.example.com",
        );
        let spam = results.iter().find(|r| r.category == "spam");
        assert!(spam.is_some());
        assert!(spam.unwrap().confidence >= 0.3);
    }

    #[test]
    fn phishing_text_detected() {
        let results = classify_text(
            "URGENT: Your account has been locked. Verify your password immediately at https://secure-login.example.com",
        );
        let phishing = results.iter().find(|r| r.category == "phishing");
        assert!(phishing.is_some());
        assert!(phishing.unwrap().confidence >= 0.3);
    }

    #[test]
    fn confidence_clamped_to_one() {
        let results = classify_text(
            "BUY NOW!!! ACT NOW!!! FREE MONEY!!! CLICK HERE!!! LIMITED TIME!!! EARN MONEY!!! MAKE MONEY FAST!!! NO OBLIGATION!!! RISK FREE!!! WINNER!!! 100% FREE!!! GUARANTEED INCOME!!! https://a.com https://b.com https://c.com https://d.com",
        );
        for r in &results {
            assert!(r.confidence <= 1.0);
        }
    }

    #[test]
    fn short_text_no_false_positive() {
        let results = classify_text("Hi!");
        assert!(results.is_empty());
    }

    #[test]
    fn empty_text_scores_zero_on_both_axes() {
        assert_eq!(spam_confidence(""), 0.0);
        assert_eq!(phishing_confidence(""), 0.0);
        assert!(classify_text("").is_empty());
    }

    #[test]
    fn caps_ratio_boundary_is_strictly_greater_than_half() {
        // Gated on text.len() >= 20; the bonus only fires for ratio > 0.5,
        // not >=. Both strings alternate case so no run of an identical
        // character exceeds the separate repeat-bonus threshold.
        let exactly_half = "AaAaAaAaAaAaAaAaAaAa"; // 20 chars, 10 upper / 10 lower
        assert_eq!(exactly_half.len(), 20);
        assert_eq!(spam_confidence(exactly_half), 0.0);

        let just_over_half = "AaAaAaAaAaAaAaAaAaAA"; // 20 chars, 11 upper / 9 lower
        assert_eq!(just_over_half.len(), 20);
        assert_eq!(spam_confidence(just_over_half), 0.3);
    }

    #[test]
    fn caps_ratio_check_skipped_under_20_chars() {
        // 19 chars, ratio ~0.53 (would clear 0.5 if checked) but the len>=20
        // gate blocks the ratio computation entirely below that floor.
        let text = "AaAaAaAaAaAaAaAaAaA";
        assert_eq!(text.len(), 19);
        assert_eq!(spam_confidence(text), 0.0);
    }

    #[test]
    fn url_count_thresholds() {
        // Exactly 1 URL: neither the >3 nor the >1-with-short-text branch fires.
        assert_eq!(
            spam_confidence("Check this out http://example.com for more details please"),
            0.0
        );
        // Exactly 2 URLs under the 200-char length floor: +0.1.
        assert_eq!(
            spam_confidence("See http://a.example.com and https://b.example.com now please"),
            0.1
        );
        // More than 3 URLs: +0.2 regardless of length.
        assert_eq!(
            spam_confidence("http://a.com http://b.com http://c.com http://d.com"),
            0.2
        );
    }

    #[test]
    fn spam_phrase_hits_are_additive() {
        // "winner" + "risk free" = 2 distinct phrase hits * 0.15 each.
        assert_eq!(spam_confidence("You are a winner! This is risk free!"), 0.3);
    }

    #[test]
    fn exclaim_count_boundary() {
        // exclaim_count counts both '!' and '?'; the bonus needs > 5, not >= 5.
        assert_eq!(spam_confidence("This is a normal sentence?????"), 0.0);
        assert_eq!(spam_confidence("This is a normal sentence??????"), 0.1);
    }

    #[test]
    fn repeat_bonus_requires_more_than_four_consecutive_alphabetic() {
        assert_eq!(spam_confidence("aaaa is normal text here"), 0.0);
        assert_eq!(spam_confidence("aaaaa is normal text here"), 0.1);
    }

    #[test]
    fn repeat_bonus_ignores_non_alphabetic_runs() {
        // A long run of an identical non-alphabetic char (here 10 dots) never
        // triggers the repeat bonus, no matter how long the run is.
        assert_eq!(spam_confidence("Hello.......... world"), 0.0);
    }

    #[test]
    fn phishing_credentials_alone_contributes_nothing() {
        // has_credentials without has_urgency takes neither `if` branch.
        assert_eq!(
            phishing_confidence("Please update your password when convenient"),
            0.0
        );
    }

    #[test]
    fn phishing_suspicious_url_patterns_are_additive() {
        // "bit.ly" + "tinyurl" = 2 distinct pattern hits * 0.15 each, with no
        // urgency/credentials words present.
        assert_eq!(
            phishing_confidence("Please go to bit.ly/xyz or tinyurl.com/abc"),
            0.3
        );
    }

    #[test]
    fn classify_text_categories_are_independent() {
        let spam_only = classify_text("BUY NOW!!! CLICK HERE!!!");
        assert!(spam_only.iter().any(|r| r.category == "spam"));
        assert!(spam_only.iter().all(|r| r.category != "phishing"));

        let phishing_only = classify_text("URGENT: verify your login immediately");
        assert!(phishing_only.iter().any(|r| r.category == "phishing"));
        assert!(phishing_only.iter().all(|r| r.category != "spam"));
    }

    #[test]
    fn non_ascii_text_does_not_panic_and_the_length_gate_uses_bytes_not_chars() {
        // 9 CJK chars, 3 bytes each: byte len (27) clears the >=20 gate that
        // spam_confidence checks, even though there are far fewer than 20
        // actual chars for the ratio denominator. CJK carries no case, so
        // caps_ratio is 0 regardless -- this confirms no panic and no
        // false-positive from the byte/char length mismatch.
        let text = "你好世界你好世界你";
        assert!(text.len() >= 20, "byte length: {}", text.len());
        assert!(
            text.chars().count() < 20,
            "char count: {}",
            text.chars().count()
        );
        assert_eq!(spam_confidence(text), 0.0);
    }
}
