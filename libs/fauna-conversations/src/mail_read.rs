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

use std::collections::BTreeSet;

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
        }
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
