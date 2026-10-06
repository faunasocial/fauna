//! Deterministic Unicode tokenizer for encrypted-search index hints.
//!
//! Determinism is a hard correctness property: the bridge tokenizes inbound
//! mail in the plaintext window, every app tokenizes user-authored content
//! locally, and all platforms must produce byte-identical output for a given
//! plaintext input. Otherwise local indices on different devices diverge and
//! search results become device-dependent.
//!
//! Algorithm:
//!   1. NFKC normalize.
//!   2. Word-segment per UAX#29 (via `unicode-segmentation`).
//!   3. Lowercase each word using Unicode case folding (`str::to_lowercase`).
//!   4. Filter: drop tokens that are < 2 chars, or that contain no
//!      alphanumerics.
//!   5. Sort lexicographically (byte-order on UTF-8) and deduplicate.
//!   6. Emit the sorted, deduped, length-prefixed canonical bytes form.

use serde::{Deserialize, Serialize};
use std::collections::BTreeSet;
use unicode_normalization::UnicodeNormalization;
use unicode_segmentation::UnicodeSegmentation;

// The positional variant (ordered tokens + byte offsets — the FTS / n-gram
// feature shape) lives in the mail-independent `fauna-text-model` crate (the
// n-gram NB model's feature extractor lives beside it); re-exported here so
// `fauna_mail::tokenizer::{PositionalToken, tokenize_positional}` resolve
// unchanged. The uniffi-exported set-form `tokenize` + `CanonicalTokenSet`
// below stay in this crate (the UniFFI binding surface must not move crates).
pub use fauna_text_model::tokenizer::{PositionalToken, tokenize_positional};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "uniffi", derive(uniffi::Record))]
pub struct CanonicalTokenSet {
    /// Sorted, deduplicated tokens. Reading order is a function of the input,
    /// not insertion order — every platform produces the same `tokens` vec
    /// for a given input.
    pub tokens: Vec<String>,
    /// Length-prefixed concatenation of `tokens`. Useful as a single input
    /// to a downstream encryption step. Format: for each token,
    /// `<u32 BE length><utf8 bytes>`. Stable across platforms.
    pub canonical_bytes: Vec<u8>,
}

#[cfg_attr(feature = "uniffi", uniffi::export)]
pub fn tokenize(input: &str) -> CanonicalTokenSet {
    // NFKC normalize.
    let normalized: String = input.nfkc().collect();

    // Word-segment + lowercase + filter.
    let mut set: BTreeSet<String> = BTreeSet::new();
    for word in normalized.unicode_words() {
        let lower: String = word.to_lowercase();
        if lower.chars().count() < 2 {
            continue;
        }
        if !lower.chars().any(|c| c.is_alphanumeric()) {
            continue;
        }
        set.insert(lower);
    }

    let tokens: Vec<String> = set.into_iter().collect();

    // Build canonical_bytes: <u32 BE len><bytes> per token.
    let mut canonical_bytes = Vec::new();
    for tok in &tokens {
        let bytes = tok.as_bytes();
        canonical_bytes.extend_from_slice(&(bytes.len() as u32).to_be_bytes());
        canonical_bytes.extend_from_slice(bytes);
    }

    CanonicalTokenSet {
        tokens,
        canonical_bytes,
    }
}

#[cfg(test)]
mod path_tokenization_tests {
    /// **What a file path tokenizes to — pinned because the File search arm
    /// depends on it** (`content-index.md` § Ingest triggers, v1 → *The
    /// files/media arms are SCOPED*: a File doc's searchable text is its path,
    /// "basename and segments, tokenized").
    ///
    /// This exists because the answer is not the obvious one. `unicode_words`
    /// splits on `/`, so directory segments become their own tokens — but it
    /// treats `.` between letters as *word-internal* (UAX #29 `MidNumLet`), so
    /// `albatross.jpg` is **one** token and a user typing `albatross` does not
    /// match it. Any change here changes what files are findable by name, and
    /// would need `TOKENIZER_PIPELINE_VERSION` raised for a re-index.
    #[test]
    fn a_path_splits_on_separators_but_not_on_the_extension_dot() {
        assert_eq!(
            super::tokenize("holidays/albatross.jpg").tokens,
            vec!["albatross.jpg".to_string(), "holidays".to_string()],
            "directory segments tokenize separately; the extension stays welded \
             to the stem, which is why the File arm must index the bare basename \
             stem as well as the whole path"
        );
    }
}
