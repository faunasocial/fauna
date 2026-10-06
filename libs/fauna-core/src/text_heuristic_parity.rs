/// Expected-score regression vectors for the canonical text heuristic.
///
/// These pin the exact spam/phishing scores for a set of representative inputs.
/// Every app consumes this one implementation — web over WASM
/// (`fauna_wasm::classifyText`), the native apps over the `fauna-ffi`
/// `classifyText` façade — so there is no second copy to drift against; the
/// vectors guard against accidental retuning of the shared algorithm. (They were
/// originally a cross-language parity pair with a hand-rolled Kotlin copy in
/// `TextHeuristic.kt`; that copy was removed once android adopted the FFI façade.)
#[cfg(test)]
mod parity_tests {
    use crate::text_heuristic::classify_text;

    fn spam_score(text: &str) -> f64 {
        classify_text(text)
            .iter()
            .find(|r| r.category == "spam")
            .map(|r| r.confidence)
            .unwrap_or(0.0)
    }

    fn phishing_score(text: &str) -> f64 {
        classify_text(text)
            .iter()
            .find(|r| r.category == "phishing")
            .map(|r| r.confidence)
            .unwrap_or(0.0)
    }

    // ── Test vectors ──
    // IMPORTANT: These exact strings and scores are replicated in the Kotlin
    // test at apps/fauna-android/app/src/test/java/com/fauna/app/TextHeuristicParityTest.kt

    #[test]
    fn parity_v1_clean() {
        assert_eq!(spam_score("Just a normal message about my day."), 0.0);
        assert_eq!(phishing_score("Just a normal message about my day."), 0.0);
    }

    #[test]
    fn parity_v2_caps() {
        // 20+ chars, >50% caps → +0.3
        let s = spam_score("THIS IS ALL CAPS TEXT FOR TESTING PURPOSES");
        assert!((s - 0.3).abs() < 0.001, "expected 0.3, got {s}");
    }

    #[test]
    fn parity_v3_urls() {
        // 4 URLs → +0.2
        let s = spam_score("Check https://a.com https://b.com https://c.com https://d.com links");
        assert!((s - 0.2).abs() < 0.001, "expected 0.2, got {s}");
    }

    #[test]
    fn parity_v4_phrases() {
        // "buy now" + "click here" = 2 * 0.15 = 0.3
        let s = spam_score("buy now and click here for great deals today");
        assert!((s - 0.3).abs() < 0.001, "expected 0.3, got {s}");
    }

    #[test]
    fn parity_v5_phishing() {
        // "urgent" + "password" → urgency+credentials = 0.5
        let s = phishing_score("urgent: verify your password immediately");
        assert!((s - 0.5).abs() < 0.001, "expected 0.5, got {s}");
    }

    #[test]
    fn parity_v6_phishing_url() {
        // "bit.ly" → 0.15
        let s = phishing_score("visit bit.ly/something for details");
        assert!((s - 0.15).abs() < 0.001, "expected 0.15, got {s}");
    }

    #[test]
    fn parity_v7_combined() {
        let text = "BUY NOW!!! CLICK HERE!!! CHECK THESE LINKS!!! https://a.com https://b.com https://c.com https://d.com";
        let s = spam_score(text);
        assert!((s - 0.6).abs() < 0.001, "expected 0.6, got {s}");
    }

    #[test]
    fn parity_v8_clamped() {
        let text = "BUY NOW!!! ACT NOW!!! FREE MONEY!!! CLICK HERE!!! LIMITED TIME!!! EARN MONEY!!! MAKE MONEY FAST!!! NO OBLIGATION!!! RISK FREE!!! WINNER!!! 100% FREE!!! GUARANTEED INCOME!!! https://a.com https://b.com https://c.com https://d.com";
        assert_eq!(spam_score(text), 1.0);
    }

    #[test]
    fn parity_v9_short() {
        assert_eq!(spam_score("Hi!"), 0.0);
        assert_eq!(phishing_score("Hi!"), 0.0);
    }

    #[test]
    fn parity_v10_exclamation() {
        // 7 exclamation marks → +0.1
        let s = spam_score("wow! great! amazing! super! cool! nice! fantastic!");
        assert!((s - 0.1).abs() < 0.001, "expected 0.1, got {s}");
    }
}
