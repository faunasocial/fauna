//! tier_1: the **receive hook fires end-to-end** — inbound mail arriving on the
//! real shared receive path reaches the index builder, sealed and published.
//!
//! Proof obligation (`docs/goal/behavior/content-index.md` § Ingest triggers,
//! v1 → *The receive hook*): the shared client receive path gains an index-sink
//! observer that app glue registers, and every inbound mail record ingested
//! through it is offered to the sink. The unit tests beside the builder prove
//! the *sink* seals and publishes correctly; this proves the **wire between
//! them** — the half a symbol-existence check cannot see.
//!
//! The flow asserted, end to end:
//!
//! ```text
//! backends::smtp::ingest_inbound_record (plaintext RFC5322 in hand)
//!   -> ConversationsManager::ingest_inbound  (dedup + thread id settle here)
//!     -> observe_for_index                   (the seam)
//!       -> IndexBuilder::observe_indexable_message  (stages a doc)
//!         -> flush()                         (tokenize -> seal -> publish)
//! ```

use std::sync::{Arc, Mutex};

use fauna_client_index::{IndexBuildError, IndexBuilder, RailEntry, SegmentRail};
use fauna_conversations::backend::{InboundMailRecord, OutboundMailSink};
use fauna_conversations::backends::smtp::{SmtpBackend, ingest_inbound_record};
use fauna_conversations::manager::ConversationsManager;
use fauna_index::{
    ContentId, ContentKind, Index, IndexSegmentKey, mailcal_manifest_path, segment_path,
};

#[derive(Default)]
struct Recorder {
    published: Mutex<Vec<(String, Vec<u8>)>>,
}

#[async_trait::async_trait]
impl SegmentRail for Recorder {
    /// Empty: this suite pins the receive-path flow, not compaction, and these
    /// manifests never reach the fold threshold. An empty listing keeps that
    /// true by construction rather than by arithmetic.
    async fn list_entries(&self) -> Result<Vec<RailEntry>, IndexBuildError> {
        Ok(Vec::new())
    }

    async fn fetch_blob(&self, blob_hash: &str) -> Result<Vec<u8>, IndexBuildError> {
        Err(IndexBuildError::Publish {
            path: blob_hash.to_string(),
            reason: "the flow recorder serves no blobs".into(),
        })
    }

    async fn publish(&self, path: &str, bytes: &[u8]) -> Result<(), IndexBuildError> {
        self.published
            .lock()
            .unwrap()
            .push((path.to_string(), bytes.to_vec()));
        Ok(())
    }
}

impl Recorder {
    fn bytes_at(&self, path: &str) -> Option<Vec<u8>> {
        self.published
            .lock()
            .unwrap()
            .iter()
            .rev()
            .find(|(p, _)| p == path)
            .map(|(_, b)| b.clone())
    }
}

/// The manager needs an outbound sink to register the SMTP rail; this test
/// never sends, so it refuses.
struct NoSend;

#[async_trait::async_trait]
impl OutboundMailSink for NoSend {
    async fn submit(&self, _recipients: Vec<String>, _raw: Vec<u8>) -> Result<(), String> {
        Err("this test never sends".into())
    }
}

const MSEK: [u8; 32] = [7u8; 32];

fn record(uid: u32, message_id: &str, subject: &str, body: &str) -> InboundMailRecord {
    let raw = format!(
        "From: someone@example.com\r\n\
         To: me@example.com\r\n\
         Subject: {subject}\r\n\
         Message-ID: {message_id}\r\n\
         \r\n\
         {body}\r\n"
    );
    InboundMailRecord {
        uid,
        message_id: uid.to_be_bytes().to_vec(),
        internal_date_ms: 1_700_000_000_000,
        rfc5322: raw.into_bytes(),
        mailbox: fauna_conversations::backend::MailFeed::Inbox,
        suppress_from_view: false,
        has_seen_flag: false,
    }
}

fn manager_with_builder() -> (Arc<ConversationsManager>, Arc<IndexBuilder>, Arc<Recorder>) {
    let manager = ConversationsManager::new();
    manager.register_backend(Arc::new(SmtpBackend::new(
        Arc::new(NoSend),
        "me@example.com",
    )));
    let recorder = Arc::new(Recorder::default());
    let builder = Arc::new(IndexBuilder::mail(&MSEK, recorder.clone()));
    // The one line of app glue the whole feature asks of a client.
    manager.set_index_observer(builder.clone());
    (manager, builder, recorder)
}

#[tokio::test]
async fn inbound_mail_reaches_the_builder_and_lands_searchable_in_a_sealed_segment() {
    let (manager, builder, recorder) = manager_with_builder();

    let ingested = ingest_inbound_record(
        &manager,
        &record(
            1,
            "<lunch@example.com>",
            "Lunch plans",
            "meet at the harbour",
        ),
    )
    .expect("ingest");
    assert!(ingested, "the record carried a usable From");
    assert_eq!(
        builder.pending_len(),
        1,
        "the receive path reached the index sink — if this is 0 the observer \
         seam is not wired at the ingest chokepoint"
    );

    let mut published = builder.flush().await.expect("flush");
    assert_eq!(published.len(), 1, "one kind staged, so one segment");
    let sealed = published.remove(0);
    assert_eq!(sealed.path, segment_path(ContentKind::Mail, 1));

    // The published segment is queryable with the MSEK-derived key, and the
    // hit's content id is the RFC Message-ID the manager threaded on — which is
    // what gives a local result row a real navigation target.
    let key =
        IndexSegmentKey::from_bytes(*fauna_mls::wrapped_blob::derive_index_segment_key(&MSEK));
    let bytes = recorder.bytes_at(&sealed.path).expect("segment published");
    let index = Index::open_encrypted_mailcal(&bytes, &key).expect("open");

    for term in ["harbour", "lunch"] {
        let hits = index
            .query(term, &[ContentKind::Mail], None, 10)
            .unwrap_or_else(|e| panic!("query `{term}`: {e}"));
        assert_eq!(hits.len(), 1, "`{term}` finds the mail (body and subject)");
        assert_eq!(
            hits[0].content_id,
            ContentId(b"<lunch@example.com>".to_vec()),
            "the hit carries the producer-owned message id"
        );
    }

    assert!(
        recorder.bytes_at(&mailcal_manifest_path()).is_some(),
        "the mail/calendar manifest is published beside the segment"
    );
}

#[tokio::test]
async fn a_message_the_manager_dedups_is_never_indexed_twice() {
    let (manager, builder, _rec) = manager_with_builder();
    let rec = record(1, "<dup@example.com>", "Same", "same body");

    ingest_inbound_record(&manager, &rec).expect("first ingest");
    // Re-delivered by a second poll pass / a Sent-copy echo: `ingest_inbound`
    // drops it on the message-id dedup before the seam fires. Hooking one frame
    // higher (`ingest_inbound_record`) would have staged it twice.
    ingest_inbound_record(&manager, &rec).expect("second ingest");

    assert_eq!(
        builder.pending_len(),
        1,
        "the seam sits past the manager's message-id dedup"
    );
}

#[tokio::test]
async fn a_client_that_registers_no_sink_is_structurally_unaffected() {
    let manager = ConversationsManager::new();
    manager.register_backend(Arc::new(SmtpBackend::new(
        Arc::new(NoSend),
        "me@example.com",
    )));
    // No `set_index_observer` — the web SPA's shape (no browser tantivy).
    let ingested = ingest_inbound_record(&manager, &record(1, "<a@example.com>", "s", "b"))
        .expect("ingest still succeeds with no sink installed");
    assert!(ingested);
    assert_eq!(
        manager.snapshot().threads.len(),
        1,
        "the mail still lands in the thread view — the index seam is an \
         observer, never a hard dependency of receiving mail"
    );
}
