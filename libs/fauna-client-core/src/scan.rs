//! Content classification and scan reports.

/// Result of running the text heuristic classifier on a piece of content.
pub struct ClassificationResult {
    pub category: String,
    /// Confidence score in [0.0, 1.0].  Uses f64 to match
    /// `fauna_core::text_heuristic::HeuristicResult`.
    pub confidence: f64,
}

/// Run the built-in text heuristic classifier on `text`.
///
/// Returns only labels where confidence > 0.0.  Categories include
/// `"spam"` and `"phishing"`.
pub fn classify_text(text: &str) -> Vec<ClassificationResult> {
    fauna_core::text_heuristic::classify_text(text)
        .into_iter()
        .map(|r| ClassificationResult {
            category: r.category,
            confidence: r.confidence,
        })
        .collect()
}

// `build_scan_report` (the client-signed scan-report builder) was removed with
// the `fauna.moderation.scan_report` WS-RPC migration, and the kind itself left
// the wire 2026-09-24 (compat-remnant sweep). Only the `classify_text`
// heuristic remains client-side.

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn classify_text_spam_like_text() {
        // Text loaded with known spam signals should yield at least one label.
        let text = "CLICK HERE NOW!! FREE MONEY!!! BUY CRYPTO WINNER PRIZE OFFER!!!";
        let results = classify_text(text);
        // We expect the heuristic to fire on this content.
        assert!(
            !results.is_empty(),
            "expected at least one classification result"
        );
        let categories: Vec<&str> = results.iter().map(|r| r.category.as_str()).collect();
        assert!(
            categories.contains(&"spam"),
            "expected 'spam' category; got: {categories:?}"
        );
        for r in &results {
            assert!(r.confidence > 0.0, "confidence must be positive");
            assert!(r.confidence <= 1.0, "confidence must be <= 1.0");
        }
    }

    #[test]
    fn classify_text_clean_text_returns_empty() {
        let text = "Hello, how are you doing today?";
        let results = classify_text(text);
        // Clean text should produce no labels (or at least no spam label).
        let has_spam = results.iter().any(|r| r.category == "spam");
        assert!(!has_spam, "clean text should not be classified as spam");
    }
}
