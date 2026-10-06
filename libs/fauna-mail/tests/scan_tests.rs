//! Unit tests for the perimeter content-scan logic (`fauna_mail::scan`).
//!
//! These exercise the **pure** functions only — the network I/O (dialing clamd,
//! POSTing to rspamd) lives Go-side in the mail bridge, mirroring the spam-gate
//! split (`spam.rs` pure scorer + Go-side DNSBL lookups). See
//! `docs/goal/architecture/content-scoring.md`.

#![cfg(feature = "scan")]

use fauna_mail::scan::{
    ClamavAction, ClamavVerdict, ScanAction, ScanPolicy, clamd_parse_reply, decide_scan_action,
    rspamd_parse_reply,
};

// ── clamd reply parsing ─────────────────────────────────────────────────────

#[test]
fn clamd_clean_reply() {
    // clamd zINSTREAM clean reply is "stream: OK\0".
    assert_eq!(clamd_parse_reply("stream: OK\0"), ClamavVerdict::Clean);
}

#[test]
fn clamd_infected_reply_extracts_signature() {
    let v = clamd_parse_reply("stream: Eicar-Test-Signature FOUND\0");
    assert_eq!(
        v,
        ClamavVerdict::Infected {
            signature: "Eicar-Test-Signature".to_string()
        }
    );
}

#[test]
fn clamd_infected_reply_multiword_signature() {
    let v = clamd_parse_reply("stream: Win.Test.EICAR_HDB-1 FOUND\0");
    assert_eq!(
        v,
        ClamavVerdict::Infected {
            signature: "Win.Test.EICAR_HDB-1".to_string()
        }
    );
}

#[test]
fn clamd_error_reply_is_error_not_clean() {
    // Critical: an ERROR reply must NOT be treated as clean (the legacy
    // clamd.rs treated everything-not-FOUND as clean — a fail-open bug).
    // ERROR → Error verdict → Tempfail downstream (never allow-without-scan).
    let v = clamd_parse_reply("INSTREAM size limit exceeded. ERROR\0");
    match v {
        ClamavVerdict::Error { .. } => {}
        other => panic!("expected Error, got {other:?}"),
    }
}

#[test]
fn clamd_unexpected_reply_is_error() {
    let v = clamd_parse_reply("garbage that is neither ok nor found nor error\0");
    match v {
        ClamavVerdict::Error { .. } => {}
        other => panic!("expected Error for unrecognized reply, got {other:?}"),
    }
}

// ── rspamd response parsing + scaling ───────────────────────────────────────

const RSPAMD_SAMPLE: &str = r#"{
    "score": 2.4,
    "required_score": 15.0,
    "action": "no action",
    "symbols": {
        "BAYES_HAM": {"name": "BAYES_HAM", "score": -2.9},
        "MIME_GOOD": {"name": "MIME_GOOD", "score": -0.1},
        "URIBL_BLACK": {"name": "URIBL_BLACK", "score": 5.4}
    }
}"#;

#[test]
fn rspamd_scales_score_by_half_to_milli() {
    // scaling_per_mille = 500 (the 0.5 default): raw 2.4 → scaled 1.2.
    let s = rspamd_parse_reply(RSPAMD_SAMPLE, 500).expect("valid rspamd json");
    assert_eq!(s.raw_milli, 2400, "raw 2.4 → 2400 milli");
    assert_eq!(s.scaled_milli, 1200, "2.4 * 0.5 = 1.2 → 1200 milli");
}

#[test]
fn rspamd_flagged_rules_sorted_and_complete() {
    let s = rspamd_parse_reply(RSPAMD_SAMPLE, 500).expect("valid rspamd json");
    // All fired symbols, sorted deterministically (JSON map order is unstable).
    assert_eq!(
        s.flagged_rules,
        vec!["BAYES_HAM", "MIME_GOOD", "URIBL_BLACK"]
    );
}

#[test]
fn rspamd_breakdown_per_rule_milli() {
    let s = rspamd_parse_reply(RSPAMD_SAMPLE, 500).expect("valid rspamd json");
    let by_rule: std::collections::HashMap<&str, i32> = s
        .breakdown
        .iter()
        .map(|c| (c.rule.as_str(), c.score_milli))
        .collect();
    assert_eq!(by_rule["BAYES_HAM"], -2900);
    assert_eq!(by_rule["MIME_GOOD"], -100);
    assert_eq!(by_rule["URIBL_BLACK"], 5400);
}

#[test]
fn rspamd_no_symbols_is_empty_not_error() {
    let s = rspamd_parse_reply(r#"{"score": 0.0, "action": "no action"}"#, 500)
        .expect("valid rspamd json without symbols");
    assert_eq!(s.raw_milli, 0);
    assert_eq!(s.scaled_milli, 0);
    assert!(s.flagged_rules.is_empty());
    assert!(s.breakdown.is_empty());
}

#[test]
fn rspamd_invalid_json_is_error() {
    assert!(rspamd_parse_reply("not json at all", 500).is_err());
}

#[test]
fn rspamd_missing_score_is_error() {
    // A 200 with no score field is malformed → error → tempfail (never
    // allow-without-score).
    assert!(rspamd_parse_reply(r#"{"action": "no action"}"#, 500).is_err());
}

// ── decide_scan_action (delivery gate) ──────────────────────────────────────

fn policy(action: ClamavAction) -> ScanPolicy {
    ScanPolicy {
        clamav_enabled: true,
        clamav_action_on_infected: action,
        rspamd_enabled: true,
        rspamd_score_scaling_per_mille: 500,
    }
}

#[test]
fn decide_clean_delivers() {
    assert_eq!(
        decide_scan_action(&ClamavVerdict::Clean, &policy(ClamavAction::Reject)),
        ScanAction::Deliver
    );
}

#[test]
fn decide_oversize_delivers() {
    assert_eq!(
        decide_scan_action(
            &ClamavVerdict::BypassedOversize,
            &policy(ClamavAction::Reject)
        ),
        ScanAction::Deliver
    );
}

#[test]
fn decide_error_tempfails() {
    let a = decide_scan_action(
        &ClamavVerdict::Error {
            detail: "clamd timeout".to_string(),
        },
        &policy(ClamavAction::Reject),
    );
    match a {
        ScanAction::Tempfail { .. } => {}
        other => panic!("expected Tempfail on scanner error, got {other:?}"),
    }
}

#[test]
fn decide_infected_default_rejects() {
    let infected = ClamavVerdict::Infected {
        signature: "Eicar-Test-Signature".to_string(),
    };
    assert_eq!(
        decide_scan_action(&infected, &policy(ClamavAction::Reject)),
        ScanAction::RejectMalware {
            signature: "Eicar-Test-Signature".to_string()
        }
    );
}

#[test]
fn decide_infected_junk_action() {
    let infected = ClamavVerdict::Infected {
        signature: "X".to_string(),
    };
    assert_eq!(
        decide_scan_action(&infected, &policy(ClamavAction::Junk)),
        ScanAction::Junk {
            signature: "X".to_string()
        }
    );
}

#[test]
fn decide_infected_tag_action() {
    let infected = ClamavVerdict::Infected {
        signature: "X".to_string(),
    };
    assert_eq!(
        decide_scan_action(&infected, &policy(ClamavAction::Tag)),
        ScanAction::Tag {
            signature: "X".to_string()
        }
    );
}

#[test]
fn scan_policy_default_is_clamav_reject_rspamd_half() {
    let p = ScanPolicy::default();
    assert!(p.clamav_enabled);
    assert!(p.rspamd_enabled);
    assert_eq!(p.clamav_action_on_infected, ClamavAction::Reject);
    assert_eq!(p.rspamd_score_scaling_per_mille, 500);
}

// ── One definition, not two agreeing copies ─────────────────────────────────
//
// Until 2026-08-18 the scan RESULT types were declared twice: here, in
// `fauna-mail::scan` (the producer), and again in
// `fauna-protocol::bridge_routing` (the wire) — the same duplication row 163
// closed one family over for the auth verdicts. A round-trip test can only
// notice drift after someone writes it; these assertions make the drift
// unrepresentable, because there is now exactly one type and both paths are
// re-exports of it.
//
// This file is the only place that can hold the pin: `fauna-mail` depends on
// `fauna-protocol` (via `kind-registry`, on by default), while
// `fauna-protocol` cannot depend on `fauna-mail` — so only this side sees both
// names. Each function below is an identity function whose argument and return
// types are spelled through the two different paths; it compiles if and only if
// they name the same type. Verified to fail with E0308 "expected …, found …"
// when the re-export is reverted to a second declaration.
//
// ⚠ Unlike the auth family, the two copies here did **not** agree — this one
// had no serde container attributes and no `Default`, protocol's had both but
// none of the UniFFI derives. The unified type in `fauna_core::mail_scan` is a
// strict union, which is only wire-neutral because this side's serde impl was
// dead on the wire (the Go MTA hand-builds the adjacently-tagged shape). The
// JSON pin below is what keeps that ruling honest going forward: it asserts the
// shape the Go mirror is written against.
#[cfg(feature = "kind-registry")]
#[test]
fn the_scan_types_have_exactly_one_definition() {
    fn clamav(v: fauna_mail::scan::ClamavVerdict) -> fauna_protocol::bridge_routing::ClamavVerdict {
        v
    }
    fn contribution(
        v: fauna_mail::scan::RspamdRuleContribution,
    ) -> fauna_protocol::bridge_routing::RspamdRuleContribution {
        v
    }
    fn score(v: fauna_mail::scan::RspamdScore) -> fauna_protocol::bridge_routing::RspamdScore {
        v
    }

    // Exercise them so the pin is a real test, not only a compile-time claim.
    assert_eq!(clamav(ClamavVerdict::Clean), ClamavVerdict::Clean);
    assert_eq!(
        clamav(ClamavVerdict::Infected {
            signature: "Eicar-Test-Signature".into()
        }),
        ClamavVerdict::Infected {
            signature: "Eicar-Test-Signature".into()
        }
    );
    let rule = fauna_mail::scan::RspamdRuleContribution {
        rule: "BAYES_SPAM".into(),
        score_milli: 5_100,
    };
    assert_eq!(contribution(rule.clone()), rule);
    let s = fauna_mail::scan::RspamdScore {
        raw_milli: 7_250,
        scaled_milli: 3_625,
        flagged_rules: vec!["BAYES_SPAM".into()],
        breakdown: vec![rule],
    };
    assert_eq!(score(s.clone()), s);
}

// The wire shape the Go mirror (`internal/wsrpc/methods.go`'s
// `ClamavVerdict{Kind, Data}`) is hand-written against. JSON rather than CBOR so
// the assertion is human-readable; serde emits the same `{"kind":…,"data":…}`
// map shape in either codec.
//
// This is the assertion that would have caught the unification getting the wire
// question WRONG — taking fauna-mail's attribute-free form (serde's default
// externally-tagged `{"Infected":{...}}`) instead of protocol's adjacent tagging.
#[test]
fn clamav_verdict_serializes_adjacently_tagged_snake_case() {
    assert_eq!(
        serde_json::to_string(&ClamavVerdict::Clean).unwrap(),
        r#"{"kind":"clean"}"#
    );
    assert_eq!(
        serde_json::to_string(&ClamavVerdict::BypassedOversize).unwrap(),
        r#"{"kind":"bypassed_oversize"}"#
    );
    assert_eq!(
        serde_json::to_string(&ClamavVerdict::NotScanned).unwrap(),
        r#"{"kind":"not_scanned"}"#
    );
    assert_eq!(
        serde_json::to_string(&ClamavVerdict::Infected {
            signature: "Eicar-Test-Signature".into()
        })
        .unwrap(),
        r#"{"kind":"infected","data":{"signature":"Eicar-Test-Signature"}}"#
    );
    assert_eq!(
        serde_json::to_string(&ClamavVerdict::Error {
            detail: "clamd unreachable".into()
        })
        .unwrap(),
        r#"{"kind":"error","data":{"detail":"clamd unreachable"}}"#
    );
}

// `Default` came from protocol's copy; this side never had it. Nest and the
// fixtures rely on it (`..Default::default()`). Until 2026-09-28 the default
// was `Clean` — so every door that never invokes the scan gate (the submission
// twin, the sender's own Sent copy, a disabled scanner) recorded a
// `message_scan_results` row and a `clamav` bus row claiming a scan that never
// ran. "Clean" is an affirmative claim about the message; the only honest
// value for a verdict nobody computed is `NotScanned`.
#[test]
fn clamav_verdict_defaults_to_not_scanned() {
    assert_eq!(ClamavVerdict::default(), ClamavVerdict::NotScanned);
}
