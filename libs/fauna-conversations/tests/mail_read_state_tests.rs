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
    MARK_SEEN_BATCH_MAX, SmtpBackend, flush_owed_mail_seen, ingest_inbound_record,
    poll_inbound_mail, sync_mail_read_state,
};
use fauna_conversations::thread::ThreadId;
use fauna_conversations::{
    ConversationsManager, MailFlagCallError, MailFlagChange, MailFlagChangesPage, MailSpamFacts,
    SpamScorerState, carries_seen_flag,
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
        spam: Default::default(),
        has_seen_flag: seen,
        warmup_hold: None,
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
    /// The pass lifecycle and each re-scored UID, in call order
    /// (`"begin"`, `"rescore 4"`, `"end"`).
    pass_log: Mutex<Vec<String>>,
    /// `rescore_held`'s answers; `Ok(Some(false))` (kept) once dry.
    rescore_answers: Mutex<VecDeque<Result<Option<bool>, MailFlagCallError>>>,
    /// What `scorer_state` answers; `ReportOnly` when unset — the state the
    /// re-open tests were written against (an actor with no trained model).
    state: Mutex<Option<SpamScorerState>>,
    /// The UIDs this pass scored (a re-scored copy the mailbox still held, or
    /// a vetoed one), which `end_pass` reports flushed.
    pass_scored: Mutex<Vec<u32>>,
    /// The disposition flush fails: `end_pass` reports nothing watermarked.
    flush_fails: Mutex<bool>,
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

    async fn begin_pass(&self) {
        self.pass_log.lock().unwrap().push("begin".into());
    }

    async fn end_pass(&self) -> Vec<u32> {
        self.pass_log.lock().unwrap().push("end".into());
        let scored = std::mem::take(&mut *self.pass_scored.lock().unwrap());
        if *self.flush_fails.lock().unwrap() {
            Vec::new()
        } else {
            scored
        }
    }

    fn scorer_state(&self) -> SpamScorerState {
        self.state
            .lock()
            .unwrap()
            .unwrap_or(SpamScorerState::ReportOnly)
    }

    fn record_vetoed(&self, uid: u32) {
        self.pass_log.lock().unwrap().push(format!("vetoed {uid}"));
        self.pass_scored.lock().unwrap().push(uid);
    }

    async fn rescore_held(&self, uid: u32) -> Result<Option<bool>, MailFlagCallError> {
        self.pass_log.lock().unwrap().push(format!("rescore {uid}"));
        let answer = self
            .rescore_answers
            .lock()
            .unwrap()
            .pop_front()
            .unwrap_or(Ok(Some(false)));
        if let Ok(Some(_)) = answer {
            self.pass_scored.lock().unwrap().push(uid);
        }
        answer
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
        spam: MailSpamFacts {
            scored: true,
            ..MailSpamFacts::default()
        },
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
async fn an_owed_set_over_the_request_cap_goes_out_in_capped_chunks() {
    // A long offline spell can owe far more than one request may carry; the
    // nest refuses an over-cap list (`mail-app-surface.md` § Read state).
    let m = manager();
    let source = FakeInbox::default();
    m.requeue_mail_seen((1..=2_500).collect());
    flush_owed_mail_seen(&source, &m).await;
    let lens: Vec<usize> = source.mark_seen_calls().iter().map(Vec::len).collect();
    assert_eq!(lens, [MARK_SEEN_BATCH_MAX, MARK_SEEN_BATCH_MAX, 500]);
    assert!(
        source.mark_seen_calls().concat() == (1..=2_500).collect::<Vec<u32>>(),
        "every owed UID sent once, ascending"
    );
    assert!(m.take_owed_mail_seen().is_empty(), "nothing left owed");
}

#[tokio::test]
async fn a_chunk_the_nest_refused_keeps_it_and_every_later_one_owed() {
    let m = manager();
    let source = FakeInbox::default();
    source.mark_seen_answers.lock().unwrap().extend([
        Ok(()),
        Err(MailFlagCallError::Failed("nest unreachable".into())),
    ]);
    m.requeue_mail_seen((1..=2_500).collect());
    flush_owed_mail_seen(&source, &m).await;
    assert_eq!(source.mark_seen_calls().len(), 2, "stops at the failure");
    assert_eq!(
        m.take_owed_mail_seen(),
        (MARK_SEEN_BATCH_MAX as u32 + 1..=2_500).collect::<Vec<u32>>(),
        "the failed chunk and the unsent tail stay owed; the sent chunk does not"
    );
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

// ── Re-opened scoring: a held copy the nest un-watermarked is scored again ──
// `report-sharing.md` § Applying the aggregate: an upward `report:spam`
// transition removes `$FaunaSpamScored` from every INBOX copy of the hash, and
// "the next pass at either scoring position scores that copy again, whole".

fn reopened(uid: u32, modseq: u64) -> MailFlagChange {
    MailFlagChange {
        uid,
        modseq,
        has_seen_flag: false,
        spam: MailSpamFacts {
            scored: false,
            not_junk: false,
            report_spam_pm: 200,
        },
    }
}

/// An un-watermarked copy with no `report:spam` row — what an actor with no
/// trained model sees on every `\Seen` change (`report-sharing.md` § Applying
/// the aggregate → *The re-open is a metadata change, not a flag change*).
fn unreported_unwatermarked(uid: u32, modseq: u64) -> MailFlagChange {
    MailFlagChange {
        spam: MailSpamFacts::default(),
        ..reopened(uid, modseq)
    }
}

/// The listed threads' labels, sorted (the list's own order is recency).
fn labels(m: &ConversationsManager) -> Vec<String> {
    let mut labels: Vec<String> = m.snapshot().threads.into_iter().map(|t| t.label).collect();
    labels.sort();
    labels
}

#[tokio::test]
async fn a_reopened_held_copy_the_scorer_files_leaves_the_view_in_one_pass() {
    let m = two_threads();
    m.offer_mail_flag_baseline(10);
    let source = FakeInbox::default();
    source.answer_changes(MailFlagChangesPage {
        changes: vec![reopened(4, 11)],
        highest_modseq: 11,
        more: false,
    });
    source
        .rescore_answers
        .lock()
        .unwrap()
        .push_back(Ok(Some(true)));
    sync_mail_read_state(&source, &m).await;
    assert_eq!(
        *source.pass_log.lock().unwrap(),
        ["begin", "rescore 4", "end"],
        "one pass of its own, so the disposition flushes once"
    );
    assert_eq!(
        labels(&m),
        ["alpha"],
        "beta's only message was filed: the emptied thread goes with it"
    );
}

#[tokio::test]
async fn only_held_unwatermarked_copies_are_rescored_and_a_kept_one_stays() {
    let m = two_threads();
    m.offer_mail_flag_baseline(10);
    let source = FakeInbox::default();
    source.answer_changes(MailFlagChangesPage {
        // 1: held, re-opened. 2: held, still watermarked. 99: not held — a
        // fresh delivery is the drain's to score, never the re-score's.
        changes: vec![reopened(1, 11), seen_change(2, 11, true), reopened(99, 11)],
        highest_modseq: 11,
        more: false,
    });
    sync_mail_read_state(&source, &m).await;
    assert_eq!(
        *source.pass_log.lock().unwrap(),
        ["begin", "rescore 1", "end"]
    );
    assert_eq!(unread_of(&m, "alpha"), 2, "the kept copy is untouched");
}

/// The rule is one sentence (`report-sharing.md` § Applying the aggregate →
/// *The re-open is a metadata change, not a flag change*): a held copy the
/// feed delivers without the watermark AND with a `report:spam` row is owed
/// one scoring pass; one without a report row is not, and a watermarked one is
/// not whatever its row says.
#[tokio::test]
async fn only_a_reported_unwatermarked_held_copy_is_owed_a_pass() {
    let m = two_threads();
    m.offer_mail_flag_baseline(10);
    let source = FakeInbox::default();
    source.answer_changes(MailFlagChangesPage {
        // 1: held, un-watermarked, reported → owed. 3: held, un-watermarked,
        // unreported (an untrained actor's `\Seen` change) → not. 2: held,
        // watermarked though reported → not.
        changes: vec![
            reopened(1, 11),
            unreported_unwatermarked(3, 11),
            MailFlagChange {
                spam: MailSpamFacts {
                    scored: true,
                    not_junk: false,
                    report_spam_pm: 200,
                },
                ..seen_change(2, 11, true)
            },
        ],
        highest_modseq: 11,
        more: false,
    });
    sync_mail_read_state(&source, &m).await;
    assert_eq!(
        *source.pass_log.lock().unwrap(),
        ["begin", "rescore 1", "end"]
    );
}

#[tokio::test]
async fn a_watermark_change_alone_runs_no_pass() {
    let m = two_threads();
    m.offer_mail_flag_baseline(10);
    let source = FakeInbox::default();
    source.answer_changes(MailFlagChangesPage {
        changes: vec![seen_change(1, 11, true)],
        highest_modseq: 11,
        more: false,
    });
    sync_mail_read_state(&source, &m).await;
    assert!(source.pass_log.lock().unwrap().is_empty());
}

#[tokio::test]
async fn a_failed_rescore_is_retried_on_the_next_sweep_and_a_filed_copy_leaves_its_thread() {
    let m = two_threads();
    m.offer_mail_flag_baseline(10);
    let source = FakeInbox::default();
    source.answer_changes(MailFlagChangesPage {
        changes: vec![reopened(1, 11), reopened(3, 11)],
        highest_modseq: 11,
        more: false,
    });
    source
        .rescore_answers
        .lock()
        .unwrap()
        .push_back(Err(MailFlagCallError::Failed("nest unreachable".into())));
    sync_mail_read_state(&source, &m).await;
    assert_eq!(
        *source.pass_log.lock().unwrap(),
        ["begin", "rescore 3", "end"],
        "newest first; the rest of the pass waits for the next sweep"
    );
    source.pass_log.lock().unwrap().clear();
    // No new change: the owed copies alone drive the next pass.
    source
        .rescore_answers
        .lock()
        .unwrap()
        .push_back(Ok(Some(true)));
    sync_mail_read_state(&source, &m).await;
    assert_eq!(
        *source.pass_log.lock().unwrap(),
        ["begin", "rescore 3", "rescore 1", "end"]
    );
    assert_eq!(
        unread_of(&m, "alpha"),
        1,
        "INBOX 3 left the thread (and its unread set); 1 stays"
    );
    // Both were scored and flushed: nothing is owed a third pass.
    source.pass_log.lock().unwrap().clear();
    sync_mail_read_state(&source, &m).await;
    assert!(source.pass_log.lock().unwrap().is_empty());
    assert_eq!(labels(&m), ["alpha", "beta"]);
}

#[tokio::test]
async fn a_source_that_does_not_rescore_drops_the_owed_copies() {
    let m = two_threads();
    m.offer_mail_flag_baseline(10);
    let source = FakeInbox::default();
    source.answer_changes(MailFlagChangesPage {
        changes: vec![reopened(1, 11)],
        highest_modseq: 11,
        more: false,
    });
    source
        .rescore_answers
        .lock()
        .unwrap()
        .push_back(Err(MailFlagCallError::Unsupported));
    sync_mail_read_state(&source, &m).await;
    source.pass_log.lock().unwrap().clear();
    sync_mail_read_state(&source, &m).await;
    assert!(
        source.pass_log.lock().unwrap().is_empty(),
        "nothing stays owed"
    );
}

// ── The app's catch-up: the held un-watermarked set ──
// `mail-spam.md` § Scoring placement → *The app's catch-up — the twin of the
// MDA's SELECT re-scan*: the app holds the set of its held `INBOX` copies that
// lack `$FaunaSpamScored`, and every mail sweep scores from it what this
// pass's scorer can score, newest first, up to the shared cap — every copy
// with a trained model, only a reported copy without one — watermarking a
// `$NotJunk` copy without opening it. Fed by the ingest, the feed and the
// app's own disposition.

/// An `INBOX` record carrying the given spam facts.
fn inbox_with(uid: u32, subject: &str, spam: MailSpamFacts) -> InboundMailRecord {
    InboundMailRecord {
        spam,
        ..inbox(uid, subject, false)
    }
}

const WATERMARKED: MailSpamFacts = MailSpamFacts {
    scored: true,
    not_junk: false,
    report_spam_pm: 0,
};

const VETOED: MailSpamFacts = MailSpamFacts {
    scored: false,
    not_junk: true,
    report_spam_pm: 0,
};

fn trained() -> FakeInbox {
    let source = FakeInbox::default();
    *source.state.lock().unwrap() = Some(SpamScorerState::Trained);
    source
}

fn pass_log(source: &FakeInbox) -> Vec<String> {
    std::mem::take(&mut *source.pass_log.lock().unwrap())
}

#[tokio::test]
async fn an_unwatermarked_ingest_enters_the_set_and_a_watermarked_one_does_not() {
    let m = manager();
    ingest_inbound_record(&m, &inbox_with(1, "alpha", MailSpamFacts::default())).unwrap();
    ingest_inbound_record(&m, &inbox_with(2, "alpha", WATERMARKED)).unwrap();
    ingest_inbound_record(&m, &inbox_with(3, "alpha", MailSpamFacts::default())).unwrap();
    // A Sent copy is own and never scored.
    ingest_inbound_record(
        &m,
        &record(MailFeed::Sent, 9, "me@host.test", "alpha", false),
    )
    .unwrap();
    let source = trained();
    sync_mail_read_state(&source, &m).await;
    assert_eq!(
        pass_log(&source),
        ["begin", "rescore 3", "rescore 1", "end"],
        "a trained pass scores every held un-watermarked copy, newest first"
    );
    // Both were scored and flushed: they left the set.
    sync_mail_read_state(&source, &m).await;
    assert!(pass_log(&source).is_empty(), "a scored set costs nothing");
}

#[tokio::test]
async fn without_a_scorer_nothing_is_taken_and_the_set_waits_for_a_model() {
    let m = manager();
    ingest_inbound_record(&m, &inbox_with(1, "alpha", MailSpamFacts::default())).unwrap();
    let source = FakeInbox::default();
    *source.state.lock().unwrap() = Some(SpamScorerState::Absent);
    sync_mail_read_state(&source, &m).await;
    assert!(pass_log(&source).is_empty(), "no scorer: no pass");
    // An untrained actor's unreported copy: the report-only scorer has
    // nothing to score, and the sweep costs nothing.
    *source.state.lock().unwrap() = Some(SpamScorerState::ReportOnly);
    sync_mail_read_state(&source, &m).await;
    assert!(pass_log(&source).is_empty());
    // The model first exists mid-run: the next sweep scores the copy held
    // since before the first lesson.
    *source.state.lock().unwrap() = Some(SpamScorerState::Trained);
    source
        .rescore_answers
        .lock()
        .unwrap()
        .push_back(Ok(Some(true)));
    sync_mail_read_state(&source, &m).await;
    assert_eq!(pass_log(&source), ["begin", "rescore 1", "end"]);
    assert!(
        labels(&m).is_empty(),
        "filed: the copy and its thread left the view"
    );
}

#[tokio::test]
async fn the_own_disposition_takes_its_uids_out_of_the_set() {
    let m = manager();
    let source = trained();
    // One drain pass ingests 1 and 2 un-watermarked; its scorer scored 2
    // (the flush reports it watermarked).
    source
        .fetch_pages
        .lock()
        .unwrap()
        .push_back(InboundMailPage {
            records: vec![
                inbox_with(1, "alpha", MailSpamFacts::default()),
                inbox_with(2, "alpha", MailSpamFacts::default()),
            ],
            ..Default::default()
        });
    source.pass_scored.lock().unwrap().push(2);
    let mut after = 0;
    poll_inbound_mail(&source, &m, &mut after, &mut HashSet::new(), 0)
        .await
        .unwrap();
    pass_log(&source);
    sync_mail_read_state(&source, &m).await;
    assert_eq!(
        pass_log(&source),
        ["begin", "rescore 1", "end"],
        "only the copy the drain's scorer did not watermark is owed"
    );
}

#[tokio::test]
async fn the_feed_adds_an_unwatermarked_held_copy_and_removes_a_watermarked_one() {
    let m = manager();
    ingest_inbound_record(&m, &inbox_with(1, "alpha", WATERMARKED)).unwrap();
    ingest_inbound_record(&m, &inbox_with(2, "alpha", MailSpamFacts::default())).unwrap();
    m.offer_mail_flag_baseline(10);
    let source = trained();
    source.answer_changes(MailFlagChangesPage {
        // 1 lost its watermark (re-opened); 2 gained it (another agent
        // scored it).
        changes: vec![unreported_unwatermarked(1, 11), seen_change(2, 11, false)],
        highest_modseq: 11,
        more: false,
    });
    sync_mail_read_state(&source, &m).await;
    assert_eq!(pass_log(&source), ["begin", "rescore 1", "end"]);
}

#[tokio::test]
async fn a_not_junk_copy_is_watermarked_without_being_opened() {
    let m = manager();
    ingest_inbound_record(&m, &inbox_with(1, "alpha", VETOED)).unwrap();
    ingest_inbound_record(&m, &inbox_with(2, "alpha", MailSpamFacts::default())).unwrap();
    let source = trained();
    sync_mail_read_state(&source, &m).await;
    assert_eq!(
        pass_log(&source),
        ["begin", "rescore 2", "vetoed 1", "end"],
        "the vetoed copy is recorded, never re-read"
    );
    sync_mail_read_state(&source, &m).await;
    assert!(pass_log(&source).is_empty(), "both were watermarked");
}

#[tokio::test]
async fn a_copy_the_mailbox_no_longer_holds_leaves_the_set() {
    let m = manager();
    ingest_inbound_record(&m, &inbox_with(1, "alpha", MailSpamFacts::default())).unwrap();
    let source = trained();
    source.rescore_answers.lock().unwrap().push_back(Ok(None));
    sync_mail_read_state(&source, &m).await;
    assert_eq!(pass_log(&source), ["begin", "rescore 1", "end"]);
    sync_mail_read_state(&source, &m).await;
    assert!(pass_log(&source).is_empty(), "a gone copy is owed nothing");
}

#[tokio::test]
async fn an_unflushed_score_stays_owed() {
    // The pass scored the copy but its flush did not land: the copy is still
    // un-watermarked on the nest, so it stays in the set, owed the next sweep.
    let m = manager();
    ingest_inbound_record(&m, &inbox_with(1, "alpha", MailSpamFacts::default())).unwrap();
    let source = trained();
    *source.flush_fails.lock().unwrap() = true;
    sync_mail_read_state(&source, &m).await;
    assert_eq!(pass_log(&source), ["begin", "rescore 1", "end"]);
    *source.flush_fails.lock().unwrap() = false;
    sync_mail_read_state(&source, &m).await;
    assert_eq!(pass_log(&source), ["begin", "rescore 1", "end"]);
    sync_mail_read_state(&source, &m).await;
    assert!(pass_log(&source).is_empty());
}
