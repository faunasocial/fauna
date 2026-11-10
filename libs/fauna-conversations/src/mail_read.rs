//! The mail rail's read-state sync — what the app owes the nest's `\Seen`
//! flags and where it is in hearing about theirs
//! (`docs/goal/behavior/conversation-read-state.md` § Mail: `\Seen` is the
//! marker; the wire is `mail-app-surface.md` § Read state).
//!
//! Plain state, no I/O: the manager holds one ([`crate::ConversationsManager`]),
//! the read chokepoint queues into it, and the receive loop's mail sweep
//! drives the two calls through the `INBOX` source
//! ([`crate::backends::smtp::sync_mail_read_state`]).
//!
//! **Why the pending set is in memory, not the persisted outbox.**
//! `fauna.email.inbox.mark_seen` is classed `OfflineSafe`
//! (`fauna_protocol::offline_class`) and the outbox holds `OfflineQueued`
//! intents only (`account-offline-mutation.md`), so a pending flag write has
//! no durable queue to ride and this one is kept for the run. A run that ends
//! first loses it and those messages are unread at the next launch —
//! under-durable, never wrong-direction.

use std::collections::{BTreeMap, BTreeSet};

use crate::backend::{MailSpamFacts, SpamScorerState};

/// The most held copies one catch-up take hands out — the MDA's per-pass cap
/// (`fauna_core::scoring::inbox_catch_up::INBOX_SPAM_RESCORE_PER_PASS_CAP`).
pub use fauna_core::scoring::inbox_catch_up::INBOX_SPAM_RESCORE_PER_PASS_CAP;

/// What the held un-watermarked set keeps of one copy: the two facts the take
/// filters on.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct UnscoredCopy {
    /// The copy's live `report:spam` score (per mille); `0` without a row.
    pub report_spam_pm: i64,
    /// The copy carries `$NotJunk`: taken [`RescoreCandidate::vetoed`].
    pub not_junk: bool,
}

/// One held copy a catch-up pass owes a score.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct RescoreCandidate {
    pub uid: u32,
    /// The copy carries `$NotJunk`: the pass records it scored and never
    /// junk without fetching it (`InboxSpamScorer::record_vetoed`), so the
    /// flush watermarks it — the MDA's `vetoedUIDs`.
    pub vetoed: bool,
}

#[derive(Debug, Default)]
pub struct MailReadSync {
    /// `INBOX` UIDs the user has read that the nest has not yet been told of.
    pending: BTreeSet<u32>,
    /// Where the next `flag_changes` call resumes, `(since_modseq, after_uid)`;
    /// `None` until the launch drain's first `INBOX` page names a baseline.
    cursor: Option<(u64, u32)>,
    /// The source does not serve the kinds (anything but `INBOX`): nothing
    /// more is sent or asked this run. Never a nest's answer — every nest
    /// serves them, and a nest refusal is a retried failure.
    unsupported: bool,
    /// The held un-watermarked set (`mail-spam.md` § Scoring placement → *The
    /// app's catch-up*): every `INBOX` copy this run holds that lacks the
    /// scorer's `$FaunaSpamScored`, with what its latest metadata read said of
    /// the veto and the report row. Fed by the ingest, the flag feed and the
    /// app's own disposition, it asks the nest nothing; every mail sweep takes
    /// from it what the pass's scorer can score
    /// ([`crate::backends::smtp::rescore_held_inbox_copies`]).
    unscored: BTreeMap<u32, UnscoredCopy>,
}

impl MailReadSync {
    /// Owe the nest `\Seen` on `uids`. A no-op once the nest is known not to
    /// serve the write: the read then lives in memory alone.
    pub fn queue(&mut self, uids: impl IntoIterator<Item = u32>) {
        if !self.unsupported {
            self.pending.extend(uids);
        }
    }

    /// Everything owed, ascending, for one batched write. The caller hands
    /// back what it could not send ([`Self::requeue`]).
    pub fn take_pending(&mut self) -> Vec<u32> {
        std::mem::take(&mut self.pending).into_iter().collect()
    }

    /// Put back a batch whose write failed transiently.
    pub fn requeue(&mut self, uids: Vec<u32>) {
        self.queue(uids);
    }

    pub fn has_pending(&self) -> bool {
        !self.pending.is_empty()
    }

    /// Whether `uid` is read here and still owed to the nest — a delivered
    /// "lacks `\Seen`" for it predates the read and must not re-enter it.
    pub fn is_pending(&self, uid: u32) -> bool {
        self.pending.contains(&uid)
    }

    /// The first `INBOX` page of the launch drain names the mailbox's
    /// `highest_modseq`; only the first counts, so a change racing the drain
    /// is delivered again rather than missed. A later offer is ignored.
    pub fn offer_baseline(&mut self, highest_modseq: u64) {
        if self.cursor.is_none() && !self.unsupported {
            self.cursor = Some((highest_modseq, 0));
        }
    }

    pub fn cursor(&self) -> Option<(u64, u32)> {
        if self.unsupported { None } else { self.cursor }
    }

    /// Where one delivered `flag_changes` page leaves the cursor: mid-write
    /// at its last change while `more`, else at the reply's
    /// `(highest_modseq, 0)`.
    pub fn advance(&mut self, next: (u64, u32)) {
        if !self.unsupported {
            self.cursor = Some(next);
        }
    }

    /// The source does not serve the kinds (non-`INBOX`, the trait default): stop, drop what is owed, show nothing.
    pub fn mark_unsupported(&mut self) {
        self.unsupported = true;
        self.pending.clear();
        self.cursor = None;
        self.unscored.clear();
    }

    /// What one metadata read — the ingest's `flags`/`scores`, or a delivered
    /// flag change — said of a held `INBOX` copy: without the watermark it is
    /// in the set with these facts, with it it leaves.
    pub fn observe_spam_facts(&mut self, uid: u32, facts: MailSpamFacts) {
        if self.unsupported {
            return;
        }
        if facts.scored {
            self.unscored.remove(&uid);
        } else {
            self.unscored.insert(
                uid,
                UnscoredCopy {
                    report_spam_pm: facts.report_spam_pm,
                    not_junk: facts.not_junk,
                },
            );
        }
    }

    /// The app's own flush watermarked `uids`: they leave the set.
    pub fn note_scored(&mut self, uids: impl IntoIterator<Item = u32>) {
        for uid in uids {
            self.unscored.remove(&uid);
        }
    }

    /// A copy the set must forget: it left the view, or the mailbox no
    /// longer holds it.
    pub fn forget_unscored(&mut self, uid: u32) {
        self.unscored.remove(&uid);
    }

    /// The source cannot re-score held copies: nothing in the set is ever
    /// owed a pass this run.
    pub fn drop_unscored(&mut self) {
        self.unscored.clear();
    }

    /// What `state`'s pass can score from the set, newest (highest UID) first,
    /// at most [`INBOX_SPAM_RESCORE_PER_PASS_CAP`]: nothing for
    /// [`SpamScorerState::Absent`], the reported copies for
    /// [`SpamScorerState::ReportOnly`], every copy for
    /// [`SpamScorerState::Trained`]. A copy `is_held` denies (it left the view)
    /// is forgotten on the way. The take leaves the set as it is — the set keeps
    /// the facts, and a copy leaves it only when watermarked
    /// ([`Self::note_scored`], or the feed) or gone ([`Self::forget_unscored`]),
    /// so whatever a pass could not score is owed the next.
    pub fn take_rescore(
        &mut self,
        state: SpamScorerState,
        is_held: impl Fn(u32) -> bool,
    ) -> Vec<RescoreCandidate> {
        if state == SpamScorerState::Absent {
            return Vec::new();
        }
        let mut gone = Vec::new();
        let mut taken = Vec::new();
        for (&uid, copy) in self.unscored.iter().rev() {
            if taken.len() == INBOX_SPAM_RESCORE_PER_PASS_CAP {
                break;
            }
            if state == SpamScorerState::ReportOnly && copy.report_spam_pm <= 0 {
                continue;
            }
            if !is_held(uid) {
                gone.push(uid);
                continue;
            }
            taken.push(RescoreCandidate {
                uid,
                vetoed: copy.not_junk,
            });
        }
        for uid in gone {
            self.unscored.remove(&uid);
        }
        taken
    }

    /// Whether the set holds any copy at all.
    pub fn has_unscored(&self) -> bool {
        !self.unscored.is_empty()
    }

    pub fn is_unsupported(&self) -> bool {
        self.unsupported
    }
}

/// The cursor after one delivered page (`mail-app-surface.md` § Read state):
/// one flag write stamps every row it touches with one modseq, so a page can
/// end inside a write — while `more`, resume at the last change's
/// `(modseq, uid)`; once drained, at `(highest_modseq, 0)`.
pub fn next_flag_cursor(page: &crate::backend::MailFlagChangesPage) -> (u64, u32) {
    match (page.more, page.changes.last()) {
        (true, Some(last)) => (last.modseq, last.uid),
        _ => (page.highest_modseq, 0),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::backend::{MailFlagChange, MailFlagChangesPage};

    #[test]
    fn owed_uids_batch_once_and_a_failed_batch_comes_back() {
        let mut s = MailReadSync::default();
        s.queue([7, 3]);
        s.queue([3, 9]);
        let batch = s.take_pending();
        assert_eq!(batch, [3, 7, 9]);
        assert!(!s.has_pending());
        s.requeue(batch);
        assert_eq!(s.take_pending(), [3, 7, 9]);
    }

    #[test]
    fn only_the_first_baseline_counts() {
        let mut s = MailReadSync::default();
        assert_eq!(s.cursor(), None);
        s.offer_baseline(40);
        s.offer_baseline(55);
        assert_eq!(s.cursor(), Some((40, 0)));
    }

    #[test]
    fn an_unsupporting_source_stops_everything_quietly() {
        let mut s = MailReadSync::default();
        s.offer_baseline(0);
        s.queue([1]);
        s.mark_unsupported();
        assert!(!s.has_pending());
        assert_eq!(s.cursor(), None);
        s.queue([2]);
        s.offer_baseline(9);
        s.advance((9, 0));
        assert!(!s.has_pending());
        assert_eq!(s.cursor(), None);
    }

    fn change(uid: u32, modseq: u64) -> MailFlagChange {
        MailFlagChange {
            uid,
            modseq,
            has_seen_flag: true,
            spam: MailSpamFacts {
                scored: true,
                ..MailSpamFacts::default()
            },
        }
    }

    // ── The held un-watermarked set (`mail-spam.md` § Scoring placement →
    //    *The app's catch-up*) ──

    fn unscored(report_spam_pm: i64, not_junk: bool) -> MailSpamFacts {
        MailSpamFacts {
            scored: false,
            not_junk,
            report_spam_pm,
        }
    }

    fn uids(c: &[RescoreCandidate]) -> Vec<u32> {
        c.iter().map(|c| c.uid).collect()
    }

    #[test]
    fn a_trained_take_is_every_copy_newest_first_capped_and_the_rest_kept() {
        let mut s = MailReadSync::default();
        for uid in 1..=150 {
            s.observe_spam_facts(uid, unscored(0, false));
        }
        let first = s.take_rescore(SpamScorerState::Trained, |_| true);
        assert_eq!(first.len(), INBOX_SPAM_RESCORE_PER_PASS_CAP);
        assert_eq!(first[0].uid, 150, "newest first");
        assert_eq!(first[99].uid, 51);
        // The pass watermarked what it took; the older tail is the next take.
        s.note_scored(uids(&first));
        assert_eq!(
            uids(&s.take_rescore(SpamScorerState::Trained, |_| true)),
            (1..=50).rev().collect::<Vec<_>>()
        );
    }

    #[test]
    fn a_report_only_take_is_the_reported_copies_and_an_absent_one_nothing() {
        let mut s = MailReadSync::default();
        s.observe_spam_facts(1, unscored(0, false));
        s.observe_spam_facts(2, unscored(200, false));
        assert!(s.take_rescore(SpamScorerState::Absent, |_| true).is_empty());
        assert_eq!(
            uids(&s.take_rescore(SpamScorerState::ReportOnly, |_| true)),
            [2]
        );
        assert_eq!(
            uids(&s.take_rescore(SpamScorerState::Trained, |_| true)),
            [2, 1]
        );
    }

    #[test]
    fn a_watermarked_read_removes_a_copy_and_an_unwatermarked_one_refreshes_its_facts() {
        let mut s = MailReadSync::default();
        s.observe_spam_facts(1, unscored(0, false));
        assert!(
            s.take_rescore(SpamScorerState::ReportOnly, |_| true)
                .is_empty()
        );
        s.observe_spam_facts(1, unscored(200, false));
        assert_eq!(
            uids(&s.take_rescore(SpamScorerState::ReportOnly, |_| true)),
            [1]
        );
        s.observe_spam_facts(
            1,
            MailSpamFacts {
                scored: true,
                ..unscored(200, false)
            },
        );
        assert!(!s.has_unscored());
    }

    #[test]
    fn a_not_junk_copy_is_taken_vetoed() {
        let mut s = MailReadSync::default();
        s.observe_spam_facts(5, unscored(0, true));
        assert_eq!(
            s.take_rescore(SpamScorerState::Trained, |_| true),
            [RescoreCandidate {
                uid: 5,
                vetoed: true
            }]
        );
    }

    #[test]
    fn a_copy_that_left_the_view_is_forgotten_by_the_take() {
        let mut s = MailReadSync::default();
        s.observe_spam_facts(1, unscored(0, false));
        s.observe_spam_facts(2, unscored(0, false));
        assert_eq!(
            uids(&s.take_rescore(SpamScorerState::Trained, |uid| uid == 1)),
            [1]
        );
        assert_eq!(
            uids(&s.take_rescore(SpamScorerState::Trained, |_| true)),
            [1]
        );
    }

    #[test]
    fn an_unsupporting_source_keeps_no_set() {
        let mut s = MailReadSync::default();
        s.observe_spam_facts(1, unscored(0, false));
        s.mark_unsupported();
        s.observe_spam_facts(2, unscored(0, false));
        assert!(!s.has_unscored());
    }

    #[test]
    fn a_page_ending_inside_one_write_resumes_at_its_last_change() {
        let page = MailFlagChangesPage {
            changes: vec![change(4, 12), change(6, 12)],
            highest_modseq: 13,
            more: true,
        };
        assert_eq!(next_flag_cursor(&page), (12, 6));
    }

    #[test]
    fn a_drained_page_carries_the_mailbox_high_water_mark() {
        let page = MailFlagChangesPage {
            changes: vec![change(4, 12)],
            highest_modseq: 13,
            more: false,
        };
        assert_eq!(next_flag_cursor(&page), (13, 0));
        assert_eq!(
            next_flag_cursor(&MailFlagChangesPage {
                highest_modseq: 13,
                ..Default::default()
            }),
            (13, 0)
        );
    }
}
