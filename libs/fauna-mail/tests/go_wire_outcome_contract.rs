//! The Rust half of the **decode-direction** wire-vocabulary contract with the
//! Go mail bridge.
//!
//! # Why a second contract, next to `go_wire_variant_contract`
//!
//! `libs/fauna-core/tests/go_wire_variant_contract.rs` pins the vocabularies the
//! Go bridge **encodes onto the wire** — the mail-auth / content-scan verdicts
//! it translates out of UniFFI types. This file pins the opposite direction: the
//! discriminator strings **nest produces and the bridge switches on**.
//!
//! That direction had nothing. The `reply-*.cbor` round-trip fixtures
//! (`libs/fauna-protocol/examples/regen_go_wsrpc_reply_fixtures.rs`) catch a
//! field RENAME or TYPE change, and structurally cannot catch a VALUE rename:
//! Go decodes `Outcome string` and re-encodes it byte-identically whatever it
//! says. So `"fetch_error"` could become `"fetcherror"` on the Rust side, every
//! Rust test that spells the literal could be updated in the same commit, every
//! fixture would still round-trip — and the Go bridge would silently stop
//! recognising the outcome across a binary boundary no test spans.
//!
//! # What it pins
//!
//! MTA-STS first, because it is the sharpest instance in the corpus:
//!
//! * The outcome is not merely read by the bridge, it is **echoed back** on
//!   `report_tls_attempt` so nest can rebuild the RFC 8460 §4.4 TLSRPT policy
//!   bucket. Producer and consumer are therefore a round trip *through the other
//!   binary*, and before this contract each end hand-wrote its own table.
//! * RFC 8461 §5 makes `fetch_error` / `invalid` load-bearing in the safety
//!   direction: a published-but-broken policy must be treated as no-policy and
//!   must **never** force plaintext. A token the bridge fails to recognise falls
//!   into its `default:` arm, and the correctness of that arm is a property of
//!   the token set, not of the arm.
//!
//! Then the SRS bounce outcome, which is the same class with the weakness on
//! the other side: Go already had a named const family while Rust hand-wrote
//! six literals against a `pub outcome: String`.
//!
//! # The chain
//!
//! 1. This test derives both token lists **live** from the owning enums
//!    ([`MtaStsOutcome::as_wire`], [`MtaStsMode::as_str`]) and compares them
//!    against the committed fixture. Rename a token and this goes red at once,
//!    printing the exact bytes the fixture must contain — there is deliberately
//!    no regen mode, because a test that can bless what it finds is not a check.
//! 2. `bins/fauna-bridges/internal/wsrpc/go_wire_outcome_contract_test.go` reads
//!    that fixture and compares it against the Go const families, in **both**
//!    directions. Once the fixture moves, the Go test stays red until the const
//!    family follows — and every Go production site spells the constant, not the
//!    literal, so the compiler carries the change from there.
//!
//! # Exhaustiveness is compiler-enforced — AND generated from one list, not two
//!
//! The `*_tokens()` functions below call [`variant_set!`], which both builds
//! the instance list and derives the exhaustive `match` from that SAME token
//! list. The sibling `fauna-core` contract had a hand-written `all` array
//! next to a hand-written `match`, which let a minimal compile-fix add a
//! match arm without adding an `all` entry — both tests stayed green on a
//! variant the Go side never learned existed. There is no second, independently-editable
//! structure a variant addition could touch just one half of: adding a
//! variant to `MtaStsOutcome` / `MtaStsMode` / `SrsBounceOutcome` makes
//! `variant_set!`'s generated match non-exhaustive, and the only fix is
//! adding the variant to the macro's list — which is also the list the
//! fixture is built from. A `_ =>` arm anywhere would retire the whole
//! guarantee.

#![cfg(all(feature = "outbound", feature = "srs"))]

use fauna_mail::outbound::mta_sts::{
    FetchedPolicy, MtaStsLookup, MtaStsMode, MtaStsOutcome, MtaStsPolicy,
};
use fauna_mail::srs::{SrsBounceOutcome, SrsError};
use std::collections::BTreeMap;
use std::path::PathBuf;

const FIXTURE: &str = "bins/fauna-bridges/internal/wsrpc/testdata/go-wire-outcomes.json";

/// Declares one enum's variant list ONCE, expanding to both the `(name,
/// instance)` list a fixture is built from and an exhaustive `match` checked
/// against every constructor `$ty` has. The two cannot drift apart because
/// they are not two pieces of code — they are one macro invocation. Adding an
/// enum variant makes the generated match non-exhaustive, and the only lever
/// available to fix that compile error is adding the variant HERE, which
/// necessarily extends the instance list too.
macro_rules! variant_set {
    ($ty:ident; $( $variant:ident $( { $($field:ident : $val:expr),* $(,)? } )? ),+ $(,)?) => {{
        let all: Vec<(&'static str, $ty)> = vec![
            $( (stringify!($variant), $ty::$variant $( { $($field: $val),* } )? ) ),+
        ];
        for (_, v) in &all {
            match v {
                $( $ty::$variant $( { $($field: _),* } )? )|+ => {}
            }
        }
        all
    }};
}

fn a_policy() -> FetchedPolicy {
    FetchedPolicy {
        id: String::new(),
        policy: MtaStsPolicy {
            version: "STSv1".to_string(),
            mode: MtaStsMode::None,
            mx: Vec::new(),
            max_age_secs: 0,
        },
    }
}

/// The four `FetchMtaStsPolicyReply::outcome` discriminators, asked of the enum
/// rather than transcribed.
fn mta_sts_outcome_tokens() -> BTreeMap<String, String> {
    variant_set!(MtaStsOutcome;
        NotPublished,
        FetchError,
        Invalid,
        Found,
    )
    .iter()
    .map(|(ident, v)| (ident.to_string(), v.as_wire().to_string()))
    .collect()
}

/// The three RFC 8461 §3.2 `mode:` tokens, which cross the wire inside
/// `MtaStsPolicyWire::mode` on the same reply.
fn mta_sts_mode_tokens() -> BTreeMap<String, String> {
    variant_set!(MtaStsMode;
        Enforce,
        Testing,
        None,
    )
    .iter()
    .map(|(ident, v)| (ident.to_string(), v.as_str().to_string()))
    .collect()
}

/// The six `DecodeSrsBounceReply::outcome` discriminators.
///
/// Joined 2026-08-23, and the direction of weakness here is the reverse of
/// MTA-STS's: the **Go** side already had the named type (`SrsBounceOutcome`,
/// a const family) while Rust hand-wrote six `"…".into()` literals against a
/// `pub outcome: String`. The weaker side was ours.
///
/// One-way, unlike the MTA-STS outcome — nothing echoes it back, so
/// `SrsBounceOutcome` has no parser and this file asserts token distinctness
/// directly instead of round-tripping. The Go `default:` arm answers an
/// unrecognised outcome with a `451` tempfail, which is the safe direction and
/// also means a drift here would silently tempfail **every** SRS bounce
/// forever rather than fail loudly.
fn srs_bounce_outcome_tokens() -> BTreeMap<String, String> {
    variant_set!(SrsBounceOutcome;
        Ok,
        NotSrs,
        Malformed,
        MacFail,
        Expired,
        Orphan,
    )
    .iter()
    .map(|(ident, v)| (ident.to_string(), v.as_wire().to_string()))
    .collect()
}

fn live_map() -> BTreeMap<String, BTreeMap<String, String>> {
    BTreeMap::from([
        ("MtaStsMode".to_string(), mta_sts_mode_tokens()),
        ("MtaStsOutcome".to_string(), mta_sts_outcome_tokens()),
        ("SrsBounceOutcome".to_string(), srs_bounce_outcome_tokens()),
    ])
}

fn workspace_root() -> PathBuf {
    // CARGO_MANIFEST_DIR = libs/fauna-mail
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .and_then(|p| p.parent())
        .expect("libs/fauna-mail sits two levels under the workspace root")
        .to_path_buf()
}

#[test]
fn the_go_outcome_fixture_matches_the_live_tokens() {
    let path = workspace_root().join(FIXTURE);
    let live = live_map();
    // Trailing newline so the file is a well-formed text file and `git diff`
    // does not report "\ No newline at end of file" on every regen.
    let rendered = format!(
        "{}\n",
        serde_json::to_string_pretty(&live).expect("the token map serializes")
    );

    // Deliberately NO regen env var — see the module doc.
    let committed = std::fs::read_to_string(&path).unwrap_or_default();

    assert_eq!(
        committed.trim_end(),
        rendered.trim_end(),
        "\n\nthe committed Go wire-OUTCOME fixture is STALE (or missing).\n\n\
         The MTA-STS wire vocabularies no longer match {FIXTURE}. This is the FIRST \
         link of the decode-direction Go contract: bring the fixture up to date, then \
         expect internal/wsrpc/go_wire_outcome_contract_test.go to go red until the Go \
         const family follows — that second red is the point of the chain, not a \
         nuisance.\n\n\
         Write EXACTLY this into {FIXTURE}:\n\n{rendered}\n\
         Do not compose it by hand from the enum source: the text above came from the \
         owning enums, and a hand-typed token list is exactly the copy this contract \
         exists to prevent.\n"
    );
}

#[test]
fn every_outcome_token_round_trips_through_its_parser() {
    // The producer/consumer pair is a round trip *through the other binary*
    // (nest emits `outcome`; the bridge echoes it back on `report_tls_attempt`;
    // nest rebuilds the lookup to derive the TLSRPT bucket). Before this
    // contract each end hand-wrote its own table, and nothing asserted the two
    // agreed. Asking the owner for both directions is what makes them one table.
    for (ident, token) in mta_sts_outcome_tokens() {
        let parsed = MtaStsOutcome::from_wire(&token).unwrap_or_else(|| {
            panic!("`{token}` (from {ident}) is emitted by as_wire but not parsed back")
        });
        assert_eq!(
            parsed.as_wire(),
            token,
            "{ident}: from_wire({token:?}) came back as a different outcome"
        );
    }
    assert!(
        MtaStsOutcome::from_wire("not-published").is_none(),
        "from_wire must reject an unknown token rather than guess — nest answers a \
         bad `mta_sts_outcome` on report_tls_attempt with `malformed`, because every \
         candidate guess is a delivery decision and the fail-safe direction differs \
         between the four"
    );
}

#[test]
fn every_srs_decode_failure_has_a_wire_token_of_its_own() {
    // The four decode failures reach the wire through `From<&SrsError>`, so the
    // failure half of the vocabulary is derived from the error type rather than
    // restated. That impl's match is exhaustive with no `_` arm, so a new
    // `SrsError` variant stops `fauna-mail` compiling until someone decides its
    // wire token **at the owner** — rather than at whichever caller notices
    // first, which is how a vocabulary grows a second home.
    let mapped = [
        (SrsError::NotSrs, "not_srs"),
        (SrsError::Malformed, "malformed"),
        (SrsError::MacFail, "mac_fail"),
        (SrsError::Expired, "expired"),
    ];
    let tokens = srs_bounce_outcome_tokens();
    for (err, want) in &mapped {
        let outcome = SrsBounceOutcome::from(err);
        assert_eq!(
            outcome.as_wire(),
            *want,
            "{err:?} maps to the wrong wire token"
        );
        assert!(
            tokens.values().any(|t| t == want),
            "{want:?} is not in the family the fixture pins"
        );
    }
    // The two success-side outcomes are unreachable from an error, and are the
    // caller's to name: only the holder of the outbound table knows whether the
    // forwarding row survived. Asserted so a future `From` impl cannot quietly
    // start inventing one of them.
    let from_errors: Vec<&str> = mapped
        .iter()
        .map(|(e, _)| SrsBounceOutcome::from(e).as_wire())
        .collect();
    for unreachable in ["ok", "orphan"] {
        assert!(
            !from_errors.contains(&unreachable),
            "{unreachable:?} came out of an SrsError — it is a row-liveness verdict, \
             not a decode result, and only the caller can make it"
        );
    }
}

#[test]
fn no_two_variants_of_a_family_share_a_wire_token() {
    // The Go halves switch on the token, so two variants sharing one would
    // collapse into a single arm silently. The MTA-STS outcome gets this for
    // free from its `from_wire` round trip above; the SRS outcome is one-way
    // (nothing echoes it back) and so has no parser to round-trip through —
    // this is the same property, asserted directly, for every family.
    for (family, tokens) in live_map() {
        let mut seen: BTreeMap<String, String> = BTreeMap::new();
        for (variant, token) in tokens {
            if let Some(prior) = seen.insert(token.clone(), variant.clone()) {
                panic!(
                    "{family}: {prior} and {variant} both serialize to {token:?} — the Go \
                     switch cannot tell them apart, and the fixture would still look complete"
                );
            }
        }
    }
}

#[test]
fn every_lookup_variant_names_its_outcome_and_only_found_carries_a_policy() {
    // The discriminator type is separate from `MtaStsLookup` (the wire carries
    // `outcome` and `policy` as two fields), so the mapping between them is its
    // own claim. Both matches below are exhaustive by construction: a new
    // `MtaStsLookup` variant stops this file compiling.
    let cases = [
        (MtaStsLookup::NotPublished, MtaStsOutcome::NotPublished),
        (MtaStsLookup::FetchError, MtaStsOutcome::FetchError),
        (MtaStsLookup::Invalid, MtaStsOutcome::Invalid),
        (MtaStsLookup::Found(a_policy()), MtaStsOutcome::Found),
    ];
    for (lookup, expected) in &cases {
        match lookup {
            MtaStsLookup::NotPublished
            | MtaStsLookup::FetchError
            | MtaStsLookup::Invalid
            | MtaStsLookup::Found(_) => {}
        }
        assert_eq!(
            &lookup.outcome(),
            expected,
            "{lookup:?} named a different outcome"
        );
    }

    for (_, outcome) in &cases {
        assert_eq!(
            MtaStsLookup::from_outcome(*outcome).is_none(),
            outcome.carries_policy(),
            "{outcome:?}: from_outcome returns None for exactly the outcomes that \
             need a policy body from the accompanying wire field"
        );
    }
}

#[test]
fn every_mode_token_round_trips_through_its_parser() {
    for (ident, token) in mta_sts_mode_tokens() {
        let parsed: MtaStsMode = token
            .parse()
            .unwrap_or_else(|_| panic!("`{token}` (from {ident}) is emitted but not parsed back"));
        assert_eq!(
            parsed.as_str(),
            token,
            "{ident}: mode round trip changed it"
        );
    }
}

#[test]
fn the_no_policy_outcomes_are_the_ones_rfc_8461_5_makes_safe() {
    // Documents the rule next to the mechanism, so a future rename cannot quietly
    // move a token out of the set the bridge treats as no-policy. RFC 8461 §5: a
    // published-but-broken policy is treated as no-policy for delivery decisions
    // — it never forces plaintext and never refuses. Exactly one token carries a
    // usable policy; the other three must not.
    let tokens = mta_sts_outcome_tokens();
    assert_eq!(
        tokens.get("Found").map(String::as_str),
        Some("found"),
        "the one outcome that carries a policy body is what the bridge keys \
         enforcement on; renaming it silently disables MTA-STS enforcement"
    );
    for ident in ["NotPublished", "FetchError", "Invalid"] {
        let token = tokens
            .get(ident)
            .unwrap_or_else(|| panic!("{ident} lost its wire token"));
        assert_ne!(
            token, "found",
            "{ident} must stay distinguishable from `found`: RFC 8460 §4.3 reports \
             them as different TLSRPT result-types even though RFC 8461 §5 treats \
             all three as no-policy for delivery"
        );
    }
}
