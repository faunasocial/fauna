//! A received mail message's "Delivered to" tag — `docs/goal/behavior/
//! mail-aliases.md` § Label. The receive path keeps each `INBOX` arrival's
//! delivery stamps, the manager reads the account's alias rows through the
//! mail rail when the thread opens (`refresh_delivered_aliases`), and
//! `thread_detail` projects the shared wording onto that message alone — never
//! onto the account's own Sent copy, which no resolver delivered.

use async_trait::async_trait;
use fauna_conversations::ConversationsManager;
use fauna_conversations::backend::{InboundMailRecord, MailFeed, OutboundMailSink};
use fauna_conversations::backends::smtp::{SmtpBackend, ingest_inbound_record};
use fauna_conversations::thread::ThreadId;
use std::sync::{Arc, Mutex};

/// A nest that answers `list_account_aliases` with `rows`, and fails the read
/// while `failing` is set.
#[derive(Default)]
struct AliasSink {
    rows: Mutex<Vec<fauna_conversations::backend::AliasLabelRow>>,
    reads: Mutex<usize>,
    failing: Mutex<bool>,
}

#[async_trait]
impl OutboundMailSink for AliasSink {
    async fn submit(&self, _recipients: Vec<String>, _raw: Vec<u8>) -> Result<(), String> {
        Ok(())
    }

    async fn account_alias_labels(
        &self,
    ) -> Result<Vec<fauna_conversations::backend::AliasLabelRow>, String> {
        *self.reads.lock().unwrap() += 1;
        if *self.failing.lock().unwrap() {
            return Err("transport down".into());
        }
        Ok(self.rows.lock().unwrap().clone())
    }
}

fn alias(kind: &str, pattern: &str, label: &str) -> fauna_conversations::backend::AliasLabelRow {
    fauna_conversations::backend::AliasLabelRow {
        kind: kind.into(),
        local_domain: "host.test".into(),
        pattern: pattern.into(),
        label: label.into(),
    }
}

/// `stamp` is the `X-Fauna-Delivered-To` value the resolver would have
/// stamped; `None` writes an unstamped message.
fn record(
    mailbox: MailFeed,
    uid: u32,
    from: &str,
    to: &str,
    stamp: Option<&str>,
) -> InboundMailRecord {
    let stamp = stamp
        .map(|s| format!("X-Fauna-Delivered-To: {s}\r\n"))
        .unwrap_or_default();
    let raw = format!(
        "{stamp}From: {from}\r\nTo: {to}\r\nSubject: tagged\r\nMessage-ID: <{mailbox:?}-{uid}@host.test>\r\n\r\nbody {uid}\r\n"
    );
    InboundMailRecord {
        uid,
        mailbox,
        message_id: format!("{mailbox:?}-{uid}").into_bytes(),
        internal_date_ms: i64::from(uid),
        rfc5322: raw.into_bytes(),
        suppress_from_view: false,
        has_seen_flag: true,
        warmup_hold: None,
    }
}

fn manager(sink: Arc<AliasSink>) -> (Arc<ConversationsManager>, ThreadId) {
    let m = ConversationsManager::new();
    m.register_backend(Arc::new(SmtpBackend::new(sink, "me@host.test")));
    for rec in [
        record(
            MailFeed::Inbox,
            1,
            "alice@far.test",
            "me@host.test",
            Some("shop@host.test"),
        ),
        // The account's own Sent copy carries a stamp it was never delivered
        // on (a forged or copied header): no tag.
        record(
            MailFeed::Sent,
            2,
            "me@host.test",
            "alice@far.test",
            Some("shop@host.test"),
        ),
        record(MailFeed::Inbox, 3, "alice@far.test", "me@host.test", None),
    ] {
        ingest_inbound_record(&m, &rec).unwrap();
    }
    let id = m.snapshot().threads[0].thread_id.clone();
    (m, id)
}

/// `(is_own, tag)` per message, in thread order.
fn painted(m: &ConversationsManager, id: &ThreadId) -> Vec<(bool, Option<String>)> {
    m.thread_detail(id.clone())
        .unwrap()
        .messages
        .into_iter()
        .map(|msg| (msg.is_own, msg.delivered_to))
        .collect()
}

#[tokio::test]
async fn an_opened_thread_tags_the_received_message_with_its_alias_and_label() {
    let sink = Arc::new(AliasSink {
        rows: Mutex::new(vec![alias("exact", "shop", "Online shops")]),
        ..Default::default()
    });
    let (m, id) = manager(sink.clone());
    assert!(
        painted(&m, &id).iter().all(|(_, tag)| tag.is_none()),
        "nothing is tagged before the alias rows are read"
    );

    m.refresh_delivered_aliases(id.clone()).await;

    assert_eq!(
        painted(&m, &id),
        [
            (
                false,
                Some("Delivered to: shop@host.test [Online shops]".to_string())
            ),
            (true, None),
            (false, None),
        ],
        "the stamped received message is tagged; the own copy and the \
         unstamped one are not"
    );
}

#[tokio::test]
async fn an_unlabelled_alias_falls_back_to_its_kind_and_pattern() {
    let sink = Arc::new(AliasSink {
        rows: Mutex::new(vec![alias("exact", "shop", "")]),
        ..Default::default()
    });
    let (m, id) = manager(sink.clone());
    m.refresh_delivered_aliases(id.clone()).await;
    assert_eq!(
        painted(&m, &id)[0].1.as_deref(),
        Some("Delivered to: shop@host.test [Exact shop]")
    );
}

#[tokio::test]
async fn a_label_edited_since_repaints_on_the_next_open() {
    let sink = Arc::new(AliasSink {
        rows: Mutex::new(vec![alias("exact", "shop", "Online shops")]),
        ..Default::default()
    });
    let (m, id) = manager(sink.clone());
    m.refresh_delivered_aliases(id.clone()).await;
    *sink.rows.lock().unwrap() = vec![alias("exact", "shop", "Receipts")];
    m.refresh_delivered_aliases(id.clone()).await;
    assert_eq!(
        painted(&m, &id)[0].1.as_deref(),
        Some("Delivered to: shop@host.test [Receipts]")
    );
}

#[tokio::test]
async fn a_failed_read_keeps_what_the_last_one_said() {
    let sink = Arc::new(AliasSink {
        rows: Mutex::new(vec![alias("exact", "shop", "Online shops")]),
        ..Default::default()
    });
    let (m, id) = manager(sink.clone());
    m.refresh_delivered_aliases(id.clone()).await;
    *sink.failing.lock().unwrap() = true;
    m.refresh_delivered_aliases(id.clone()).await;
    assert_eq!(*sink.reads.lock().unwrap(), 2);
    assert_eq!(
        painted(&m, &id)[0].1.as_deref(),
        Some("Delivered to: shop@host.test [Online shops]")
    );
}

#[tokio::test]
async fn a_thread_with_no_stamped_received_message_reads_nothing() {
    let sink = Arc::new(AliasSink::default());
    let m = ConversationsManager::new();
    m.register_backend(Arc::new(SmtpBackend::new(sink.clone(), "me@host.test")));
    ingest_inbound_record(
        &m,
        &record(MailFeed::Inbox, 1, "alice@far.test", "me@host.test", None),
    )
    .unwrap();
    let id = m.snapshot().threads[0].thread_id.clone();
    m.refresh_delivered_aliases(id).await;
    assert_eq!(*sink.reads.lock().unwrap(), 0);
}

#[tokio::test]
async fn an_identity_change_forgets_the_outgoing_accounts_tags() {
    let sink = Arc::new(AliasSink {
        rows: Mutex::new(vec![alias("exact", "shop", "Online shops")]),
        ..Default::default()
    });
    let (m, id) = manager(sink.clone());
    m.refresh_delivered_aliases(id).await;
    m.clear_for_identity_change();
    ingest_inbound_record(
        &m,
        &record(
            MailFeed::Inbox,
            1,
            "alice@far.test",
            "me@host.test",
            Some("shop@host.test"),
        ),
    )
    .unwrap();
    let id = m.snapshot().threads[0].thread_id.clone();
    assert!(painted(&m, &id).iter().all(|(_, tag)| tag.is_none()));
}
