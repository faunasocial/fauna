//! Spam disposition policy: the combined-score → routing decision.
//!
//! rspamd is the **sole deployment-wide content scorer** (it runs its own
//! SPF/DKIM/DMARC/RBL + statistical + content rules); the disposition is driven
//! by the combined score `max(rspamd_scaled, weighted_bayesian)` on the 0–15
//! scale (`docs/goal/behavior/mail-spam.md` § Combined-score formula). The
//! per-user Bayesian classifier never runs at the inbound-MX perimeter — that
//! position has no unwrap capability for a per-user secret, so it always
//! passes `weighted_bayesian_milli = 0`, permanently, not as a cold-start gap.
//! The per-user term is scored post-delivery at the authenticated agent
//! instead (`mail-spam.md` § Scoring placement), so at the perimeter the
//! combined score is just rspamd's scaled score.
//!
//! **Permissive auto-Junk default** (user direction 2026-05-24, memory
//! `mail-spam-permissive-default-direction`): the default policy auto-files to
//! Junk above `spam_folder` but does NOT 550-reject on the content score.
//! There are two score tiers, Junk and reject, and no hold
//! (`mail-policy-config.md` § Inbound perimeter). The reject tier is opt-in —
//! an admin enables it by setting a non-zero threshold; `0 = disabled`.
//! Perimeter hard-gates
//! (TLS / SPF / DKIM / DMARC verify / DNSBL reject-class / ClamAV / greylist /
//! rate-limits) are enforced elsewhere and are unaffected by this policy.
//!
//! Pure-functional: the caller (Go MTA) runs rspamd, scales the score, and
//! passes the combined milli-score in. dag-cbor forbids floats, so the score is
//! carried in milli-units of the 0–15 scale (1.2 → 1200).

// The per-user Naive Bayes classifier + confidence-weighting formula. Re-exported
// at the `spam` module root so consumers reach `fauna_mail::spam::SpamModel` etc.
pub mod classifier;
pub use classifier::*;

// The client-side per-user spam-model WRITE orchestrator (unwrap → mutate →
// re-seal) — the tier-1 model-write half of the moderation/ranking frame's first
// sealing slice. Needs the seal/open primitives (`segments-receive`) on top of
// the classifier, so it is gated on that feature in addition to the parent
// `spam-classifier`.
#[cfg(feature = "segments-receive")]
pub mod model_write;

// `decide_spam_disposition` (below) is the only auth-dependent item; it is gated
// on the full `spam` feature, so its `crate::auth` import is too. Everything else
// in this module (the classifier, `SpamDisposition`, `SpamPolicy`,
// `combined_spam_score_milli`) is auth-free and lives under `spam-classifier`.
#[cfg(feature = "spam")]
use crate::auth::{AuthVerdicts, DmarcPolicy, DmarcVerdict};
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "uniffi", derive(uniffi::Enum))]
pub enum SpamDisposition {
    /// Deliver to INBOX.
    Accept,
    /// The combined score reached the Junk tier — file to the recipient's Junk.
    AcceptToSpamFolder,
    /// Filed to the recipient's Junk by a rule, whatever the score: the
    /// sender's own DMARC `p=quarantine` on a failing message, a ClamAV hit
    /// under the `junk` action, or an unlisted-recipient penalty that reached
    /// the reject tier (one recipient of a multi-recipient message cannot be
    /// refused). Kept apart from `AcceptToSpamFolder` so the record says why.
    PolicyJunk,
    /// 554 at the MTA; never ingested.
    Reject,
}

/// Score thresholds passed in by the bridge (projected from nest config at
/// startup). Thresholds are integer **points** on the 0–15 combined-score
/// scale. **`0 = disabled`** for that tier — the tier's action never fires.
///
/// Default is the permissive auto-Junk policy: spam_folder=5 (→ Junk),
/// reject=0 (off). When both tiers are non-zero, `spam_folder < reject` must
/// hold.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "uniffi", derive(uniffi::Record))]
pub struct SpamPolicy {
    /// Combined score (points) >= this → `AcceptToSpamFolder` (Junk). `0` = no
    /// auto-Junk (everything below the reject tier lands in INBOX).
    pub spam_folder_threshold: u32,
    /// Combined score (points) >= this → `Reject` (550 at the MTA). `0` = off
    /// (the default; admin opts in by setting a non-zero value).
    pub reject_threshold: u32,
    /// If true, a DMARC Quarantine-policy fail short-circuits to `PolicyJunk`
    /// regardless of the score — honoring the *sender's* published `p=quarantine`
    /// DMARC policy (the word is the DMARC standard's; the mail lands in Junk).
    ///
    /// DMARC *reject* (`p=reject`) is NOT honored here — it is enforced one
    /// stage earlier as a 550 5.7.1 by the bridge's auth-enforce gate
    /// (`bins/fauna-bridges/internal/mta/auth_enforce.go`), which also
    /// honors `LogOnly`.
    pub honor_dmarc_quarantine: bool,
}

impl Default for SpamPolicy {
    fn default() -> Self {
        Self {
            spam_folder_threshold: 5,
            reject_threshold: 0,
            honor_dmarc_quarantine: true,
        }
    }
}

// ── Scoring + disposition ──────────────────────────────────────────────────────

/// Combine the deployment-wide rspamd score with the per-user Bayesian score.
///
/// `max(rspamd_scaled_milli, weighted_bayesian_milli)`, floored at 0. Both
/// inputs are in milli-units of the 0–15 scale (dag-cbor forbids floats). The
/// `max` captures the "at least one classifier flags it" semantic: a peer
/// claiming a message is legitimate (low rspamd) must not cancel the user's own
/// "I've marked many like this as spam" (high Bayesian), and vice-versa.
///
/// At the inbound-MX perimeter, `weighted_bayesian_milli` is always `0` —
/// permanently, not a cold-start gap — because that position has no unwrap
/// capability for a per-user secret (`mail-spam.md` § Scoring placement). The
/// Bayesian weighting (`bayesian * confidence_factor * bayesian_weight`,
/// `mail-spam.md` § Combined-score formula) is computed by the authenticated
/// agent that scores post-delivery instead; this function only takes the
/// already-weighted value.
#[cfg_attr(feature = "uniffi", uniffi::export)]
pub fn combined_spam_score_milli(rspamd_scaled_milli: i32, weighted_bayesian_milli: i32) -> i32 {
    rspamd_scaled_milli.max(weighted_bayesian_milli).max(0)
}

/// Add the deployment-wide unlisted-recipient penalty to a combined milli-score.
///
/// The recipient-whitelist model treats mail to any address **not** on the
/// user's exact-alias whitelist (a catch-all match — never-registered or
/// dropped) as spam: `penalty_points` (a `SpamPolicyThresholds` admin knob,
/// default 0) is added to the combined score as `points * 1000` before the
/// disposition decision (`mail-spam.md` § Unlisted-recipient penalty). The Go
/// MTA per-recipient loop calls this (via UniFFI) only for a recipient whose
/// delivery carries the `X-Fauna-Address-Catchall` stamp; a listed recipient
/// passes `penalty_points = 0` (or is never called), so its score is unchanged.
///
/// `penalty_points = 0` is a no-op (the opt-in-off default). A large penalty
/// (e.g. `1000`) dominates any tier → guaranteed Junk. Saturating: a pathologic
/// penalty clamps at `i32::MAX` rather than wrapping.
#[cfg_attr(feature = "uniffi", uniffi::export)]
pub fn apply_unlisted_recipient_penalty_milli(combined_milli: i32, penalty_points: u32) -> i32 {
    let sum = i64::from(combined_milli) + i64::from(penalty_points) * 1000;
    sum.clamp(i64::from(i32::MIN), i64::from(i32::MAX)) as i32
}

/// Decide the delivery disposition from the combined spam score.
///
/// A DMARC Quarantine-policy fail short-circuits to `PolicyJunk` when the policy
/// honors it (the sender's published intent). Otherwise the combined score is
/// compared against the policy's enabled tiers (`0 = disabled`), highest first:
/// `reject` → `spam_folder` → else `Accept` (INBOX).
///
/// `combined_score_milli` is in milli-units of the 0–15 scale; thresholds are
/// integer points, so the comparison scales the threshold to milli.
///
/// Gated on the full `spam` feature: it consumes `crate::auth::AuthVerdicts` for
/// the DMARC short-circuit, so it is native-only (the WASM-safe `spam-classifier`
/// surface stops at `combined_spam_score_milli`).
#[cfg(feature = "spam")]
#[cfg_attr(feature = "uniffi", uniffi::export)]
pub fn decide_spam_disposition(
    combined_score_milli: i32,
    verdicts: &AuthVerdicts,
    policy: &SpamPolicy,
) -> SpamDisposition {
    debug_assert!(
        ordered_nonzero(policy),
        "SpamPolicy non-zero tiers must satisfy spam_folder < reject; got spam_folder={}, reject={}",
        policy.spam_folder_threshold,
        policy.reject_threshold,
    );

    // DMARC quarantine short-circuit — honors the sender's p=quarantine policy
    // by filing to Junk (reject is enforced upstream at the auth-enforce gate —
    // see doc).
    if let DmarcVerdict::Fail {
        policy: dmarc_policy,
    } = verdicts.dmarc
        && policy.honor_dmarc_quarantine
        && dmarc_policy == DmarcPolicy::Quarantine
    {
        return SpamDisposition::PolicyJunk;
    }

    // Score-based tiers; `0 = disabled` so a tier only fires when enabled.
    let reached = |threshold: u32| threshold != 0 && combined_score_milli >= milli(threshold);
    if reached(policy.reject_threshold) {
        SpamDisposition::Reject
    } else if reached(policy.spam_folder_threshold) {
        SpamDisposition::AcceptToSpamFolder
    } else {
        SpamDisposition::Accept
    }
}

/// Scale an integer-point threshold to milli-units for comparison.
/// Used only by `decide_spam_disposition`, so gated with it on `spam`.
#[cfg(feature = "spam")]
fn milli(points: u32) -> i32 {
    (points as i32).saturating_mul(1000)
}

/// True when the policy's two tiers are ordered `spam_folder < reject`
/// (a disabled `0` tier is ignored).
/// Used only by `decide_spam_disposition`, so gated with it on `spam`.
#[cfg(feature = "spam")]
fn ordered_nonzero(policy: &SpamPolicy) -> bool {
    policy.spam_folder_threshold == 0
        || policy.reject_threshold == 0
        || policy.spam_folder_threshold < policy.reject_threshold
}
