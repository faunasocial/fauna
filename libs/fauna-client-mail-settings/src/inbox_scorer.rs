//! The shared on-device INBOX spam-scoring accumulator — the Rust twin of the
//! Go MDA's `scoreSelectedInbox` (`bins/fauna-bridges/internal/mda/imap/
//! spam_score.go`), for the **Fauna-app** scoring position
//! (`docs/goal/behavior/mail-spam.md` § Scoring placement, § Re-file timing).
//!
//! A pure-web/app user reads mail solely over `fauna.email.inbox.fetch` WS-RPC
//! and never opens IMAP, so the MDA's `SELECT INBOX`-time scoring never runs for
//! them — their INBOX is never per-user-scored. This accumulator closes that
//! gap: driven from each app's receive loop, it scores every drained INBOX
//! message on-device (post-decrypt) with the *same* shared classifier
//! (`fauna_mail::spam`) at the *same* effective threshold + knobs the MDA/nest
//! use, so the score — and the INBOX↔Junk placement — is byte-identical at every
//! scoring position (`mail-spam.md` § Architectural rules, "Scoring placement =
//! search placement"; priority #2/#3).
//!
//! # Shape: score-at-ingest, not a separate scan
//!
//! The MDA runs a *separate* pass (list INBOX → fetch ciphertext → decrypt →
//! score) because its scoring is decoupled from delivery. A Fauna app instead
//! **already decrypts every message at ingest** (the receive loop opens each
//! sealed body to thread it), so this accumulator hooks into that existing
//! decrypt: the loop feeds each just-decrypted message here via [`observe`], and
//! at the end of the drain pass drains the two UID lists via [`take`] and issues
//! **one** `fauna.email.apply_spam_disposition`
//! ([`EmailClient::apply_spam_disposition`](fauna_client_email::EmailClient)) —
//! the `User`-class watermark-then-move the client needs (the MDA's
//! `BridgeMda`-only `store_flags`/`move` are unreachable to a `User` caller).
//!
//! [`observe`]: InboxSpamScorer::observe
//! [`take`]: InboxSpamScorer::take
//!
//! # This is a plain Rust struct, not a UniFFI object
//!
//! Every app's receive loop is Rust — web = wasm `WasmConversationsManager`;
//! all four native apps share `fauna_client_conversations::NestMailInboundSource`
//! (linux directly, apple/windows/android via the `fauna-ffi` `conversations_session`
//! factory), which holds this accumulator across its per-drain-pass
//! `begin_pass`/`fetch`/`end_pass` lifecycle. So the accumulator is consumed in-Rust
//! everywhere and needs no FFI binding. (`observe` takes `&mut self`; the shared
//! native source wraps it in a `Mutex` for the `&self` `InboundMailSource` seam.)

use fauna_mail::aliases::read_spam_threshold_stamp;
use fauna_mail::spam::{
    BayesianKnobs, combined_spam_score_milli, weighted_bayesian_milli_for_model,
};
use fauna_protocol::email::SPAM_SCORED_KEYWORD;

/// Milli-units per integer threshold point on the 0–15 combined-score scale
/// (dag-cbor forbids floats, so the combined score is carried in milli). MUST
/// match the Go MDA's `spamScoreMilliScale` (`spam_score.go`) and the nest's
/// `milli()` (`fauna_mail::spam::mod`), the scale the disposition compares
/// against.
const SPAM_SCORE_MILLI_SCALE: i32 = 1000;

/// Accumulates the on-device scorer's outcome over one receive-loop drain pass:
/// the caller [`observe`](Self::observe)s each just-decrypted INBOX message, then
/// [`take`](Self::take)s the `(scored_uids, junk_uids)` to feed a single
/// `apply_spam_disposition`.
///
/// **Construction contract:** build one only when a *trained* model exists —
/// `MailAccountClient::fetch_spam_model` returned `Some` — with the
/// **admin-effective** `spam_folder` threshold + Bayesian knobs (not the
/// catalog defaults), so the client agrees with the MDA/nest on the Junk line.
/// An untrained actor (fetch returned `None`) MUST get no scorer at all: scoring
/// an empty model would watermark every INBOX message as "scored" without a
/// basis, freezing that mail out of scoring once the actor later trains (the
/// MDA's `len(sealed) == 0 ⇒ return` early-exit). The threshold passed here is
/// only the FALLBACK for a message with no delivery stamp: a stamped message is
/// judged against its own stamp, and a threshold of `0` — either one — means
/// "scored, never re-filed" for that message. There is deliberately no
/// session-wide early exit: under per-message thresholds it would skip exactly
/// the messages a user's own override was set for (the MDA deleted its twin
/// 2026-08-18, `mail-aliases.md` § Spam-threshold override).
pub struct InboxSpamScorer {
    /// The caller's per-user model, already **unwrapped** (the sealed
    /// `fetch_spam_model` blob opened under the receive-loop's decrypt
    /// capability) — the opaque `SpamModel::to_bytes` serde_json the shared
    /// scorer decodes. The raw model never crosses to JS / the nest in the clear.
    model_bytes: Vec<u8>,
    /// The admin-effective Bayesian confidence-ramp + weight knobs (the wasm
    /// mirror of the MDA's `s.bayesianKnobs`), so an admin's override reaches the
    /// on-device score identically.
    knobs: BayesianKnobs,
    /// The admin-effective `spam_folder` tier in integer points (0–15; `0` =
    /// auto-Junk disabled) — the fallback for a message carrying no
    /// `X-Fauna-Spam-Threshold` stamp. A combined score
    /// `>= threshold * SPAM_SCORE_MILLI_SCALE` re-files to Junk.
    spam_folder_threshold: u32,
    /// UIDs scored this pass (watermarked `$FaunaSpamScored`), in observe order.
    scored: Vec<u32>,
    /// The subset of `scored` classified spam (moved INBOX→Junk), in observe order.
    junk: Vec<u32>,
}

impl InboxSpamScorer {
    /// Build an accumulator for one drain pass. See the type's *Construction
    /// contract* — `model_bytes` must be a trained model's unwrapped bytes.
    pub fn new(model_bytes: Vec<u8>, spam_folder_threshold: u32, knobs: BayesianKnobs) -> Self {
        Self {
            model_bytes,
            knobs,
            spam_folder_threshold,
            scored: Vec::new(),
            junk: Vec::new(),
        }
    }

    /// Score one just-decrypted INBOX message.
    ///
    /// - Skips (no-op) any message already carrying the `$FaunaSpamScored`
    ///   watermark in `flags` — the cross-agent coordination channel that keeps
    ///   the Fauna app and a third-party IMAP MUA (whose MDA pass also
    ///   watermarks) from re-scoring each other's messages, and keeps a reloaded
    ///   client from re-scoring mail it already scored last session
    ///   (`mail-spam.md` § Re-file timing).
    /// - Otherwise records `uid` as scored (→ watermarked) and, iff the combined
    ///   score crosses the `spam_folder` threshold, as junk (→ moved to Junk).
    ///
    /// Returns `true` iff this message was classified spam this call (→ it will be
    /// moved to Junk), so the caller keeps it OUT of the client's INBOX thread view
    /// — the client-side twin of the MDA moving spam out of INBOX *before* the
    /// `SELECT` snapshot (`mail-spam.md` § Re-file timing), so the user never sees
    /// on-device-detected spam in their inbox. Returns `false` for a watermark-skip,
    /// a message whose threshold (its stamp, else the session's) is `0`, or a ham
    /// verdict (all kept in INBOX).
    ///
    /// `rfc5322_text` is the full decrypted message (the same text shape the
    /// `\Junk`-train path ships), scored through the shared canonical tokenizer —
    /// identical at train + score, so the per-user score agrees across positions.
    /// The rspamd term is `0` (rspamd already scored + routed at ingest, so
    /// nothing it would flag remains in INBOX); this pass adds only the per-user
    /// Bayesian term, exactly as the MDA's `CombinedSpamScoreMilli(0, weighted)`.
    pub fn observe(&mut self, uid: u32, flags: &[String], rfc5322_text: &str) -> bool {
        if flags.iter().any(|f| f == SPAM_SCORED_KEYWORD) {
            return false;
        }
        // THIS message's spam-folder tier: the delivery stamp nest folded from
        // per-alias > per-account > admin default at RCPT (`mail-aliases.md`
        // § Spam-threshold override), or the session policy when the message
        // carries none — the MDA's `ReadSpamThresholdStamp` rule, so the user's
        // own threshold sorts their mail at every scoring position.
        let threshold = read_spam_threshold_stamp(rfc5322_text.as_bytes())
            .unwrap_or(self.spam_folder_threshold);
        let weighted =
            weighted_bayesian_milli_for_model(&self.model_bytes, rfc5322_text, self.knobs);
        let combined = combined_spam_score_milli(0, weighted);
        self.scored.push(uid);
        // Mirrors the MDA's `reached()`: `threshold != 0 && combined >= milli`. A
        // `0` means auto-Junk is off for this message: still scored (watermarked
        // once, never re-decrypted), never re-filed.
        // Widened: a stamp is any u32, and `u32 * 1000` would wrap an i32.
        let spam_folder_milli = i64::from(threshold) * i64::from(SPAM_SCORE_MILLI_SCALE);
        if threshold != 0 && i64::from(combined) >= spam_folder_milli {
            self.junk.push(uid);
            return true;
        }
        false
    }

    /// True when nothing was scored (nothing to flush) — the caller skips the
    /// `apply_spam_disposition` round-trip.
    pub fn is_empty(&self) -> bool {
        self.scored.is_empty()
    }

    /// Drain the accumulated `(scored_uids, junk_uids)` to feed one
    /// `apply_spam_disposition` (watermark all scored, move the junk subset
    /// INBOX→Junk). `junk_uids ⊆ scored_uids` by construction (every `junk` uid
    /// was first pushed to `scored`), satisfying the handler's subset guard.
    /// Leaves the accumulator empty (reusable for the next pass).
    pub fn take(&mut self) -> (Vec<u32>, Vec<u32>) {
        (
            std::mem::take(&mut self.scored),
            std::mem::take(&mut self.junk),
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use fauna_mail::spam::SpamModel;

    /// A model trained on enough spam samples to clear the cold-start ramp
    /// (`full_confidence_samples = 200`), so the per-user term is un-clamped and
    /// a matching message scores well above the `spam_folder = 5` tier.
    fn spam_trained_model() -> Vec<u8> {
        let mut m = SpamModel::default();
        for _ in 0..250 {
            m.train_spam("cheap pills viagra casino winner claim your prize now");
            m.train_ham("lunch tomorrow at noon the usual place see you there");
        }
        m.to_bytes()
    }

    fn knobs() -> BayesianKnobs {
        BayesianKnobs::default()
    }

    #[test]
    fn spam_message_is_scored_and_junked() {
        let mut s = InboxSpamScorer::new(spam_trained_model(), 5, knobs());
        let verdict = s.observe(
            10,
            &[],
            "Subject: win\r\n\r\ncheap pills viagra casino winner claim your prize now",
        );
        assert!(
            verdict,
            "observe returns true for a spam verdict (kept out of the inbox view)"
        );
        let (scored, junk) = s.take();
        assert_eq!(scored, vec![10], "spam message is watermarked");
        assert_eq!(junk, vec![10], "spam message is moved to Junk");
    }

    #[test]
    fn ham_message_is_scored_but_not_junked() {
        let mut s = InboxSpamScorer::new(spam_trained_model(), 5, knobs());
        let verdict = s.observe(
            11,
            &[],
            "Subject: lunch\r\n\r\nlunch tomorrow at noon the usual place see you there",
        );
        assert!(
            !verdict,
            "observe returns false for a ham verdict (stays in the inbox view)"
        );
        let (scored, junk) = s.take();
        assert_eq!(
            scored,
            vec![11],
            "ham message is still watermarked (scored)"
        );
        assert!(junk.is_empty(), "ham message is NOT moved to Junk");
    }

    #[test]
    fn already_watermarked_message_is_skipped() {
        let mut s = InboxSpamScorer::new(spam_trained_model(), 5, knobs());
        // Even a would-be-spam message is skipped entirely once watermarked —
        // the MDA (or a prior client pass) already scored + filed it.
        let verdict = s.observe(
            12,
            &[SPAM_SCORED_KEYWORD.to_string(), "\\Seen".to_string()],
            "cheap pills viagra casino winner claim your prize now",
        );
        assert!(
            !verdict,
            "a watermarked message is not re-judged spam (stays in the view)"
        );
        let (scored, junk) = s.take();
        assert!(
            scored.is_empty(),
            "watermarked message is neither re-scored"
        );
        assert!(junk.is_empty(), "nor re-moved");
    }

    #[test]
    fn session_threshold_zero_never_files_an_unstamped_message() {
        let mut s = InboxSpamScorer::new(spam_trained_model(), 0, knobs());
        let verdict = s.observe(
            13,
            &[],
            "cheap pills viagra casino winner claim your prize now",
        );
        assert!(
            !verdict,
            "threshold 0 (auto-Junk off) never classifies spam"
        );
        let (scored, junk) = s.take();
        // Scored (watermarked once, never re-decrypted) but never re-filed — the
        // MDA's per-message `0` semantics (spam_score.go).
        assert_eq!(scored, vec![13]);
        assert!(junk.is_empty(), "threshold 0 (auto-Junk off) moves nothing");
    }

    /// The spam body the stamp tests share, with a delivery stamp prepended the
    /// way the MTA prepends it before sealing (`mail-aliases.md` § Spam-threshold
    /// override).
    fn stamped_spam(threshold: u32) -> String {
        format!(
            "X-Fauna-Spam-Threshold: {threshold}\r\nSubject: win\r\n\r\n\
             cheap pills viagra casino winner claim your prize now"
        )
    }

    #[test]
    fn a_stamped_zero_keeps_the_message_out_of_junk_but_still_scores_it() {
        // The user's per-account (or per-alias) override said "auto-Junk off":
        // the session's admin default of 5 must not re-file this one message.
        let mut s = InboxSpamScorer::new(spam_trained_model(), 5, knobs());
        assert!(
            !s.observe(40, &[], &stamped_spam(0)),
            "stamped 0 is never Junk"
        );
        let (scored, junk) = s.take();
        assert_eq!(scored, vec![40], "still watermarked, so never re-decrypted");
        assert!(junk.is_empty());
    }

    #[test]
    fn a_stamped_threshold_above_the_score_outranks_the_session_default() {
        let mut s = InboxSpamScorer::new(spam_trained_model(), 5, knobs());
        // 16 points is past the 0–15 scale: nothing can reach it.
        assert!(!s.observe(41, &[], &stamped_spam(16)));
        let (_, junk) = s.take();
        assert!(
            junk.is_empty(),
            "the stamp, not the session default, decides"
        );
    }

    #[test]
    fn a_stamped_threshold_files_to_junk_when_the_session_default_is_off() {
        // The admin turned auto-Junk off deployment-wide, but this user set their
        // own threshold — their message is still sorted by it (mail-spam.md
        // § Scoring placement; the MDA's per-message rule).
        let mut s = InboxSpamScorer::new(spam_trained_model(), 0, knobs());
        assert!(s.observe(42, &[], &stamped_spam(5)));
        let (scored, junk) = s.take();
        assert_eq!(scored, vec![42]);
        assert_eq!(junk, vec![42]);
    }

    #[test]
    fn junk_is_always_a_subset_of_scored() {
        let mut s = InboxSpamScorer::new(spam_trained_model(), 5, knobs());
        assert!(s.observe(
            20,
            &[],
            "cheap pills viagra casino winner claim your prize now",
        )); // spam
        assert!(!s.observe(
            21,
            &[],
            "lunch tomorrow at noon the usual place see you there",
        )); // ham
        assert!(s.observe(
            22,
            &[],
            "cheap pills viagra casino winner claim your prize now",
        )); // spam
        let (scored, junk) = s.take();
        assert_eq!(scored, vec![20, 21, 22]);
        assert_eq!(junk, vec![20, 22]);
        assert!(
            junk.iter().all(|u| scored.contains(u)),
            "every junk uid was first scored (subset guard holds)"
        );
    }

    #[test]
    fn take_leaves_accumulator_reusable() {
        let mut s = InboxSpamScorer::new(spam_trained_model(), 5, knobs());
        assert!(s.observe(
            30,
            &[],
            "cheap pills viagra casino winner claim your prize now",
        ));
        let _ = s.take();
        assert!(s.is_empty(), "take drained the first pass");
        assert!(!s.observe(
            31,
            &[],
            "lunch tomorrow at noon the usual place see you there",
        ));
        let (scored, junk) = s.take();
        assert_eq!(scored, vec![31]);
        assert!(junk.is_empty());
    }
}
