//! A received mail message's own scan verdict — `docs/goal/behavior/
//! mail-content-scanning.md` § User sees their own message's row. The manager
//! keeps each `INBOX` arrival's nest record id, reads the verdict through the
//! mail rail when the thread opens (`refresh_message_scan_results`), and
//! `thread_detail` projects the lines onto that message alone — never onto the
//! account's own Sent copy, which the perimeter scanner never sees.

use async_trait::async_trait;
use fauna_conversations::ConversationsManager;
use fauna_conversations::backend::{InboundMailRecord, MailFeed, OutboundMailSink};
use fauna_conversations::backends::smtp::{SmtpBackend, ingest_inbound_record};
use fauna_conversations::thread::ThreadId;
use fauna_core::localized::LocalizedText;
use std::collections::HashMap;
use std::sync::{Arc, Mutex};

/// A nest that holds a scan row for some record ids, answers `not_found`
/// (`Ok(None)`) for the rest, and fails a read while `failing` is set.
/// `hidden` is the account's `mail.account.scan_result_visible` turned off.
#[derive(Default)]
struct ScanSink {
    rows: HashMap<Vec<u8>, Vec<LocalizedText>>,
    reads: Mutex<Vec<Vec<u8>>>,
    failing: Mutex<bool>,
    hidden: Mutex<bool>,
}

#[async_trait]
impl OutboundMailSink for ScanSink {
    async fn submit(&self, _recipients: Vec<String>, _raw: Vec<u8>) -> Result<(), String> {
        Ok(())
    }

    async fn message_scan_result(
        &self,
        nest_message_id: Vec<u8>,
    ) -> Result<Option<Vec<LocalizedText>>, String> {
        self.reads.lock().unwrap().push(nest_message_id.clone());
        if *self.failing.lock().unwrap() {
            return Err("transport down".into());
        }
        Ok(self.rows.get(&nest_message_id).cloned())
    }

    async fn scan_result_visible(&self) -> Result<bool, String> {
        Ok(!*self.hidden.lock().unwrap())
    }
}

fn record(mailbox: MailFeed, uid: u32, from: &str, to: &str) -> InboundMailRecord {
    let raw = format!(
        "From: {from}\r\nTo: {to}\r\nSubject: scanned\r\nMessage-ID: <{mailbox:?}-{uid}@host.test>\r\n\r\nbody {uid}\r\n"
    );
    InboundMailRecord {
        uid,
        mailbox,
        message_id: format!("{mailbox:?}-{uid}").into_bytes(),
        internal_date_ms: 1,
        rfc5322: raw.into_bytes(),
        suppress_from_view: false,
        has_seen_flag: true,
        warmup_hold: None,
    }
}

fn verdict() -> Vec<LocalizedText> {
    fauna_core::mail_scan::scan_result_lines(
        &fauna_core::mail_scan::ClamavVerdict::Clean,
        None,
        fauna_core::mail_scan::ScanActionTaken::Delivered,
    )
}

fn manager(sink: Arc<ScanSink>) -> (Arc<ConversationsManager>, ThreadId) {
    let m = ConversationsManager::new();
    m.register_backend(Arc::new(SmtpBackend::new(sink, "me@host.test")));
    for rec in [
        record(MailFeed::Inbox, 1, "alice@far.test", "me@host.test"),
        record(MailFeed::Sent, 1, "me@host.test", "alice@far.test"),
        record(MailFeed::Inbox, 2, "alice@far.test", "me@host.test"),
    ] {
        ingest_inbound_record(&m, &rec).unwrap();
    }
    let id = m.snapshot().threads[0].thread_id.clone();
    (m, id)
}

/// `(is_own, scan lines)` per message, in thread order.
fn painted(m: &ConversationsManager, id: &ThreadId) -> Vec<(bool, Vec<LocalizedText>)> {
    m.thread_detail(id.clone())
        .unwrap()
        .messages
        .into_iter()
        .map(|msg| (msg.is_own, msg.scan_result_lines))
        .collect()
}

#[tokio::test]
async fn an_opened_thread_paints_the_received_messages_verdict_and_never_the_own_copy() {
    let sink = Arc::new(ScanSink {
        rows: HashMap::from([(b"Inbox-1".to_vec(), verdict())]),
        ..Default::default()
    });
    let (m, id) = manager(sink.clone());
    assert!(
        painted(&m, &id).iter().all(|(_, lines)| lines.is_empty()),
        "nothing is painted before the thread opens"
    );

    m.refresh_message_scan_results(id.clone()).await;

    let mut reads = sink.reads.lock().unwrap().clone();
    reads.sort();
    assert_eq!(
        reads,
        [b"Inbox-1".to_vec(), b"Inbox-2".to_vec()],
        "one read per received message, by its nest record id — never the Sent copy"
    );
    assert_eq!(
        painted(&m, &id),
        [(false, verdict()), (true, Vec::new()), (false, Vec::new())],
        "the scanned message shows its verdict; the own copy and the one the \
         nest holds no row for show none"
    );

    // Every received message is answered, so a re-open asks nothing again.
    m.refresh_message_scan_results(id.clone()).await;
    assert_eq!(sink.reads.lock().unwrap().len(), 2);
}

#[tokio::test]
async fn a_failed_read_is_asked_again_on_the_next_open() {
    let sink = Arc::new(ScanSink {
        rows: HashMap::from([(b"Inbox-1".to_vec(), verdict())]),
        failing: Mutex::new(true),
        ..Default::default()
    });
    let (m, id) = manager(sink.clone());
    m.refresh_message_scan_results(id.clone()).await;
    assert!(painted(&m, &id).iter().all(|(_, lines)| lines.is_empty()));

    *sink.failing.lock().unwrap() = false;
    m.refresh_message_scan_results(id.clone()).await;
    assert_eq!(painted(&m, &id)[0], (false, verdict()));
    assert_eq!(sink.reads.lock().unwrap().len(), 4, "both reads retried");
}

#[tokio::test]
async fn an_identity_change_forgets_the_outgoing_accounts_verdicts() {
    let sink = Arc::new(ScanSink {
        rows: HashMap::from([(b"Inbox-1".to_vec(), verdict())]),
        ..Default::default()
    });
    let (m, id) = manager(sink.clone());
    m.refresh_message_scan_results(id.clone()).await;
    m.clear_for_identity_change();
    // The same record re-ingested under the next identity starts unanswered.
    ingest_inbound_record(
        &m,
        &record(MailFeed::Inbox, 1, "alice@far.test", "me@host.test"),
    )
    .unwrap();
    let id = m.snapshot().threads[0].thread_id.clone();
    assert!(painted(&m, &id).iter().all(|(_, lines)| lines.is_empty()));
}

#[tokio::test]
async fn the_accounts_switch_off_hides_the_row_and_skips_the_reads_and_on_brings_it_back() {
    // `mail-content-scanning.md` § Per-account opt-out for visible scan
    // details: with `mail.account.scan_result_visible` off the row is hidden
    // from every received bubble — decided here, in shared Rust, never by an
    // app — and the data stays on the nest, so turning it back on paints it.
    let sink = Arc::new(ScanSink {
        rows: HashMap::from([(b"Inbox-1".to_vec(), verdict())]),
        ..Default::default()
    });
    let (m, id) = manager(sink.clone());
    m.refresh_message_scan_results(id.clone()).await;
    assert_eq!(painted(&m, &id)[0], (false, verdict()));

    // Off: the painted row goes, and nothing is read.
    *sink.hidden.lock().unwrap() = true;
    m.refresh_message_scan_results(id.clone()).await;
    assert!(
        painted(&m, &id).iter().all(|(_, lines)| lines.is_empty()),
        "the switch off hides the row an earlier open painted"
    );

    // A fresh manager with the switch already off never asks the nest.
    let fresh = Arc::new(ScanSink {
        rows: HashMap::from([(b"Inbox-1".to_vec(), verdict())]),
        hidden: Mutex::new(true),
        ..Default::default()
    });
    let (m2, id2) = manager(fresh.clone());
    m2.refresh_message_scan_results(id2.clone()).await;
    assert!(
        fresh.reads.lock().unwrap().is_empty(),
        "no verdict read while hidden"
    );
    assert!(painted(&m2, &id2).iter().all(|(_, lines)| lines.is_empty()));

    // Back on: the verdict is read and painted again.
    *fresh.hidden.lock().unwrap() = false;
    m2.refresh_message_scan_results(id2.clone()).await;
    assert_eq!(painted(&m2, &id2)[0], (false, verdict()));
    *sink.hidden.lock().unwrap() = false;
    m.refresh_message_scan_results(id.clone()).await;
    assert_eq!(painted(&m, &id)[0], (false, verdict()));
}
