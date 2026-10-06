//! The Rust half of the Go wire-mirror variant-name contract with the Go MTA.
//!
//! Covers two families, because they are the same mechanism with the same gap:
//! the **mail-auth verdicts** ([`fauna_core::mail_auth`], joined 2026-08-17) and
//! the **content-scan results** ([`fauna_core::mail_scan`], joined 2026-08-18).
//!
//! # What this guards
//!
//! `bins/fauna-bridges/internal/wsrpc/methods.go` hand-writes a **third** copy of
//! each verdict shape — a per-verdict `{Kind string; Data *…}` struct — and
//! `internal/mailfauna/mailfauna.go`'s `*ToWire` switches translate the UniFFI
//! tagged-union Go types onto it. That copy is a legitimate **language-boundary
//! encoder**, not drift: UniFFI's Go enums do not serialize to the serde
//! adjacently-tagged map nest decodes, so something has to translate by hand.
//!
//! What was pinned by nothing is the **variant strings** those switches emit.
//! `wsrpc_conformance_test.go`'s list covers the mirror structs' `cbor:` FIELD
//! tags only. So a variant added on the Rust side compiled everywhere, the Go
//! switch simply had no arm for it, and the failure surfaced as a wrong wire
//! VALUE rather than a red test — falling through to a `default:` arm that, in
//! both families, had been written for wire-VALIDITY without anyone asking which
//! direction it failed in. Auth fell back to `"none"` (the most permissive point
//! of an authentication lattice); scan fell back to `"clean"`, which is worse
//! still — an affirmative claim that no malware signature matched.
//!
//! # The chain, and why it has no stale-fixture hole
//!
//! 1. This test derives each enum's serde names **live**, from serde itself, and
//!    compares them against the committed fixture. Add a Rust variant and this
//!    test goes red immediately — the fixture is stale — and the failure prints
//!    the exact bytes the fixture must contain, so nobody has to *remember* a
//!    regen command (there deliberately is none: a test with a write mode is a
//!    test that can be made to bless whatever it finds).
//! 2. `internal/wsrpc/go_wire_variant_contract_test.go` reads that fixture and
//!    parses the ToWire switches' AST. Once the fixture is regenerated, the Go
//!    test goes red until the switch grows the matching arm.
//!
//! So a Rust-side variant addition reds link 1, then link 2, and cannot reach the
//! wire un-translated. Neither link is a hand-typed name list: link 1 asks serde,
//! link 2 asks the Go AST.
//!
//! # Exhaustiveness is compiler-enforced — AND generated from one list, not two
//!
//! Each `*_variants()` below calls [`variant_set!`], which both builds the
//! instance list and derives the exhaustive `match` from that SAME token
//! list. An earlier hand-written `all` array next to a hand-written `match`
//! let a minimal compile-fix add a match arm without adding an `all` entry —
//! both tests stayed green on an enum variant the Go side never learned
//! existed. There is no second,
//! independently-editable structure a variant addition could touch just one
//! half of: adding a variant to `fauna_core::{mail_auth,mail_scan}` makes
//! `variant_set!`'s generated match non-exhaustive, and the only fix is
//! adding the variant to the macro's list — which is also the list the
//! fixture is built from. A `_ =>` arm anywhere would retire the whole
//! guarantee — never add one.

use fauna_core::mail_auth::{ArcVerdict, DkimVerdict, DmarcPolicy, DmarcVerdict, SpfVerdict};
use fauna_core::mail_scan::ClamavVerdict;
use serde::Serialize;
use serde_json::Value;
use std::collections::BTreeMap;
use std::path::PathBuf;

const FIXTURE: &str = "bins/fauna-bridges/internal/wsrpc/testdata/go-wire-variants.json";

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

/// The serde name a value actually serializes to.
///
/// Asked of serde rather than transcribed, so a `rename_all` change or a
/// per-variant `#[serde(rename = …)]` is picked up for free. The four verdict
/// enums are adjacently tagged (`{"kind": …}`); `DmarcPolicy` is a bare string.
fn serde_name<T: Serialize>(v: &T) -> String {
    match serde_json::to_value(v).expect("mail-auth verdicts serialize") {
        Value::Object(map) => match map.get("kind") {
            Some(Value::String(s)) => s.clone(),
            other => panic!(
                "adjacently-tagged verdict lost its `kind` string (got {other:?}) — the \
                 `#[serde(tag = \"kind\", content = \"data\")]` attribute is what the Go \
                 mirror's `Kind string` field models; changing it is a wire break"
            ),
        },
        Value::String(s) => s,
        other => panic!("unexpected serde shape for a mail-auth verdict: {other:?}"),
    }
}

fn dkim_variants() -> BTreeMap<String, String> {
    variant_set!(DkimVerdict;
        None,
        Pass,
        Fail { reason: String::new() },
        Neutral,
        PermError,
        TempError,
    )
    .iter()
    .map(|(ident, v)| (ident.to_string(), serde_name(v)))
    .collect()
}

fn spf_variants() -> BTreeMap<String, String> {
    variant_set!(SpfVerdict;
        None,
        Pass,
        Fail,
        SoftFail,
        Neutral,
        PermError,
        TempError,
    )
    .iter()
    .map(|(ident, v)| (ident.to_string(), serde_name(v)))
    .collect()
}

fn dmarc_policy_variants() -> BTreeMap<String, String> {
    variant_set!(DmarcPolicy;
        None,
        Quarantine,
        Reject,
    )
    .iter()
    .map(|(ident, v)| (ident.to_string(), serde_name(v)))
    .collect()
}

fn dmarc_variants() -> BTreeMap<String, String> {
    variant_set!(DmarcVerdict;
        None,
        Pass,
        Fail { policy: DmarcPolicy::None },
        PermError,
        TempError,
    )
    .iter()
    .map(|(ident, v)| (ident.to_string(), serde_name(v)))
    .collect()
}

fn arc_variants() -> BTreeMap<String, String> {
    variant_set!(ArcVerdict;
        None,
        Pass,
        Fail,
        PermError,
        TempError,
    )
    .iter()
    .map(|(ident, v)| (ident.to_string(), serde_name(v)))
    .collect()
}

fn clamav_variants() -> BTreeMap<String, String> {
    variant_set!(ClamavVerdict;
        Clean,
        Infected { signature: String::new() },
        Error { detail: String::new() },
        BypassedOversize,
        NotScanned,
    )
    .iter()
    .map(|(ident, v)| (ident.to_string(), serde_name(v)))
    .collect()
}

fn live_map() -> BTreeMap<String, BTreeMap<String, String>> {
    BTreeMap::from([
        ("ArcVerdict".to_string(), arc_variants()),
        // The scan family joined this contract 2026-08-18 with row 172, which
        // moved these types into `fauna_core::mail_scan` and ruled
        // `ClamavVerdictToWire`'s default arm off `clean` — a ruling that needs
        // a pin, or the next session reverts it silently.
        ("ClamavVerdict".to_string(), clamav_variants()),
        ("DkimVerdict".to_string(), dkim_variants()),
        ("DmarcPolicy".to_string(), dmarc_policy_variants()),
        ("DmarcVerdict".to_string(), dmarc_variants()),
        ("SpfVerdict".to_string(), spf_variants()),
    ])
}

fn workspace_root() -> PathBuf {
    // CARGO_MANIFEST_DIR = libs/fauna-core
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .and_then(|p| p.parent())
        .expect("libs/fauna-core sits two levels under the workspace root")
        .to_path_buf()
}

#[test]
fn the_go_variant_fixture_matches_the_live_serde_names() {
    let path = workspace_root().join(FIXTURE);
    let live = live_map();
    // Trailing newline so the file is a well-formed text file and `git diff`
    // does not report "\ No newline at end of file" on every regen.
    let rendered = format!(
        "{}\n",
        serde_json::to_string_pretty(&live).expect("the variant map serializes")
    );

    // Deliberately NO regen env var. The failure below prints the exact bytes the
    // fixture must contain, so bringing it up to date is a copy — there is no
    // second command to remember, and no mode in which this test silently
    // rewrites the file it is supposed to be checking.
    let committed = std::fs::read_to_string(&path).unwrap_or_default();

    assert_eq!(
        committed.trim_end(),
        rendered.trim_end(),
        "\n\nthe committed Go wire-variant fixture is STALE (or missing).\n\n\
         `fauna_core::{{mail_auth,mail_scan}}`'s serde names no longer match {FIXTURE}. \
         This is the FIRST link of the Go contract: bring the fixture up to date, then \
         expect internal/wsrpc/go_wire_variant_contract_test.go to go red until \
         mailfauna.go's ToWire switch grows a matching arm — that second red is the \
         point of the whole chain, not a nuisance.\n\n\
         Write EXACTLY this into {FIXTURE}:\n\n{rendered}\n\
         Do not compose it by hand from the enum source: the text above came from \
         serde, and a hand-typed name list is the fourth copy this contract exists to \
         prevent.\n"
    );
}

#[test]
fn every_verdict_carries_a_fail_safe_error_variant() {
    // The Go switches' `default:` arm needs somewhere safe to land an unmappable
    // variant, and `internal/wsrpc/go_wire_variant_contract_test.go` pins that
    // arm to each enum's error member. That pin is only meaningful while the
    // error member exists — if one ever loses it, the Go default silently has to
    // become something else, so fail here rather than there.
    //
    // DmarcPolicy is deliberately absent: RFC 7489 fixes its set at exactly
    // none/quarantine/reject and there is no error member to defer to. Its Go
    // default is ruled separately, at the switch.
    let live = live_map();
    for verdict in ["ArcVerdict", "DkimVerdict", "DmarcVerdict", "SpfVerdict"] {
        let names = &live[verdict];
        assert_eq!(
            names.get("TempError").map(String::as_str),
            Some("temp_error"),
            "{verdict} lost its TempError variant (or renamed it) — the Go ToWire \
             switch's fail-safe default arm has nowhere safe to land"
        );
    }

    // The scan family's error member is spelled `Error`, not `TempError` — same
    // role, different vocabulary (clamd either answered or it did not; there is
    // no perm/temp split). `fauna_mail::scan`'s own doc calls it out: an Error
    // "must NOT be treated as clean".
    assert_eq!(
        live["ClamavVerdict"].get("Error").map(String::as_str),
        Some("error"),
        "ClamavVerdict lost its Error variant — ClamavVerdictToWire's default arm \
         has nowhere safe to land, and the only alternative is the permissive `clean`"
    );
}

#[test]
fn the_none_variant_is_the_permissive_end_and_is_not_the_go_default() {
    // Documents the asymmetry the 2026-08-17 review measured, so the reasoning
    // survives next to the mechanism: on an authentication verdict `none` means
    // "no policy published / no determination made" — the MOST permissive point
    // of the lattice. It is the right value when it is TRUE, and the wrong value
    // to invent when Go simply could not map a variant. `temp_error` says
    // "could not determine", which is what is actually the case.
    let live = live_map();
    for verdict in ["ArcVerdict", "DkimVerdict", "DmarcVerdict", "SpfVerdict"] {
        assert_eq!(
            live[verdict].get("None").map(String::as_str),
            Some("none"),
            "{verdict}'s None variant must serialize as `none` — nest flattens it to \
             the RFC-canonical string and records it"
        );
    }

    // The scan family's permissive end is `clean` — and it is MORE dangerous to
    // invent than `none` is, because "no signature matched" is an affirmative
    // claim about malware rather than an absence of policy. Row 172 moved
    // ClamavVerdictToWire's default arm off it for exactly that reason.
    assert_eq!(
        live["ClamavVerdict"].get("Clean").map(String::as_str),
        Some("clean"),
        "ClamavVerdict's Clean variant must serialize as `clean` — nest records it, \
         and the Go default arm is ruled AWAY from it"
    );
}
