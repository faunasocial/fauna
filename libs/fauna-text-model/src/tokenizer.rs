//! Deterministic Unicode **positional** tokenizer.
//!
//! Determinism is a hard correctness property: the bridge tokenizes inbound
//! mail in the plaintext window, every app tokenizes user-authored content
//! locally, and all platforms must produce byte-identical output for a given
//! plaintext input. Otherwise local indices / model features on different
//! devices diverge.
//!
//! Shared filter / normalization pipeline (identical to the canonical
//! set-form tokenizer, `fauna_mail::tokenizer::tokenize`, which stays in
//! `fauna-mail` with its UniFFI export):
//!   1. NFKC normalize.
//!   2. Word-segment per UAX#29 (via `unicode-segmentation`).
//!   3. Lowercase each word using Unicode case folding (`str::to_lowercase`).
//!   4. Filter: drop tokens that are < 2 chars, or that contain no
//!      alphanumerics.
//!
//! This module emits the tokens in original document order with byte offsets
//! and monotonically increasing positions (the FTS / n-gram feature shape);
//! the set form additionally sorts + dedupes.
//!
//! ## Byte offsets point into the RAW input (S1 fix, 2026-08-02)
//!
//! Tokenization runs over the NFKC-normalized text, but the offsets a consumer
//! needs point into the **string it passed in** — Tantivy hands them back for
//! snippet rendering over the raw text, and an offset into the normalized
//! string is wrong the moment the input contains an NFKC-changing codepoint
//! (ligatures, full-width forms, combining sequences). The fix is an offset
//! map built from **normalization segments**: the raw input is split at cut
//! points where normalization cannot merge across the boundary (the next
//! char's NFKD starts with a starter — canonical combining class 0 — that does
//! not canonically compose with the previous char's normalized tail; Hangul
//! jamo pairs and combining marks therefore stay inside one segment), each
//! segment normalizes independently, and every segment start records a
//! `(normalized_offset, raw_offset)` boundary pair. A token's raw span is then
//! the floor boundary of its normalized start and the ceiling boundary of its
//! normalized end — exact whenever token edges land on segment edges (always,
//! for word-segmented text; a single raw char whose expansion spans several
//! words, e.g. U+33C2 → "a.m.", attributes all of them to that char's span).
//!
//! The segment walk is verified at runtime against the one-pass `nfkc()` of
//! the whole input; if a boundary rule ever misses a merge (none is known),
//! the map degrades to a single whole-input segment — offsets stay valid raw
//! char boundaries, determinism is preserved, and nothing panics.

use serde::{Deserialize, Serialize};
use unicode_normalization::UnicodeNormalization;
use unicode_normalization::char::{canonical_combining_class, compose};
use unicode_segmentation::UnicodeSegmentation;

/// One token in original document order, with byte offsets into the **raw
/// input** (see the module docs). Used for FTS-style positional indexing.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PositionalToken {
    /// Lowercased token text (post-NFKC, post-case-fold).
    pub text: String,
    /// Byte offset into the **raw input** of the first byte of the token's
    /// originating text. Always a char boundary of the raw input. Used for
    /// snippet rendering.
    pub byte_offset_from: usize,
    /// Byte offset into the **raw input** one past the token's last byte.
    /// Always a char boundary of the raw input.
    pub byte_offset_to: usize,
    /// Zero-based position among emitted tokens. Used by Tantivy for phrase
    /// matching.
    pub position: usize,
}

/// NFKC-normalize `input` while recording `(normalized_offset, raw_offset)`
/// boundary pairs at every normalization-segment start, plus the final
/// `(normalized_len, raw_len)` pair. Boundaries are strictly increasing on
/// both axes.
fn nfkc_with_offset_map(input: &str) -> (String, Vec<(usize, usize)>) {
    let mut normalized = String::new();
    let mut boundaries: Vec<(usize, usize)> = Vec::new();

    let mut seg_start_raw = 0usize;
    let mut seg = String::new();
    // Last char of the *normalization* of the segment so far — the composition
    // target the cut test probes. Tracked per raw char (its own NFKC tail),
    // which pairs correctly for the pairwise canonical compositions (Hangul
    // L+V/V+T); anything more exotic is caught by the verify pass below.
    let mut prev_norm_last: Option<char> = None;

    for (raw_off, c) in input.char_indices() {
        let first_decomp = c.nfkd().next().unwrap_or(c);
        let cuts = canonical_combining_class(first_decomp) == 0
            && match prev_norm_last {
                Some(p) => compose(p, first_decomp).is_none(),
                None => true,
            };
        if cuts && !seg.is_empty() {
            boundaries.push((normalized.len(), seg_start_raw));
            normalized.extend(seg.nfkc());
            seg.clear();
            seg_start_raw = raw_off;
        }
        if seg.is_empty() {
            seg_start_raw = raw_off;
        }
        seg.push(c);
        prev_norm_last = c.nfkc().last().or(prev_norm_last);
    }
    if !seg.is_empty() {
        boundaries.push((normalized.len(), seg_start_raw));
        normalized.extend(seg.nfkc());
    }
    boundaries.push((normalized.len(), input.len()));

    // Verify the segment walk against the one-pass normalization of the whole
    // input. A mismatch means a boundary rule missed a cross-segment merge —
    // fall back to the degenerate single-segment map rather than emit offsets
    // computed from bytes that differ from what callers will index.
    if !normalized.chars().eq(input.nfkc()) {
        let whole: String = input.nfkc().collect();
        let len = whole.len();
        return (whole, vec![(0, 0), (len, input.len())]);
    }

    (normalized, boundaries)
}

/// Largest boundary with `normalized_offset <= norm_off`, i.e. the raw start
/// of the segment containing `norm_off`.
fn raw_floor(boundaries: &[(usize, usize)], norm_off: usize) -> usize {
    match boundaries.binary_search_by_key(&norm_off, |(n, _)| *n) {
        Ok(i) => boundaries[i].1,
        Err(i) => boundaries[i.saturating_sub(1)].1,
    }
}

/// Smallest boundary with `normalized_offset >= norm_off`, i.e. the raw end
/// of the segment containing `norm_off`'s predecessor.
fn raw_ceil(boundaries: &[(usize, usize)], norm_off: usize) -> usize {
    match boundaries.binary_search_by_key(&norm_off, |(n, _)| *n) {
        Ok(i) => boundaries[i].1,
        Err(i) => boundaries[i.min(boundaries.len() - 1)].1,
    }
}

/// Positional variant of the canonical set-form tokenizer
/// (`fauna_mail::tokenizer::tokenize`): emits the same filtered, normalized,
/// case-folded tokens but in original document order with **raw-input** byte
/// offsets and monotonically increasing positions.
///
/// Determinism is preserved — given identical input, every platform produces
/// an identical `Vec<PositionalToken>`. The shared filter / normalization
/// pipeline is the same as `tokenize`'s; only the output shape differs.
pub fn tokenize_positional(input: &str) -> Vec<PositionalToken> {
    let (normalized, boundaries) = nfkc_with_offset_map(input);
    let mut out = Vec::new();
    let mut position = 0usize;
    for (norm_from, word) in normalized.unicode_word_indices() {
        let lower: String = word.to_lowercase();
        if lower.chars().count() < 2 {
            continue;
        }
        if !lower.chars().any(|c| c.is_alphanumeric()) {
            continue;
        }
        let norm_to = norm_from + word.len();
        out.push(PositionalToken {
            text: lower,
            byte_offset_from: raw_floor(&boundaries, norm_from),
            byte_offset_to: raw_ceil(&boundaries, norm_to),
            position,
        });
        position += 1;
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The map's exactness property over a torture set: per-segment
    /// normalization must byte-equal the one-pass normalization (i.e. the
    /// fallback never fires for these), and every boundary pair must be a
    /// char boundary of its string.
    #[test]
    fn offset_map_matches_one_pass_nfkc_and_lands_on_char_boundaries() {
        let cases = [
            "plain ascii only",
            "of\u{FB01}cial \u{FB02}ow",              // fi / fl ligatures
            "\u{FF21}\u{FF22}\u{FF23} full width",    // Ａ Ｂ Ｃ
            "cafe\u{301} e\u{301}le\u{300}ve",        // combining acute/grave
            "\u{1100}\u{1161}\u{11A8} hangul jamo",   // L+V+T composes to 각
            "\u{33C2}\u{3300} squared forms",         // ㏂ ㌀ (multi-word expansions)
            "mixed \u{FB01}x \u{FF41}nd e\u{301} ok", // everything at once
            "\u{301}leading combiner",                // pathological: no starter first
        ];
        for input in cases {
            let (normalized, boundaries) = nfkc_with_offset_map(input);
            let one_pass: String = input.nfkc().collect();
            assert_eq!(
                normalized, one_pass,
                "segment walk must equal one-pass NFKC: {input:?}"
            );
            assert!(boundaries.len() >= 2, "at least start+end: {input:?}");
            for w in boundaries.windows(2) {
                assert!(w[0].0 < w[1].0 || (w[0].0 == w[1].0 && w[0].1 <= w[1].1));
            }
            for &(n, r) in &boundaries {
                assert!(
                    normalized.is_char_boundary(n),
                    "norm boundary {n} in {input:?}"
                );
                assert!(input.is_char_boundary(r), "raw boundary {r} in {input:?}");
            }
            assert_eq!(boundaries.last().unwrap(), &(normalized.len(), input.len()));
        }
    }

    #[test]
    fn ascii_offsets_are_identity() {
        let tokens = tokenize_positional("foo bar baz");
        let spans: Vec<(usize, usize)> = tokens
            .iter()
            .map(|t| (t.byte_offset_from, t.byte_offset_to))
            .collect();
        assert_eq!(spans, vec![(0, 3), (4, 7), (8, 11)]);
    }

    #[test]
    fn multi_word_expansion_of_one_raw_char_attributes_both_words_to_it() {
        // U+33C2 ㏂ NFKC-expands to "a.m." — two 1-char words ("a", "m") plus
        // punctuation, all inside ONE raw char. Both fail the <2-char filter,
        // so pad with a real word and check the raw span of what survives.
        let input = "\u{33C2}\u{3300} go";
        // ㌀ expands to アパート (one word, survives).
        let tokens = tokenize_positional(input);
        let apart = tokens
            .iter()
            .find(|t| t.text.contains('\u{30A2}'))
            .expect("the squared-katakana expansion tokenizes");
        assert_eq!(
            &input[apart.byte_offset_from..apart.byte_offset_to],
            "\u{3300}",
            "the expanded word's raw span is exactly its originating char"
        );
    }
}
