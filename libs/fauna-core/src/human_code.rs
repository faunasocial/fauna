//! The one owner of Fauna's **human-readable short code** format — the
//! ambiguity-free alphabet, the display grouping, and the comparison
//! normalization every such code shares.
//!
//! Two kinds of short code exist in the product and they have *nothing* in
//! common except this format:
//!
//!  - the **admin claim code** ([`crate::claim_code`]) — read off a terminal and
//!    **typed** into a client once, protected by the claim throttle;
//!  - the **ATProto consent binding code** (F4 rung 2,
//!    `docs/goal/behavior/atproto-pds-full.md` § F4 detail) — **compared by eye**
//!    between a browser tab and an approval card, never typed.
//!
//! They share the alphabet because both are read by a human off one screen and
//! matched against another, so `0`/`O` and `1`/`I` confusions are the failure
//! mode in both. They share **nothing else**: their lengths, their entropy
//! arguments, and what bounds an attacker are all different, and each consumer
//! states its own.
//!
//! **Why this module exists rather than a second copy of fifteen lines.** The
//! alphabet and the "reducing a uniform byte `% 32` is unbiased" property are
//! security-relevant facts; a copy is a place they can drift apart (priority
//! #4). **And why the consumers do not simply call each other:** `claim_code`'s
//! length is a *user decision* (2026-07-24) deliberately tied to the claim
//! surface's throttle, so a second consumer calling `claim_code::generate()`
//! would silently inherit — and later be broken by — an argument about a
//! completely different endpoint.

/// 32-symbol ambiguity-free alphabet — `A`–`Z` without the confusable `I`/`O`
/// (24 letters) plus the digits `2`–`9` (8 digits), so no `0 1 I O`. Uppercase
/// only; [`normalize`] upcases on the way in. `256 % 32 == 0`, so reducing a
/// uniform random byte `% 32` maps onto this alphabet without bias.
pub const ALPHABET: &[u8; 32] = b"ABCDEFGHJKLMNPQRSTUVWXYZ23456789";

/// Bits of entropy each generated character carries: `log2(32)`.
pub const BITS_PER_CHAR: usize = 5;

/// Mint a code of `len` alphabet characters in display form, with a hyphen
/// inserted after every `group` characters (`generate(8, 4)` → `K7Q2-M9XJ`;
/// `generate(6, 3)` → `K7Q-2M9`).
///
/// Entropy is `len * BITS_PER_CHAR` bits. The caller owns that number and the
/// argument for why it is enough on *its* surface — see the module doc.
///
/// A trailing hyphen is impossible by construction: the separator is emitted
/// *before* a character that starts a group, never after one that ends it, so
/// `len` being an exact multiple of `group` yields `ABCD-EFGH` (one hyphen) and
/// not `ABCD-EFGH-`.
pub fn generate(len: usize, group: usize) -> String {
    assert!(len > 0, "a human code needs at least one character");
    assert!(group > 0, "group size must be positive");
    let mut bytes = vec![0u8; len];
    getrandom::fill(&mut bytes).expect("getrandom failed");
    let mut out = String::with_capacity(len + len / group);
    for (i, &b) in bytes.iter().enumerate() {
        if i != 0 && i % group == 0 {
            out.push('-');
        }
        out.push(ALPHABET[(b as usize) % 32] as char);
    }
    out
}

/// Canonical comparison form: uppercase, with every character that is not an
/// ASCII letter or digit removed — so display hyphens, stray spaces, lower-case
/// typing and a trailing newline all wash out, and a plain hyphen-less code is
/// just as valid typed input and normalizes to itself.
pub fn normalize(code: &str) -> String {
    code.chars()
        .filter(char::is_ascii_alphanumeric)
        .map(|c| c.to_ascii_uppercase())
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The alphabet's two load-bearing properties: no confusable characters,
    /// and a size that divides 256 so the `% 32` reduction is unbiased. A
    /// future edit that adds `O` back, or trims the set to 31 symbols, is a
    /// silent entropy or legibility regression — this is what catches it.
    #[test]
    fn the_alphabet_is_ambiguity_free_and_unbiased() {
        assert_eq!(ALPHABET.len(), 32);
        assert_eq!(256 % ALPHABET.len(), 0, "`% 32` must be unbiased");
        assert_eq!(1usize << BITS_PER_CHAR, ALPHABET.len());
        for c in ALPHABET {
            assert!(
                !matches!(c, b'0' | b'1' | b'I' | b'O'),
                "confusable character {} is in the alphabet",
                *c as char
            );
            assert!(c.is_ascii_uppercase() || c.is_ascii_digit());
        }
        // No duplicates — a repeated symbol would quietly cost entropy.
        let mut sorted = *ALPHABET;
        sorted.sort_unstable();
        let mut deduped = sorted.to_vec();
        deduped.dedup();
        assert_eq!(deduped.len(), 32);
    }

    /// Grouping never leaves a trailing hyphen, including the exact-multiple
    /// case (8 chars in groups of 4 is `ABCD-EFGH`, one hyphen, not two) —
    /// the case a non-multiple length never exercises.
    #[test]
    fn grouping_has_no_trailing_hyphen_even_at_an_exact_multiple() {
        for (len, group, want_hyphens) in [
            (8, 4, 1),
            (6, 3, 1),
            (9, 3, 2),
            (7, 4, 1),
            (4, 4, 0),
            (1, 4, 0),
        ] {
            let code = generate(len, group);
            assert!(!code.ends_with('-'), "{code} ends with a hyphen");
            assert!(!code.starts_with('-'), "{code} starts with a hyphen");
            assert_eq!(
                code.matches('-').count(),
                want_hyphens,
                "{code} (len {len}, group {group})"
            );
            assert_eq!(normalize(&code).len(), len);
        }
    }

    #[test]
    fn every_generated_character_is_in_the_alphabet() {
        for _ in 0..64 {
            for c in normalize(&generate(8, 4)).bytes() {
                assert!(ALPHABET.contains(&c), "{} is off-alphabet", c as char);
            }
        }
    }

    #[test]
    fn normalize_strips_grouping_case_and_whitespace() {
        assert_eq!(normalize("k7q2-m9xj"), "K7Q2M9XJ");
        assert_eq!(normalize(" K7Q2-M9XJ \n"), "K7Q2M9XJ");
        assert_eq!(normalize("K7Q2 M9XJ"), "K7Q2M9XJ");
    }
}
