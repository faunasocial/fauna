//! Structural intent detector for phishing and solicitation (Layer 3).
//!
//! Analyses the *rhetorical structure* of a message rather than individual
//! tokens. A phishing message must follow a recognisable template
//! (greeting → flattery → offer → call-to-action) and this structural
//! invariant persists even when every word is paraphrased.

// ---------------------------------------------------------------------------
// Phrase lists
// ---------------------------------------------------------------------------

const GREETING: &[&str] = &["hey ", "hi ", "hello ", "dear ", "greetings"];

const PERSONAL_REF: &[&str] = &[
    "your post",
    "your recent",
    "your profile",
    "saw you",
    "noticed you",
    "your work",
    "your content",
];

const FLATTERY: &[&str] = &[
    "love your",
    "great post",
    "impressed by",
    "amazing work",
    "fantastic",
    "incredible",
    "awesome",
    "really enjoyed",
    "big fan of",
    "admire your",
];

const TRANSITION: &[&str] = &[
    "speaking of",
    "by the way",
    "actually",
    "on that note",
    "which reminds me",
    "incidentally",
    "that said",
    "in fact",
    "funny you mention",
];

const OFFER: &[&str] = &[
    "exclusive",
    "opportunity",
    "discount",
    "deal",
    "offer",
    "partnership",
    "collaborate",
    "invest",
    "earn",
    "profit",
    "free trial",
    "special price",
    "limited offer",
];

const URGENCY: &[&str] = &[
    "act now",
    "limited time",
    "expires",
    "hurry",
    "don't miss",
    "last chance",
    "urgent",
    "immediately",
    "right away",
    "only today",
    "ending soon",
];

const CTA: &[&str] = &[
    "check out",
    "click",
    "visit",
    "sign up",
    "register",
    "join",
    "subscribe",
    "download",
    "get started",
    "claim your",
    "dm me",
];

const SHORTENERS: &[&str] = &[
    "bit.ly",
    "t.co",
    "tinyurl.com",
    "goo.gl",
    "ow.ly",
    "is.gd",
    "buff.ly",
    "adf.ly",
    "tiny.cc",
    "rb.gy",
    "cutt.ly",
    "shorturl.at",
];

const SUSPICIOUS_TLDS: &[&str] = &[
    "xyz", "tk", "click", "buzz", "top", "gq", "ml", "cf", "ga", "work", "icu", "monster", "cam",
];

// ---------------------------------------------------------------------------
// Public meta-signal type
// ---------------------------------------------------------------------------

/// Contextual signals about the sender / conversation supplied by the caller.
///
/// All optional fields default to `None` / absent when not available.
#[derive(Debug, Clone, Default)]
pub struct MetaSignals {
    /// True if this is the first direct message between sender and recipient.
    pub is_first_contact: bool,
    /// Social-graph hop distance; `None` means no path found.
    pub sender_social_distance: Option<u8>,
    /// Sender account age in seconds.
    pub sender_account_age: Option<u64>,
    /// Pre-computed behavioural anomaly score [0.0, 1.0]; `None` = unavailable.
    pub behavioral_anomaly: Option<f64>,
}

// ---------------------------------------------------------------------------
// Internal types
// ---------------------------------------------------------------------------

/// Structural signals extracted from raw message text.
#[derive(Debug, Default)]
struct IntentSignals {
    greeting: bool,
    personal_reference: bool,
    flattery: bool,
    transition: bool,
    offer: bool,
    urgency: bool,
    call_to_action: bool,
    link_count: usize,
    url_shortener: bool,
    suspicious_tld: bool,
    follows_template: bool,
}

/// Rhetorical segment classification for a single sentence.
#[derive(Debug, Clone, PartialEq)]
enum SegmentType {
    Greeting,
    PersonalRef,
    Flattery,
    Transition,
    Offer,
    Urgency,
    CallToAction,
    Link,
    Neutral,
}

// ---------------------------------------------------------------------------
// Internal helpers
// ---------------------------------------------------------------------------

/// Split `text` into sentences on `.`, `!`, or `?` followed by whitespace or
/// end of string. Returns borrowed slices into the original string.
fn split_sentences(text: &str) -> Vec<&str> {
    let mut sentences: Vec<&str> = Vec::new();
    let bytes = text.as_bytes();
    let len = bytes.len();
    let mut start = 0;

    let mut i = 0;
    while i < len {
        let b = bytes[i];
        if b == b'.' || b == b'!' || b == b'?' {
            // Include the punctuation in the sentence.
            let end = i + 1;
            let slice = text[start..end].trim();
            if !slice.is_empty() {
                sentences.push(slice);
            }
            // Skip whitespace after the punctuation.
            i += 1;
            while i < len && bytes[i] == b' ' {
                i += 1;
            }
            start = i;
        } else {
            i += 1;
        }
    }

    // Trailing fragment without terminal punctuation.
    let tail = text[start..].trim();
    if !tail.is_empty() {
        sentences.push(tail);
    }

    sentences
}

/// Return all URL tokens (words starting with `http://` or `https://`) in `text`.
fn extract_links(text: &str) -> Vec<&str> {
    text.split_whitespace()
        .filter(|w| w.starts_with("http://") || w.starts_with("https://"))
        .collect()
}

/// Strip scheme and path, returning just the host portion of a URL.
fn extract_domain(url: &str) -> &str {
    // Strip scheme.
    let after_scheme = if let Some(pos) = url.find("://") {
        &url[pos + 3..]
    } else {
        url
    };
    // Take up to the first `/` or end.
    match after_scheme.find('/') {
        Some(pos) => &after_scheme[..pos],
        None => after_scheme,
    }
}

/// Classify a single sentence (lowercased) into its dominant rhetorical role.
fn classify_segment(sentence: &str) -> SegmentType {
    let lower = sentence.to_lowercase();

    // Link check first.
    if lower.contains("http://") || lower.contains("https://") {
        return SegmentType::Link;
    }

    // Check phrase lists in priority order (most specific first).
    for phrase in GREETING {
        if lower.starts_with(phrase) || lower.contains(phrase) {
            return SegmentType::Greeting;
        }
    }
    for phrase in PERSONAL_REF {
        if lower.contains(phrase) {
            return SegmentType::PersonalRef;
        }
    }
    for phrase in FLATTERY {
        if lower.contains(phrase) {
            return SegmentType::Flattery;
        }
    }
    for phrase in TRANSITION {
        if lower.contains(phrase) {
            return SegmentType::Transition;
        }
    }
    for phrase in URGENCY {
        if lower.contains(phrase) {
            return SegmentType::Urgency;
        }
    }
    for phrase in OFFER {
        if lower.contains(phrase) {
            return SegmentType::Offer;
        }
    }
    for phrase in CTA {
        if lower.contains(phrase) {
            return SegmentType::CallToAction;
        }
    }

    SegmentType::Neutral
}

/// Determine whether the ordered segment list matches a known phishing template.
///
/// Two templates are recognised:
/// 1. **Rapport → Offer → Action**: at least one rapport segment
///    (Greeting | PersonalRef | Flattery) appears before an Offer, which
///    appears before an action segment (CallToAction | Link).
/// 2. **Offer + Urgency + Action**: an Offer followed (anywhere after) by both
///    Urgency and an action segment.
fn check_phishing_template(segments: &[SegmentType]) -> bool {
    // Template 1: rapport → Offer → action
    let is_rapport = |s: &SegmentType| {
        matches!(
            s,
            SegmentType::Greeting | SegmentType::PersonalRef | SegmentType::Flattery
        )
    };
    let is_action = |s: &SegmentType| matches!(s, SegmentType::CallToAction | SegmentType::Link);

    // Find first rapport index.
    if let Some(rapport_idx) = segments.iter().position(is_rapport) {
        // Find first Offer after rapport.
        if let Some(offer_rel) = segments[rapport_idx + 1..]
            .iter()
            .position(|s| *s == SegmentType::Offer)
        {
            let offer_idx = rapport_idx + 1 + offer_rel;
            // Find action after offer.
            if segments[offer_idx + 1..].iter().any(is_action) {
                return true;
            }
        }
    }

    // Template 2: Offer + Urgency + Action (in order, not necessarily consecutive)
    if let Some(offer_idx) = segments.iter().position(|s| *s == SegmentType::Offer) {
        let after_offer = &segments[offer_idx + 1..];
        let has_urgency = after_offer.contains(&SegmentType::Urgency);
        let has_action = after_offer.iter().any(is_action);
        if has_urgency && has_action {
            return true;
        }
    }

    false
}

/// Extract all structural signals from the raw message text.
fn extract_intent_signals(text: &str) -> IntentSignals {
    let lower = text.to_lowercase();
    let mut signals = IntentSignals::default();

    // Per-word link analysis.
    let links = extract_links(text);
    signals.link_count = links.len();
    for link in &links {
        let domain = extract_domain(link);
        if is_url_shortener(domain) {
            signals.url_shortener = true;
        }
        // Extract TLD: last segment of domain after the last dot.
        if let Some(tld) = domain.rsplit('.').next()
            && is_suspicious_tld(tld)
        {
            signals.suspicious_tld = true;
        }
    }

    // Phrase-list signals over the full lowercased text.
    signals.greeting = GREETING.iter().any(|p| lower.contains(p));
    signals.personal_reference = PERSONAL_REF.iter().any(|p| lower.contains(p));
    signals.flattery = FLATTERY.iter().any(|p| lower.contains(p));
    signals.transition = TRANSITION.iter().any(|p| lower.contains(p));
    signals.offer = OFFER.iter().any(|p| lower.contains(p));
    signals.urgency = URGENCY.iter().any(|p| lower.contains(p));
    signals.call_to_action = CTA.iter().any(|p| lower.contains(p));

    // Structural template check.
    let sentences = split_sentences(text);
    let segment_types: Vec<SegmentType> = sentences.iter().map(|s| classify_segment(s)).collect();
    signals.follows_template = check_phishing_template(&segment_types);

    signals
}

// ---------------------------------------------------------------------------
// Scoring functions
// ---------------------------------------------------------------------------

/// Compute phishing confidence from extracted signals and meta context.
///
/// Score contributions:
/// - Template match → +0.4
/// - Flattery + Offer together → +0.15
/// - URL shortener detected → +0.15
/// - Suspicious TLD detected → +0.1
/// - First contact → +0.1
/// - No social distance (sender unknown to graph) → +0.1
///
/// Result is clamped to [0.0, 1.0].
fn compute_phishing_score(signals: &IntentSignals, meta: &MetaSignals) -> f64 {
    let mut score = 0.0_f64;

    if signals.follows_template {
        score += 0.4;
    }
    if signals.flattery && signals.offer {
        score += 0.15;
    }
    if signals.url_shortener {
        score += 0.15;
    }
    if signals.suspicious_tld {
        score += 0.1;
    }
    if meta.is_first_contact {
        score += 0.1;
    }
    if meta.sender_social_distance.is_none() {
        score += 0.1;
    }

    score.clamp(0.0, 1.0)
}

/// Compute solicitation confidence from extracted signals and meta context.
///
/// Score contributions:
/// - Offer + CTA present → +0.25
/// - Urgency present → +0.2
/// - Link present with shortener or suspicious TLD → +0.15
/// - Offer + Urgency + CTA all present → +0.15
///
/// Result is clamped to [0.0, 1.0].
fn compute_solicitation_score(signals: &IntentSignals, _meta: &MetaSignals) -> f64 {
    let mut score = 0.0_f64;

    if signals.offer && signals.call_to_action {
        score += 0.25;
    }
    if signals.urgency {
        score += 0.2;
    }
    if signals.link_count > 0 && (signals.url_shortener || signals.suspicious_tld) {
        score += 0.15;
    }
    if signals.offer && signals.urgency && signals.call_to_action {
        score += 0.15;
    }

    score.clamp(0.0, 1.0)
}

// ---------------------------------------------------------------------------
// Public API
// ---------------------------------------------------------------------------

/// Returns `true` if `domain` is a known URL-shortening service.
pub fn is_url_shortener(domain: &str) -> bool {
    let lower = domain.to_lowercase();
    SHORTENERS.contains(&lower.as_str())
}

/// Returns `true` if `tld` (e.g. `"xyz"`) is in the suspicious TLD list.
pub fn is_suspicious_tld(tld: &str) -> bool {
    let lower = tld.to_lowercase();
    SUSPICIOUS_TLDS.contains(&lower.as_str())
}

/// Analyse `text` for phishing and solicitation intent using structural signals
/// and the caller-supplied `meta` context.
///
/// Returns a list of `(category, confidence)` pairs for each detected intent
/// whose confidence exceeds a minimum threshold. Categories currently emitted:
/// - `"phishing/intent"`
/// - `"spam/solicitation"`
pub fn detect_intent(text: &str, meta: &MetaSignals) -> Vec<(String, f64)> {
    const MIN_CONFIDENCE: f64 = 0.15;

    let signals = extract_intent_signals(text);

    let phishing = compute_phishing_score(&signals, meta);
    let solicitation = compute_solicitation_score(&signals, meta);

    let mut results = Vec::new();
    if phishing >= MIN_CONFIDENCE {
        results.push(("phishing/intent".to_string(), phishing));
    }
    if solicitation >= MIN_CONFIDENCE {
        results.push(("spam/solicitation".to_string(), solicitation));
    }

    results
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    // -----------------------------------------------------------------------
    // Test 1: clean message → no labels
    // -----------------------------------------------------------------------
    #[test]
    fn clean_message_no_labels() {
        let text = "Hey, want to grab coffee tomorrow?";
        let meta = MetaSignals::default();
        let labels = detect_intent(text, &meta);
        assert!(
            labels.is_empty(),
            "clean social message should produce no labels, got: {labels:?}"
        );
    }

    // -----------------------------------------------------------------------
    // Test 2: classic phishing template → phishing/intent >= 0.5
    // -----------------------------------------------------------------------
    #[test]
    fn classic_phishing_template() {
        let text = "Hey Alice! I love your recent post about hiking in Colorado. \
                    Speaking of which, my cousin runs an outdoor gear company and \
                    they're offering an exclusive 50% discount. \
                    Check it out here: https://bit.ly/deals123";
        let meta = MetaSignals {
            is_first_contact: true,
            sender_social_distance: None,
            ..MetaSignals::default()
        };
        let labels = detect_intent(text, &meta);
        let phishing = labels
            .iter()
            .find(|(cat, _)| cat == "phishing/intent")
            .map(|(_, conf)| *conf);
        assert!(
            phishing.is_some(),
            "classic phishing template should produce a phishing/intent label"
        );
        let conf = phishing.unwrap();
        assert!(
            conf >= 0.5,
            "classic phishing template confidence should be >= 0.5, got {conf}"
        );
    }

    // -----------------------------------------------------------------------
    // Test 3: urgency + link with suspicious TLD → non-empty labels
    // -----------------------------------------------------------------------
    #[test]
    fn urgency_with_link_is_suspicious() {
        let text = "Act now! This limited-time offer expires today. \
                    Sign up at https://totally-legit.xyz/offer";
        let meta = MetaSignals::default();
        let labels = detect_intent(text, &meta);
        assert!(
            !labels.is_empty(),
            "urgency + suspicious link should produce at least one label"
        );
    }

    // -----------------------------------------------------------------------
    // Test 4: legitimate link sharing → empty or all confidence < 0.3
    // -----------------------------------------------------------------------
    #[test]
    fn normal_link_sharing_not_flagged() {
        let text = "I found this interesting article about Rust async patterns. \
                    https://blog.rust-lang.org/2024/async.html";
        let meta = MetaSignals::default();
        let labels = detect_intent(text, &meta);
        let all_low = labels.iter().all(|(_, conf)| *conf < 0.3);
        assert!(
            labels.is_empty() || all_low,
            "normal link sharing should not be flagged above 0.3, got: {labels:?}"
        );
    }

    // -----------------------------------------------------------------------
    // Test 5: URL shortener detection
    // -----------------------------------------------------------------------
    #[test]
    fn url_shortener_detected() {
        assert!(is_url_shortener("bit.ly"), "bit.ly should be a shortener");
        assert!(is_url_shortener("t.co"), "t.co should be a shortener");
        assert!(
            is_url_shortener("tinyurl.com"),
            "tinyurl.com should be a shortener"
        );
        assert!(
            !is_url_shortener("github.com"),
            "github.com should NOT be a shortener"
        );
        assert!(
            !is_url_shortener("rust-lang.org"),
            "rust-lang.org should NOT be a shortener"
        );
    }

    // -----------------------------------------------------------------------
    // Test 6: suspicious TLD detection
    // -----------------------------------------------------------------------
    #[test]
    fn suspicious_tld_detected() {
        assert!(is_suspicious_tld("xyz"), "xyz should be suspicious");
        assert!(is_suspicious_tld("click"), "click should be suspicious");
        assert!(is_suspicious_tld("buzz"), "buzz should be suspicious");
        assert!(!is_suspicious_tld("com"), "com should NOT be suspicious");
        assert!(!is_suspicious_tld("org"), "org should NOT be suspicious");
        assert!(
            !is_suspicious_tld("social"),
            "social should NOT be suspicious"
        );
    }

    // -----------------------------------------------------------------------
    // split_sentences edge cases
    // -----------------------------------------------------------------------
    #[test]
    fn split_sentences_fragment_without_terminal_punctuation() {
        let text = "just a fragment with no ending punctuation";
        assert_eq!(split_sentences(text), vec![text]);
    }

    #[test]
    fn split_sentences_consecutive_punctuation_each_becomes_own_fragment() {
        // Documents current (somewhat surprising) behaviour: runs of
        // terminal punctuation are not collapsed, each yields its own
        // one-character "sentence".
        let sentences = split_sentences("Wait... really?! Yes.");
        assert_eq!(sentences, vec!["Wait.", ".", ".", "really?", "!", "Yes."]);
    }

    #[test]
    fn split_sentences_empty_and_whitespace_only_text() {
        assert_eq!(split_sentences(""), Vec::<&str>::new());
        assert_eq!(split_sentences("   "), Vec::<&str>::new());
        assert_eq!(split_sentences("  \n\t  "), Vec::<&str>::new());
    }

    #[test]
    fn split_sentences_trailing_fragment_after_terminated_sentence() {
        let sentences = split_sentences("Hello there. and a trailing bit");
        assert_eq!(sentences, vec!["Hello there.", "and a trailing bit"]);
    }

    // -----------------------------------------------------------------------
    // extract_domain edge cases
    // -----------------------------------------------------------------------
    #[test]
    fn extract_domain_strips_scheme_and_path() {
        assert_eq!(
            extract_domain("https://example.com/path/to/page"),
            "example.com"
        );
    }

    #[test]
    fn extract_domain_no_scheme_no_path() {
        assert_eq!(extract_domain("example.com"), "example.com");
    }

    #[test]
    fn extract_domain_scheme_without_path() {
        assert_eq!(extract_domain("https://bit.ly"), "bit.ly");
    }

    // -----------------------------------------------------------------------
    // classify_segment priority-order ties
    // -----------------------------------------------------------------------
    #[test]
    fn classify_segment_link_outranks_greeting() {
        let seg = classify_segment("hey check this out https://bit.ly/x");
        assert_eq!(seg, SegmentType::Link);
    }

    #[test]
    fn classify_segment_greeting_outranks_offer() {
        let seg = classify_segment("hey there, exclusive deal just for you");
        assert_eq!(seg, SegmentType::Greeting);
    }

    #[test]
    fn classify_segment_personal_ref_outranks_flattery() {
        let seg = classify_segment("i saw your profile and it's amazing");
        assert_eq!(seg, SegmentType::PersonalRef);
    }

    #[test]
    fn classify_segment_neutral_when_nothing_matches() {
        let seg = classify_segment("the weather today is quite pleasant");
        assert_eq!(seg, SegmentType::Neutral);
    }

    // -----------------------------------------------------------------------
    // check_phishing_template — both templates, order-sensitivity
    // -----------------------------------------------------------------------
    #[test]
    fn check_phishing_template_rapport_offer_action_matches_template_one() {
        let segments = vec![
            SegmentType::Greeting,
            SegmentType::Offer,
            SegmentType::CallToAction,
        ];
        assert!(check_phishing_template(&segments));
    }

    #[test]
    fn check_phishing_template_offer_preceding_rapport_does_not_match() {
        // Offer appears BEFORE the rapport segment, and no Urgency is
        // present, so neither template fires.
        let segments = vec![
            SegmentType::Offer,
            SegmentType::Greeting,
            SegmentType::CallToAction,
        ];
        assert!(!check_phishing_template(&segments));
    }

    #[test]
    fn check_phishing_template_offer_urgency_action_matches_template_two() {
        let segments = vec![
            SegmentType::Neutral,
            SegmentType::Offer,
            SegmentType::Urgency,
            SegmentType::CallToAction,
        ];
        assert!(check_phishing_template(&segments));
    }

    #[test]
    fn check_phishing_template_offer_alone_matches_neither_template() {
        let segments = vec![SegmentType::Offer];
        assert!(!check_phishing_template(&segments));
    }

    #[test]
    fn check_phishing_template_action_before_offer_is_not_counted() {
        // CallToAction precedes Offer; template 2 only looks *after* the
        // offer index, so this action doesn't count and Urgency alone
        // can't complete it either.
        let segments = vec![
            SegmentType::CallToAction,
            SegmentType::Offer,
            SegmentType::Urgency,
        ];
        assert!(!check_phishing_template(&segments));
    }

    // -----------------------------------------------------------------------
    // compute_phishing_score — individual contributions + clamping
    // -----------------------------------------------------------------------
    #[test]
    fn compute_phishing_score_template_contribution() {
        let signals = IntentSignals {
            follows_template: true,
            ..Default::default()
        };
        let meta = MetaSignals {
            sender_social_distance: Some(1),
            ..Default::default()
        };
        assert_eq!(compute_phishing_score(&signals, &meta), 0.4);
    }

    #[test]
    fn compute_phishing_score_flattery_and_offer_contribution() {
        let signals = IntentSignals {
            flattery: true,
            offer: true,
            ..Default::default()
        };
        let meta = MetaSignals {
            sender_social_distance: Some(1),
            ..Default::default()
        };
        assert_eq!(compute_phishing_score(&signals, &meta), 0.15);
    }

    #[test]
    fn compute_phishing_score_shortener_contribution() {
        let signals = IntentSignals {
            url_shortener: true,
            ..Default::default()
        };
        let meta = MetaSignals {
            sender_social_distance: Some(1),
            ..Default::default()
        };
        assert_eq!(compute_phishing_score(&signals, &meta), 0.15);
    }

    #[test]
    fn compute_phishing_score_suspicious_tld_contribution() {
        let signals = IntentSignals {
            suspicious_tld: true,
            ..Default::default()
        };
        let meta = MetaSignals {
            sender_social_distance: Some(1),
            ..Default::default()
        };
        assert_eq!(compute_phishing_score(&signals, &meta), 0.1);
    }

    #[test]
    fn compute_phishing_score_first_contact_contribution() {
        let signals = IntentSignals::default();
        let meta = MetaSignals {
            is_first_contact: true,
            sender_social_distance: Some(1),
            ..Default::default()
        };
        assert_eq!(compute_phishing_score(&signals, &meta), 0.1);
    }

    #[test]
    fn compute_phishing_score_unknown_social_distance_contribution() {
        let signals = IntentSignals::default();
        let meta = MetaSignals {
            sender_social_distance: None,
            ..Default::default()
        };
        assert_eq!(compute_phishing_score(&signals, &meta), 0.1);
    }

    #[test]
    fn compute_phishing_score_clamps_at_one_when_every_signal_fires() {
        let signals = IntentSignals {
            follows_template: true,
            flattery: true,
            offer: true,
            url_shortener: true,
            suspicious_tld: true,
            ..Default::default()
        };
        let meta = MetaSignals {
            is_first_contact: true,
            sender_social_distance: None,
            ..Default::default()
        };
        // 0.4 + 0.15 + 0.15 + 0.1 + 0.1 + 0.1 == 1.0: the additive design
        // tops out exactly at the ceiling, so this also pins that the sum
        // of every contribution can never exceed the clamp.
        assert_eq!(compute_phishing_score(&signals, &meta), 1.0);
    }

    // -----------------------------------------------------------------------
    // compute_solicitation_score — individual contributions
    // -----------------------------------------------------------------------
    #[test]
    fn compute_solicitation_score_offer_and_cta_contribution() {
        let signals = IntentSignals {
            offer: true,
            call_to_action: true,
            ..Default::default()
        };
        let meta = MetaSignals::default();
        assert_eq!(compute_solicitation_score(&signals, &meta), 0.25);
    }

    #[test]
    fn compute_solicitation_score_urgency_contribution() {
        let signals = IntentSignals {
            urgency: true,
            ..Default::default()
        };
        let meta = MetaSignals::default();
        assert_eq!(compute_solicitation_score(&signals, &meta), 0.2);
    }

    #[test]
    fn compute_solicitation_score_link_with_shortener_contribution() {
        let signals = IntentSignals {
            link_count: 1,
            url_shortener: true,
            ..Default::default()
        };
        let meta = MetaSignals::default();
        assert_eq!(compute_solicitation_score(&signals, &meta), 0.15);
    }

    #[test]
    fn compute_solicitation_score_link_with_suspicious_tld_contribution() {
        let signals = IntentSignals {
            link_count: 1,
            suspicious_tld: true,
            ..Default::default()
        };
        let meta = MetaSignals::default();
        assert_eq!(compute_solicitation_score(&signals, &meta), 0.15);
    }

    #[test]
    fn compute_solicitation_score_all_three_offer_urgency_cta_adds_bonus() {
        let signals = IntentSignals {
            offer: true,
            urgency: true,
            call_to_action: true,
            ..Default::default()
        };
        let meta = MetaSignals::default();
        // 0.25 (offer+cta) + 0.2 (urgency) + 0.15 (all-three bonus) == 0.6
        assert_eq!(compute_solicitation_score(&signals, &meta), 0.6);
    }

    // -----------------------------------------------------------------------
    // detect_intent — MIN_CONFIDENCE (0.15) boundary
    // -----------------------------------------------------------------------
    #[test]
    fn detect_intent_includes_label_at_exactly_min_confidence() {
        // A shortener-only link with no other signal scores exactly 0.15
        // for both categories; MIN_CONFIDENCE uses `>=`, so both must
        // still appear.
        let text = "Check https://bit.ly/x";
        let meta = MetaSignals {
            is_first_contact: false,
            sender_social_distance: Some(5),
            ..Default::default()
        };
        let labels = detect_intent(text, &meta);
        let phishing = labels.iter().find(|(cat, _)| cat == "phishing/intent");
        let solicitation = labels.iter().find(|(cat, _)| cat == "spam/solicitation");
        assert!(
            phishing.is_some(),
            "expected phishing/intent at threshold, got {labels:?}"
        );
        assert!(
            solicitation.is_some(),
            "expected spam/solicitation at threshold, got {labels:?}"
        );
        assert!((phishing.unwrap().1 - 0.15).abs() < 1e-12);
        assert!((solicitation.unwrap().1 - 0.15).abs() < 1e-12);
    }

    #[test]
    fn detect_intent_excludes_label_below_min_confidence() {
        let text = "See https://example.com for more info.";
        let meta = MetaSignals {
            is_first_contact: false,
            sender_social_distance: Some(5),
            ..Default::default()
        };
        let labels = detect_intent(text, &meta);
        assert!(
            labels.is_empty(),
            "a plain, non-shortened, non-suspicious link should score 0, got: {labels:?}"
        );
    }

    // -----------------------------------------------------------------------
    // Case-folding on the public lookup helpers
    // -----------------------------------------------------------------------
    #[test]
    fn is_url_shortener_case_insensitive() {
        assert!(is_url_shortener("BIT.LY"));
        assert!(is_url_shortener("Bit.Ly"));
    }

    #[test]
    fn is_suspicious_tld_case_insensitive() {
        assert!(is_suspicious_tld("XYZ"));
        assert!(is_suspicious_tld("Click"));
    }
}
