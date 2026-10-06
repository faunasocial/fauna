//! Mail read state — `docs/goal/behavior/conversation-read-state.md` § Mail:
//! `\Seen` is the marker, and the *Mail* arm of § How the carriers meet the
//! in-memory set. The mail rail's read marker is the message's IMAP `\Seen`
//! flag: a mail message is unread exactly when it is not own and lacks it, with
//! no launch floor and no clock consulted.

use async_trait::async_trait;
use fauna_conversations::backend::{
    InboundMailPage, InboundMailRecord, InboundMailSource, MailFeed, OutboundMailSink,
};
use fauna_conversations::backends::smtp::{
    SmtpBackend, flush_owed_mail_seen, ingest_inbound_record, poll_inbound_mail,
    sync_mail_read_state,
};
use fauna_conversations::thread::ThreadId;
use fauna_conversations::{
    ConversationsManager, MailFlagCallError, MailFlagChange, MailFlagChangesPage, carries_seen_flag,
};
use std::collections::{HashSet, VecDeque};
use std::sync::{Arc, Mutex};

/// Where this run's news starts, pinned so no assertion races the clock. Every
/// mail below is stamped under it: were the floor consulted, all of it would
/// read as history.
const FLOOR_MS: i64 = 1_000_000;
const BEFORE_FLOOR_MS: i64 = FLOOR_MS - 1;

struct NullSink;

#[async_trait]
impl OutboundMailSink for NullSink {
    async fn submit(&self, _recipients: Vec<String>, _raw: Vec<u8>) -> Result<(), String> {
        Ok(())
    }
}

fn manager() -> Arc<ConversationsManager> {
    let m = ConversationsManager::new();
    m.register_backend(Arc::new(SmtpBackend::new(
        Arc::new(NullSink),
        "me@host.test",
    )));
    m.set_launch_floor_for_test(FLOOR_MS);
    m
}

fn record(mailbox: MailFeed, uid: u32, from: &str, subject: &str, seen: bool) -> InboundMailRecord {
    let raw = format!(
        "From: {from}\r\nTo: me@host.test\r\nSubject: {subject}\r\nMessage-ID: <{mailbox:?}-{uid}@host.test>\r\n\r\nbody {uid}\r\n"
    );
    InboundMailRecord {
        uid,
        mailbox,
        message_id: format!("{mailbox:?}-{uid}").into_bytes(),
        internal_date_ms: BEFORE_FLOOR_MS,
        rfc5322: raw.into_bytes(),
        suppress_from_view: false,
        has_seen_flag: seen,
    }
}

fn inbox(uid: u32, subject: &str, seen: bool) -> InboundMailRecord {
    record(MailFeed::Inbox, uid, "them@elsewhere.test", subject, seen)
}

fn unread_of(m: &ConversationsManager, label: &str) -> u32 {
    m.snapshot()
        .threads
        .iter()
        .find(|t| t.label == label)
        .unwrap_or_else(|| panic!("no thread labelled {label:?}"))
        .unread_count
}

#[test]
fn the_seen_flag_is_read_in_any_spelling() {
    assert!(carries_seen_flag(&["\\Seen".into()]));
    assert!(carries_seen_flag(&["\\Answered".into(), "\\SEEN".into()]));
    assert!(!carries_seen_flag(&[
        "\\Answered".into(),
        "$FaunaSpamScored".into()
    ]));
    assert!(!carries_seen_flag(&[]));
}

#[test]
fn a_mail_without_seen_is_unread_with_no_floor_consulted() {
    // Stamped before the floor: under the floor rule this would be replayed
    // history. On the mail rail the flag is the truth — a mail that arrived
    // while every app was closed has no `\Seen` and is unread at launch.
    let m = manager();
    ingest_inbound_record(&m, &inbox(1, "alpha", false)).unwrap();
    assert_eq!(unread_of(&m, "alpha"), 1);
    ingest_inbound_record(&m, &inbox(2, "alpha", false)).unwrap();
    assert_eq!(unread_of(&m, "alpha"), 2);
}

#[test]
fn a_mail_carrying_seen_is_read() {
    let m = manager();
    ingest_inbound_record(&m, &inbox(1, "alpha", true)).unwrap();
    assert_eq!(unread_of(&m, "alpha"), 0);
    ingest_inbound_record(&m, &inbox(2, "alpha", false)).unwrap();
    assert_eq!(unread_of(&m, "alpha"), 1, "only the unflagged one counts");
}

#[test]
fn a_seen_mail_arriving_mid_run_is_read_whatever_its_date() {
    // The other direction of "no floor": a mail stamped after the floor that
    // another client already read arrives read.
    let m = manager();
    let mut rec = inbox(1, "alpha", true);
    rec.internal_date_ms = FLOOR_MS + 10_000;
    ingest_inbound_record(&m, &rec).unwrap();
    assert_eq!(unread_of(&m, "alpha"), 0);
}

#[test]
fn a_sent_copy_is_own_and_never_unread() {
    let m = manager();
    let sent = record(MailFeed::Sent, 1, "me@host.test", "alpha", false);
    ingest_inbound_record(&m, &sent).unwrap();
    assert_eq!(unread_of(&m, "alpha"), 0);
}

// ── Syncing: the `\Seen` write, flag changes, an unserved source ───────

/// An `INBOX` source that records the read-state calls made on it and answers
/// them from scripted queues — `Ok` / an empty drained page once a queue runs
/// dry.
#[derive(Default)]
struct FakeInbox {
    /// The page `fetch` answers with (its `highest_modseq` is the baseline).
    fetch_pages: Mutex<VecDeque<InboundMailPage>>,
    mark_seen_calls: Mutex<Vec<Vec<u32>>>,
    mark_seen_answers: Mutex<VecDeque<Result<(), MailFlagCallError>>>,
    flag_change_calls: Mutex<Vec<(u64, u32)>>,
    flag_change_answers: Mutex<VecDeque<Result<MailFlagChangesPage, MailFlagCallError>>>,
}

impl FakeInbox {
    fn refusing_as_unsupported() -> Self {
        let fake = Self::default();
        for _ in 0..4 {
            fake.mark_seen_answers
                .lock()
                .unwrap()
                .push_back(Err(MailFlagCallError::Unsupported));
            fake.flag_change_answers
                .lock()
                .unwrap()
                .push_back(Err(MailFlagCallError::Unsupported));
        }
        fake
    }

    fn answer_changes(&self, page: MailFlagChangesPage) {
        self.flag_change_answers.lock().unwrap().push_back(Ok(page));
    }

    fn mark_seen_calls(&self) -> Vec<Vec<u32>> {
        self.mark_seen_calls.lock().unwrap().clone()
    }
}

#[async_trait]
impl InboundMailSource for FakeInbox {
    async fn fetch(&self, _after_uid: u32, _limit: u32) -> Result<InboundMailPage, String> {
        Ok(self
            .fetch_pages
            .lock()
            .unwrap()
            .pop_front()
            .unwrap_or_default())
    }

    async fn mark_seen(&self, uids: Vec<u32>) -> Result<(), MailFlagCallError> {
        self.mark_seen_calls.lock().unwrap().push(uids);
        self.mark_seen_answers
            .lock()
            .unwrap()
            .pop_front()
            .unwrap_or(Ok(()))
    }

    async fn flag_changes(
        &self,
        since_modseq: u64,
        after_uid: u32,
        _limit: u32,
    ) -> Result<MailFlagChangesPage, MailFlagCallError> {
        self.flag_change_calls
            .lock()
            .unwrap()
            .push((since_modseq, after_uid));
        self.flag_change_answers
            .lock()
            .unwrap()
            .pop_front()
            .unwrap_or_else(|| {
                Ok(MailFlagChangesPage {
                    highest_modseq: since_modseq,
                    ..Default::default()
                })
            })
    }
}

fn id_of(m: &ConversationsManager, label: &str) -> ThreadId {
    m.snapshot()
        .threads
        .iter()
        .find(|t| t.label == label)
        .unwrap_or_else(|| panic!("no thread labelled {label:?}"))
        .thread_id
        .clone()
}

fn seen_change(uid: u32, modseq: u64, has_seen_flag: bool) -> MailFlagChange {
    MailFlagChange {
        uid,
        modseq,
        has_seen_flag,
    }
}

/// `alpha` holds INBOX 1 (unread), 2 (read elsewhere) and 3 (unread); `beta`
/// holds INBOX 4 (unread).
fn two_threads() -> Arc<ConversationsManager> {
    let m = manager();
    for rec in [
        inbox(1, "alpha", false),
        inbox(2, "alpha", true),
        inbox(3, "alpha", false),
        inbox(4, "beta", false),
    ] {
        ingest_inbound_record(&m, &rec).unwrap();
    }
    assert_eq!(unread_of(&m, "alpha"), 2);
    m
}

#[tokio::test]
async fn reading_a_thread_emits_one_batched_write_naming_exactly_its_unread_inbox_uids() {
    let m = two_threads();
    let source = FakeInbox::default();
    m.select_thread(id_of(&m, "alpha"));
    assert_eq!(unread_of(&m, "alpha"), 0);
    flush_owed_mail_seen(&source, &m).await;
    assert_eq!(
        source.mark_seen_calls(),
        [vec![1, 3]],
        "one call, the thread's unread INBOX UIDs only — not the one already \
         read, not the other thread's"
    );
    // Re-reading an already-read thread owes nothing.
    m.select_thread(id_of(&m, "beta"));
    m.select_thread(id_of(&m, "alpha"));
    flush_owed_mail_seen(&source, &m).await;
    assert_eq!(source.mark_seen_calls(), [vec![1, 3], vec![4]]);
}

#[tokio::test]
async fn a_write_the_nest_did_not_take_stays_owed_until_it_does() {
    let m = two_threads();
    let source = FakeInbox::default();
    source
        .mark_seen_answers
        .lock()
        .unwrap()
        .push_back(Err(MailFlagCallError::Failed("nest unreachable".into())));
    m.mark_read(id_of(&m, "alpha"));
    flush_owed_mail_seen(&source, &m).await;
    assert_eq!(
        unread_of(&m, "alpha"),
        0,
        "the read holds in memory meanwhile"
    );
    flush_owed_mail_seen(&source, &m).await;
    assert_eq!(source.mark_seen_calls(), [vec![1, 3], vec![1, 3]]);
    flush_owed_mail_seen(&source, &m).await;
    assert_eq!(source.mark_seen_calls().len(), 2, "sent once it landed");
}

#[tokio::test]
async fn a_delivered_flag_change_moves_one_message_each_way() {
    let m = two_threads();
    m.offer_mail_flag_baseline(10);
    let source = FakeInbox::default();
    // Another device read INBOX 1; a mail client marked INBOX 2 unread again.
    source.answer_changes(MailFlagChangesPage {
        changes: vec![seen_change(1, 11, true), seen_change(2, 12, false)],
        highest_modseq: 12,
        more: false,
    });
    sync_mail_read_state(&source, &m).await;
    assert_eq!(unread_of(&m, "alpha"), 2);
    assert_eq!(unread_of(&m, "beta"), 1, "untouched");
    assert_eq!(m.mail_flag_cursor(), Some((12, 0)));
    // Which two: reading the thread now owes exactly {2, 3} — 1 left the set,
    // 2 re-entered it.
    m.mark_read(id_of(&m, "alpha"));
    assert_eq!(m.take_owed_mail_seen(), [2, 3]);
}

#[tokio::test]
async fn the_drain_resumes_inside_a_write_then_carries_the_high_water_mark() {
    let m = two_threads();
    m.offer_mail_flag_baseline(10);
    let source = FakeInbox::default();
    source.answer_changes(MailFlagChangesPage {
        changes: vec![seen_change(1, 12, true)],
        highest_modseq: 14,
        more: true,
    });
    source.answer_changes(MailFlagChangesPage {
        changes: vec![seen_change(3, 12, true)],
        highest_modseq: 14,
        more: false,
    });
    sync_mail_read_state(&source, &m).await;
    assert_eq!(
        *source.flag_change_calls.lock().unwrap(),
        [(10, 0), (12, 1)],
        "starts at the baseline, resumes at the last change's (modseq, uid)"
    );
    assert_eq!(m.mail_flag_cursor(), Some((14, 0)));
    assert_eq!(unread_of(&m, "alpha"), 0);
}

#[tokio::test]
async fn a_change_that_predates_a_read_still_owed_does_not_unread_it() {
    let m = two_threads();
    m.offer_mail_flag_baseline(10);
    m.mark_read(id_of(&m, "alpha"));
    let source = FakeInbox::default();
    source
        .mark_seen_answers
        .lock()
        .unwrap()
        .push_back(Err(MailFlagCallError::Failed("nest unreachable".into())));
    // The nest has not heard of the read: its rows still lack `\Seen`.
    source.answer_changes(MailFlagChangesPage {
        changes: vec![seen_change(1, 11, false)],
        highest_modseq: 11,
        more: false,
    });
    sync_mail_read_state(&source, &m).await;
    assert_eq!(unread_of(&m, "alpha"), 0);
}

#[tokio::test]
async fn the_first_inbox_page_names_the_baseline_and_later_pages_do_not_move_it() {
    let m = manager();
    let source = FakeInbox::default();
    for highest_modseq in [0, 40, 55] {
        source
            .fetch_pages
            .lock()
            .unwrap()
            .push_back(InboundMailPage {
                highest_modseq,
                ..Default::default()
            });
    }
    let mut after_uid = 0;
    for _ in 0..3 {
        poll_inbound_mail(&source, &m, &mut after_uid, &mut HashSet::new(), 0)
            .await
            .unwrap();
    }
    assert_eq!(
        m.mail_flag_cursor(),
        Some((40, 0)),
        "zero is no baseline; the first real one stands"
    );
}

#[tokio::test]
async fn an_unserved_source_degrades_silently_to_launch_flags_and_in_memory_reads() {
    let m = two_threads();
    let source = FakeInbox::refusing_as_unsupported();
    // No baseline is named (zero `highest_modseq`), so no cursor exists and
    // no flag change is ever asked for.
    assert_eq!(m.mail_flag_cursor(), None);
    m.select_thread(id_of(&m, "alpha"));
    sync_mail_read_state(&source, &m).await;
    assert_eq!(source.mark_seen_calls(), [vec![1, 3]], "tried once");
    assert!(source.flag_change_calls.lock().unwrap().is_empty());
    assert_eq!(unread_of(&m, "alpha"), 0, "the read holds for the run");
    assert!(m.snapshot().error.is_none(), "no error is shown");
    // Nothing further is owed or asked, and a later baseline is ignored.
    m.offer_mail_flag_baseline(99);
    m.select_thread(id_of(&m, "beta"));
    sync_mail_read_state(&source, &m).await;
    assert_eq!(source.mark_seen_calls().len(), 1);
    assert!(source.flag_change_calls.lock().unwrap().is_empty());
    assert_eq!(unread_of(&m, "beta"), 0);
}

#[tokio::test]
async fn a_flag_change_refused_as_unsupported_is_soft_too() {
    let m = two_threads();
    m.offer_mail_flag_baseline(10);
    let source = FakeInbox::refusing_as_unsupported();
    sync_mail_read_state(&source, &m).await;
    assert_eq!(m.mail_flag_cursor(), None);
    assert_eq!(unread_of(&m, "alpha"), 2, "launch flags stand");
    assert!(m.snapshot().error.is_none());
}
