//! Shared admin claim-code format — the single source of truth for how the
//! one-time nest admin claim code is generated, displayed, and compared.
//!
//! Two surfaces *mint* a claim code, both via [`generate`]:
//!  - the nest itself, on first boot / factory-reset / the `install.sh`
//!    installer (`bins/fauna-nest/src/claim.rs`, `install.sh`), and
//!  - the client provisioning flow, which mints the code before the box exists
//!    and tells the admin to inject it via cloud-init
//!    (`fauna_provisioning::generate_claim_code`).
//!
//! Exactly one surface *checks* a submitted code against the stored one — the
//! nest's `claim_admin_core` — and it does so through [`normalize`], so the
//! admin may type the code with or without the display grouping, in any
//! case, and a trailing newline from the `claim-code` file washes out.
//!
//! Format: 8 characters from a 32-symbol ambiguity-free alphabet (`A`–`Z`
//! without the confusable `I`/`O`, plus the digits `2`–`9` — i.e. no
//! `0 1 I O`), i.e. **40 bits of entropy** (8 × log2 32), displayed as two
//! hyphen-separated groups of four for transcription (`K7Q2-M9XJ`).
//!
//! **Why 40 bits, and what it rests on (user decision, 2026-07-24).** This is a
//! deliberate reduction from the 26-char / 130-bit code that stood between
//! 2026-05-31 and 2026-07-24, taken for transcription comfort: a claim code is
//! read off a terminal and typed into a client by hand, once. 40 bits is not
//! brute-forceable at any *attainable* guess rate — the global claim throttle
//! (60 attempts/60 s across all sources) caps exhaustion at ~35 000 years
//! (~17 000 expected) — but unlike the 130-bit code it is **not** safe on
//! entropy alone.
//!
//! So the throttles in `bins/fauna-nest/src/anonymous_rate_limit.rs`
//! (`claim_config`, `global_claim_config`) are once again the **primary bound**,
//! not defense-in-depth: they were demoted to defense-in-depth when the code was
//! 130 bits, and this change promotes them back. **Do not loosen or remove
//! either throttle without first restoring the code length.** The known cost is
//! the availability tradeoff documented at `global_claim_config`: an attacker
//! can saturate the global bucket to *delay* (never take over) a legitimate
//! claim. The accepted mitigation is to firewall the box to your own address
//! while claiming — it is claimed once, usually within minutes of deploy.
//! **That "usually within minutes" premise is itself the client's automatic
//! post-provisioning claim, and it shipped silently broken for every
//! wizard-provisioned box until 2026-08-29 — measured only on live paid
//! Hetzner runs, because the crate's own test double couldn't tell "dialed
//! the box" from "dialed the empty string".** What now holds the premise up below the
//! live-VPS tier:
//! `fauna_onboarding_machine::machine::tests::run_provisioning_claim_dials_its_own_parameter_not_the_stale_state_field`.
//!
//! ⚠ **That mitigation is not universal, and the exception is structural.** A
//! box that must obtain the IP bridge cert — the publicly-trusted certificate
//! for its own address, without which a *browser* cannot reach a domainless box
//! at all (`docs/goal/architecture/nest/tls-certificates.md` § B-IP) — has to be
//! reachable on `:80` from arbitrary CA validation vantage points, from first
//! boot. So on the **web** client-provisioning path the firewall mitigation and
//! onboarding are mutually exclusive: the user gets one or the other. It remains
//! available on the **native** path, which reaches the box by its IP under the
//! channel binding and needs no public cert. § B-IP records why the residual
//! availability cost is accepted rather than designed away.
//!
//! Authority + history: `docs/goal/architecture/federation.md` § Security. The
//! original code was 24-bit/6-hex, which *was* brute-forceable at the root; the
//! floor that matters is that the keyspace stay unexhaustible under the throttle
//! ceiling, which 40 bits clears by orders of magnitude and 24 bits did not.

/// Number of alphabet characters in a generated code. 8 × 5 bits = 40 bits of
/// entropy — safe only *because* the claim surface is throttled; see the module
/// doc before changing this or the throttles.
///
/// This constant is **this surface's**, not the format's: the shared
/// [`crate::human_code`] module owns the alphabet and the grouping, and every
/// consumer states its own length and its own reason for it. A second consumer
/// must never reach for `claim_code::generate()` to get "a short code" — it
/// would inherit the throttle argument above, which is about this endpoint
/// alone.
const CODE_LEN: usize = 8;

/// Display grouping: a hyphen is inserted after every `GROUP` characters, so an
/// 8-character code reads `ABCD-EFGH`.
const GROUP: usize = 4;

/// Mint a fresh claim code in its display form (grouped, hyphenated, e.g.
/// `K7Q2-M9XJ`).
pub fn generate() -> String {
    crate::human_code::generate(CODE_LEN, GROUP)
}

/// Canonical comparison form: uppercase, with every character that is not an
/// ASCII letter or digit removed. The nest applies this to BOTH the stored code
/// and the submitted code before comparing, so the display hyphens, stray
/// spaces, lower-case typing, and the trailing newline from the `claim-code`
/// file all wash out — and a plain code with no hyphens (e.g. a 6-hex code
/// typed or pinned by a test) still normalizes to itself.
pub fn normalize(code: &str) -> String {
    crate::human_code::normalize(code)
}

/// A parsed claim-page input: the bare code plus, when the input was the
/// console-printed [`claim_uri`], the `nest_actor_id` the console vouched for.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ImportedClaim {
    /// The claim code as typed / as embedded in the URI (display form — the
    /// nest normalizes at compare, per [`normalize`]).
    pub code: String,
    /// 64-hex `nest_actor_id` when the input carried one. The onboarding
    /// machine holds it as the first-contact identity root for the claim
    /// host, so the channel the code is sent over is verified against the
    /// identity the console printed (`security.md` § Transport trust, Axis 2 —
    /// the self-hosted public-domain shape's out-of-band root).
    pub nest_actor_id: Option<String>,
}

/// Encode the console banner's claim URI — the code plus this nest's identity,
/// on its own `fauna://claim` host (the deliberate sibling of
/// `fauna://recovery` / `fauna://identity`, so a claim payload can never be
/// mistaken for a secret import). The console is the out-of-band channel the
/// code already travels on; carrying the identity beside it is what gives a
/// hand-deployed public-domain nest an Axis-2 root at claim time.
pub fn claim_uri(code: &str, nest_actor_id_hex: &str) -> String {
    format!("fauna://claim?code={code}&nest={nest_actor_id_hex}")
}

/// Parse a claim-page input: a typed bare code, or a pasted/scanned
/// [`claim_uri`].
///
/// Anything that does not start with the `fauna://claim` scheme (ASCII
/// case-insensitive) is the code itself, verbatim — exactly the pre-URI
/// behavior, so old consoles and hand-typed codes keep working. A recognized
/// URI must carry a non-empty `code=`; a `nest=` value that is present but not
/// 64-hex fails the WHOLE parse (`None`) rather than silently dropping the pin
/// — an input that looks protected must never quietly lose its protection.
pub fn parse_claim_input(input: &str) -> Option<ImportedClaim> {
    let trimmed = input.trim();
    const SCHEME: &str = "fauna://claim";
    let Some(head) = trimmed.get(..SCHEME.len()) else {
        // Shorter than the scheme (in bytes) — a bare code.
        return Some(ImportedClaim {
            code: trimmed.to_string(),
            nest_actor_id: None,
        });
    };
    if !head.eq_ignore_ascii_case(SCHEME) {
        return Some(ImportedClaim {
            code: trimmed.to_string(),
            nest_actor_id: None,
        });
    }

    let query = trimmed[SCHEME.len()..].strip_prefix('?')?;
    let mut code = None;
    let mut nest = None;
    for param in query.split('&') {
        if let Some(v) = param.strip_prefix("code=") {
            code = Some(v);
        } else if let Some(v) = param.strip_prefix("nest=") {
            nest = Some(v);
        }
    }
    let code = code.filter(|c| !c.is_empty())?;
    let nest_actor_id = match nest.filter(|n| !n.is_empty()) {
        Some(n) if crate::hex32::is_hex64(n) => Some(n.to_string()),
        Some(_) => return None,
        None => None,
    };
    Some(ImportedClaim {
        code: code.to_string(),
        nest_actor_id,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::human_code::ALPHABET;

    /// Number of hyphens this length/grouping produces: one before each group
    /// boundary *strictly inside* the code. Note this is `(CODE_LEN - 1) /
    /// GROUP`, not `CODE_LEN / GROUP` — those differ exactly when `CODE_LEN` is
    /// a multiple of `GROUP` (8 chars is `ABCD-EFGH`: one hyphen, not two),
    /// which is precisely the case the old 26-char format never exercised.
    /// Test-only: the generator itself lives in [`crate::human_code`], which
    /// pins the no-trailing-hyphen property for every consumer.
    const fn hyphen_count() -> usize {
        (CODE_LEN - 1) / GROUP
    }

    #[test]
    fn generated_code_has_expected_shape() {
        let code = generate();
        assert!(code.contains('-'), "display form is grouped: {code}");
        let norm = normalize(&code);
        assert_eq!(norm.len(), CODE_LEN, "{CODE_LEN} alphabet chars: {code}");
        assert!(
            norm.bytes().all(|b| ALPHABET.contains(&b)),
            "alphabet only: {code}"
        );
        assert!(!norm.contains('-'), "normalize strips hyphens: {norm}");
    }

    /// 8 chars over a 32-symbol alphabet = 40 bits. Const on purpose — the
    /// assert pins `CODE_LEN` against a silent shrink.
    ///
    /// The floor is **40 bits, not 128**: the 2026-07-24 user decision traded
    /// entropy for transcription comfort and made the claim throttles the
    /// primary bound again (module doc). 40 bits is ~35 000 years to exhaust at
    /// the 60/60 s global cap; the original 24-bit code, which *was* genuinely
    /// brute-forceable, is what this floor exists to keep us above. Shrinking
    /// further requires re-deriving that arithmetic against the throttle
    /// ceiling, not just lowering this number.
    #[test]
    fn entropy_is_at_least_40_bits() {
        #[allow(clippy::assertions_on_constants)]
        const _ENTROPY_OK: () = assert!(
            CODE_LEN * 5 >= 40,
            "code must carry >= 40 bits of entropy (see module doc: the claim \
             throttles are the primary bound at this length)"
        );
    }

    #[test]
    fn ambiguous_characters_are_never_emitted() {
        for _ in 0..512 {
            let code = normalize(&generate());
            for bad in ['0', '1', 'I', 'O'] {
                assert!(!code.contains(bad), "ambiguous char {bad} in {code}");
            }
        }
    }

    #[test]
    fn fresh_codes_differ() {
        assert_ne!(generate(), generate(), "codes must be random");
    }

    #[test]
    fn normalize_is_forgiving_of_grouping_case_and_whitespace() {
        let code = generate();
        let lower = code.to_lowercase();
        let spaced = code.replace('-', "  ");
        let with_newline = format!("{code}\n");
        let stripped = code.replace('-', "");
        assert_eq!(normalize(&code), normalize(&lower));
        assert_eq!(normalize(&code), normalize(&spaced));
        assert_eq!(normalize(&code), normalize(&with_newline));
        assert_eq!(normalize(&code), normalize(&stripped));
    }

    #[test]
    fn normalize_round_trips_plain_codes() {
        // Plain hyphen-less codes (e.g. 6-hex, as pinned by tests)
        // still normalize to themselves, so the nest comparison keeps matching.
        assert_eq!(normalize("aabbcc"), "AABBCC");
        assert_eq!(normalize("AABBCC"), "AABBCC");
        assert_eq!(normalize("A1B2C3"), "A1B2C3");
    }

    #[test]
    fn normalize_of_empty_and_all_punctuation_is_empty() {
        assert_eq!(normalize(""), "");
        assert_eq!(normalize("---   \n\t--"), "");
        assert_eq!(normalize("!!!@@@###"), "");
    }

    #[test]
    fn normalize_drops_non_ascii_but_keeps_ascii_alphanumerics() {
        // is_ascii_alphanumeric is false for any non-ASCII char regardless of
        // script, so accented letters / non-ASCII digits are dropped, not
        // transliterated.
        assert_eq!(normalize("héllo"), "HLLO");
        assert_eq!(normalize("a\u{0967}b"), "AB"); // Devanagari digit one
        assert_eq!(normalize("café-42"), "CAF42");
    }

    #[test]
    fn normalize_is_idempotent() {
        let code = generate();
        let once = normalize(&code);
        let twice = normalize(&once);
        assert_eq!(
            once, twice,
            "normalize(normalize(x)) must equal normalize(x)"
        );
        let junk = normalize("not-a-real-code!!");
        assert_eq!(junk, normalize(&junk));
    }

    #[test]
    fn generate_output_has_exact_grouping_shape() {
        // 8 chars in two groups of 4, joined by exactly ONE hyphen -> display
        // length 9 (`ABCD-EFGH`). Derived from CODE_LEN/GROUP rather than
        // hard-coded, so this test keeps holding if the length moves again.
        //
        // NB the hyphen count is (CODE_LEN - 1) / GROUP, not CODE_LEN / GROUP —
        // they diverge exactly when CODE_LEN is a multiple of GROUP, which the
        // old 26-char format never was, so the previous formula was silently
        // wrong-but-unexercised and would have over-counted by one here.
        let code = generate();
        assert_eq!(
            code.len(),
            CODE_LEN + hyphen_count(),
            "display length: {code}"
        );
        let groups: Vec<&str> = code.split('-').collect();
        assert_eq!(
            groups.len(),
            hyphen_count() + 1,
            "one group per hyphen boundary, plus the last: {groups:?}"
        );
        for g in &groups[..groups.len() - 1] {
            assert_eq!(g.len(), GROUP, "full group must be {GROUP} chars: {g}");
        }
        // The last group holds whatever the full groups didn't. At 8 chars that
        // is a full 4 (`CODE_LEN % GROUP` would say 0 — wrong, and the reason
        // this is derived by subtraction instead).
        let last = groups[groups.len() - 1];
        assert_eq!(
            last.len(),
            CODE_LEN - GROUP * hyphen_count(),
            "last group: {last:?}"
        );
        assert!(!code.starts_with('-') && !code.ends_with('-'));
        assert!(!code.contains("--"));
    }

    const NEST_HEX: &str = "d04ab232742bb4ab3a1368bd4615e4e6d0224ab71a016baf8520a332c9778737";

    #[test]
    fn claim_uri_round_trips_code_and_identity() {
        let code = generate();
        let uri = claim_uri(&code, NEST_HEX);
        let parsed = parse_claim_input(&uri).expect("the printed URI parses");
        assert_eq!(parsed.code, code);
        assert_eq!(parsed.nest_actor_id.as_deref(), Some(NEST_HEX));
    }

    #[test]
    fn a_bare_code_parses_as_itself_with_no_pin() {
        for input in ["K7Q2-M9XJ", "k7q2m9xj", "  K7Q2-M9XJ\n", "aabbcc"] {
            let parsed = parse_claim_input(input).expect("bare codes always parse");
            assert_eq!(parsed.code, input.trim());
            assert_eq!(parsed.nest_actor_id, None, "no URI ⇒ no pin: {input}");
        }
    }

    #[test]
    fn a_uri_without_a_nest_param_parses_unpinned() {
        let parsed = parse_claim_input("fauna://claim?code=K7Q2-M9XJ").expect("parses");
        assert_eq!(parsed.code, "K7Q2-M9XJ");
        assert_eq!(parsed.nest_actor_id, None);
    }

    #[test]
    fn scheme_is_case_insensitive() {
        let parsed = parse_claim_input(&format!("FAUNA://Claim?code=ABCD-EFGH&nest={NEST_HEX}"))
            .expect("parses");
        assert_eq!(parsed.code, "ABCD-EFGH");
        assert_eq!(parsed.nest_actor_id.as_deref(), Some(NEST_HEX));
    }

    /// A malformed pin must fail the whole parse — an input that LOOKS
    /// protected (it carries `nest=`) must never silently continue unpinned.
    #[test]
    fn a_malformed_nest_param_fails_the_whole_parse() {
        for bad in [
            "fauna://claim?code=ABCD-EFGH&nest=nothex",
            "fauna://claim?code=ABCD-EFGH&nest=abcd",
            // 63 hex chars — one short.
            "fauna://claim?code=ABCD-EFGH&nest=d04ab232742bb4ab3a1368bd4615e4e6d0224ab71a016baf8520a332c977873",
        ] {
            assert_eq!(parse_claim_input(bad), None, "must refuse loudly: {bad}");
        }
    }

    #[test]
    fn a_uri_missing_its_code_fails_the_parse() {
        assert_eq!(parse_claim_input("fauna://claim?nest=aabb"), None);
        assert_eq!(
            parse_claim_input(&format!("fauna://claim?nest={NEST_HEX}")),
            None
        );
        assert_eq!(parse_claim_input("fauna://claim?code="), None);
        assert_eq!(parse_claim_input("fauna://claim"), None);
    }

    /// A paste with a multi-byte character where the scheme would end must not
    /// panic (byte-boundary slicing) — it is just a (garbage) bare code.
    #[test]
    fn multibyte_input_never_panics() {
        let parsed = parse_claim_input("fauna://cla\u{00e9}m?code=x").expect("bare-code fallback");
        assert_eq!(parsed.nest_actor_id, None);
        let short = parse_claim_input("fauna://clai\u{00e9}").expect("bare-code fallback");
        assert_eq!(short.nest_actor_id, None);
    }

    // The alphabet's own properties (ambiguity-free, no duplicates, unbiased
    // under `% 32`) moved with the alphabet to
    // `crate::human_code::tests::the_alphabet_is_ambiguity_free_and_unbiased`,
    // which pins them once for every consumer instead of once per consumer.
    // `ambiguous_characters_are_never_emitted` above stays here deliberately:
    // it asserts the property through *this* surface's `generate`, which is
    // what a claim code's reader actually depends on.
}
