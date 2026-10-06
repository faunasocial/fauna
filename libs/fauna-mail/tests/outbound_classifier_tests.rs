//! Permanent-failure classifier — docs/goal/behavior/smtp-server.md
//! § Permanent-failure bounce generation.

#![cfg(feature = "outbound")]

use fauna_mail::outbound::classifier::{BouncePolicy, DefaultBouncePolicy, Verdict, WireResponse};

fn pol() -> DefaultBouncePolicy {
    DefaultBouncePolicy::default()
}

#[test]
fn delivered_on_2xx() {
    let resp = WireResponse {
        code: 250,
        enhanced_status: Some("2.0.0"),
        text: "ok",
    };
    assert!(matches!(pol().classify(&resp), Verdict::Delivered));
}

#[test]
fn tempfail_on_4xx() {
    let resp = WireResponse {
        code: 421,
        enhanced_status: Some("4.7.0"),
        text: "try again later",
    };
    match pol().classify(&resp) {
        Verdict::TempFail { enhanced, .. } => assert_eq!(enhanced.as_deref(), Some("4.7.0")),
        v => panic!("expected TempFail, got {v:?}"),
    }
}

#[test]
fn permfail_on_5xx_with_enhanced() {
    let resp = WireResponse {
        code: 550,
        enhanced_status: Some("5.1.1"),
        text: "no such user",
    };
    match pol().classify(&resp) {
        Verdict::PermFail {
            enhanced,
            last_error,
        } => {
            assert_eq!(enhanced, "5.1.1");
            assert!(last_error.contains("550"));
            assert!(last_error.contains("no such user"));
        }
        v => panic!("expected PermFail, got {v:?}"),
    }
}

#[test]
fn allowlisted_5xx_demotes_to_tempfail() {
    let mut p = DefaultBouncePolicy::default();
    p.treat_5xx_as_transient.push("5.7.1".to_string());
    let resp = WireResponse {
        code: 550,
        enhanced_status: Some("5.7.1"),
        text: "policy says no",
    };
    match p.classify(&resp) {
        Verdict::TempFail { enhanced, .. } => assert_eq!(enhanced.as_deref(), Some("5.7.1")),
        v => panic!("expected allowlisted TempFail, got {v:?}"),
    }
}

#[test]
fn classify_enhanced_permfails_on_5_x_x_string() {
    let v = pol().classify_enhanced(Some("5.1.1"), "550 nope");
    match v {
        Verdict::PermFail { enhanced, .. } => assert_eq!(enhanced, "5.1.1"),
        other => panic!("expected PermFail, got {other:?}"),
    }
}

#[test]
fn classify_enhanced_tempfails_when_no_enhanced() {
    let v = pol().classify_enhanced(None, "connect timeout");
    assert!(matches!(v, Verdict::TempFail { enhanced: None, .. }));
}

#[test]
fn classify_enhanced_demotes_allowlisted_5xx() {
    let mut p = DefaultBouncePolicy::default();
    p.treat_5xx_as_transient.push("5.7.1".to_string());
    let v = p.classify_enhanced(Some("5.7.1"), "550 policy");
    assert!(matches!(
        v,
        Verdict::TempFail {
            enhanced: Some(s), ..
        } if s == "5.7.1"
    ));
}

#[test]
fn missing_enhanced_status_on_5xx_uses_5_0_0() {
    let resp = WireResponse {
        code: 554,
        enhanced_status: None,
        text: "no relay",
    };
    match pol().classify(&resp) {
        Verdict::PermFail { enhanced, .. } => assert_eq!(enhanced, "5.0.0"),
        v => panic!("expected PermFail, got {v:?}"),
    }
}
