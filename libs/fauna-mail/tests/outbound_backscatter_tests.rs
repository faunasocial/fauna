//! Backscatter suppression — docs/goal/behavior/smtp-server.md
//! § Backscatter suppression. Four conditions plus per-deployment
//! toggles (defaults all-on).

#![cfg(feature = "outbound")]

use fauna_mail::outbound::backscatter::{
    InboundVerdictsSnapshot, SuppressReason, SuppressorToggles, should_suppress,
};

fn pass_verdicts() -> InboundVerdictsSnapshot {
    InboundVerdictsSnapshot {
        spf: "pass".into(),
        dmarc: "pass".into(),
        dmarc_policy: "none".into(),
    }
}

#[test]
fn suppresses_on_spf_hardfail() {
    let mut v = pass_verdicts();
    v.spf = "fail".into();
    let r = should_suppress(
        &v,
        "alice@example.com",
        false,
        true,
        &SuppressorToggles::default(),
    );
    assert_eq!(r, Some(SuppressReason::SpfHardfail));
}

#[test]
fn suppresses_on_dmarc_reject() {
    let mut v = pass_verdicts();
    v.dmarc = "fail".into();
    v.dmarc_policy = "reject".into();
    let r = should_suppress(
        &v,
        "alice@example.com",
        false,
        true,
        &SuppressorToggles::default(),
    );
    assert_eq!(r, Some(SuppressReason::DmarcRejectQuarantine));
}

#[test]
fn suppresses_on_dmarc_quarantine() {
    let mut v = pass_verdicts();
    v.dmarc = "fail".into();
    v.dmarc_policy = "quarantine".into();
    let r = should_suppress(
        &v,
        "alice@example.com",
        false,
        true,
        &SuppressorToggles::default(),
    );
    assert_eq!(r, Some(SuppressReason::DmarcRejectQuarantine));
}

#[test]
fn suppresses_on_null_sender() {
    let r = should_suppress(
        &pass_verdicts(),
        "",
        false,
        true,
        &SuppressorToggles::default(),
    );
    assert_eq!(r, Some(SuppressReason::NullSender));
}

#[test]
fn suppresses_forwarded_5xx() {
    let r = should_suppress(
        &pass_verdicts(),
        "alice@example.com",
        true,
        true,
        &SuppressorToggles::default(),
    );
    assert_eq!(r, Some(SuppressReason::ForwardedAndExternallyBounced));
}

#[test]
fn forwarded_but_no_5xx_is_not_suppressed() {
    let r = should_suppress(
        &pass_verdicts(),
        "alice@example.com",
        true,
        false,
        &SuppressorToggles::default(),
    );
    assert_eq!(r, None);
}

#[test]
fn passes_through_when_no_condition_triggers() {
    let r = should_suppress(
        &pass_verdicts(),
        "alice@example.com",
        false,
        true,
        &SuppressorToggles::default(),
    );
    assert_eq!(r, None);
}

#[test]
fn dmarc_fail_with_policy_none_does_not_suppress() {
    // DMARC failed but the published policy is `none` — sender hasn't
    // asked us to reject/quarantine, so we still bounce.
    let mut v = pass_verdicts();
    v.dmarc = "fail".into();
    v.dmarc_policy = "none".into();
    let r = should_suppress(
        &v,
        "alice@example.com",
        false,
        true,
        &SuppressorToggles::default(),
    );
    assert_eq!(r, None);
}

#[test]
fn operator_can_disable_specific_suppressors() {
    // SPF hardfail is normally suppressed — admin can opt out per
    // mail-policy-config.md, accepting backscatter responsibility.
    let mut v = pass_verdicts();
    v.spf = "fail".into();
    let toggles = SuppressorToggles {
        on_spf_hardfail: false,
        ..SuppressorToggles::default()
    };
    assert_eq!(
        should_suppress(&v, "alice@example.com", false, true, &toggles),
        None
    );

    // But DMARC and null-sender still fire.
    v.spf = "pass".into();
    v.dmarc = "fail".into();
    v.dmarc_policy = "reject".into();
    assert_eq!(
        should_suppress(&v, "alice@example.com", false, true, &toggles),
        Some(SuppressReason::DmarcRejectQuarantine)
    );
}
