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

use fauna_core::scoring::ScoreEntry;
use fauna_mail::aliases::read_spam_threshold_stamp;
use fauna_mail::spam::{
    BayesianKnobs, SPAM_SCORE_MILLI_SCALE, bayesian_has_say_for_model, effective_spam_score_milli,
    weighted_bayesian_milli_for_model,
};
use fauna_protocol::email::{NOT_JUNK_KEYWORD, SPAM_SCORED_KEYWORD};

/// Accumulates the on-device scorer's outcome over one receive-loop drain pass:
/// the caller [`observe`](Self::observe)s each just-decrypted INBOX message, then
/// [`take`](Self::take)s the `(scored_uids, junk_uids)` to feed a single
/// `apply_spam_disposition`.
///
/// **Construction contract:** build it with the **admin-effective**
/// `spam_folder` threshold + Bayesian knobs (not the catalog defaults), so the
/// client agrees with the MDA/nest on the Junk line — [`Self::new`] when a
/// model exists (`MailAccountClient::fetch_spam_model` returned `Some`; pass
/// the bytes AFTER the baseline fold), [`Self::report_only`] when it does not.
/// [`Self::new`] decides once whether the model has a **say**
/// ([`bayesian_has_say_for_model`] over the folded model, the knobs and the
/// session `spam_folder` tier — `mail-spam.md` § Scoring placement → *The
/// scorer's say*). A scorer without a say — no model, or one too young to file
/// anything — is report-only: it scores ONLY a copy carrying a community
/// `report:spam` row (`report-sharing.md` § Applying the aggregate) and leaves
/// every other copy un-watermarked: watermarking it on a verdict the ramp
/// decided before the open would freeze that mail out of scoring once the
/// model can judge — the MDA's `scoreSelectedInbox` step 2 (whose say gate
/// waits on the regenerated Go binding — `mail-spam.md` § Implementation
/// status today, item 6). A
/// reported copy is still scored with the model's (sub-line) Bayesian term
/// whenever a model exists. The threshold passed here is
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
    /// `None` for an actor with no model: no Bayesian term, the report term alone.
    model_bytes: Option<Vec<u8>>,
    /// Whether the model has a say (`bayesian_has_say_for_model`, decided once
    /// at construction): without one only a reported copy is scored.
    has_say: bool,
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
    /// contract* — `model_bytes` are the unwrapped model's bytes after the
    /// baseline fold; whether they have a say is decided here.
    pub fn new(model_bytes: Vec<u8>, spam_folder_threshold: u32, knobs: BayesianKnobs) -> Self {
        let has_say = bayesian_has_say_for_model(&model_bytes, knobs, spam_folder_threshold);
        Self {
            model_bytes: Some(model_bytes),
            has_say,
            knobs,
            spam_folder_threshold,
            scored: Vec::new(),
            junk: Vec::new(),
        }
    }

    /// Build an accumulator for an actor with NO trained model: it scores only
    /// the copies carrying a `report:spam` row, by the report term alone (see
    /// the type's *Construction contract*).
    pub fn report_only(spam_folder_threshold: u32) -> Self {
        Self {
            model_bytes: None,
            has_say: false,
            knobs: BayesianKnobs::default(),
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
    /// - Skips, leaving it un-watermarked, a copy with no `report:spam` row when
    ///   this scorer has no say (no model, or one under the say line) —
    ///   nothing that could file it yet.
    /// - Records a copy carrying `$NotJunk` (RFC 5788, the per-message ham veto)
    ///   as scored but never as junk: a message the user moved out of Junk stays
    ///   out whatever the community later reports (`report-sharing.md`
    ///   § Applying the aggregate → *`$NotJunk`*).
    /// - Otherwise records `uid` as scored (→ watermarked) and, iff the
    ///   effective score crosses the message's threshold, as junk (→ moved to
    ///   Junk).
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
    /// nothing it would flag remains in INBOX); this pass adds the per-user
    /// Bayesian term and, on top of the max, the copy's community `report:spam`
    /// term read off `scores` — exactly the MDA's
    /// `EffectiveSpamScoreMilli(0, weighted, report_pm)`.
    pub fn observe(
        &mut self,
        uid: u32,
        flags: &[String],
        scores: &[ScoreEntry],
        rfc5322_text: &str,
    ) -> bool {
        if flags.iter().any(|f| f == SPAM_SCORED_KEYWORD) {
            return false;
        }
        let report_pm = report_spam_pm(scores);
        // No say ⇒ no term that could file this copy: a copy with no community
        // report stays un-watermarked, owed to the sweep at which the say
        // begins (`mail-spam.md` § Scoring placement → *The scorer's say*).
        if !self.has_say && report_pm <= 0 {
            return false;
        }
        // The veto: watermarked (never re-decrypted), never re-filed.
        if flags.iter().any(|f| f == NOT_JUNK_KEYWORD) {
            self.scored.push(uid);
            return false;
        }
        // THIS message's spam-folder tier: the delivery stamp nest folded from
        // per-alias > per-account > admin default at RCPT (`mail-aliases.md`
        // § Spam-threshold override), or the session policy when the message
        // carries none — the MDA's `ReadSpamThresholdStamp` rule, so the user's
        // own threshold sorts their mail at every scoring position.
        let threshold = read_spam_threshold_stamp(rfc5322_text.as_bytes())
            .unwrap_or(self.spam_folder_threshold);
        let weighted = self.model_bytes.as_deref().map_or(0, |model| {
            weighted_bayesian_milli_for_model(model, rfc5322_text, self.knobs)
        });
        let combined = effective_spam_score_milli(0, weighted, report_pm);
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

    /// What this scorer can score — the filter the app's held un-watermarked
    /// set is taken through (`mail-spam.md` § Scoring placement → *The app's
    /// catch-up*): every copy when the model has a say, only a reported one
    /// when not (`Trained` reads "has a say").
    pub fn state(&self) -> SpamScorerState {
        if self.has_say {
            SpamScorerState::Trained
        } else {
            SpamScorerState::ReportOnly
        }
    }

    /// Record a held copy carrying `$NotJunk` that the catch-up took without
    /// fetching it: scored (so this pass's flush watermarks it and it is never
    /// re-decrypted), never junk — the MDA's `vetoedUIDs` in
    /// `scoreSelectedInbox`.
    pub fn record_vetoed(&mut self, uid: u32) {
        self.scored.push(uid);
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

/// The copy's live `report:spam` score — lifted to
/// [`fauna_core::scoring::reports::report_spam_pm`] so the flag-change mapping
/// web and native share reads it too; re-exported here for the scorer's callers.
pub use fauna_core::scoring::reports::report_spam_pm;

/// The catch-up's scorer state and per-pass cap, beside the scorer that
/// reports the one and stays under the other. They live in `fauna-core`
/// because the conversations store's held set filters by them too (see
/// [`fauna_core::scoring::inbox_catch_up`]); `INBOX_SPAM_RESCORE_PER_PASS_CAP`
/// is the twin of the MDA's `spamScorePerPassCap` and stays equal to it.
pub use fauna_core::scoring::inbox_catch_up::{INBOX_SPAM_RESCORE_PER_PASS_CAP, SpamScorerState};

/// What one INBOX copy's post-decrypt step decided: the scorer's junk verdict
/// and whether the copy goes on to the mailed-`REPLY` merge.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct InboxCopyVerdict {
    /// The scorer filed this copy Junk this call (→ kept out of the thread view).
    pub junk: bool,
    /// The copy is handed to `CalDavClient::apply_inbound_reply_from_mail`.
    pub merge_reply: bool,
}

/// The ONE place the "score, then may this copy reach the `REPLY` merge?"
/// decision lives — called by the native `NestMailInboundSource::fetch_page` and
/// the web `WasmConversationsManager::ingest_sealed_inbound`, neither of which
/// keeps its own `if` (priority #2).
///
/// The scorer runs only for a fresh INBOX delivery (`Sent` is never scored nor
/// merged; a re-read must not score outside the pass its scorer was built for),
/// and **always before** the merge decision: a copy the app's own scorer files
/// Junk is never merged (`inbound-scheduling-authority.md` § The mail rail;
/// invitation rule 3, `caldav-server.md` § Server-side auto-schedule). `scorer`
/// `None` (cold start, mail off) is "not junk": the merge's stamp gate still
/// applies downstream.
pub fn judge_inbox_copy(
    scorer: Option<&mut InboxSpamScorer>,
    is_inbox: bool,
    is_fresh_delivery: bool,
    uid: u32,
    flags: &[String],
    scores: &[ScoreEntry],
    rfc5322_text: &str,
) -> InboxCopyVerdict {
    if !(is_inbox && is_fresh_delivery) {
        return InboxCopyVerdict {
            junk: false,
            merge_reply: false,
        };
    }
    let junk = scorer.is_some_and(|s| s.observe(uid, flags, scores, rfc5322_text));
    InboxCopyVerdict {
        junk,
        merge_reply: !junk,
    }
}

/// The held-copy twin of [`judge_inbox_copy`]: an INBOX copy the app already
/// threaded, re-read because the nest re-opened its scoring (an upward
/// `report:spam` transition removed its watermark — `report-sharing.md`
/// § Applying the aggregate). Scored whole, like a fresh delivery; never
/// handed to the `REPLY` merge, which ran (or declined) when it first arrived.
/// Returns whether the scorer filed it Junk (→ the caller takes it out of the
/// view). `scorer` `None` files nothing.
pub fn judge_held_inbox_copy(
    scorer: Option<&mut InboxSpamScorer>,
    uid: u32,
    flags: &[String],
    scores: &[ScoreEntry],
    rfc5322_text: &str,
) -> bool {
    scorer.is_some_and(|s| s.observe(uid, flags, scores, rfc5322_text))
}

#[cfg(test)]
mod tests {
    use super::*;
    use fauna_core::scoring::factor::REPORT_SPAM;
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
            &[],
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
            !s.observe(40, &[], &[], &stamped_spam(0)),
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
        assert!(!s.observe(41, &[], &[], &stamped_spam(16)));
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
        assert!(s.observe(42, &[], &[], &stamped_spam(5)));
        let (scored, junk) = s.take();
        assert_eq!(scored, vec![42]);
        assert_eq!(junk, vec![42]);
    }

    const REPLY_AS_SPAM: &str =
        "Subject: win\r\n\r\ncheap pills viagra casino winner claim your prize now";
    const REPLY_AS_HAM: &str =
        "Subject: lunch\r\n\r\nlunch tomorrow at noon the usual place see you there";

    #[test]
    fn a_junk_verdict_never_reaches_the_reply_merge() {
        let mut s = InboxSpamScorer::new(spam_trained_model(), 5, knobs());
        let v = judge_inbox_copy(Some(&mut s), true, true, 1, &[], &[], REPLY_AS_SPAM);
        assert!(v.junk, "the armed scorer files this copy Junk");
        assert!(!v.merge_reply, "a Junk copy is never merged");
    }

    #[test]
    fn a_clean_fresh_inbox_delivery_is_merged() {
        let mut s = InboxSpamScorer::new(spam_trained_model(), 5, knobs());
        let v = judge_inbox_copy(Some(&mut s), true, true, 2, &[], &[], REPLY_AS_HAM);
        assert!(!v.junk);
        assert!(v.merge_reply);
    }

    #[test]
    fn a_reread_or_the_sent_feed_is_never_merged_nor_scored() {
        let mut s = InboxSpamScorer::new(spam_trained_model(), 5, knobs());
        for (inbox, fresh) in [(true, false), (false, true), (false, false)] {
            let v = judge_inbox_copy(Some(&mut s), inbox, fresh, 3, &[], &[], REPLY_AS_HAM);
            assert!(!v.merge_reply, "inbox={inbox} fresh={fresh}");
            assert!(!v.junk);
        }
        assert!(s.is_empty(), "none of them was scored");
    }

    #[test]
    fn no_scorer_merges_because_the_stamp_gate_still_applies() {
        let v = judge_inbox_copy(None, true, true, 4, &[], &[], REPLY_AS_SPAM);
        assert!(!v.junk);
        assert!(v.merge_reply);
    }

    #[test]
    fn junk_is_always_a_subset_of_scored() {
        let mut s = InboxSpamScorer::new(spam_trained_model(), 5, knobs());
        assert!(s.observe(
            20,
            &[],
            &[],
            "cheap pills viagra casino winner claim your prize now",
        )); // spam
        assert!(!s.observe(
            21,
            &[],
            &[],
            "lunch tomorrow at noon the usual place see you there",
        )); // ham
        assert!(s.observe(
            22,
            &[],
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
            &[],
            "cheap pills viagra casino winner claim your prize now",
        ));
        let _ = s.take();
        assert!(s.is_empty(), "take drained the first pass");
        assert!(!s.observe(
            31,
            &[],
            &[],
            "lunch tomorrow at noon the usual place see you there",
        ));
        let (scored, junk) = s.take();
        assert_eq!(scored, vec![31]);
        assert!(junk.is_empty());
    }

    /// A model whose verdict on [`WEAK_BODY`] is a WEAK spam lean — about 2.4
    /// of the 15 points (60 spam / 190 ham samples of the same tokens, past the
    /// cold-start ramp): under the default line of 5 on its own.
    fn weakly_spammy_model() -> Vec<u8> {
        let mut m = SpamModel::default();
        for i in 0..250 {
            if i < 60 {
                m.train_spam("parcel held fee");
            } else {
                m.train_ham("parcel held fee");
            }
        }
        m.to_bytes()
    }

    const WEAK_BODY: &str = "Subject: parcel\r\n\r\nparcel held fee";

    /// The copy's `report:spam` bus row at `pm` per mille — 200 is the curve's
    /// value at exactly k = 3 reporters (`report-sharing.md` § Applying the
    /// aggregate), i.e. 3 points on the mail scale.
    fn report(pm: i64) -> Vec<ScoreEntry> {
        vec![ScoreEntry {
            factor: REPORT_SPAM.to_string(),
            score: pm,
            tier: fauna_core::scoring::TIER_COMMUNITY,
            scorer_version: 1,
        }]
    }

    #[test]
    fn the_weak_lean_alone_stays_under_the_default_line() {
        let weighted =
            weighted_bayesian_milli_for_model(&weakly_spammy_model(), WEAK_BODY, knobs());
        assert!(
            (1000..5000).contains(&weighted),
            "fixture precondition: a weak lean under 5 points, got {weighted}"
        );
        let mut s = InboxSpamScorer::new(weakly_spammy_model(), 5, knobs());
        assert!(
            !s.observe(50, &[], &[], WEAK_BODY),
            "the lean alone files nothing"
        );
    }

    #[test]
    fn three_reports_plus_a_weak_lean_cross_the_default_line() {
        let mut s = InboxSpamScorer::new(weakly_spammy_model(), 5, knobs());
        assert!(
            s.observe(51, &[], &report(200), WEAK_BODY),
            "k = 3 (3 points) on top of the weak lean crosses 5"
        );
        assert_eq!(s.take(), (vec![51], vec![51]));
    }

    #[test]
    fn three_reports_alone_do_not_cross_the_default_line() {
        // A model that calls the body ham: the Bayesian term is 0, so only the
        // report term's 3 points remain — under the default line of 5.
        let mut s = InboxSpamScorer::new(spam_trained_model(), 5, knobs());
        assert!(!s.observe(52, &[], &report(200), REPLY_AS_HAM));
        let (scored, junk) = s.take();
        assert_eq!(scored, vec![52], "scored, so watermarked");
        assert!(junk.is_empty(), "three reports alone file nothing at 5");
    }

    #[test]
    fn not_junk_is_watermarked_and_never_filed() {
        let mut s = InboxSpamScorer::new(spam_trained_model(), 5, knobs());
        // A message every term calls spam: the model's verdict plus fifty reports.
        assert!(!s.observe(
            53,
            &[NOT_JUNK_KEYWORD.to_string()],
            &report(1000),
            REPLY_AS_SPAM,
        ));
        let (scored, junk) = s.take();
        assert_eq!(scored, vec![53], "watermarked, so never re-decrypted");
        assert!(junk.is_empty(), "the veto keeps it out of Junk");
    }

    #[test]
    fn a_missing_report_entry_scores_as_before() {
        // Another factor's row on the copy is not the report term.
        let other = vec![ScoreEntry {
            factor: "rspamd".to_string(),
            score: 900,
            ..Default::default()
        }];
        for scores in [Vec::new(), other] {
            let mut s = InboxSpamScorer::new(weakly_spammy_model(), 5, knobs());
            assert!(!s.observe(54, &[], &scores, WEAK_BODY));
            let mut s = InboxSpamScorer::new(spam_trained_model(), 5, knobs());
            assert!(s.observe(55, &[], &scores, REPLY_AS_SPAM));
        }
    }

    #[test]
    fn a_reopened_held_copy_is_scored_again_whole() {
        // The copy was scored ham at first delivery; the nest removed its
        // watermark at k = 3, so its flags no longer carry it.
        let mut s = InboxSpamScorer::new(weakly_spammy_model(), 5, knobs());
        assert!(judge_held_inbox_copy(
            Some(&mut s),
            70,
            &["\\Seen".to_string()],
            &report(200),
            WEAK_BODY
        ));
        assert!(!judge_held_inbox_copy(
            None,
            71,
            &[],
            &report(1000),
            REPLY_AS_SPAM
        ));
        assert_eq!(s.take(), (vec![70], vec![70]));
    }

    #[test]
    fn a_scorer_reports_its_own_state() {
        assert_eq!(
            InboxSpamScorer::new(spam_trained_model(), 5, knobs()).state(),
            SpamScorerState::Trained
        );
        assert_eq!(
            InboxSpamScorer::report_only(5).state(),
            SpamScorerState::ReportOnly
        );
    }

    /// A model of `n` samples (balanced) that calls [`REPLY_AS_SPAM`] spam.
    fn model_with_samples(n: u32) -> Vec<u8> {
        let mut m = SpamModel::default();
        for i in 0..n {
            if i % 2 == 0 {
                m.train_spam("cheap pills viagra casino winner claim your prize now");
            } else {
                m.train_ham("lunch tomorrow at noon the usual place see you there");
            }
        }
        m.to_bytes()
    }

    #[test]
    fn a_model_without_a_say_leaves_an_unreported_copy_unwatermarked() {
        // One lesson: the model exists, but its term could never file anything.
        let mut s = InboxSpamScorer::new(model_with_samples(1), 5, knobs());
        assert_eq!(s.state(), SpamScorerState::ReportOnly);
        assert!(!s.observe(90, &[], &[], REPLY_AS_SPAM));
        assert!(s.is_empty(), "owed to the sweep at which the say begins");
        // A reported copy is still scored — the report term, plus the model's
        // (zero, at one sample) Bayesian fraction.
        assert!(!s.observe(91, &[], &report(200), REPLY_AS_SPAM));
        assert_eq!(s.take(), (vec![91], vec![]));
        // Just under the line at the default tier: still no say.
        assert_eq!(
            InboxSpamScorer::new(model_with_samples(121), 5, knobs()).state(),
            SpamScorerState::ReportOnly
        );
    }

    #[test]
    fn a_model_with_a_say_scores_an_unreported_copy() {
        let mut s = InboxSpamScorer::new(model_with_samples(122), 5, knobs());
        assert_eq!(s.state(), SpamScorerState::Trained);
        assert!(
            s.observe(92, &[], &[], REPLY_AS_SPAM),
            "filed at 122 samples"
        );
        assert_eq!(s.take(), (vec![92], vec![92]));
    }

    #[test]
    fn the_say_reads_the_session_tier() {
        // Tier off: the ramp floor alone — 51 samples has a say, 50 has none.
        assert_eq!(
            InboxSpamScorer::new(model_with_samples(51), 0, knobs()).state(),
            SpamScorerState::Trained
        );
        assert_eq!(
            InboxSpamScorer::new(model_with_samples(50), 0, knobs()).state(),
            SpamScorerState::ReportOnly
        );
        // A tier past the term's 10.5-point ceiling: report-only for good.
        assert_eq!(
            InboxSpamScorer::new(spam_trained_model(), 11, knobs()).state(),
            SpamScorerState::ReportOnly
        );
    }

    #[test]
    fn a_vetoed_held_copy_is_watermarked_and_never_filed() {
        // The catch-up takes a `$NotJunk` copy without fetching it: recorded
        // scored (so the flush watermarks it), never junk — the MDA's
        // `vetoedUIDs`.
        let mut s = InboxSpamScorer::report_only(5);
        s.record_vetoed(80);
        assert!(!s.is_empty());
        assert_eq!(s.take(), (vec![80], vec![]));
    }

    #[test]
    fn the_catch_up_cap_is_the_mdas() {
        assert_eq!(INBOX_SPAM_RESCORE_PER_PASS_CAP, 100);
    }

    #[test]
    fn an_untrained_actor_scores_a_reported_copy_by_the_report_term_alone() {
        let mut s = InboxSpamScorer::report_only(5);
        // No report: nothing to score, left un-watermarked for a later model.
        assert!(!s.observe(60, &[], &[], &stamped_spam(3)));
        // At the copy's own line of 3, k = 3 is exactly 3 points: filed.
        assert!(s.observe(61, &[], &report(200), &stamped_spam(3)));
        // At the default 5 the same report files nothing, but the copy is scored.
        assert!(!s.observe(62, &[], &report(200), REPLY_AS_SPAM));
        let (scored, junk) = s.take();
        assert_eq!(
            scored,
            vec![61, 62],
            "an unreported copy stays un-watermarked"
        );
        assert_eq!(junk, vec![61]);
    }
}
