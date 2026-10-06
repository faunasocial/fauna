use async_trait::async_trait;
use fauna_conversations::ConversationsManager;
use fauna_conversations::Rail;
use fauna_conversations::address::TypedAddress;
use fauna_conversations::attachment_blocks;
use fauna_conversations::backend::{
    InboundMailPage, InboundMailRecord, InboundMailSource, MailFeed, OutboundMailSink, RailBackend,
    ResolveResult, SkippedMailRecord,
};
use fauna_conversations::backends::smtp::{SmtpBackend, ingest_inbound_record, poll_inbound_mail};
use fauna_conversations::capabilities::derive_capabilities;
use fauna_conversations::compose::ComposeState;
use fauna_conversations::snapshot::ThreadDetail;
use fauna_conversations::thread::{ThreadFlavor, ThreadId};
use fauna_conversations::{BodyFormat, MessageId, RailInboundMessage};
use std::collections::{HashSet, VecDeque};
use std::sync::{Arc, Mutex};

/// Records what `SmtpBackend::send` submitted, and can be primed to fail.
#[derive(Default)]
struct CapturingSink {
    calls: Mutex<Vec<(Vec<String>, Vec<u8>)>>,
    fail_with: Mutex<Option<String>>,
}

#[async_trait]
impl OutboundMailSink for CapturingSink {
    async fn submit(&self, recipients: Vec<String>, raw_rfc5322: Vec<u8>) -> Result<(), String> {
        if let Some(reason) = self.fail_with.lock().unwrap().clone() {
            return Err(reason);
        }
        self.calls.lock().unwrap().push((recipients, raw_rfc5322));
        Ok(())
    }
}

fn backend_with(sink: Arc<CapturingSink>, self_addr: &str) -> SmtpBackend {
    SmtpBackend::new(sink, self_addr)
}

fn null_backend() -> SmtpBackend {
    SmtpBackend::new(Arc::new(CapturingSink::default()), "me@localhost")
}

fn email(addr: &str) -> TypedAddress {
    TypedAddress::Email {
        email_address: addr.to_string(),
    }
}

fn smtp_thread(participants: Vec<TypedAddress>) -> ThreadDetail {
    ThreadDetail {
        thread_id: ThreadId("t-1".into()),
        rail: Rail::Smtp,
        glyph: Rail::Smtp.glyph(),
        flavor: ThreadFlavor::SubjectKeyed,
        label: "thread".into(),
        participant_displays: participants.iter().map(|p| p.display()).collect(),
        participants,
        capabilities: derive_capabilities(Rail::Smtp, ThreadFlavor::SubjectKeyed),
        messages: vec![],
        compose: ComposeState::default(),
        selected_message_id: None,
        bridge: None,
        guardian_state: None,
        room: None,
    }
}

fn inbound_from(sender: &str) -> RailInboundMessage {
    RailInboundMessage {
        rail: Rail::Smtp,
        sender: email(sender),
        recipients: vec![email("me@localhost")],
        subject: Some("subject".into()),
        body: "a received message".into(),
        body_format: BodyFormat::PlainText,
        timestamp_ms: 0,
        message_id: MessageId("msg-1".into()),
        in_reply_to: None,
        attachments: vec![],
        badges: Default::default(),
        legal_takedown_ref: None,
        plane_ref: None,
    }
}

// ── resolve / inbound (pure, no sink needed) ──────────────────────────

/// Pin:
/// `SmtpBackend::bucket_inbound` must derive `is_own == false` for a message a real
/// sender sent — the field the mark-as-spam gesture gates
/// (`dm-message-mark-as-spam-button`), shipped `!is_own`-gated on all 7 apps: linux
/// `message_bubble.rs`, windows `DmMessageBubble.xaml.cs`, macOS+iOS's shared
/// `DmMessageBubble.swift`, android `ConversationDetailScreen.kt`, web `+page.svelte`,
/// tui `conversations/mod.rs`. It does **not** gate
/// `ConversationsManager::observe_local_detection` — that hook's sole call site is the
/// MLS rail's `ingest_inbound_with_server_labels` (`manager.rs:2223`); SMTP routes
/// through `ingest_inbound` (`manager.rs:1452`) → `ingest_inbound_identified`
/// (`manager.rs:1469`), which never calls it. This identical overclaim survived into this comment regardless.
/// `bucket_inbound` derived `is_own` from a sender-vs-self-address compare for one
/// track, then replaced that compare with the record's `mailbox`
/// **provenance** (`Some(MailFeed::Sent)`, never the forgeable `From:` header) —
/// that compare was itself forgeable by a `From:` claiming the self address
/// (see [`bucket_inbound_derives_is_own_false_for_a_forged_self_address_inbox_message`]
/// below, that finding's actual fix). This test and
/// [`ingest_inbound_record_derives_is_own_true_for_a_sent_mailbox_self_authored_message`]
/// below are the two halves of one witness. Red-verify by reverting `mailbox ==
/// Some(MailFeed::Sent)` to an unconditional `false`: this test keeps passing but the
/// other reddens; reverting to an unconditional `true` flips which one reddens.
/// Restore the real derivation before commit.
#[test]
fn bucket_inbound_derives_is_own_false_for_a_received_message() {
    let backend = null_backend();
    let bucket = backend
        .bucket_inbound(
            inbound_from("someone-else@example.com"),
            Some(MailFeed::Inbox),
        )
        .expect("a well-formed inbound message buckets cleanly");
    assert!(
        !bucket.message.is_own,
        "a message from a real sender must never read as our own — doing so \
         silently removes the mark-as-spam gesture for this message on every app"
    );
}

/// **The vulnerability:** a message
/// merely *claiming* the self address in `From:` is not proof of anything — `From:`
/// is written by whoever sends the mail. Before this fix `bucket_inbound` compared
/// the header against the live self-address cell, so this exact record read as the
/// user's own message and lost the mark-as-spam gesture
/// (`dm-message-mark-as-spam-button`, gated `!is_own`) — a standing phishing lever
/// ("mail from yourself"). Pin by mutation: swap the `mailbox == Some(MailFeed::Sent)`
/// compare in `bucket_inbound` back to a sender-vs-self-address compare and this
/// reddens, since the record's `From:` is forged to equal the self address while its
/// mailbox is `INBOX`, never `Sent`.
#[test]
fn bucket_inbound_derives_is_own_false_for_a_forged_self_address_inbox_message() {
    let backend = backend_with(Arc::new(CapturingSink::default()), "me@host.test");
    let bucket = backend
        .bucket_inbound(inbound_from("me@host.test"), Some(MailFeed::Inbox))
        .expect("a well-formed inbound message buckets cleanly");
    assert!(
        !bucket.message.is_own,
        "an INBOX record can never prove authorship, however its `From:` reads — \
         trusting it here would silently remove the mark-as-spam gesture from an \
         attacker's own phishing message"
    );
}

#[tokio::test]
async fn resolves_well_formed_email() {
    let backend = null_backend();
    let result = backend.resolve_address("alice@example.com").await;
    assert_eq!(
        result,
        ResolveResult::Resolved(TypedAddress::Email {
            email_address: "alice@example.com".into(),
        })
    );
}

#[tokio::test]
async fn rejects_malformed_address_no_at() {
    let backend = null_backend();
    let result = backend.resolve_address("notanemail").await;
    assert_eq!(result, ResolveResult::NotFound);
}

#[tokio::test]
async fn rejects_leading_at() {
    let backend = null_backend();
    let result = backend.resolve_address("@example.com").await;
    assert_eq!(result, ResolveResult::NotFound);
}

#[tokio::test]
async fn rejects_trailing_at() {
    let backend = null_backend();
    let result = backend.resolve_address("user@").await;
    assert_eq!(result, ResolveResult::NotFound);
}

// ── send ──────────────────────────────────────────────────────────────

#[tokio::test]
async fn send_assembles_message_and_submits_to_recipients() {
    let sink = Arc::new(CapturingSink::default());
    let backend = backend_with(sink.clone(), "alice@localhost");
    let thread = smtp_thread(vec![email("bob@external.test")]);
    let compose = ComposeState {
        body_draft: "Hello from the client".into(),
        subject_draft: Some("A subject".into()),
        ..Default::default()
    };

    let outcome = backend.send(&thread, &compose, &[]).await.expect("send ok");

    let calls = sink.calls.lock().unwrap();
    assert_eq!(calls.len(), 1, "exactly one submission");
    let (recipients, raw) = &calls[0];
    assert_eq!(recipients, &vec!["bob@external.test".to_string()]);

    let text = String::from_utf8(raw.clone()).unwrap();
    assert!(text.contains("From: alice@localhost\r\n"), "{text:?}");
    assert!(text.contains("To: bob@external.test\r\n"));
    assert!(text.contains("Subject: A subject\r\n"));
    assert!(text.contains("Hello from the client"));
    // The returned message-id is the one embedded in the assembled message.
    assert!(text.contains(&format!("Message-ID: {}\r\n", outcome.message_id.0)));
    assert_eq!(
        outcome.sender,
        TypedAddress::Email {
            email_address: "alice@localhost".into()
        }
    );
}

#[tokio::test]
async fn send_excludes_self_from_recipients() {
    let sink = Arc::new(CapturingSink::default());
    let backend = backend_with(sink.clone(), "alice@localhost");
    // A reply thread whose participant set includes self (inbound bucketing
    // adds self) — self must not appear in the envelope recipients.
    let thread = smtp_thread(vec![email("alice@localhost"), email("bob@external.test")]);
    let compose = ComposeState {
        body_draft: "reply".into(),
        ..Default::default()
    };

    backend.send(&thread, &compose, &[]).await.expect("send ok");
    let calls = sink.calls.lock().unwrap();
    assert_eq!(calls[0].0, vec!["bob@external.test".to_string()]);
}

#[tokio::test]
async fn send_uses_reply_recipients_subset_when_set() {
    let sink = Arc::new(CapturingSink::default());
    let backend = backend_with(sink.clone(), "alice@localhost");
    // The thread has three peers, but the reply draft was edited down to one:
    // the editable To line must win over the historical participant set, so
    // only `bob` is addressed — not `carol`/`dave` (`conversations.md`
    // § Participants vs reply recipients).
    let thread = smtp_thread(vec![
        email("alice@localhost"),
        email("bob@external.test"),
        email("carol@external.test"),
        email("dave@external.test"),
    ]);
    let compose = ComposeState {
        body_draft: "reply to bob only".into(),
        reply_recipients: vec![email("bob@external.test")],
        ..Default::default()
    };

    backend.send(&thread, &compose, &[]).await.expect("send ok");
    let calls = sink.calls.lock().unwrap();
    assert_eq!(calls[0].0, vec!["bob@external.test".to_string()]);
}

#[tokio::test]
async fn send_drops_self_from_reply_recipients() {
    let sink = Arc::new(CapturingSink::default());
    let backend = backend_with(sink.clone(), "alice@localhost");
    // Even if self somehow ends up in the reply set, it never goes on the wire.
    let thread = smtp_thread(vec![email("alice@localhost"), email("bob@external.test")]);
    let compose = ComposeState {
        body_draft: "reply-all".into(),
        reply_recipients: vec![email("Alice@LOCALHOST"), email("bob@external.test")],
        ..Default::default()
    };

    backend.send(&thread, &compose, &[]).await.expect("send ok");
    let calls = sink.calls.lock().unwrap();
    assert_eq!(calls[0].0, vec!["bob@external.test".to_string()]);
}

#[test]
fn self_address_is_the_send_from() {
    let backend = backend_with(Arc::new(CapturingSink::default()), "alice@localhost");
    assert_eq!(
        backend.self_address(),
        Some(TypedAddress::Email {
            email_address: "alice@localhost".into()
        })
    );
}

#[tokio::test]
async fn send_with_no_email_recipients_errors() {
    let backend = null_backend();
    // Only self → nothing to send to.
    let thread = smtp_thread(vec![email("me@localhost")]);
    let err = backend
        .send(&thread, &ComposeState::default(), &[])
        .await
        .expect_err("no recipients must error");
    assert!(matches!(
        err,
        fauna_conversations::backend::BackendError::Refusal(_)
    ));
}

#[tokio::test]
async fn send_without_a_self_address_is_refused_before_reaching_the_sink() {
    // mail-app-surface.md § First-party client send: the nest does sender-handle
    // verification on every `fauna.email.send`. An app that cannot resolve the
    // user's own `<handle>@<domain>` has no address it may legitimately claim,
    // so it must refuse locally — the same shape as the inline-ceiling
    // pre-check below.
    //
    // Before this pin the empty address was carried straight into
    // `rfc5322::build_message`, which emits a literal `From: \r\n`. That is
    // doubly wrong: it puts a malformed RFC 5322 message on the public
    // internet, AND it *evades* the nest's gate — `email_handlers.rs`'s
    // `from_addr.rsplit_once('@')` finds no `@`, so the same-domain handle
    // check never runs. Six apps shipped that path (linux's `app.rs` yields
    // `String::new()` whenever the account cache has no handle, which is
    // exactly what an admin-admitted actor has).
    let sink = Arc::new(CapturingSink::default());
    let backend = backend_with(sink.clone(), "");
    let thread = smtp_thread(vec![email("bob@external.test")]);
    let compose = ComposeState {
        body_draft: "Hello from an account with no handle".into(),
        ..Default::default()
    };

    let err = backend
        .send(&thread, &compose, &[])
        .await
        .expect_err("a send with no resolvable From must be refused");
    match err {
        fauna_conversations::backend::BackendError::Refusal(message) => {
            assert_eq!(message, fauna_i18n::strings::error::email::NO_HANDLE);
        }
        other => panic!("expected BackendError::Refusal(no-handle message), got {other:?}"),
    }
    assert!(
        sink.calls.lock().unwrap().is_empty(),
        "the sink must never see a send with no From"
    );
}

#[tokio::test]
async fn send_with_a_domainless_self_address_is_refused_too() {
    // The guard is "is this a usable address", not "is this non-empty" — a
    // bare handle with no domain would render `From: alice\r\n` and evade the
    // gate identically (`rsplit_once('@')` → None). tui's
    // `resolve_self_address` already refuses to build one, so this pins the
    // shared floor under every app rather than one app's caller.
    let sink = Arc::new(CapturingSink::default());
    let backend = backend_with(sink.clone(), "alice");
    let thread = smtp_thread(vec![email("bob@external.test")]);

    let err = backend
        .send(&thread, &ComposeState::default(), &[])
        .await
        .expect_err("a domainless From must be refused");
    assert!(matches!(
        err,
        fauna_conversations::backend::BackendError::Refusal(ref m)
            if m == fauna_i18n::strings::error::email::NO_HANDLE
    ));
    assert!(sink.calls.lock().unwrap().is_empty());
}

#[tokio::test]
async fn send_with_an_empty_local_part_self_address_is_refused_too() {
    // conversations.md § Self-address: live, never baked — an empty local part
    // (`"@nest.example"`) counts as unresolved, exactly like a missing address.
    // The shape is reachable in production: a client that synthesizes
    // `<handle-or-empty>@<nest-URL-host>` before the handle resolves builds
    // precisely this string, which slips past a bare `contains('@')` guard and
    // then claims a handle the nest does not back — the § Errors & edge cases
    // forbidden substitute (the nest refuses it with an RPC code instead of the
    // honest local `no_handle`).
    let sink = Arc::new(CapturingSink::default());
    let backend = backend_with(sink.clone(), "@nest.example");
    let thread = smtp_thread(vec![email("bob@external.test")]);

    let err = backend
        .send(&thread, &ComposeState::default(), &[])
        .await
        .expect_err("an empty-local-part From must be refused");
    assert!(matches!(
        err,
        fauna_conversations::backend::BackendError::Refusal(ref m)
            if m == fauna_i18n::strings::error::email::NO_HANDLE
    ));
    assert!(sink.calls.lock().unwrap().is_empty());
}

#[tokio::test]
async fn set_self_address_heals_a_backend_built_before_identity_resolved() {
    // conversations.md § Self-address: live, never baked — the address is a live
    // cell read at send time, so a backend built in the identity-resolution race
    // window (empty address) starts sending the moment the address lands,
    // without being rebuilt. Pre-cell, web baked the race-window `''` into its
    // cached manager permanently: every send refused `no_handle` forever, even
    // after the handle resolved.
    let sink = Arc::new(CapturingSink::default());
    let cell = fauna_conversations::backend::SelfAddress::new("");
    let backend = SmtpBackend::new_shared(sink.clone(), Arc::clone(&cell));
    let thread = smtp_thread(vec![email("bob@external.test")]);
    let compose = ComposeState {
        body_draft: "sent after the handle resolved".into(),
        ..Default::default()
    };

    backend
        .send(&thread, &compose, &[])
        .await
        .expect_err("unresolved address still refuses");
    assert!(sink.calls.lock().unwrap().is_empty());

    cell.set("alice@nest-a.test");
    backend
        .send(&thread, &compose, &[])
        .await
        .expect("the resolved address heals the same backend instance");
    let calls = sink.calls.lock().unwrap();
    assert_eq!(calls.len(), 1);
    let raw = String::from_utf8_lossy(&calls[0].1);
    assert!(
        raw.contains("From: alice@nest-a.test"),
        "the send-time read picks up the resolved address; got:\n{raw}"
    );
}

#[tokio::test]
async fn a_mid_session_rename_updates_from_and_self_drop_on_the_next_send() {
    // conversations.md § Self-address: live, never baked — a server-side handle
    // rename reaches the send path with no rebuild: the next send's `From:` is
    // the new address, and the reply-recipient self-drop compares against the
    // new address too (the renamed self is no longer mailed a copy).
    let sink = Arc::new(CapturingSink::default());
    let cell = fauna_conversations::backend::SelfAddress::new("alice@nest-a.test");
    let backend = SmtpBackend::new_shared(sink.clone(), Arc::clone(&cell));
    let compose = ComposeState {
        body_draft: "hello".into(),
        ..Default::default()
    };

    let thread = smtp_thread(vec![email("bob@external.test")]);
    backend
        .send(&thread, &compose, &[])
        .await
        .expect("send before the rename");

    cell.set("alicia@nest-b.test");
    // The thread's historical participants include the RENAMED self — the
    // self-drop must key on the current address, not the construction-time one.
    let thread = smtp_thread(vec![
        email("alicia@nest-b.test"),
        email("bob@external.test"),
    ]);
    backend
        .send(&thread, &compose, &[])
        .await
        .expect("send after the rename");

    let calls = sink.calls.lock().unwrap();
    assert_eq!(calls.len(), 2);
    let first = String::from_utf8_lossy(&calls[0].1);
    assert!(first.contains("From: alice@nest-a.test"));
    let second = String::from_utf8_lossy(&calls[1].1);
    assert!(
        second.contains("From: alicia@nest-b.test"),
        "the next send carries the renamed From; got:\n{second}"
    );
    assert_eq!(
        calls[1].0,
        vec!["bob@external.test".to_string()],
        "the renamed self is dropped from recipients by the CURRENT address"
    );
}

#[tokio::test]
async fn send_over_inline_ceiling_is_refused_before_reaching_the_sink() {
    // smtp-server.md § Message size limits: the assembled request rides the WS
    // frame inline, so an over-inline-ceiling raw message must be refused
    // locally — never handed to the sink, which would either overflow the
    // frame and sever the connection, or (in a fake) silently "succeed" on a
    // send that could never actually cross the wire.
    let sink = Arc::new(CapturingSink::default());
    let backend = backend_with(sink.clone(), "alice@localhost");
    let thread = smtp_thread(vec![email("bob@external.test")]);
    let compose = ComposeState {
        body_draft: "x".repeat(fauna_mail::transport_limits::MAX_INLINE_RAW_MESSAGE_BYTES as usize),
        ..Default::default()
    };

    let err = backend
        .send(&thread, &compose, &[])
        .await
        .expect_err("over-ceiling send must be refused");
    match err {
        fauna_conversations::backend::BackendError::Refusal(message) => {
            assert_eq!(message, fauna_i18n::strings::error::email::TOO_LARGE);
        }
        other => panic!("expected BackendError::Refusal(too-large message), got {other:?}"),
    }
    assert!(
        sink.calls.lock().unwrap().is_empty(),
        "the sink must never see an over-ceiling send"
    );
}

#[tokio::test]
async fn send_at_inline_ceiling_boundary_still_submits() {
    // The pre-check compares the fully-assembled request (headers + MIME
    // framing included), not the bare body draft, so a body sized to land the
    // assembled message exactly one byte under the ceiling must still submit.
    let sink = Arc::new(CapturingSink::default());
    let backend = backend_with(sink.clone(), "alice@localhost");
    let thread = smtp_thread(vec![email("bob@external.test")]);
    let compose = ComposeState {
        body_draft: "x".repeat(1_000),
        ..Default::default()
    };

    backend
        .send(&thread, &compose, &[])
        .await
        .expect("well-under-ceiling send must submit");
    assert_eq!(sink.calls.lock().unwrap().len(), 1);
}

#[tokio::test]
async fn send_propagates_sink_transport_error() {
    let sink = Arc::new(CapturingSink::default());
    *sink.fail_with.lock().unwrap() = Some("nest unreachable".into());
    let backend = backend_with(sink, "alice@localhost");
    let thread = smtp_thread(vec![email("bob@external.test")]);
    let err = backend
        .send(&thread, &ComposeState::default(), &[])
        .await
        .expect_err("transport error must propagate");
    match err {
        fauna_conversations::backend::BackendError::Transport(reason) => {
            assert_eq!(reason.as_str(), "nest unreachable")
        }
        other => panic!("expected Transport, got {other:?}"),
    }
}

// ── inbound receive driver (poll_inbound_mail) ────────────────────────

fn inbound_record(uid: u32, id: &[u8], from: &str, subject: &str, body: &str) -> InboundMailRecord {
    // Stands in for the decrypted RFC 5322 the platform's InboundMailSource
    // produces after decode-outer + open_mail_record.
    let raw = format!("From: {from}\r\nTo: me@host.test\r\nSubject: {subject}\r\n\r\n{body}\r\n");
    InboundMailRecord {
        uid,
        message_id: id.to_vec(),
        internal_date_ms: 1_700_000_000_000,
        rfc5322: raw.into_bytes(),
        mailbox: MailFeed::Inbox,
        suppress_from_view: false,
        has_seen_flag: false,
    }
}

/// Returns its fixed records as one page, honoring the `after_uid > uid` cursor
/// like the real feed.
struct FixedInbound {
    records: Vec<InboundMailRecord>,
}

#[async_trait]
impl InboundMailSource for FixedInbound {
    async fn fetch(&self, after_uid: u32, _limit: u32) -> Result<InboundMailPage, String> {
        let records = self
            .records
            .iter()
            .filter(|r| r.uid > after_uid)
            .cloned()
            .collect();
        Ok(InboundMailPage {
            skipped: Vec::new(),
            highest_modseq: 0,
            records,
            more: false,
        })
    }
}

/// Pops one queued page per `fetch` — drives the `more=true` paging loop.
struct PagedInbound {
    pages: Mutex<VecDeque<InboundMailPage>>,
}

#[async_trait]
impl InboundMailSource for PagedInbound {
    async fn fetch(&self, _after_uid: u32, _limit: u32) -> Result<InboundMailPage, String> {
        Ok(self.pages.lock().unwrap().pop_front().unwrap_or_default())
    }
}

fn smtp_manager() -> Arc<ConversationsManager> {
    let m = ConversationsManager::new();
    m.register_backend(Arc::new(SmtpBackend::new(
        Arc::new(CapturingSink::default()),
        "me@host.test",
    )));
    m
}

#[tokio::test]
async fn poll_ingests_decrypted_mail_into_a_thread() {
    let m = smtp_manager();
    let source = FixedInbound {
        records: vec![inbound_record(
            5,
            b"seg-id-0001",
            "alice@external.test",
            "Lunch?",
            "Are you free today?",
        )],
    };
    let mut cursor = 0u32;
    let mut seen = HashSet::new();

    let n = poll_inbound_mail(&source, &m, &mut cursor, &mut seen, 50)
        .await
        .expect("poll ok");

    assert_eq!(n, 1, "one new message ingested");
    assert_eq!(cursor, 5, "cursor advanced to the highest UID");
    let snap = m.snapshot();
    assert_eq!(snap.threads.len(), 1, "one subject-keyed thread");
    assert_eq!(snap.threads[0].label, "Lunch?");
    let detail = m.thread_detail(snap.threads[0].thread_id.clone()).unwrap();
    assert_eq!(detail.messages.len(), 1);
    assert_eq!(detail.messages[0].body, "Are you free today?");
    assert_eq!(
        detail.messages[0].sender,
        TypedAddress::Email {
            email_address: "alice@external.test".into()
        }
    );
}

#[tokio::test]
async fn poll_dedups_by_server_message_id_across_calls() {
    let m = smtp_manager();
    let source = FixedInbound {
        records: vec![inbound_record(
            5,
            b"seg-id-0001",
            "alice@external.test",
            "Lunch?",
            "body",
        )],
    };
    let mut seen = HashSet::new();

    // First poll ingests it.
    let mut cursor = 0u32;
    assert_eq!(
        poll_inbound_mail(&source, &m, &mut cursor, &mut seen, 50)
            .await
            .unwrap(),
        1
    );
    // Re-poll from a reset cursor (e.g. a fresh feed read) — the same segment
    // record is returned, but `seen` keeps it from re-reaching ingest at all (the
    // cheap per-feed guard, keyed on segment id). The manager ALSO dedups by RFC
    // Message-ID (see `ingest_dedups_same_rfc_message_id_across_server_records`),
    // which is what catches the local-echo-vs-Sent-copy case `seen` can't; here
    // `seen` short-circuits first.
    let mut cursor = 0u32;
    assert_eq!(
        poll_inbound_mail(&source, &m, &mut cursor, &mut seen, 50)
            .await
            .unwrap(),
        0,
        "already-seen record is not re-ingested"
    );
    let snap = m.snapshot();
    let detail = m.thread_detail(snap.threads[0].thread_id.clone()).unwrap();
    assert_eq!(detail.messages.len(), 1, "no duplicate message");
}

/// A source that marks some records `suppress_from_view` (the on-device spam
/// scorer's junk verdict) and counts its per-pass lifecycle calls, so the driver's
/// contract is checked without a real nest.
struct LifecycleInbound {
    records: Vec<InboundMailRecord>,
    began: std::sync::atomic::AtomicUsize,
    ended: std::sync::atomic::AtomicUsize,
}

#[async_trait]
impl InboundMailSource for LifecycleInbound {
    async fn fetch(&self, after_uid: u32, _limit: u32) -> Result<InboundMailPage, String> {
        let records = self
            .records
            .iter()
            .filter(|r| r.uid > after_uid)
            .cloned()
            .collect();
        Ok(InboundMailPage {
            skipped: Vec::new(),
            highest_modseq: 0,
            records,
            more: false,
        })
    }
    async fn begin_pass(&self) {
        self.began.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
    }
    async fn end_pass(&self) {
        self.ended.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
    }
}

/// A junk verdict (`suppress_from_view`) is kept OUT of the thread view, but the
/// cursor still advances past it (so it isn't re-fetched) and it is dedup-tracked;
/// `begin_pass`/`end_pass` fire exactly once (`mail-spam.md` § Re-file timing).
#[tokio::test]
async fn poll_suppresses_scored_spam_from_view_but_advances_cursor() {
    let m = smtp_manager();
    let ham = inbound_record(
        5,
        b"seg-ham",
        "alice@external.test",
        "Lunch?",
        "free today?",
    );
    let mut spam = inbound_record(
        6,
        b"seg-spam",
        "spammer@bad.test",
        "WINNER",
        "claim your prize",
    );
    spam.suppress_from_view = true; // the on-device scorer classified it spam
    let source = LifecycleInbound {
        records: vec![ham, spam],
        began: std::sync::atomic::AtomicUsize::new(0),
        ended: std::sync::atomic::AtomicUsize::new(0),
    };
    let mut cursor = 0u32;
    let mut seen = HashSet::new();

    let n = poll_inbound_mail(&source, &m, &mut cursor, &mut seen, 50)
        .await
        .expect("poll ok");

    assert_eq!(
        n, 1,
        "only the ham message is ingested (the scored spam is suppressed)"
    );
    assert_eq!(
        cursor, 6,
        "cursor advances past the suppressed spam so it isn't re-fetched"
    );
    assert!(
        seen.contains(b"seg-spam".as_slice()),
        "the suppressed spam is still dedup-tracked"
    );
    let snap = m.snapshot();
    assert_eq!(snap.threads.len(), 1, "only the ham thread is in the view");
    assert_eq!(snap.threads[0].label, "Lunch?");
    assert_eq!(
        source.began.load(std::sync::atomic::Ordering::SeqCst),
        1,
        "begin_pass fires once per pass"
    );
    assert_eq!(
        source.ended.load(std::sync::atomic::Ordering::SeqCst),
        1,
        "end_pass fires once per pass"
    );
}

/// A source whose `fetch` errors mid-pass — `end_pass` must still run so a junk
/// verdict recorded on an earlier page is still flushed (watermark + move), never
/// suppressed-but-left-in-INBOX.
struct ErroringInbound {
    ended: std::sync::atomic::AtomicUsize,
}

#[async_trait]
impl InboundMailSource for ErroringInbound {
    async fn fetch(&self, _after_uid: u32, _limit: u32) -> Result<InboundMailPage, String> {
        Err("boom".into())
    }
    async fn end_pass(&self) {
        self.ended.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
    }
}

#[tokio::test]
async fn end_pass_runs_even_when_fetch_errors() {
    let m = smtp_manager();
    let source = ErroringInbound {
        ended: std::sync::atomic::AtomicUsize::new(0),
    };
    let mut cursor = 0u32;
    let mut seen = HashSet::new();

    let r = poll_inbound_mail(&source, &m, &mut cursor, &mut seen, 50).await;

    assert!(r.is_err(), "the mid-pass fetch error propagates");
    assert_eq!(
        source.ended.load(std::sync::atomic::Ordering::SeqCst),
        1,
        "end_pass still flushes the accumulated disposition after a fetch error"
    );
}

#[tokio::test]
async fn poll_drains_multiple_pages() {
    let m = smtp_manager();
    let mut pages = VecDeque::new();
    pages.push_back(InboundMailPage {
        skipped: Vec::new(),
        highest_modseq: 0,
        records: vec![inbound_record(
            1,
            b"seg-1",
            "alice@external.test",
            "Topic",
            "first",
        )],
        more: true,
    });
    pages.push_back(InboundMailPage {
        skipped: Vec::new(),
        highest_modseq: 0,
        records: vec![inbound_record(
            2,
            b"seg-2",
            "alice@external.test",
            "Topic",
            "second",
        )],
        more: false,
    });
    let source = PagedInbound {
        pages: Mutex::new(pages),
    };
    let mut cursor = 0u32;
    let mut seen = HashSet::new();

    let n = poll_inbound_mail(&source, &m, &mut cursor, &mut seen, 1)
        .await
        .expect("poll ok");

    assert_eq!(n, 2, "both pages ingested");
    assert_eq!(cursor, 2);
    let snap = m.snapshot();
    // Same subject → both messages merge into one subject-keyed thread.
    assert_eq!(snap.threads.len(), 1);
    let detail = m.thread_detail(snap.threads[0].thread_id.clone()).unwrap();
    assert_eq!(detail.messages.len(), 2);
}

/// One record the source could not open does not stop the mailbox: the cursor
/// moves past it, the record after it ingests, and the manager counts it for
/// the page (`mail-app-surface.md` § Inbound client receive → *Unopenable
/// records*). A record that opens on a later pass retires its entry.
#[tokio::test]
async fn poll_skips_an_unopenable_record_and_keeps_receiving() {
    let m = smtp_manager();
    let mut pages = VecDeque::new();
    // uid 1 did not open and is the ONLY record on its page — the exact shape
    // that used to wedge the drain at `after_uid = 0` forever.
    pages.push_back(InboundMailPage {
        skipped: vec![SkippedMailRecord {
            mailbox: MailFeed::Inbox,
            uid: 1,
            reason: "open inner sealed mail record: HPKE open failed".into(),
        }],
        records: Vec::new(),
        more: true,
        highest_modseq: 0,
    });
    pages.push_back(InboundMailPage {
        skipped: Vec::new(),
        highest_modseq: 0,
        records: vec![inbound_record(
            2,
            b"seg-2",
            "alice@external.test",
            "Topic",
            "after the bad one",
        )],
        more: false,
    });
    let source = PagedInbound {
        pages: Mutex::new(pages),
    };
    let mut cursor = 0u32;
    let mut seen = HashSet::new();

    let n = poll_inbound_mail(&source, &m, &mut cursor, &mut seen, 1)
        .await
        .expect("a skipped record is not a poll error");

    assert_eq!(n, 1, "the record after the unopenable one ingested");
    assert_eq!(cursor, 2, "the cursor moved past both");
    assert_eq!(
        m.unopenable_mail_count(),
        1,
        "the skip is counted for the page"
    );
    let snap = m.snapshot();
    assert_eq!(snap.threads.len(), 1);

    // A later pass (a re-drain under changed keys) that opens uid 1 retires it.
    let source = FixedInbound {
        records: vec![inbound_record(
            1,
            b"seg-1",
            "alice@external.test",
            "Topic",
            "now it opens",
        )],
    };
    let mut cursor = 0u32;
    let mut seen = HashSet::new();
    poll_inbound_mail(&source, &m, &mut cursor, &mut seen, 50)
        .await
        .expect("poll ok");
    assert_eq!(
        m.unopenable_mail_count(),
        0,
        "an opened record retires its skip"
    );
}

#[tokio::test]
async fn poll_skips_unaddressable_message_without_stalling() {
    let m = smtp_manager();
    // A record with no From: can't be bucketed — it must be skipped, and the
    // following well-formed record still ingests.
    let source = FixedInbound {
        records: vec![
            InboundMailRecord {
                uid: 3,
                message_id: b"seg-bad".to_vec(),
                internal_date_ms: 1,
                rfc5322: b"To: me@host.test\r\nSubject: orphan\r\n\r\nno sender\r\n".to_vec(),
                mailbox: MailFeed::Inbox,
                suppress_from_view: false,
                has_seen_flag: false,
            },
            inbound_record(4, b"seg-good", "alice@external.test", "Hi", "ok"),
        ],
    };
    let mut cursor = 0u32;
    let mut seen = HashSet::new();

    let n = poll_inbound_mail(&source, &m, &mut cursor, &mut seen, 50)
        .await
        .expect("poll ok");

    assert_eq!(n, 1, "only the addressable message ingested");
    assert_eq!(cursor, 4, "cursor advances past the skipped record too");
    assert_eq!(m.snapshot().threads.len(), 1);
}

// ── single-record ingest (ingest_inbound_record) ──────────────────────
// The wasm client's JS-driven poll can't use the `InboundMailSource: Send + Sync`
// async seam (its WS-RPC client is `!Send`), so it fetches + decrypts itself and
// calls `ingest_inbound_record` per record. Assert that direct path buckets onto
// the Smtp rail identically to `poll_inbound_mail`.

#[test]
fn ingest_inbound_record_buckets_one_message_on_the_smtp_rail() {
    let m = smtp_manager();
    let rec = inbound_record(
        7,
        b"seg-direct",
        "bob@external.test",
        "Direct",
        "hello there",
    );

    let ingested = ingest_inbound_record(&m, &rec).expect("ingest ok");

    assert!(ingested, "an addressable record is ingested");
    let snap = m.snapshot();
    assert_eq!(snap.threads.len(), 1, "one subject-keyed thread");
    assert_eq!(snap.threads[0].rail, Rail::Smtp, "lands on the Smtp rail");
    assert_eq!(snap.threads[0].label, "Direct");
    let detail = m.thread_detail(snap.threads[0].thread_id.clone()).unwrap();
    assert_eq!(detail.messages[0].body, "hello there");
    assert_eq!(
        detail.messages[0].sender,
        TypedAddress::Email {
            email_address: "bob@external.test".into()
        }
    );
}

/// `conversations.md:1571`: the mail parse fills a subject that shows as the
/// thread's label. A raw `=?utf-8?B?…?=` there is a defect against that
/// text.
#[test]
fn ingest_inbound_record_decodes_an_rfc2047_encoded_word_subject() {
    let m = smtp_manager();
    let rec = inbound_record(
        8,
        b"seg-encoded-subject",
        "bob@external.test",
        "=?utf-8?B?THVuY2g=?=",
        "see you at noon",
    );

    let ingested = ingest_inbound_record(&m, &rec).expect("ingest ok");

    assert!(ingested, "an addressable record is ingested");
    let snap = m.snapshot();
    assert_eq!(snap.threads.len(), 1);
    assert_eq!(
        snap.threads[0].label, "Lunch",
        "the encoded-word subject must render decoded, not as the raw =?…?= form"
    );
}

/// An encoded and a plain spelling of one subject are the same subject,
/// decoded, not a mismatch — so subject-keyed threading (`manager.rs:1602`)
/// and the content index's Title (`manager.rs:1619-1621`) group them under one
/// thread with one label. Independent of `ingest_inbound_record_own_upgrade_*`
/// (a *same*-Message-ID compare): this pins the
/// *different*-Message-ID subject-keyed bucketing, unaffected by however that compare
/// is redesigned.
#[test]
fn ingest_inbound_record_threads_an_encoded_and_plain_subject_spelling_together() {
    let m = smtp_manager();
    let plain = inbound_record(
        9,
        b"seg-subject-plain",
        "bob@external.test",
        "Lunch",
        "noon?",
    );
    let encoded = inbound_record(
        10,
        b"seg-subject-encoded",
        "bob@external.test",
        "=?utf-8?B?THVuY2g=?=",
        "noon works",
    );

    assert!(ingest_inbound_record(&m, &plain).expect("ingest ok"));
    assert!(ingest_inbound_record(&m, &encoded).expect("ingest ok"));

    let snap = m.snapshot();
    assert_eq!(
        snap.threads.len(),
        1,
        "an encoded and a plain spelling of the same subject are not a mismatch \
         — both must key into one thread"
    );
    assert_eq!(snap.threads[0].label, "Lunch");
    let detail = m.thread_detail(snap.threads[0].thread_id.clone()).unwrap();
    assert_eq!(
        detail.messages.len(),
        2,
        "both messages land in that one thread"
    );
}

/// The scenario `bucket_inbound`'s mailbox-provenance derivation exists for: mail
/// sent from another MUA/device on the same account has no local echo to dedup
/// against, so it reaches here as an ordinary `Sent`-mailbox record
/// (`conversations.md:1571`, `session.rs:664-671`). The nest only ever files a
/// record into `Sent` via a submission this account authenticated
/// (`mail-app-surface.md` § Inbound client receive → *Sibling Sent feed*), so the
/// derivation doesn't need to look at `From:` at all — this record's header is left
/// in a different case than the stored self-address (a third-party client may write
/// it differently) specifically to prove that: `is_own` must come out `true` here
/// even though a header compare, case-insensitive or not, is no longer in the loop.
#[test]
fn ingest_inbound_record_derives_is_own_true_for_a_sent_mailbox_self_authored_message() {
    let m = smtp_manager(); // self address: "me@host.test"
    let rec = InboundMailRecord {
        uid: 11,
        message_id: b"seg-sent-self".to_vec(),
        internal_date_ms: 1_700_000_000_000,
        rfc5322: b"From: ME@Host.Test\r\nTo: bob@external.test\r\nSubject: Lunch\r\n\r\nsee you at noon\r\n"
            .to_vec(),
        mailbox: MailFeed::Sent,
        suppress_from_view: false,
        has_seen_flag: false,
    };

    let ingested = ingest_inbound_record(&m, &rec).expect("ingest ok");

    assert!(ingested, "an addressable Sent-mailbox record is ingested");
    let snap = m.snapshot();
    assert_eq!(snap.threads.len(), 1, "one thread");
    let detail = m.thread_detail(snap.threads[0].thread_id.clone()).unwrap();
    assert!(
        detail.messages[0].is_own,
        "mail this account sent from another MUA/device must read as our own — \
         missing this silently offers mark-as-spam on the user's own content"
    );
}

/// Self-addressed mail lands in **both** `INBOX` (delivery) and `Sent` (the durable
/// proof of the account's own send) under the same RFC Message-ID; `INBOX` is polled
/// first (`session.rs:1416-1431`), so this is the common arrival order. The `INBOX`
/// copy alone is never proof of authorship — see
/// [`bucket_inbound_derives_is_own_false_for_a_forged_self_address_inbox_message`] —
/// so it ingests `is_own == false`; the `Sent` copy that follows dedups by
/// Message-ID (`ingest_inbound_identified`) and must **upgrade** the already-held
/// message rather than leave it stranded as receivable + spam-markable
/// (`ThreadStore::mark_message_own`). Pin by mutation: remove the `if
/// bucket.message.is_own { self.threads.mark_message_own(...) }` upgrade arm in
/// `ingest_inbound_identified`'s dedup branch and this reddens.
#[test]
fn ingest_inbound_record_upgrades_a_self_addressed_message_to_own_on_the_later_sent_copy() {
    let m = smtp_manager(); // self address: "me@host.test"
    let raw = b"From: me@host.test\r\nTo: me@host.test\r\nSubject: note to self\r\n\
                Message-ID: <self-1@host.test>\r\n\r\nremember the milk\r\n"
        .to_vec();
    let inbox_copy = InboundMailRecord {
        uid: 20,
        message_id: b"seg-self-inbox".to_vec(),
        internal_date_ms: 1_700_000_000_000,
        rfc5322: raw.clone(),
        mailbox: MailFeed::Inbox,
        suppress_from_view: false,
        has_seen_flag: false,
    };
    ingest_inbound_record(&m, &inbox_copy).expect("ingest inbox copy");
    let snap = m.snapshot();
    let detail = m.thread_detail(snap.threads[0].thread_id.clone()).unwrap();
    assert!(
        !detail.messages[0].is_own,
        "sanity: the INBOX arrival alone is not proof of authorship"
    );

    let sent_copy = InboundMailRecord {
        uid: 21,
        message_id: b"seg-self-sent".to_vec(), // different segment id, same Message-ID
        internal_date_ms: 1_700_000_000_000,
        rfc5322: raw,
        mailbox: MailFeed::Sent,
        suppress_from_view: false,
        has_seen_flag: false,
    };
    ingest_inbound_record(&m, &sent_copy).expect("ingest sent copy");

    let snap = m.snapshot();
    assert_eq!(
        snap.threads.len(),
        1,
        "still one thread — an id-dedup upgrade, not a new message"
    );
    let detail = m.thread_detail(snap.threads[0].thread_id.clone()).unwrap();
    assert_eq!(
        detail.messages.len(),
        1,
        "the dedup match upgrades in place, not a second bubble"
    );
    assert!(
        detail.messages[0].is_own,
        "the durable Sent copy proves this account sent the message — the held \
         INBOX-sourced copy must upgrade to own, not stay stranded as receivable \
         + spam-markable"
    );
}

/// The upgrade path above must stay strictly one-directional: a **forged** `INBOX`
/// duplicate of an already-held message can never grant ownership, because
/// `ingest_inbound_identified` only calls `mark_message_own` when the *new* record's
/// own `is_own` came back `true` — and an `INBOX` record's `is_own` is always
/// `false` regardless of its (forgeable) `From:`. Pin by mutation: change the upgrade
/// arm's guard from `bucket.message.is_own` to an unconditional upgrade and this
/// reddens.
#[test]
fn ingest_inbound_record_dedup_duplicate_from_inbox_never_grants_ownership() {
    let m = smtp_manager(); // self address: "me@host.test"
    let original = InboundMailRecord {
        uid: 40,
        message_id: b"seg-real-original".to_vec(),
        internal_date_ms: 1_700_000_000_000,
        rfc5322: b"From: someone-else@example.com\r\nTo: me@host.test\r\nSubject: Hi\r\n\
                   Message-ID: <real-1@example.com>\r\n\r\nhello\r\n"
            .to_vec(),
        mailbox: MailFeed::Inbox,
        suppress_from_view: false,
        has_seen_flag: false,
    };
    ingest_inbound_record(&m, &original).expect("ingest original");

    // Same Message-ID, forged self-address `From:`, still delivered via INBOX —
    // the dedup match must not let this claim ownership of the held message.
    let forged_dup = InboundMailRecord {
        uid: 41,
        message_id: b"seg-forged-dup".to_vec(),
        internal_date_ms: 1_700_000_000_000,
        rfc5322: b"From: me@host.test\r\nTo: me@host.test\r\nSubject: Hi\r\n\
                   Message-ID: <real-1@example.com>\r\n\r\nhello\r\n"
            .to_vec(),
        mailbox: MailFeed::Inbox,
        suppress_from_view: false,
        has_seen_flag: false,
    };
    ingest_inbound_record(&m, &forged_dup).expect("ingest forged dup");

    let snap = m.snapshot();
    let detail = m.thread_detail(snap.threads[0].thread_id.clone()).unwrap();
    assert!(
        !detail.messages[0].is_own,
        "a forged INBOX duplicate must never grant ownership of an already-held \
         message, even when it dedup-matches by Message-ID"
    );
}

/// A Sent-copy dedup match must never grant ownership of content it doesn't
/// actually match — and must never let that content hide the message
/// the account sent. `mark_message_own` compares the
/// held message's parse against the Sent copy's, field by field; a mismatch is a
/// squat on the account's Message-ID, and the `Sent` copy displaces it
/// (`conversations.md:1572`). Here an `INBOX` record from `mallory@evil.test`
/// reuses the Message-ID of a genuine message this account later sends; when the
/// real `Sent` copy arrives and dedup-matches by that Message-ID, the forged
/// message leaves — its thread with it, since it held nothing else — and the
/// account's own message shows in a thread of its own, with its own content and
/// the time the nest filed it. It differs in several fields at once, so it
/// witnesses the compare as a whole: drop every compare from
/// `ThreadStore::mark_message_own` and the forged copy upgrades in place instead,
/// which reddens this. Each compare's own witness is one of the single-field pins
/// further down.
#[test]
fn ingest_inbound_record_message_id_collision_with_different_content_shows_the_sent_copy_not_the_squat()
 {
    let m = smtp_manager(); // self address: "me@host.test"
    let forged = InboundMailRecord {
        uid: 50,
        message_id: b"seg-forged-inbox".to_vec(),
        internal_date_ms: 1_700_000_000_000,
        rfc5322: b"From: mallory@evil.test\r\nTo: me@host.test\r\nSubject: urgent\r\n\
                   Message-ID: <sent-1@host.test>\r\n\r\npay the new account 12345\r\n"
            .to_vec(),
        mailbox: MailFeed::Inbox,
        suppress_from_view: false,
        has_seen_flag: false,
    };
    ingest_inbound_record(&m, &forged).expect("ingest forged inbox copy");
    let squat_thread = m.snapshot().threads[0].thread_id.clone();
    assert_eq!(
        m.thread_detail(squat_thread.clone()).unwrap().messages[0].sender,
        email("mallory@evil.test"),
        "sanity: until the Sent copy arrives, the squat is what the device holds"
    );

    let genuine_sent = InboundMailRecord {
        uid: 51,
        message_id: b"seg-genuine-sent".to_vec(), // same RFC Message-ID as above
        internal_date_ms: 1_700_000_001_000,
        rfc5322: b"From: me@host.test\r\nTo: bob@external.test\r\nSubject: Lunch\r\n\
                   Message-ID: <sent-1@host.test>\r\n\r\nsee you at noon\r\n"
            .to_vec(),
        mailbox: MailFeed::Sent,
        suppress_from_view: false,
        has_seen_flag: false,
    };
    ingest_inbound_record(&m, &genuine_sent).expect("ingest genuine sent copy");

    let snap = m.snapshot();
    assert_eq!(
        snap.threads.len(),
        1,
        "one thread: the squat's held nothing else, so it left with the squat, and the \
         genuine message started its own"
    );
    assert!(
        m.thread_detail(squat_thread).is_none(),
        "the squat's thread is gone, not left behind empty"
    );
    let summary = &snap.threads[0];
    assert_eq!(
        summary.label, "Lunch",
        "the remaining thread is labelled by the genuine subject, not the forged one"
    );
    assert_eq!(
        summary.last_activity_ms, 1_700_000_001_000,
        "the thread's activity time is the time the nest filed the Sent copy"
    );
    let detail = m.thread_detail(summary.thread_id.clone()).unwrap();
    assert!(
        detail.participants.contains(&email("bob@external.test"))
            && !detail.participants.contains(&email("mallory@evil.test")),
        "the thread's participants are the genuine message's: {:?}",
        detail.participants
    );
    assert_eq!(
        detail.messages.len(),
        1,
        "one bubble: the account's own message, no squat beside it"
    );
    let held = &detail.messages[0];
    assert!(
        held.is_own,
        "the account's genuine sent message shows as its own — the squat never hid it"
    );
    assert_eq!(
        held.sender,
        email("me@host.test"),
        "the own message carries the account's sender, not the forged one"
    );
    assert_eq!(
        held.body, "see you at noon",
        "the own message carries the text the account sent, not the forged text"
    );
    assert_eq!(
        held.timestamp_ms, 1_700_000_001_000,
        "the own message shows the Sent copy's filing time, not the squat's delivery time"
    );
}

/// The mail bridge prepends a canonical `Received:` header to every message it
/// delivers to `INBOX` before sealing (`libs/fauna-mail/src/received_header.rs`),
/// but never to a record filed into `Sent` by a first-party submission — so a
/// genuine self-send's two copies routinely differ byte-for-byte while parsing
/// to the identical sender and body. The upgrade in
/// [`ingest_inbound_record_upgrades_a_self_addressed_message_to_own_on_the_later_sent_copy`]
/// must still fire in that realistic shape, not just on byte-identical raw
/// input. Pin by mutation: compare `rec.rfc5322` (or a digest of it) instead of
/// the parsed fields in `ThreadStore::mark_message_own` and this reddens.
#[test]
fn ingest_inbound_record_upgrades_self_send_despite_bridge_added_received_header() {
    let m = smtp_manager(); // self address: "me@host.test"
    let core = b"From: me@host.test\r\nTo: me@host.test\r\nSubject: note to self\r\n\
                 Message-ID: <self-2@host.test>\r\n\r\nremember the milk\r\n";
    let mut with_received: Vec<u8> =
        b"Received: from mx.example.com ([203.0.113.7])\r\n\tby fauna-bridge.invalid with ESMTP\r\n\
          \tid a1b2c3d4e5f60718;\r\n\tThu, 01 Jan 1970 00:00:00 +0000\r\n"
            .to_vec();
    with_received.extend_from_slice(core);

    let inbox_copy = InboundMailRecord {
        uid: 22,
        message_id: b"seg-received-inbox".to_vec(),
        internal_date_ms: 1_700_000_000_000,
        rfc5322: with_received, // the bridge's stamped delivery copy
        mailbox: MailFeed::Inbox,
        suppress_from_view: false,
        has_seen_flag: false,
    };
    ingest_inbound_record(&m, &inbox_copy).expect("ingest inbox copy");
    let snap = m.snapshot();
    let detail = m.thread_detail(snap.threads[0].thread_id.clone()).unwrap();
    assert!(!detail.messages[0].is_own, "sanity: not yet proven own");

    let sent_copy = InboundMailRecord {
        uid: 23,
        message_id: b"seg-received-sent".to_vec(),
        internal_date_ms: 1_700_000_000_000,
        rfc5322: core.to_vec(), // no Received: header — this account's own submission
        mailbox: MailFeed::Sent,
        suppress_from_view: false,
        has_seen_flag: false,
    };
    ingest_inbound_record(&m, &sent_copy).expect("ingest sent copy");

    let snap = m.snapshot();
    let detail = m.thread_detail(snap.threads[0].thread_id.clone()).unwrap();
    assert_eq!(
        detail.messages.len(),
        1,
        "the dedup match upgrades in place"
    );
    assert!(
        detail.messages[0].is_own,
        "a bridge-added Received: header must not block the same-content upgrade"
    );
}

/// The attachment compare must not strand an honest self-send that carries a
/// file: its bridge-stamped `INBOX` copy and its `Sent` copy extract the same
/// attachment blocks. Pin by mutation: remove the upgrade arm in
/// `ingest_inbound_identified`, or compare anything the bridge's `Received:`
/// stamp changes, and this reddens.
#[test]
fn ingest_inbound_record_upgrades_a_self_send_carrying_an_attachment() {
    let m = smtp_manager(); // self address: "me@host.test"
    let submitted = fauna_conversations::rfc5322::build_message(
        "me@host.test",
        &["me@host.test".to_string()],
        Some("receipts"),
        "filed for later",
        "<self-3@host.test>",
        None,
        1_700_000_000,
        &[fauna_conversations::rfc5322::MimeAttachment {
            filename: "receipt.pdf",
            mime_type: "application/pdf",
            bytes: b"%PDF-1.4\n",
        }],
    );
    let mut delivered: Vec<u8> =
        b"Received: from mx.example.com ([203.0.113.7])\r\n\tby fauna-bridge.invalid with ESMTP\r\n\
          \tid a1b2c3d4e5f60718;\r\n\tThu, 01 Jan 1970 00:00:00 +0000\r\n"
            .to_vec();
    delivered.extend_from_slice(&submitted);
    for (uid, mailbox, rfc5322) in [
        (24, MailFeed::Inbox, delivered), // the bridge's stamped delivery copy
        (25, MailFeed::Sent, submitted),  // this account's own submission
    ] {
        let rec = InboundMailRecord {
            uid,
            message_id: format!("seg-attached-self-{uid}").into_bytes(),
            internal_date_ms: 1_700_000_000_000,
            rfc5322,
            mailbox,
            suppress_from_view: false,
            has_seen_flag: false,
        };
        assert!(ingest_inbound_record(&m, &rec).expect("ingest ok"));
    }

    let snap = m.snapshot();
    let detail = m.thread_detail(snap.threads[0].thread_id.clone()).unwrap();
    assert_eq!(
        detail.messages.len(),
        1,
        "the dedup match upgrades in place"
    );
    let files: Vec<String> = attachment_blocks(&detail.messages[0].document)
        .into_iter()
        .map(|a| a.filename)
        .collect();
    assert_eq!(files, ["receipt.pdf"], "both copies carry the file");
    assert!(
        detail.messages[0].is_own,
        "an honest self-send carrying a file must still upgrade to own"
    );
}

// ── The own upgrade compares each parsed field on its own ──────
// `ThreadStore::mark_message_own` upgrades a held copy in place only when the
// later `Sent` copy parses to the same message in every field the parse fills
// (`conversations.md:1572`); a held copy that differs in any of them is a squat
// on the account's Message-ID, and the `Sent` copy displaces it: the squat
// leaves its thread and the account's own message ingests afresh, by its own
// key. A conjunction hides a dropped arm behind its
// siblings, so each compared field has a pin of its own: a forged `INBOX` copy
// of one genuine message that differs from it in exactly that field, ingested
// first as `poll_mail` orders the mailboxes (`session.rs:1416-1436`). Removing
// one compare makes exactly its pin's forged copy upgrade in place — the forged
// content badged as the account's own, and the bubble left where the squat sat
// — which reddens that pin alone. The control shows the unforged base does
// upgrade in place, so each pin's displacement is its one field's doing.

/// The genuine message every pin below forges a one-field variant of: this
/// account (`me@host.test`) writing to someone else, so only its `Sent` copy
/// proves authorship.
const GENUINE_SENT: &str = "From: me@host.test\r\nTo: bob@external.test\r\nSubject: Lunch\r\n\
                            Message-ID: <sent-7@host.test>\r\n\r\nsee you at noon\r\n";

/// When the nest filed [`GENUINE_SENT`] into `Sent`: the time the account's own
/// message shows, whatever a colliding `INBOX` copy was delivered at.
const GENUINE_SENT_FILED_MS: i64 = 1_700_000_001_000;

/// When the forged `INBOX` copy was delivered — before the Sent copy was filed,
/// as a squat that reaches the mailbox first would be.
const FORGED_COPY_DELIVERED_MS: i64 = 1_700_000_000_000;

/// A reply from the genuine recipient into the same thread, delivered to
/// `INBOX` after the forged copy and polled before the `Sent` mailbox — so
/// where the own message ends up sitting tells an in-place upgrade (it keeps
/// the squat's place, before the reply) from a displacement (it ingests afresh,
/// after the reply).
const BOBS_REPLY: &str = "From: bob@external.test\r\nTo: me@host.test\r\nSubject: Lunch\r\n\
                          Message-ID: <bob-2@external.test>\r\n\r\nnoon works\r\n";
const BOBS_REPLY_DELIVERED_MS: i64 = 1_700_000_002_000;

/// [`GENUINE_SENT`] with `original` replaced by `forged`, asserting the
/// replacement happened so no pin can pass on an unforged copy.
fn forge(original: &str, forged: &str) -> String {
    assert!(
        GENUINE_SENT.contains(original),
        "the genuine message carries {original:?}"
    );
    GENUINE_SENT.replace(original, forged)
}

fn own_compare_record(
    uid: u32,
    mailbox: MailFeed,
    raw: &str,
    delivered_ms: i64,
) -> InboundMailRecord {
    InboundMailRecord {
        uid,
        message_id: format!("seg-own-compare-{uid}").into_bytes(),
        internal_date_ms: delivered_ms,
        rfc5322: raw.as_bytes().to_vec(),
        mailbox,
        suppress_from_view: false,
        has_seen_flag: false,
    }
}

/// Ingest `inbox_copy` from `INBOX` alone and return it as held — the pins'
/// sanity checks on what the forged copy parsed to, before any `Sent` copy
/// settles it.
fn held_inbox_copy(inbox_copy: &str) -> fauna_conversations::MessageSnapshot {
    let m = smtp_manager();
    let rec = own_compare_record(60, MailFeed::Inbox, inbox_copy, FORGED_COPY_DELIVERED_MS);
    assert!(ingest_inbound_record(&m, &rec).expect("ingest ok"));
    let snap = m.snapshot();
    let detail = m.thread_detail(snap.threads[0].thread_id.clone()).unwrap();
    detail.messages[0].clone()
}

/// Ingest `inbox_copy` from `INBOX`, then [`BOBS_REPLY`] from `INBOX`, then
/// [`GENUINE_SENT`] from `Sent` — the order one `poll_mail` pass delivers them
/// in — and return the one thread the store holds afterwards. Any thread the
/// forged copy started for itself must be gone with it.
fn thread_after_inbox_copy_then_genuine_sent(inbox_copy: &str) -> ThreadDetail {
    let m = smtp_manager(); // self address: "me@host.test"
    for rec in [
        own_compare_record(60, MailFeed::Inbox, inbox_copy, FORGED_COPY_DELIVERED_MS),
        own_compare_record(62, MailFeed::Inbox, BOBS_REPLY, BOBS_REPLY_DELIVERED_MS),
        own_compare_record(61, MailFeed::Sent, GENUINE_SENT, GENUINE_SENT_FILED_MS),
    ] {
        assert!(ingest_inbound_record(&m, &rec).expect("ingest ok"));
    }
    let snap = m.snapshot();
    assert_eq!(
        snap.threads.len(),
        1,
        "one thread, the genuine message's — a thread the forged copy started for \
         itself leaves with it, and the Sent copy starts none of its own"
    );
    m.thread_detail(snap.threads[0].thread_id.clone()).unwrap()
}

/// What every pin ends on: the thread is the genuine message's, and the one own
/// message in it is exactly what the account sent — its content, its thread,
/// the `Sent` copy's filing time — sitting *after* the reply that reached
/// `INBOX` before the `Sent` poll, because a displaced squat's replacement
/// ingests afresh rather than in the squat's place. Nothing of the forged copy
/// shows.
fn assert_the_squat_was_displaced_by_the_genuine_sent_message(detail: &ThreadDetail) {
    assert_eq!(
        detail.label, "Lunch",
        "the thread is labelled by the genuine subject"
    );
    assert!(
        detail.participants.contains(&email("bob@external.test"))
            && detail.participants.contains(&email("me@host.test"))
            && detail.participants.len() == 2,
        "the thread's participants are the genuine message's: {:?}",
        detail.participants
    );
    assert_eq!(
        detail.messages.len(),
        2,
        "the reply and the own message — no squat bubble beside them"
    );
    assert_eq!(
        detail.messages[0].message_id,
        MessageId("<bob-2@external.test>".into()),
        "the reply sits first: the own message did not keep the squat's place"
    );
    assert!(!detail.messages[0].is_own, "the reply is untouched");
    let own = &detail.messages[1];
    assert_eq!(
        own.message_id,
        MessageId("<sent-7@host.test>".into()),
        "the own message ingested afresh, after the reply"
    );
    assert!(
        own.is_own,
        "the account's genuine sent message shows as its own — the squat never hid it"
    );
    assert_eq!(
        own.sender,
        email("me@host.test"),
        "the own message carries the account's sender"
    );
    assert_eq!(
        own.body, "see you at noon",
        "the own message carries the text the account sent"
    );
    assert_eq!(own.reply_to, None, "the own message replies to nothing");
    assert!(
        attachment_blocks(&own.document).is_empty(),
        "the own message carries no file the account never sent"
    );
    assert_eq!(
        own.timestamp_ms, GENUINE_SENT_FILED_MS,
        "the own message shows the Sent copy's filing time"
    );
}

#[test]
fn ingest_inbound_record_own_upgrade_control_an_identical_inbox_copy_upgrades_in_place() {
    let detail = thread_after_inbox_copy_then_genuine_sent(GENUINE_SENT);
    assert_eq!(
        detail.messages.len(),
        2,
        "the upgraded copy and the reply — an upgrade adds no bubble"
    );
    let held = &detail.messages[0];
    assert_eq!(
        held.message_id,
        MessageId("<sent-7@host.test>".into()),
        "an INBOX copy identical to the Sent copy keeps its place, before the reply — \
         otherwise every displacement below could be the base message's, not its one \
         forged field's"
    );
    assert!(
        held.is_own,
        "an INBOX copy identical to the Sent copy must upgrade"
    );
    assert_eq!(
        held.timestamp_ms, GENUINE_SENT_FILED_MS,
        "an upgraded copy shows the Sent copy's filing time, not when its INBOX copy \
         was delivered"
    );
    assert!(!detail.messages[1].is_own, "the reply is untouched");
}

#[test]
fn ingest_inbound_record_own_upgrade_displaces_a_copy_differing_only_in_sender() {
    let detail = thread_after_inbox_copy_then_genuine_sent(&forge(
        "From: me@host.test",
        "From: mallory@evil.test",
    ));
    // Upgraded in place, the device would show another sender's words as the
    // account's own.
    assert_the_squat_was_displaced_by_the_genuine_sent_message(&detail);
}

#[test]
fn ingest_inbound_record_own_upgrade_displaces_a_copy_differing_only_in_recipients() {
    let detail = thread_after_inbox_copy_then_genuine_sent(&forge(
        "To: bob@external.test",
        "To: mallory@evil.test",
    ));
    // Upgraded in place, the device would show the account writing to someone
    // it never wrote to, in a thread with that someone as its participant.
    assert_the_squat_was_displaced_by_the_genuine_sent_message(&detail);
}

#[test]
fn ingest_inbound_record_own_upgrade_displaces_a_copy_differing_only_in_subject() {
    let detail = thread_after_inbox_copy_then_genuine_sent(&forge(
        "Subject: Lunch",
        "Subject: Wire the deposit today",
    ));
    // Upgraded in place, the device would label a thread with words the account
    // never wrote.
    assert_the_squat_was_displaced_by_the_genuine_sent_message(&detail);
}

#[test]
fn ingest_inbound_record_own_upgrade_displaces_a_copy_differing_only_in_reply_reference() {
    let forged = forge(
        "Message-ID: <sent-7@host.test>\r\n",
        "Message-ID: <sent-7@host.test>\r\nIn-Reply-To: <invoice-3@evil.test>\r\n",
    );
    assert_eq!(
        held_inbox_copy(&forged).reply_to,
        Some(MessageId("<invoice-3@evil.test>".into())),
        "sanity: the forged copy carries the forged reply reference"
    );
    let detail = thread_after_inbox_copy_then_genuine_sent(&forged);
    // Upgraded in place, the account's message would route into another
    // message's thread as a reply it never wrote.
    assert_the_squat_was_displaced_by_the_genuine_sent_message(&detail);
}

#[test]
fn ingest_inbound_record_own_upgrade_displaces_a_copy_differing_only_in_body() {
    let detail = thread_after_inbox_copy_then_genuine_sent(&forge(
        "see you at noon",
        "pay the new account 12345",
    ));
    // Upgraded in place, the device would show text the account never wrote as
    // its own — the review record's measured hole.
    assert_the_squat_was_displaced_by_the_genuine_sent_message(&detail);
}

/// The one field whose forged copy renders exactly like the genuine message:
/// identical text sent as HTML converts to the same body. So this pin has no
/// content to catch an in-place upgrade by — only the bubble's place. Displaced,
/// the own message ingests afresh after the reply; upgraded in place, it keeps
/// the squat's spot before it. That is the whole difference the compare's
/// body-format arm makes, and it is what reddens when the arm is dropped.
#[test]
fn ingest_inbound_record_own_upgrade_displaces_a_copy_differing_only_in_body_format() {
    let forged = forge(
        "Message-ID: <sent-7@host.test>\r\n\r\nsee you at noon\r\n",
        "Message-ID: <sent-7@host.test>\r\nContent-Type: text/html; charset=utf-8\r\n\r\n\
         <p>see you at noon</p>\r\n",
    );
    assert_eq!(
        held_inbox_copy(&forged).body,
        "see you at noon",
        "sanity: the HTML part converts to the genuine text, so its format is the one \
         field that differs"
    );
    let detail = thread_after_inbox_copy_then_genuine_sent(&forged);
    assert_the_squat_was_displaced_by_the_genuine_sent_message(&detail);
}

/// The record's probe: the forged copy claims this account's
/// `From:`, reuses the genuine Message-ID and text, and adds `invoice.pdf`. With
/// only sender and body compared, the held forged copy became own and kept
/// rendering the attacker's file, badged as the account's own sent message.
#[test]
fn ingest_inbound_record_own_upgrade_displaces_a_copy_differing_only_in_attachments() {
    let forged = forge(
        "Message-ID: <sent-7@host.test>\r\n\r\nsee you at noon\r\n",
        "Message-ID: <sent-7@host.test>\r\nMIME-Version: 1.0\r\n\
         Content-Type: multipart/mixed; boundary=\"forged\"\r\n\r\n\
         --forged\r\nContent-Type: text/plain; charset=utf-8\r\n\r\nsee you at noon\r\n\
         --forged\r\nContent-Type: application/pdf\r\n\
         Content-Disposition: attachment; filename=\"invoice.pdf\"\r\n\
         Content-Transfer-Encoding: base64\r\n\r\nJVBERi0xLjQK\r\n--forged--\r\n",
    );
    let squat = held_inbox_copy(&forged);
    assert_eq!(
        squat.body, "see you at noon",
        "sanity: the forged copy's text matches the genuine message"
    );
    let files: Vec<String> = attachment_blocks(&squat.document)
        .into_iter()
        .map(|a| a.filename)
        .collect();
    assert_eq!(
        files,
        ["invoice.pdf"],
        "sanity: the forged copy renders the forged file until the Sent copy settles it"
    );
    let detail = thread_after_inbox_copy_then_genuine_sent(&forged);
    // Upgraded in place, the device would badge the attacker's file as one the
    // account sent.
    assert_the_squat_was_displaced_by_the_genuine_sent_message(&detail);
}

// ── Displacement's edges ─────────────────────────────
// A displaced squat leaves only itself; the reverse arrival order needs no
// displacement at all; and an in-place upgrade adopts the Sent copy's time.

/// The squat's thread survives when it holds other mail: only the squat leaves.
/// Here the squatter's earlier, honest message shares the squat's participants
/// and subject, so the two sit in one thread until the `Sent` copy arrives.
#[test]
fn ingest_inbound_record_a_displaced_squat_leaves_its_thread_mates_in_place() {
    let m = smtp_manager(); // self address: "me@host.test"
    const MALLORYS_OWN: &str = "From: mallory@evil.test\r\nTo: me@host.test\r\nSubject: urgent\r\n\
                                Message-ID: <m-1@evil.test>\r\n\r\nplease read\r\n";
    const SQUAT: &str = "From: mallory@evil.test\r\nTo: me@host.test\r\nSubject: urgent\r\n\
                         Message-ID: <sent-7@host.test>\r\n\r\npay the new account 12345\r\n";
    for rec in [
        own_compare_record(70, MailFeed::Inbox, MALLORYS_OWN, FORGED_COPY_DELIVERED_MS),
        own_compare_record(71, MailFeed::Inbox, SQUAT, FORGED_COPY_DELIVERED_MS + 500),
        own_compare_record(72, MailFeed::Sent, GENUINE_SENT, GENUINE_SENT_FILED_MS),
    ] {
        assert!(ingest_inbound_record(&m, &rec).expect("ingest ok"));
    }
    let snap = m.snapshot();
    assert_eq!(
        snap.threads.len(),
        2,
        "the squatter's thread stays for its other mail; the genuine message has its own"
    );
    let by_label = |label: &str| -> ThreadDetail {
        let summary = snap
            .threads
            .iter()
            .find(|t| t.label == label)
            .unwrap_or_else(|| panic!("a thread labelled {label:?} among {:?}", snap.threads));
        m.thread_detail(summary.thread_id.clone()).unwrap()
    };
    let squatters = by_label("urgent");
    let ids: Vec<&MessageId> = squatters.messages.iter().map(|m| &m.message_id).collect();
    assert_eq!(
        ids,
        [&MessageId("<m-1@evil.test>".into())],
        "the squatter's honest message stays; the squat alone left"
    );
    assert!(
        squatters.messages.iter().all(|m| !m.is_own),
        "nothing of the squatter's is the account's own"
    );
    let genuine = by_label("Lunch");
    assert_eq!(
        genuine.messages.len(),
        1,
        "the own message, alone in its thread"
    );
    assert!(genuine.messages[0].is_own, "and it is own");
    assert_eq!(genuine.messages[0].body, "see you at noon");
}

/// The reverse order: a device that already holds the `Sent` copy (own) sees
/// the squat arrive later from `INBOX`. An `INBOX` record never displaces a held
/// copy, so the dedup drops it like any duplicate — the outcome is the same
/// whichever copy reached the device first.
#[test]
fn ingest_inbound_record_a_squat_arriving_after_the_sent_copy_is_dropped() {
    let m = smtp_manager(); // self address: "me@host.test"
    const SQUAT: &str = "From: mallory@evil.test\r\nTo: me@host.test\r\nSubject: urgent\r\n\
                         Message-ID: <sent-7@host.test>\r\n\r\npay the new account 12345\r\n";
    for rec in [
        own_compare_record(80, MailFeed::Sent, GENUINE_SENT, GENUINE_SENT_FILED_MS),
        own_compare_record(81, MailFeed::Inbox, SQUAT, GENUINE_SENT_FILED_MS + 5_000),
    ] {
        assert!(ingest_inbound_record(&m, &rec).expect("ingest ok"));
    }
    let snap = m.snapshot();
    assert_eq!(snap.threads.len(), 1, "no thread for the squat");
    assert_eq!(snap.threads[0].label, "Lunch");
    let detail = m.thread_detail(snap.threads[0].thread_id.clone()).unwrap();
    assert_eq!(detail.messages.len(), 1, "one bubble");
    let held = &detail.messages[0];
    assert!(held.is_own, "the Sent copy stays own");
    assert_eq!(held.sender, email("me@host.test"));
    assert_eq!(held.body, "see you at noon");
    assert_eq!(
        held.timestamp_ms, GENUINE_SENT_FILED_MS,
        "and keeps the Sent copy's filing time"
    );
}

/// The delivery-time half of the ruling (`conversations.md:1572`): a held
/// `INBOX` copy identical to the `Sent` copy upgrades in place, and the own
/// message then shows the `Sent` copy's filing time — never the `INBOX` copy's
/// delivery time, which whoever delivers a later identical copy would otherwise
/// set. The thread's activity time follows the message.
#[test]
fn ingest_inbound_record_an_upgraded_copy_shows_the_sent_copys_filing_time() {
    let m = smtp_manager(); // self address: "me@host.test"
    let inbox_delivered_ms = GENUINE_SENT_FILED_MS + 4_000;
    for rec in [
        own_compare_record(90, MailFeed::Inbox, GENUINE_SENT, inbox_delivered_ms),
        own_compare_record(91, MailFeed::Sent, GENUINE_SENT, GENUINE_SENT_FILED_MS),
    ] {
        assert!(ingest_inbound_record(&m, &rec).expect("ingest ok"));
    }
    let snap = m.snapshot();
    assert_eq!(snap.threads.len(), 1, "one thread");
    assert_eq!(
        snap.threads[0].last_activity_ms, GENUINE_SENT_FILED_MS,
        "the thread's activity time follows the own message to the Sent copy's filing time"
    );
    let detail = m.thread_detail(snap.threads[0].thread_id.clone()).unwrap();
    assert_eq!(detail.messages.len(), 1, "an in-place upgrade, one bubble");
    assert!(detail.messages[0].is_own, "upgraded");
    assert_eq!(
        detail.messages[0].timestamp_ms, GENUINE_SENT_FILED_MS,
        "the own message shows when the nest filed it, not when its INBOX copy landed"
    );
}

#[test]
fn ingest_inbound_record_skips_a_record_with_no_from() {
    let m = smtp_manager();
    let rec = InboundMailRecord {
        uid: 1,
        message_id: b"seg-orphan".to_vec(),
        internal_date_ms: 1,
        rfc5322: b"To: me@host.test\r\nSubject: orphan\r\n\r\nno sender\r\n".to_vec(),
        mailbox: MailFeed::Inbox,
        suppress_from_view: false,
        has_seen_flag: false,
    };

    let ingested = ingest_inbound_record(&m, &rec).expect("ingest ok");

    assert!(!ingested, "an unaddressable record is skipped, not fatal");
    assert_eq!(m.snapshot().threads.len(), 0, "no thread created");
}

// ── Sent-copy dedup: same RFC Message-ID, different server records ─────
// A first-party `fauna.email.send` is echoed locally on send AND comes back over
// `fauna.email.sent.fetch` as nest's durable server-side Sent copy. The two carry
// the SAME RFC `Message-ID` but DIFFERENT server segment-record ids, so the
// per-feed `seen` set (keyed on segment id) structurally can't dedup them — only
// the manager's id-level dedup can. These pin both halves of the durability fix:
// in-session the message shows once (not twice); after a restart (empty store) the
// Sent copy reloads (smtp-server.md § Inbound client receive).

fn record_with_msgid(
    uid: u32,
    seg_id: &[u8],
    msgid: &str,
    from: &str,
    to: &str,
    body: &str,
) -> InboundMailRecord {
    let raw = format!(
        "From: {from}\r\nTo: {to}\r\nSubject: Lunch\r\nMessage-ID: {msgid}\r\n\r\n{body}\r\n"
    );
    InboundMailRecord {
        uid,
        message_id: seg_id.to_vec(),
        internal_date_ms: 1_700_000_000_000,
        rfc5322: raw.into_bytes(),
        mailbox: MailFeed::Inbox,
        suppress_from_view: false,
        has_seen_flag: false,
    }
}

#[test]
fn ingest_dedups_same_rfc_message_id_across_server_records() {
    let m = smtp_manager();
    // The local echo (or first arrival) of the user's own sent message.
    let echo = record_with_msgid(
        1,
        b"seg-echo",
        "<m1.42@host.test>",
        "me@host.test",
        "bob@external.test",
        "see you at noon",
    );
    ingest_inbound_record(&m, &echo).expect("ingest echo");
    // nest's durable server-side Sent copy of the SAME message: same RFC
    // Message-ID, DIFFERENT server segment-record id (a segment-id `seen` set
    // wouldn't catch this — only the manager's id-level dedup can).
    let sent_copy = record_with_msgid(
        9,
        b"seg-sent-copy",
        "<m1.42@host.test>",
        "me@host.test",
        "bob@external.test",
        "see you at noon",
    );
    ingest_inbound_record(&m, &sent_copy).expect("ingest sent copy");

    let snap = m.snapshot();
    assert_eq!(snap.threads.len(), 1, "one thread");
    let detail = m.thread_detail(snap.threads[0].thread_id.clone()).unwrap();
    assert_eq!(
        detail.messages.len(),
        1,
        "the user's sent message shows exactly once in-session, not twice"
    );
}

// ── Faithful sent-path round-trip: client send → server Sent-copy re-ingest ──
// The previous dedup test hand-rolls the "echo" raw. This one drives the REAL
// client send (`ConversationsManager::send` via the new-thread compose, which
// generates the `Message-ID` and builds the `multipart/alternative` raw through
// `rfc5322::build_message`), captures the EXACT bytes the client submitted, and
// re-ingests them as nest's durable server-side Sent copy — which seals the
// sender-supplied raw VERBATIM (`bridge_routing_handlers::seal_and_persist_local`
// → `seal_to_recipient(raw_rfc5322)`), so the `Sent` feed returns those same
// bytes. This is the production sent path end-to-end through shared Rust; the
// thread must show the message exactly ONCE, not twice (the user-reported
// duplication bug). Reproduces case (i) if the local-echo id and the parsed-back
// id diverge for the real `build_message` output.
#[tokio::test]
async fn real_send_then_server_sent_copy_shows_one_bubble() {
    let sink = Arc::new(CapturingSink::default());
    let m = ConversationsManager::new();
    m.register_backend(Arc::new(SmtpBackend::new(sink.clone(), "alice@host.test")));

    // Drive a real client send exactly as the UI does (generates Message-ID,
    // builds the multipart raw, appends the local echo).
    m.start_new_conversation();
    m.set_new_thread_recipient_input("bob@external.test".into());
    m.resolve_recipient().await;
    assert!(m.accept_current_recipient_chip());
    m.set_new_thread_subject(Some("Lunch".into()));
    m.set_new_thread_body("see you at noon".into());
    let tid = m
        .send_new_thread()
        .await
        .expect("send ok")
        .expect("thread id");

    // After send: exactly the local echo.
    let detail = m.thread_detail(tid.clone()).expect("thread");
    assert_eq!(detail.messages.len(), 1, "local echo appended once");

    // Capture the EXACT raw the client submitted — what nest seals verbatim into
    // the sender's `Sent` mailbox.
    let submitted_raw = sink.calls.lock().unwrap()[0].1.clone();

    // nest's durable Sent copy: the same raw bytes, a DIFFERENT server
    // segment-record id (a fresh sealed record). The receive loop polls the
    // `Sent` feed and ingests it.
    let sent_copy = InboundMailRecord {
        uid: 9,
        message_id: b"seg-server-sent-copy".to_vec(),
        internal_date_ms: 1_700_000_000_000,
        rfc5322: submitted_raw,
        mailbox: MailFeed::Sent,
        suppress_from_view: false,
        has_seen_flag: false,
    };
    ingest_inbound_record(&m, &sent_copy).expect("ingest sent copy");

    let detail = m.thread_detail(tid).expect("thread");
    assert_eq!(
        detail.messages.len(),
        1,
        "the user's sent message must show exactly once, not twice (dedup must \
         catch the server Sent-copy against the local echo)"
    );
}

/// A sent MAIL reaches the content index in the same session — through its
/// server `Sent` copy, not at send time (`content-index-ingest.md` § Ingest
/// triggers, v1 → *The Conversation kind's catch-up*, the own-send ruling).
///
/// Mail's doc identity carries the nest record id as its secondary id, and the
/// local echo has none: offered at send, the next launch's re-page of the same
/// `Sent` record (record id present, own) would take the builder's
/// replace-and-rewrite arm — one wasted segment rewrite per sent mail. So the
/// send offers nothing for mail, and the `Sent` copy — which the manager drops
/// against the held own echo — is offered in its place, carrying the record id
/// the next launch will present again.
#[tokio::test]
async fn a_sent_mail_reaches_the_index_through_its_sent_copy_in_the_same_session() {
    use fauna_conversations::index_sink::{IndexableKind, IndexableMessage, MessageIndexObserver};

    /// One observed message: kind, body, is-own, nest message id.
    type Observed = (IndexableKind, String, bool, Option<Vec<u8>>);
    #[derive(Default)]
    struct Recorder(Mutex<Vec<Observed>>);
    impl MessageIndexObserver for Recorder {
        fn observe_indexable_message(&self, msg: IndexableMessage<'_>) {
            self.0.lock().unwrap().push((
                msg.kind,
                msg.body.to_string(),
                msg.is_own,
                msg.nest_message_id.map(<[u8]>::to_vec),
            ));
        }
    }

    let sink = Arc::new(CapturingSink::default());
    let m = ConversationsManager::new();
    m.register_backend(Arc::new(SmtpBackend::new(sink.clone(), "alice@host.test")));
    let recorder = Arc::new(Recorder::default());
    m.set_index_observer(recorder.clone());

    m.start_new_conversation();
    m.set_new_thread_recipient_input("bob@external.test".into());
    m.resolve_recipient().await;
    assert!(m.accept_current_recipient_chip());
    m.set_new_thread_subject(Some("Lunch".into()));
    m.set_new_thread_body("see you at noon".into());
    m.send_new_thread()
        .await
        .expect("send ok")
        .expect("thread id");
    assert!(
        recorder.0.lock().unwrap().is_empty(),
        "a mail send offers nothing itself — its Sent copy carries the record id"
    );

    let submitted_raw = sink.calls.lock().unwrap()[0].1.clone();
    let sent_copy = InboundMailRecord {
        uid: 9,
        message_id: b"seg-server-sent-copy".to_vec(),
        internal_date_ms: 1_700_000_000_000,
        rfc5322: submitted_raw,
        mailbox: MailFeed::Sent,
        suppress_from_view: false,
        has_seen_flag: false,
    };
    ingest_inbound_record(&m, &sent_copy).expect("ingest sent copy");

    let offered = recorder.0.lock().unwrap().clone();
    assert_eq!(offered.len(), 1, "exactly the Sent copy: {offered:?}");
    let (kind, body, is_own, record) = &offered[0];
    assert_eq!(*kind, IndexableKind::Mail);
    assert!(body.contains("see you at noon"), "the sent body: {body:?}");
    assert!(*is_own, "a Sent copy is the account's own");
    assert_eq!(record.as_deref(), Some(&b"seg-server-sent-copy"[..]));
}

#[test]
fn ingest_into_empty_store_reloads_the_sent_copy() {
    // The post-restart path: the in-memory store is empty (the local echo is gone),
    // so the durable server-side Sent copy ingests and the sent message reloads.
    let m = smtp_manager();
    let sent_copy = record_with_msgid(
        9,
        b"seg-sent-copy",
        "<m1.42@host.test>",
        "me@host.test",
        "bob@external.test",
        "see you at noon",
    );
    ingest_inbound_record(&m, &sent_copy).expect("ingest sent copy");

    let snap = m.snapshot();
    assert_eq!(snap.threads.len(), 1);
    let detail = m.thread_detail(snap.threads[0].thread_id.clone()).unwrap();
    assert_eq!(
        detail.messages.len(),
        1,
        "the previously-sent message reloads after restart"
    );
    assert_eq!(detail.messages[0].body, "see you at noon");
}

// ── Attachments: compose → send (inline MIME) → inbound extract ─────────
//
// The full SMTP attachment flow (`docs/goal/ui/conversations.md` § Attachments):
// a staged attachment is inlined as a multipart/mixed part on send, and the
// inbound parse on the *same* bytes (the server Sent-copy / a recipient) extracts
// it back to an `AttachmentSnapshot` whose `blob_hash` matches the bytes the
// sender cached — proving the outbound encode and inbound decode agree, and that
// the bytes are loadable through `attachment_bytes` on both sides.
#[tokio::test]
async fn attachment_round_trips_outbound_to_inbound() {
    let png: &[u8] = b"\x89PNG\r\n\x1a\nnot-a-real-image-but-distinct-bytes";

    let sink = Arc::new(CapturingSink::default());
    let m = ConversationsManager::new();
    m.register_backend(Arc::new(SmtpBackend::new(sink.clone(), "alice@host.test")));

    // Compose a new thread with a staged image attachment, exactly as the UI does.
    m.start_new_conversation();
    m.set_new_thread_recipient_input("bob@external.test".into());
    m.resolve_recipient().await;
    assert!(m.accept_current_recipient_chip());
    m.set_new_thread_body("see attached".into());
    let staged_hash = m
        .add_new_thread_attachment("pic.png".into(), "image/png".into(), png.to_vec())
        .expect("new-thread compose open");
    // The staged bytes are cached and loadable immediately.
    assert_eq!(
        m.attachment_bytes(staged_hash.clone()).as_deref(),
        Some(png),
        "staged bytes cached under the returned hash"
    );

    let tid = m
        .send_new_thread()
        .await
        .expect("send ok")
        .expect("thread id");

    // Sender's own echo renders the attachment, loadable via the same handle.
    let detail = m.thread_detail(tid).expect("thread");
    assert_eq!(detail.messages.len(), 1);
    let sent = &detail.messages[0];
    let sent_atts = attachment_blocks(&sent.document);
    assert_eq!(sent_atts.len(), 1, "echo carries the attachment");
    assert_eq!(sent_atts[0].blob_hash, staged_hash);
    assert_eq!(sent_atts[0].filename, "pic.png");
    assert_eq!(sent_atts[0].mime_type, "image/png");
    assert!(sent_atts[0].is_image);
    assert_eq!(sent_atts[0].size_bytes, png.len() as u64);

    // The on-wire raw is a multipart/mixed carrying a base64 attachment part.
    let submitted_raw = sink.calls.lock().unwrap()[0].1.clone();
    let raw_text = String::from_utf8_lossy(&submitted_raw);
    assert!(
        raw_text.contains("Content-Type: multipart/mixed;"),
        "raw is multipart/mixed: {raw_text:?}"
    );
    assert!(
        raw_text.contains("Content-Disposition: attachment; filename=\"pic.png\""),
        "attachment part present: {raw_text:?}"
    );

    // A *fresh* manager (a recipient, or this user post-restart) ingests those
    // exact bytes and extracts the attachment back — same content handle.
    let recipient = ConversationsManager::new();
    recipient.register_backend(Arc::new(SmtpBackend::new(
        Arc::new(CapturingSink::default()),
        "bob@external.test",
    )));
    let delivered = InboundMailRecord {
        uid: 1,
        message_id: b"seg-delivered".to_vec(),
        internal_date_ms: 1_700_000_000_000,
        rfc5322: submitted_raw,
        mailbox: MailFeed::Inbox,
        suppress_from_view: false,
        has_seen_flag: false,
    };
    assert!(ingest_inbound_record(&recipient, &delivered).expect("ingest ok"));

    let snap = recipient.snapshot();
    let in_detail = recipient
        .thread_detail(snap.threads[0].thread_id.clone())
        .unwrap();
    let inbound_msg = &in_detail.messages[0];
    let inbound_atts = attachment_blocks(&inbound_msg.document);
    assert_eq!(inbound_atts.len(), 1, "inbound extracts the file");
    let att = &inbound_atts[0];
    assert_eq!(att.filename, "pic.png");
    assert_eq!(att.mime_type, "image/png");
    assert!(att.is_image);
    assert!(!att.c2pa, "no client-side C2PA on the SMTP rail yet");
    // The decoded inbound bytes hash to the SAME handle the sender staged —
    // outbound base64 encode + inbound mail-parser decode round-trip exactly.
    assert_eq!(
        att.blob_hash, staged_hash,
        "content handle agrees both ways"
    );
    assert_eq!(
        recipient.attachment_bytes(att.blob_hash.clone()).as_deref(),
        Some(png),
        "inbound bytes cached + loadable via the handle"
    );
}

#[test]
fn plain_inbound_message_has_no_attachments() {
    let m = smtp_manager();
    let rec = inbound_record(3, b"seg-plain", "alice@external.test", "Hi", "just text");
    assert!(ingest_inbound_record(&m, &rec).expect("ingest ok"));
    let snap = m.snapshot();
    let detail = m.thread_detail(snap.threads[0].thread_id.clone()).unwrap();
    assert!(
        attachment_blocks(&detail.messages[0].document).is_empty(),
        "a body-only message extracts zero attachments"
    );
}

// ── attachment refill: an evicted mail attachment is re-read from its record ──
//
// `conversations.md` § Attachments → *Retention*: the store is a bounded cache,
// and a mail attachment's bytes keep resting in its record's MIME, so a render
// that misses an evicted one has the mail sweep re-read exactly that record —
// by mailbox and UID, since the two mailboxes number UIDs independently.

/// A decrypted mail record carrying one attachment per `files` entry, built by
/// the same MIME writer the SMTP send path uses.
fn record_with_attachments(
    mailbox: MailFeed,
    uid: u32,
    subject: &str,
    files: &[(&str, &[u8])],
) -> InboundMailRecord {
    let parts: Vec<fauna_conversations::rfc5322::MimeAttachment<'_>> = files
        .iter()
        .map(
            |&(filename, bytes)| fauna_conversations::rfc5322::MimeAttachment {
                filename,
                mime_type: "application/octet-stream",
                bytes,
            },
        )
        .collect();
    let rfc5322 = fauna_conversations::rfc5322::build_message(
        "alice@external.test",
        &["me@host.test".to_string()],
        Some(subject),
        "see attached",
        &format!("<{subject}-{uid}@external.test>"),
        None,
        1_700_000_000,
        &parts,
    );
    InboundMailRecord {
        uid,
        mailbox,
        message_id: format!("seg-{subject}-{uid}").into_bytes(),
        internal_date_ms: 1_700_000_000_000,
        rfc5322,
        suppress_from_view: false,
        has_seen_flag: false,
    }
}

fn handle(bytes: &[u8]) -> String {
    blake3::hash(bytes).to_hex().to_string()
}

/// One mailbox, paging like the real feed and journalling every `fetch` as
/// `(after_uid, limit)` — so a test sees exactly which re-reads the refill made
/// through the seam's default `fetch_one`. Records can be removed to model a
/// message moved to Junk or expunged.
struct JournalledMailbox {
    records: Mutex<Vec<InboundMailRecord>>,
    fetches: Mutex<Vec<(u32, u32)>>,
}

impl JournalledMailbox {
    fn new(records: Vec<InboundMailRecord>) -> Self {
        Self {
            records: Mutex::new(records),
            fetches: Mutex::new(Vec::new()),
        }
    }

    fn fetches_since(&self, from: usize) -> Vec<(u32, u32)> {
        self.fetches.lock().unwrap()[from..].to_vec()
    }
}

#[async_trait]
impl InboundMailSource for JournalledMailbox {
    async fn fetch(&self, after_uid: u32, limit: u32) -> Result<InboundMailPage, String> {
        self.fetches.lock().unwrap().push((after_uid, limit));
        let mut records: Vec<InboundMailRecord> = self
            .records
            .lock()
            .unwrap()
            .iter()
            .filter(|r| r.uid > after_uid)
            .cloned()
            .collect();
        records.sort_by_key(|r| r.uid);
        if limit > 0 {
            records.truncate(limit as usize);
        }
        Ok(InboundMailPage {
            skipped: Vec::new(),
            highest_modseq: 0,
            records,
            more: false,
        })
    }
}

#[tokio::test]
async fn an_evicted_mail_attachment_is_re_read_from_its_record_on_the_next_mail_sweep() {
    let m = smtp_manager();
    m.set_attachment_store_budget_for_test(100);
    let (a, b, c) = ([b'a'; 40], [b'b'; 40], [b'c'; 40]);
    let inbox = JournalledMailbox::new(vec![
        record_with_attachments(MailFeed::Inbox, 1, "one", &[("a.bin", &a)]),
        record_with_attachments(MailFeed::Inbox, 2, "two", &[("b.bin", &b)]),
        record_with_attachments(MailFeed::Inbox, 3, "three", &[("c.bin", &c)]),
    ]);
    let (mut cursor, mut seen) = (0u32, HashSet::new());
    assert_eq!(
        poll_inbound_mail(&inbox, &m, &mut cursor, &mut seen, 50)
            .await
            .unwrap(),
        3
    );
    assert!(
        m.attachment_bytes(handle(&a)).is_none(),
        "three 40-byte attachments under a 100-byte budget: the first was evicted"
    );

    // That miss was a render asking for the bytes: the mail sweep re-reads the
    // one record they rest in, and nothing else.
    let before = inbox.fetches.lock().unwrap().len();
    let refilled = fauna_conversations::backends::smtp::refill_evicted_mail_attachments(
        Some(&inbox),
        None,
        &m,
    )
    .await;
    assert_eq!(refilled, 1, "one wanted attachment, one refill");
    assert_eq!(
        inbox.fetches_since(before),
        vec![(0, 1)],
        "one read of one record — the record at uid 1 — no re-page of the mailbox"
    );
    assert_eq!(
        m.attachment_bytes(handle(&a)).as_deref(),
        Some(&a[..]),
        "the evicted bytes resolve again, re-parsed from the record's MIME"
    );
    assert!(
        m.attachment_store_resident_bytes() <= 100,
        "the refill respects the budget too"
    );

    // Nothing wanted → nothing read.
    let before = inbox.fetches.lock().unwrap().len();
    assert_eq!(
        fauna_conversations::backends::smtp::refill_evicted_mail_attachments(
            Some(&inbox),
            None,
            &m,
        )
        .await,
        0
    );
    assert!(inbox.fetches_since(before).is_empty());
}

#[tokio::test]
async fn a_record_the_mailbox_no_longer_holds_leaves_its_attachment_declared_and_forgotten() {
    let m = smtp_manager();
    m.set_attachment_store_budget_for_test(50);
    let (a, b) = ([b'a'; 40], [b'b'; 40]);
    let inbox = JournalledMailbox::new(vec![
        record_with_attachments(MailFeed::Inbox, 1, "one", &[("a.bin", &a)]),
        record_with_attachments(MailFeed::Inbox, 2, "two", &[("b.bin", &b)]),
    ]);
    let (mut cursor, mut seen) = (0u32, HashSet::new());
    poll_inbound_mail(&inbox, &m, &mut cursor, &mut seen, 50)
        .await
        .unwrap();

    // The message moved to Junk: INBOX no longer holds uid 1, so the read one
    // past uid 0 answers with uid 2 instead — a genuine gone.
    inbox.records.lock().unwrap().retain(|r| r.uid != 1);
    assert!(m.attachment_bytes(handle(&a)).is_none());
    let before = inbox.fetches.lock().unwrap().len();
    assert_eq!(
        fauna_conversations::backends::smtp::refill_evicted_mail_attachments(
            Some(&inbox),
            None,
            &m,
        )
        .await,
        0
    );
    assert_eq!(
        inbox.fetches_since(before),
        vec![(0, 1)],
        "the miss was wanted and its record asked for once — so what follows \
         proves a forget, not a handle that was never remembered"
    );
    assert!(m.attachment_bytes(handle(&a)).is_none(), "declared");

    // Forgotten, not merely missed: the next render's miss wants nothing, so
    // the next sweep reads nothing.
    let before = inbox.fetches.lock().unwrap().len();
    assert_eq!(
        fauna_conversations::backends::smtp::refill_evicted_mail_attachments(
            Some(&inbox),
            None,
            &m,
        )
        .await,
        0
    );
    assert!(
        inbox.fetches_since(before).is_empty(),
        "a gone record is never asked for again"
    );
}

/// UIDs are per mailbox: INBOX and Sent both hold a record 1, carrying
/// different files, so each evicted attachment must be re-read from the mailbox
/// it arrived from — a UID alone would fetch the wrong record (and the
/// content-address check would then forget a perfectly fetchable attachment).
#[tokio::test]
async fn the_refill_re_reads_each_attachment_from_the_mailbox_it_came_from() {
    let m = smtp_manager();
    m.set_attachment_store_budget_for_test(50);
    let (received, sent_file) = ([b'r'; 40], [b's'; 40]);
    let inbox = JournalledMailbox::new(vec![record_with_attachments(
        MailFeed::Inbox,
        1,
        "received",
        &[("r.bin", &received)],
    )]);
    let sent = JournalledMailbox::new(vec![record_with_attachments(
        MailFeed::Sent,
        1,
        "sent",
        &[("s.bin", &sent_file)],
    )]);
    let (mut inbox_uid, mut sent_uid) = (0u32, 0u32);
    poll_inbound_mail(&inbox, &m, &mut inbox_uid, &mut HashSet::new(), 50)
        .await
        .unwrap();
    poll_inbound_mail(&sent, &m, &mut sent_uid, &mut HashSet::new(), 50)
        .await
        .unwrap();
    assert!(
        m.attachment_bytes(handle(&received)).is_none(),
        "the INBOX attachment was evicted by the Sent one"
    );

    let (inbox_before, sent_before) = (
        inbox.fetches.lock().unwrap().len(),
        sent.fetches.lock().unwrap().len(),
    );
    assert_eq!(
        fauna_conversations::backends::smtp::refill_evicted_mail_attachments(
            Some(&inbox),
            Some(&sent),
            &m,
        )
        .await,
        1
    );
    assert_eq!(inbox.fetches_since(inbox_before), vec![(0, 1)]);
    assert!(
        sent.fetches_since(sent_before).is_empty(),
        "Sent's record 1 is a different message"
    );
    assert_eq!(
        m.attachment_bytes(handle(&received)).as_deref(),
        Some(&received[..])
    );

    // That refill evicted the Sent attachment in turn; it comes back from Sent.
    assert!(m.attachment_bytes(handle(&sent_file)).is_none());
    let (inbox_before, sent_before) = (
        inbox.fetches.lock().unwrap().len(),
        sent.fetches.lock().unwrap().len(),
    );
    assert_eq!(
        fauna_conversations::backends::smtp::refill_evicted_mail_attachments(
            Some(&inbox),
            Some(&sent),
            &m,
        )
        .await,
        1
    );
    assert_eq!(sent.fetches_since(sent_before), vec![(0, 1)]);
    assert!(inbox.fetches_since(inbox_before).is_empty());
    assert_eq!(
        m.attachment_bytes(handle(&sent_file)).as_deref(),
        Some(&sent_file[..])
    );
}
