//! T3.1 — combined-score → disposition (permissive auto-Junk default).
//!
//! rspamd is the sole deployment-wide content scorer; the disposition is driven
//! by the combined score `max(rspamd_scaled, weighted_bayesian)` (cold-start:
//! Bayesian = 0 → combined == rspamd). Default policy is **auto-Junk only**:
//! spam_folder=5 (→ Junk), reject=0 (off). `0 = disabled` for a tier; an admin
//! opts into reject by setting a non-zero value. There is no hold tier.
//! See `docs/goal/behavior/mail-spam.md` § Combined-score formula + § Routing
//! and memory `mail-spam-permissive-default-direction`.

use fauna_mail::auth::{AuthVerdicts, DmarcPolicy, DmarcVerdict};
use fauna_mail::spam::{
    SpamDisposition, SpamPolicy, apply_unlisted_recipient_penalty_milli, combined_spam_score_milli,
    decide_spam_disposition,
};

// ── combined_spam_score_milli ────────────────────────────────────────────────

#[test]
fn combined_is_max_of_rspamd_and_bayesian() {
    // rspamd 3.0 (3000 milli) vs weighted-bayesian 7.5 (7500 milli) → 7500.
    assert_eq!(combined_spam_score_milli(3000, 7500), 7500);
    assert_eq!(combined_spam_score_milli(9000, 1200), 9000);
}

#[test]
fn combined_cold_start_is_just_rspamd() {
    // No per-user model yet → weighted_bayesian = 0 → combined == rspamd.
    assert_eq!(combined_spam_score_milli(4200, 0), 4200);
    // A negative rspamd score (rspamd can emit negatives for ham signals)
    // clamps to 0 at the floor.
    assert_eq!(combined_spam_score_milli(-1500, 0), 0);
}

// ── apply_unlisted_recipient_penalty_milli ───────────────────────────────────

#[test]
fn unlisted_penalty_zero_is_a_noop() {
    // Opt-in-off default: a listed recipient (penalty 0) keeps its score.
    assert_eq!(apply_unlisted_recipient_penalty_milli(3000, 0), 3000);
    assert_eq!(apply_unlisted_recipient_penalty_milli(0, 0), 0);
}

#[test]
fn unlisted_penalty_adds_points_times_1000() {
    // A soft penalty of 3 points tips a borderline 3.0 score to 6.0 (→ Junk
    // under the default spam_folder=5).
    assert_eq!(apply_unlisted_recipient_penalty_milli(3000, 3), 6000);
    // A large penalty dominates any tier → guaranteed Junk.
    assert_eq!(apply_unlisted_recipient_penalty_milli(0, 1000), 1_000_000);
}

#[test]
fn unlisted_penalty_saturates_not_wraps() {
    // A pathologic penalty clamps at i32::MAX rather than overflowing.
    assert_eq!(
        apply_unlisted_recipient_penalty_milli(i32::MAX, u32::MAX),
        i32::MAX
    );
}

// ── decide_spam_disposition (combined score; permissive default) ──────────────

fn clean() -> AuthVerdicts {
    AuthVerdicts::default()
}

#[test]
fn default_policy_auto_junks_but_never_rejects() {
    let p = SpamPolicy::default(); // 5 / 0(off)
    // Below spam_folder → INBOX.
    assert_eq!(
        decide_spam_disposition(4999, &clean(), &p),
        SpamDisposition::Accept
    );
    // At/above spam_folder → Junk.
    assert_eq!(
        decide_spam_disposition(5000, &clean(), &p),
        SpamDisposition::AcceptToSpamFolder
    );
    // A very high score still only reaches Junk — no default reject.
    assert_eq!(
        decide_spam_disposition(30_000, &clean(), &p),
        SpamDisposition::AcceptToSpamFolder
    );
}

#[test]
fn admin_opt_in_reject_threshold_enables_550() {
    // Admin sets reject=15 (non-zero) → high score rejects; mid score still Junk.
    let p = SpamPolicy {
        spam_folder_threshold: 5,
        reject_threshold: 15,
        honor_dmarc_quarantine: true,
    };
    assert_eq!(
        decide_spam_disposition(15_000, &clean(), &p),
        SpamDisposition::Reject
    );
    assert_eq!(
        decide_spam_disposition(14_999, &clean(), &p),
        SpamDisposition::AcceptToSpamFolder
    );
}

#[test]
fn spam_folder_disabled_delivers_everything_to_inbox() {
    // spam_folder=0 (off) AND reject off → pure deliver-all (the
    // admin's "deliver everything to INBOX" opt-out).
    let p = SpamPolicy {
        spam_folder_threshold: 0,
        reject_threshold: 0,
        honor_dmarc_quarantine: false,
    };
    assert_eq!(
        decide_spam_disposition(30_000, &clean(), &p),
        SpamDisposition::Accept
    );
}

#[test]
fn dmarc_quarantine_files_to_junk_when_honored() {
    // The sender's published p=quarantine DMARC policy is honored regardless of
    // the score: the message is filed to the recipient's Junk (the PolicyJunk
    // disposition). This is the sender's intent, distinct from the deployment's
    // score thresholds — and the witness that a DMARC p=quarantine failure
    // lands in Junk, never in a hold.
    let verdicts = AuthVerdicts {
        dmarc: DmarcVerdict::Fail {
            policy: DmarcPolicy::Quarantine,
        },
        ..Default::default()
    };
    assert_eq!(
        decide_spam_disposition(0, &verdicts, &SpamPolicy::default()),
        SpamDisposition::PolicyJunk
    );
}

#[test]
fn dmarc_reject_is_not_a_scorer_short_circuit() {
    // DMARC p=reject enforcement lives at the bridge auth-enforce gate
    // (550 5.7.1, T1.5); the scorer does NOT short-circuit to Reject. A bare
    // DMARC Fail{Reject} with a zero score routes to INBOX here.
    let verdicts = AuthVerdicts {
        dmarc: DmarcVerdict::Fail {
            policy: DmarcPolicy::Reject,
        },
        ..Default::default()
    };
    assert_eq!(
        decide_spam_disposition(0, &verdicts, &SpamPolicy::default()),
        SpamDisposition::Accept
    );
}
