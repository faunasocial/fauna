//! Canonical report-hash — the cross-user, cross-nest content-equality key
//! for distributed report sharing.
//!
//! `docs/goal/behavior/report-sharing.md` § Content identity owns the
//! definition:
//!
//! ```text
//! report_hash = blake3("fauna:report-hash:v1\0" ‖ canon(Subject) ‖ "\n" ‖ canon(BodyText))
//! canon(s)    = trim(collapse_ws(lowercase(nfc(s))))
//! ```
//!
//! Computed once per message at the MTA perimeter, pre-seal (the sanctioned
//! plaintext position, `content-scoring.md` § The two plaintext positions),
//! identical for every recipient of the same message on every nest. The
//! domain-separation prefix keeps it disjoint from every other 32-byte id
//! (content CIDs, `content_id_for_document`, chunk hashes).
//!
//! One definition, shared Rust: the Go MTA calls this over the UniFFI
//! binding (like [`crate::tokenizer::tokenize`]); there is no Go
//! reimplementation.

use unicode_normalization::UnicodeNormalization;

/// v1 domain-separation prefix. A canonicalization change is a new version —
/// a new prefix — never a silent redefinition (old aggregates would silently
/// stop matching new hashes without the version fence).
const DOMAIN_PREFIX_V1: &[u8] = b"fauna:report-hash:v1\0";

/// NFC-normalize, lowercase, collapse every Unicode-whitespace run to one
/// ASCII space, trim. Deliberately simpler than the index tokenizer (which
/// segments/folds/sorts): equality wants a stable *sequence*, not a token
/// set — two different sentences over the same words must not collide.
fn canon(s: &str) -> String {
    let lowered = s.nfc().collect::<String>().to_lowercase();
    let mut out = String::with_capacity(lowered.len());
    let mut pending_space = false;
    for ch in lowered.chars() {
        if ch.is_whitespace() {
            pending_space = !out.is_empty();
        } else {
            if pending_space {
                out.push(' ');
                pending_space = false;
            }
            out.push(ch);
        }
    }
    out
}

/// The canonical 32-byte report-hash over a message's subject + body text —
/// the same parsed fields the index tokenizer consumes
/// (`ParsedMessage.Subject`/`.BodyText`).
#[cfg_attr(feature = "uniffi", uniffi::export)]
pub fn report_hash(subject: &str, body_text: &str) -> Vec<u8> {
    let mut hasher = blake3::Hasher::new();
    hasher.update(DOMAIN_PREFIX_V1);
    hasher.update(canon(subject).as_bytes());
    hasher.update(b"\n");
    hasher.update(canon(body_text).as_bytes());
    hasher.finalize().as_bytes().to_vec()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn is_32_bytes() {
        assert_eq!(report_hash("subject", "body").len(), 32);
    }

    #[test]
    fn case_whitespace_and_nfc_invariant() {
        let base = report_hash("Win A PRIZE", "click here now");
        // Case.
        assert_eq!(report_hash("win a prize", "CLICK HERE NOW"), base);
        // Whitespace runs (tabs, newlines, multiple spaces) + leading/trailing.
        assert_eq!(
            report_hash("  Win\tA  PRIZE ", "click\nhere\r\n now\n"),
            base
        );
        // NFC: "é" precomposed (U+00E9) vs decomposed (e + U+0301).
        let precomposed = report_hash("caf\u{00e9}", "body");
        let decomposed = report_hash("cafe\u{0301}", "body");
        assert_eq!(precomposed, decomposed);
    }

    #[test]
    fn distinct_content_distinct_hash() {
        let a = report_hash("subject", "body one");
        assert_ne!(report_hash("subject", "body two"), a);
        assert_ne!(report_hash("subject two", "body one"), a);
    }

    #[test]
    fn subject_body_boundary_is_preserved() {
        // The "\n" join fires after canonicalization, so text cannot slide
        // between subject and body without changing the hash.
        assert_ne!(report_hash("a b", "c"), report_hash("a", "b c"));
        // A sequence-preserving canon: same words, different order, no collision.
        assert_ne!(report_hash("s", "one two"), report_hash("s", "two one"));
    }

    #[test]
    fn empty_inputs_are_stable_and_distinct() {
        assert_eq!(report_hash("", ""), report_hash(" \t\n", ""));
        assert_ne!(report_hash("", ""), report_hash("x", ""));
        assert_ne!(report_hash("x", ""), report_hash("", "x"));
    }
}
