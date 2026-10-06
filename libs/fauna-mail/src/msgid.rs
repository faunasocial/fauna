//! The Fauna Message-ID mint — **self-describing**, so provenance is
//! *verifiable*, never inferred from a lookalike shape.
//!
//! `docs/goal/behavior/family-safety.md` § The mail gate owns the consumer:
//! the guardian mail gate seeds its DSN correlation only for Message-IDs a
//! Fauna path minted, because whether a third-party MUA's id is *guessable*
//! is undecidable, and an id of unknowable entropy must never become an
//! authorizing token for delivery past the gate.
//!
//! The first predicate tested only the mint's **shape** (32 lowercase hex at
//! a domain) — which is also the exact shape of an MD5 hex digest and of
//! Python's `uuid4().hex`, both common third-party local parts, some derived
//! predictably. So the mint now embeds its own proof:
//!
//! ```text
//! local = hex( random[12] ‖ tag[4] )        (still exactly 32 lowercase hex)
//! tag   = sha256("fauna-msgid-v1" ‖ random)[..4]
//! ```
//!
//! [`is_fauna_minted_msgid`] recomputes the tag — it *verifies* rather than
//! pattern-matches. An innocent third-party id of the same shape passes with
//! probability 2⁻³²; 96 random bits keep the id unguessable.
//!
//! **Deliberately keyless.** The tag is publicly computable, and that is
//! enough: the property the gate needs is *distinguishability from innocent
//! third-party formats*, not unforgeability. The only party who could
//! "forge" a tag-valid id into a ward's seed set is the ward's own
//! (modified) client choosing weak randomness — and a colluding ward gains
//! nothing, since any ward can already admit any correspondent by simply
//! mailing them (the outbound allowlist auto-seed). A nest-keyed MAC would
//! buy no security and cost key distribution to every client of every nest.
//!
//! Both mint paths share this one implementation (priority #2): the native
//! compose path (`fauna_conversations::rfc5322::new_message_id`, which
//! supplies its own randomness and calls [`mint_local`]) and the Go
//! submission server's stamp (over the UniFFI export, like
//! [`crate::dedup_key::mail_dedup_keys`] — no Go reimplementation).
//!
//! The local part stays 32 lowercase hex. A tag-less mint does not seed —
//! that ward's bounces are held for guardian release, the ratified
//! fail-toward-holding direction.

use sha2::{Digest, Sha256};

/// Domain-separation constant under the tag hash. Changing it (or the tag
/// construction) is a new mint version: old ids would stop verifying, which
/// fails toward *holding* null-path reports, never toward delivering them —
/// but do it deliberately, alongside the seed/verify sites.
const DOMAIN_SEP: &[u8] = b"fauna-msgid-v1";

/// Random prefix length, bytes. 96 bits — unguessable within any realistic
/// correlation window (the seed retention is 30 days).
pub const MSGID_RANDOM_LEN: usize = 12;

/// Verification-tag length, bytes. 2⁻³² accidental-pass for a lookalike; the
/// tag defends against *innocent format collision*, not an adversary, so 32
/// bits is the right size (see module docs — deliberately keyless).
pub const MSGID_TAG_LEN: usize = 4;

/// Mint the 32-lowercase-hex Message-ID **local part** from caller-supplied
/// randomness: `hex(random ‖ sha256(sep ‖ random)[..4])`.
///
/// Pure so the wasm-built compose path can call it with its own `getrandom`
/// bytes; native callers who just want a fresh local part use
/// [`new_fauna_msgid_local`] (feature `msgid-mint`).
pub fn mint_local(random: &[u8; MSGID_RANDOM_LEN]) -> String {
    let mut hasher = Sha256::new();
    hasher.update(DOMAIN_SEP);
    hasher.update(random);
    let tag = hasher.finalize();
    let mut local = String::with_capacity((MSGID_RANDOM_LEN + MSGID_TAG_LEN) * 2);
    for b in random.iter().chain(&tag[..MSGID_TAG_LEN]) {
        use std::fmt::Write as _;
        let _ = write!(local, "{b:02x}");
    }
    local
}

/// Is this **normalized** Message-ID (see
/// [`crate::dedup_key::normalize_message_id`]) one a Fauna path minted?
/// Parses the `32-lowercase-hex@domain` shape and **recomputes the embedded
/// tag** — a verification, not a pattern match (module docs). `false` for
/// anything else, including the MD5/`uuid4().hex` lookalikes the shape test
/// used to admit.
///
/// The guardian mail gate seeds its DSN correlation only for ids that pass
/// (`family-safety.md` § The mail gate).
pub fn is_fauna_minted_msgid(normalized: &str) -> bool {
    let Some((local, domain)) = normalized.split_once('@') else {
        return false;
    };
    if domain.is_empty() || local.len() != (MSGID_RANDOM_LEN + MSGID_TAG_LEN) * 2 {
        return false;
    }
    let Some(bytes) = decode_hex_lower(local) else {
        return false;
    };
    let mut random = [0u8; MSGID_RANDOM_LEN];
    random.copy_from_slice(&bytes[..MSGID_RANDOM_LEN]);
    // Not secret-dependent: the tag is publicly computable (keyless by
    // design), so a constant-time comparison would protect nothing.
    mint_local(&random) == local
}

/// Strict lowercase-hex decode; `None` on any non-`[0-9a-f]` byte (an
/// uppercase digit is outside the mint shape — normalization lowercases
/// before this predicate runs, exactly as the seed does).
fn decode_hex_lower(s: &str) -> Option<Vec<u8>> {
    fn nibble(b: u8) -> Option<u8> {
        match b {
            b'0'..=b'9' => Some(b - b'0'),
            b'a'..=b'f' => Some(b - b'a' + 10),
            _ => None,
        }
    }
    let bytes = s.as_bytes();
    if !bytes.len().is_multiple_of(2) {
        return None;
    }
    bytes
        .as_chunks::<2>()
        .0
        .iter()
        .map(|p| Some(nibble(p[0])? << 4 | nibble(p[1])?))
        .collect()
}

/// A fresh Fauna-minted Message-ID local part — RNG included, for callers
/// without their own (the Go submission server stamps
/// `Message-ID: <{this}@{domain}>` when the submitting MUA omitted one).
/// Native-only (`msgid-mint`); the wasm compose path supplies its own
/// randomness to [`mint_local`] instead.
#[cfg(feature = "msgid-mint")]
#[cfg_attr(feature = "uniffi", uniffi::export)]
pub fn new_fauna_msgid_local() -> String {
    let mut random = [0u8; MSGID_RANDOM_LEN];
    getrandom::fill(&mut random).expect("getrandom failed");
    mint_local(&random)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Golden vector, derived independently (Python `hashlib`) from the
    /// module-doc construction rather than from this implementation:
    /// `sha256(b"fauna-msgid-v1" + bytes(range(12)))[:4] = 4257c1d5`. Pins
    /// the domain-separation constant, the tag truncation, and the hex
    /// layout at once — a silent change to any of them would strand every
    /// already-seeded id (fails toward holding, but deliberately, not by
    /// refactor accident).
    #[test]
    fn mint_matches_the_golden_vector() {
        let random: [u8; MSGID_RANDOM_LEN] = core::array::from_fn(|i| i as u8);
        assert_eq!(mint_local(&random), "000102030405060708090a0b4257c1d5");
    }

    #[test]
    fn a_minted_local_verifies_at_any_domain() {
        let local = mint_local(&[0xAB; MSGID_RANDOM_LEN]);
        assert_eq!(local.len(), 32, "the local part stays 32 hex");
        assert!(is_fauna_minted_msgid(&format!("{local}@fauna.test")));
        assert!(is_fauna_minted_msgid(&format!("{local}@other.example")));
        assert!(!is_fauna_minted_msgid(&local), "a domain is required");
        assert!(!is_fauna_minted_msgid(&format!("{local}@")));
    }

    #[test]
    fn a_lookalike_32_hex_id_fails_verification() {
        // The exact shapes the old predicate admitted: an MD5 hex digest and
        // a uuid4().hex — 32 lowercase hex of unknowable entropy.
        for lookalike in [
            "d41d8cd98f00b204e9800998ecf8427e@mail.example.com",
            "0f47c1a2b3d4e5f60718293a4b5c6d7e@laptop.local",
        ] {
            assert!(
                !is_fauna_minted_msgid(lookalike),
                "{lookalike:?} is shape, not provenance"
            );
        }
    }

    #[test]
    fn a_flipped_bit_anywhere_fails_verification() {
        let local = mint_local(&[0x5A; MSGID_RANDOM_LEN]);
        for i in 0..local.len() {
            let mut broken = local.clone().into_bytes();
            broken[i] = if broken[i] == b'0' { b'1' } else { b'0' };
            let broken = String::from_utf8(broken).unwrap();
            if broken == local {
                continue;
            }
            assert!(
                !is_fauna_minted_msgid(&format!("{broken}@fauna.test")),
                "flipping hex digit {i} must break the tag or the random-tag binding"
            );
        }
    }

    #[test]
    fn non_mint_shapes_fail_fast() {
        for id in [
            "1699999999.12345@wards-laptop",          // timestamp.counter
            "abc@example.com",                        // short local
            "0123456789ABCDEF0123456789ABCDEF@d.tld", // uppercase (pre-normalize)
            "0123456789abcdef0123456789abcdeg@d.tld", // non-hex char
            "000102030405060708090a0b4257c1d5",       // no domain at all
            "",
        ] {
            assert!(!is_fauna_minted_msgid(id), "{id:?} must not verify");
        }
    }

    #[cfg(feature = "msgid-mint")]
    #[test]
    fn the_rng_mint_verifies_and_never_repeats() {
        let a = new_fauna_msgid_local();
        let b = new_fauna_msgid_local();
        assert_ne!(a, b);
        for local in [a, b] {
            assert!(is_fauna_minted_msgid(&format!("{local}@fauna.test")));
        }
    }
}
