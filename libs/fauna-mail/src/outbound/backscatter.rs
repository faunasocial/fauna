//! Backscatter suppression — docs/goal/behavior/smtp-server.md
//! § Backscatter suppression.
//!
//! A bounce is **not generated** when any of these held for the original
//! message at inbound time:
//!
//! 1. SPF returned hardfail (sender domain disowned the source IP).
//! 2. DMARC alignment failed and DMARC policy was `reject` or
//!    `quarantine`.
//! 3. The MAIL FROM address was a null sender (`<>`).
//! 4. The original message was forwarded *to* us and the inbound
//!    delivery would have been re-bounced anyway (RFC 7489 §3.1).
//!
//! Defaults: all four suppressors on. Admins can disable individual
//! suppressors via mail-policy-config (`outbound.suppress_ndr_*`), which
//! accepts backscatter responsibility for the deployment.

/// Snapshot of the original-message verdicts captured at inbound DATA
/// accept time. Persisted on the `outbound_mail_queue` row so the
/// suppressors can be evaluated at bounce time without re-running the
/// auth checks.
#[derive(Debug, Clone, Default)]
pub struct InboundVerdictsSnapshot {
    /// RFC 7208 §2.6: pass | fail | softfail | neutral | none |
    /// temperror | permerror.
    pub spf: String,
    /// RFC 7489 §6.7: pass | fail | none.
    pub dmarc: String,
    /// RFC 7489 published policy (`p=`): none | quarantine | reject.
    pub dmarc_policy: String,
}

#[derive(Debug, Clone, Copy)]
pub struct SuppressorToggles {
    pub on_spf_hardfail: bool,
    pub on_dmarc_reject_quarantine: bool,
    pub on_null_sender: bool,
    pub on_forwarded_5xx: bool,
}

impl Default for SuppressorToggles {
    fn default() -> Self {
        Self {
            on_spf_hardfail: true,
            on_dmarc_reject_quarantine: true,
            on_null_sender: true,
            on_forwarded_5xx: true,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SuppressReason {
    SpfHardfail,
    DmarcRejectQuarantine,
    NullSender,
    ForwardedAndExternallyBounced,
}

/// Returns `Some(reason)` when the bounce should be suppressed (and the
/// `outbound_mail_queue` row should land in status `suppressed_backscatter`).
/// Returns `None` when no suppressor fires; the caller falls through to
/// the NDR rate-limit and then DSN generation.
pub fn should_suppress(
    verdicts: &InboundVerdictsSnapshot,
    original_sender: &str,
    is_forwarded: bool,
    final_was_5xx: bool,
    toggles: &SuppressorToggles,
) -> Option<SuppressReason> {
    if toggles.on_null_sender && original_sender.is_empty() {
        return Some(SuppressReason::NullSender);
    }
    if toggles.on_spf_hardfail && verdicts.spf.eq_ignore_ascii_case("fail") {
        return Some(SuppressReason::SpfHardfail);
    }
    if toggles.on_dmarc_reject_quarantine
        && verdicts.dmarc.eq_ignore_ascii_case("fail")
        && (verdicts.dmarc_policy.eq_ignore_ascii_case("reject")
            || verdicts.dmarc_policy.eq_ignore_ascii_case("quarantine"))
    {
        return Some(SuppressReason::DmarcRejectQuarantine);
    }
    if toggles.on_forwarded_5xx && is_forwarded && final_was_5xx {
        return Some(SuppressReason::ForwardedAndExternallyBounced);
    }
    None
}
