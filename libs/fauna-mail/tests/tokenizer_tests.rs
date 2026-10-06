use fauna_mail::tokenizer::tokenize;

#[test]
fn ascii_words_are_lowercased_and_deduped() {
    let tokens = tokenize("Hello hello WORLD World");
    let set: Vec<&str> = tokens.tokens.iter().map(|s| s.as_str()).collect();
    assert_eq!(set, vec!["hello", "world"]);
}

#[test]
fn punctuation_is_dropped() {
    let tokens = tokenize("foo, bar! baz?");
    let set: Vec<&str> = tokens.tokens.iter().map(|s| s.as_str()).collect();
    assert_eq!(set, vec!["bar", "baz", "foo"]);
}

#[test]
fn one_char_tokens_are_filtered() {
    let tokens = tokenize("a is a test");
    let set: Vec<&str> = tokens.tokens.iter().map(|s| s.as_str()).collect();
    assert_eq!(set, vec!["is", "test"]);
}

#[test]
fn nfkc_normalizes_compatibility_forms() {
    // U+FB01 LATIN SMALL LIGATURE FI should NFKC-decompose to 'fi'
    let tokens = tokenize("of\u{FB01}cial");
    let set: Vec<&str> = tokens.tokens.iter().map(|s| s.as_str()).collect();
    assert_eq!(set, vec!["official"]);
}

#[test]
fn cjk_input_does_not_panic() {
    // UAX#29 word segmentation in unicode-segmentation 1.x treats each kana
    // / ideograph as a single-character word. After the <2-char filter, all
    // tokens are dropped — so CJK content currently produces an empty index.
    // This is a known indexing gap (proper CJK indexing would require
    // n-gram segmentation, deferred). The test confirms the algorithm
    // doesn't panic on CJK input.
    let result = tokenize("こんにちは 世界");
    assert!(
        result.tokens.is_empty(),
        "CJK currently produces no tokens (known limitation)"
    );
}

#[test]
fn mixed_input_is_sorted_alphabetically() {
    let tokens = tokenize("zebra apple monkey");
    let set: Vec<&str> = tokens.tokens.iter().map(|s| s.as_str()).collect();
    assert_eq!(set, vec!["apple", "monkey", "zebra"]);
}

#[test]
fn empty_input_produces_empty_token_set() {
    let tokens = tokenize("");
    assert!(tokens.tokens.is_empty());
}

#[test]
fn whitespace_only_produces_empty() {
    let tokens = tokenize("   \t\n\r   ");
    assert!(tokens.tokens.is_empty());
}

#[test]
fn determinism_repeated_invocations_produce_identical_output() {
    let input = "Hello World こんにちは Foo Bar";
    let a = tokenize(input);
    let b = tokenize(input);
    assert_eq!(a, b);
}

#[test]
fn determinism_serialization_is_canonical() {
    // Same input must produce byte-identical canonical_bytes output.
    let a = tokenize("foo bar baz");
    let b = tokenize("foo bar baz");
    assert_eq!(a.canonical_bytes, b.canonical_bytes);
}

const REPRODUCIBILITY_VECTORS: &[(&str, &str)] = &[
    ("foo bar baz", "000000036261720000000362617a00000003666f6f"),
    ("ABC Def", "0000000361626300000003646566"),
    ("", ""),
    ("a is a test", "0000000269730000000474657374"),
];

#[test]
fn reproducibility_vectors_are_stable() {
    for (input, expected_hex) in REPRODUCIBILITY_VECTORS.iter().copied() {
        let tokens = tokenize(input);
        let actual_hex: String = tokens
            .canonical_bytes
            .iter()
            .map(|b| format!("{:02x}", b))
            .collect();
        assert_eq!(
            actual_hex, expected_hex,
            "tokenizer output drift for input {:?}: expected {} got {}",
            input, expected_hex, actual_hex
        );
    }
}

use fauna_mail::tokenizer::tokenize_positional;

#[test]
fn positional_preserves_order_and_position() {
    let tokens = tokenize_positional("Hello WORLD hello");
    let extracted: Vec<(&str, usize)> = tokens
        .iter()
        .map(|t| (t.text.as_str(), t.position))
        .collect();
    assert_eq!(
        extracted,
        vec![("hello", 0), ("world", 1), ("hello", 2)],
        "positional tokenization keeps repetition + order"
    );
}

#[test]
fn positional_byte_offsets_point_into_input() {
    let input = "foo bar";
    let tokens = tokenize_positional(input);
    // First token covers "foo" (0..3); second covers "bar" (4..7).
    assert_eq!(tokens[0].text, "foo");
    assert_eq!(tokens[0].byte_offset_from, 0);
    assert_eq!(tokens[0].byte_offset_to, 3);
    assert_eq!(tokens[1].text, "bar");
    assert_eq!(tokens[1].byte_offset_from, 4);
    assert_eq!(tokens[1].byte_offset_to, 7);
}

#[test]
fn positional_drops_short_and_non_alphanumeric_tokens_like_canonical() {
    // Same filter rules as `tokenize`: <2 chars dropped, all-non-alphanumeric dropped.
    let tokens = tokenize_positional("a is, ! test");
    let extracted: Vec<&str> = tokens.iter().map(|t| t.text.as_str()).collect();
    assert_eq!(extracted, vec!["is", "test"]);
}

#[test]
fn positional_nfkc_normalizes() {
    // U+FB01 LATIN SMALL LIGATURE FI → "fi"
    let tokens = tokenize_positional("of\u{FB01}cial run");
    let extracted: Vec<&str> = tokens.iter().map(|t| t.text.as_str()).collect();
    assert_eq!(extracted, vec!["official", "run"]);
}

#[test]
fn positional_byte_offsets_are_raw_input_offsets_for_nfkc_changing_text() {
    // The S1 raw-offset fix: offsets point into
    // the RAW input, not the NFKC-normalized string — snippet rendering slices
    // the raw text with them.

    // U+FB01 LATIN SMALL LIGATURE FI (3 bytes raw) expands to "fi" (2 bytes).
    let input = "of\u{FB01}cial run";
    let tokens = tokenize_positional(input);
    assert_eq!(tokens[0].text, "official");
    assert_eq!(
        (tokens[0].byte_offset_from, tokens[0].byte_offset_to),
        (0, 9),
        "the token must span the raw ligature bytes"
    );
    assert_eq!(
        &input[tokens[0].byte_offset_from..tokens[0].byte_offset_to],
        "of\u{FB01}cial"
    );
    assert_eq!(tokens[1].text, "run");
    assert_eq!(
        &input[tokens[1].byte_offset_from..tokens[1].byte_offset_to],
        "run"
    );

    // Full-width forms: each raw char is 3 bytes, normalized to 1.
    let input = "x \u{FF21}\u{FF22}\u{FF23} y";
    let tokens = tokenize_positional(input);
    assert_eq!(tokens[0].text, "abc");
    assert_eq!(
        &input[tokens[0].byte_offset_from..tokens[0].byte_offset_to],
        "\u{FF21}\u{FF22}\u{FF23}"
    );

    // A combining sequence that NFC-composes: raw "e" + U+0301 (3 bytes) → "é" (2 bytes).
    let input = "cafe\u{301} time";
    let tokens = tokenize_positional(input);
    assert_eq!(tokens[0].text, "café");
    assert_eq!(
        &input[tokens[0].byte_offset_from..tokens[0].byte_offset_to],
        "cafe\u{301}"
    );
    assert_eq!(tokens[1].text, "time");
    assert_eq!(
        &input[tokens[1].byte_offset_from..tokens[1].byte_offset_to],
        "time"
    );

    // Hangul jamo compose across starters (L+V): the segment map must not cut
    // inside the composition.
    let input = "\u{1100}\u{1161}\u{1100}\u{1161} ok";
    let tokens = tokenize_positional(input);
    assert_eq!(
        &input[tokens[0].byte_offset_from..tokens[0].byte_offset_to],
        "\u{1100}\u{1161}\u{1100}\u{1161}"
    );

    // Offsets always lie on raw char boundaries (slicing must never panic).
    for t in tokenize_positional("of\u{FB01}cial \u{FF21}b cafe\u{301} \u{33C2}now") {
        let _ = &"of\u{FB01}cial \u{FF21}b cafe\u{301} \u{33C2}now"
            [t.byte_offset_from..t.byte_offset_to];
    }
}
