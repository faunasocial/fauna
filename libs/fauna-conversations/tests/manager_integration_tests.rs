use fauna_conversations::backend::{ConvRpcError, LinkPreviewResolution, LinkPreviewRpc};
use fauna_conversations::backends::mock::MockRailBackend;
use fauna_conversations::*;
use fauna_core::data::UnattestedVerdict;
use std::sync::Arc;

fn smtp_inbound(
    sender: &str,
    subject: Option<&str>,
    body: &str,
    in_reply_to: Option<MessageId>,
) -> RailInboundMessage {
    RailInboundMessage {
        rail: Rail::Smtp,
        sender: TypedAddress::Email {
            email_address: sender.to_string(),
        },
        recipients: vec![TypedAddress::Email {
            email_address: "me@host.test".into(),
        }],
        subject: subject.map(str::to_string),
        body: body.to_string(),
        body_format: BodyFormat::PlainText,
        timestamp_ms: 0,
        message_id: MessageId(format!("msg-{}", body)),
        in_reply_to,
        attachments: vec![],
        badges: Default::default(),
        legal_takedown_ref: None,
        plane_ref: None,
    }
}

/// The thread labels in `snapshot().threads`, sorted for order-independent
/// assertions.
fn sorted_labels(snap: &ConversationsSnapshot) -> Vec<String> {
    let mut v: Vec<String> = snap.threads.iter().map(|t| t.label.clone()).collect();
    v.sort();
    v
}

#[test]
fn injected_attachment_is_renderable_via_attachment_bytes() {
    // The test-injection seam (`make_attachment_for_test` + an inbound message
    // carrying the snapshot) must reproduce what a real inbound MIME parse does:
    // the rendered message carries the attachment metadata AND the bytes resolve
    // back through the shared loader, so the per-app bubble paints the real
    // image (`dm-attachment-image`) instead of a stub icon.
    let m = ConversationsManager::new();
    m.register_backend(Arc::new(MockRailBackend::new(Rail::Smtp)));

    let bytes = b"\x89PNG\r\n\x1a\n-distinct-png-ish-bytes".to_vec();
    let att = m.make_attachment_for_test("pic.png".into(), "image/png".into(), bytes.clone());
    assert!(att.is_image, "image/* derives is_image");
    assert!(!att.c2pa, "bytes with no C2PA manifest carry no verdict");
    assert_eq!(att.size_bytes, bytes.len() as u64);
    assert_eq!(att.filename, "pic.png");
    assert_eq!(
        m.attachment_bytes(att.blob_hash.clone()).as_deref(),
        Some(bytes.as_slice()),
        "bytes cached under the content-addressed handle",
    );

    let mut msg = smtp_inbound("alice@host.test", Some("photos"), "see pic", None);
    msg.attachments = vec![att.clone()];
    m.ingest_inbound(msg).unwrap();

    let snap = m.snapshot();
    let detail = m.thread_detail(snap.threads[0].thread_id.clone()).unwrap();
    let rendered = &detail.messages[0];
    // Attachments are first-class `Attachment` blocks in the render document
    // (render-model.md § D2), not a sibling snapshot field — every app paints
    // them by walking `document`.
    let rendered_atts = attachment_blocks(&rendered.document);
    assert_eq!(rendered_atts.len(), 1, "attachment flows to the bubble");
    assert_eq!(rendered_atts[0].blob_hash, att.blob_hash);
    assert_eq!(
        m.attachment_bytes(rendered_atts[0].blob_hash.clone())
            .as_deref(),
        Some(bytes.as_slice()),
        "render path resolves the handle to the real bytes",
    );
}

/// The inject seam runs the receive path's own probe
/// (`fauna_media::process::detect_c2pa`) over the injected bytes, so a signed
/// picture carries the real verdict to the bubble's per-attachment
/// `c2pa-badge` — the same reading `attachments_to_inbound` gives a delivered
/// one (`docs/goal/ui/conversations.md` § Attachments "C2PA on-device").
#[test]
fn injected_signed_picture_carries_the_c2pa_verdict_to_the_bubble() {
    let m = ConversationsManager::new();
    m.register_backend(Arc::new(MockRailBackend::new(Rail::Smtp)));
    let signed = include_bytes!("../../../tests/fixtures/c2pa-signed.png").to_vec();
    let att = m.make_attachment_for_test("signed.png".into(), "image/png".into(), signed);
    assert!(att.c2pa, "a C2PA-signed picture is detected at the seam");

    let mut msg = smtp_inbound("alice@host.test", Some("photos"), "signed pic", None);
    msg.attachments = vec![att];
    m.ingest_inbound(msg).unwrap();
    let snap = m.snapshot();
    let detail = m.thread_detail(snap.threads[0].thread_id.clone()).unwrap();
    let rendered = attachment_blocks(&detail.messages[0].document);
    assert_eq!(rendered.len(), 1);
    assert!(rendered[0].c2pa, "the verdict reaches the render document");
}

/// The eviction seam drops exactly the named attachments' bytes from one
/// thread, leaves every other resident, and notifies — so an e2e reaches the
/// re-fetch of an evicted attachment, and the declared placeholder of one with
/// nowhere to be fetched from, without first filling the 128 MiB store
/// (`conversations.md` § Attachments → *Retention*).
#[test]
fn the_eviction_seam_drops_only_the_named_attachments_of_one_thread() {
    let m = ConversationsManager::new();
    m.register_backend(Arc::new(MockRailBackend::new(Rail::Smtp)));
    let pic =
        m.make_attachment_for_test("pic.png".into(), "image/png".into(), b"pic-bytes".to_vec());
    let doc =
        m.make_attachment_for_test("doc.txt".into(), "text/plain".into(), b"doc-bytes".to_vec());
    let mut msg = smtp_inbound("alice@host.test", Some("photos"), "see pic", None);
    msg.attachments = vec![pic.clone(), doc.clone()];
    m.ingest_inbound(msg).unwrap();
    let tid = m.snapshot().threads[0].thread_id.clone();

    struct Ticks(Arc<std::sync::atomic::AtomicUsize>);
    impl SnapshotObserver for Ticks {
        fn on_changed(&self) {
            self.0.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        }
    }
    let notified = Arc::new(std::sync::atomic::AtomicUsize::new(0));
    m.add_observer(Arc::new(Ticks(Arc::clone(&notified))));

    assert_eq!(
        m.evict_thread_attachments_for_test(tid.clone(), "nope.png".into()),
        0
    );
    assert_eq!(
        notified.load(std::sync::atomic::Ordering::SeqCst),
        0,
        "nothing evicted, nothing to redraw"
    );

    assert!(
        m.attachment_resident(pic.blob_hash.clone()),
        "held bytes read as resident"
    );
    assert_eq!(
        m.evict_thread_attachments_for_test(tid.clone(), "pic.png".into()),
        1
    );
    // The render-side residency peek follows the eviction — what an app keying its
    // bubble rebuild on it needs to see — and leaves the other file alone.
    assert!(
        !m.attachment_resident(pic.blob_hash.clone()),
        "an evicted attachment no longer reads as resident"
    );
    assert!(m.attachment_resident(doc.blob_hash.clone()));
    assert!(
        m.attachment_bytes(pic.blob_hash.clone()).is_none(),
        "the named attachment's bytes are gone"
    );
    assert_eq!(
        m.attachment_bytes(doc.blob_hash.clone()).as_deref(),
        Some(&b"doc-bytes"[..]),
        "an attachment of another name stays resident"
    );
    assert!(
        notified.load(std::sync::atomic::Ordering::SeqCst) >= 1,
        "an eviction redraws, so the render misses"
    );
    assert_eq!(
        m.evict_thread_attachments_for_test(tid, "pic.png".into()),
        0,
        "bytes already gone are not evicted twice"
    );
}

#[test]
fn reveal_remote_images_projects_revealed_onto_the_revealed_message_only() {
    // D3 (render-model.md § D3): remote-image reveal state is manager-owned, not a
    // per-app `revealedRemote`/`remoteLoaded` dictionary. A message with a remote
    // `![](http://...)` image renders blocked (RemoteImage.revealed:false) until
    // `reveal_remote_images(id)` opts it in; the next `thread_detail` projects
    // `revealed:true` for THAT message only (the set is keyed by message id), and the
    // posture stays no-persistence (in-memory only).
    let m = ConversationsManager::new();
    m.register_backend(Arc::new(MockRailBackend::new(Rail::Smtp)));

    // Two markdown messages in the same (subject-keyed) thread, each with a remote
    // image. (`smtp_inbound` defaults to PlainText, which doesn't parse markdown —
    // flip to Markdown so the body produces a `RemoteImage` block.)
    let mut a = smtp_inbound(
        "alice@host.test",
        Some("pics"),
        "a ![x](http://img.test/a.png)",
        None,
    );
    a.body_format = BodyFormat::Markdown;
    let aid = a.message_id.clone();
    m.ingest_inbound(a).unwrap();
    let mut b = smtp_inbound(
        "alice@host.test",
        Some("pics"),
        "b ![y](http://img.test/b.png)",
        None,
    );
    b.body_format = BodyFormat::Markdown;
    let bid = b.message_id.clone();
    m.ingest_inbound(b).unwrap();

    let tid = m.snapshot().threads[0].thread_id.clone();

    // Before any reveal: BOTH messages carry a blocked remote image.
    let detail = m.thread_detail(tid.clone()).unwrap();
    assert_eq!(
        detail.messages.len(),
        2,
        "both messages merged into one thread"
    );
    for msg in &detail.messages {
        assert!(
            msg.document.has_blocked_remote_images(),
            "remote images are blocked by default for every message",
        );
    }

    // Reveal only message `a`.
    m.reveal_remote_images(aid.clone());

    let detail = m.thread_detail(tid.clone()).unwrap();
    let by_id = |id: &MessageId| {
        detail
            .messages
            .iter()
            .find(|m| &m.message_id == id)
            .expect("message present")
    };
    assert!(
        !by_id(&aid).document.has_blocked_remote_images(),
        "the revealed message's remote images are now projected revealed",
    );
    assert!(
        by_id(&bid).document.has_blocked_remote_images(),
        "an un-revealed message stays blocked (reveal is scoped per message id)",
    );

    // Idempotent: a repeat reveal of the same id keeps it revealed (no panic / flip).
    m.reveal_remote_images(aid.clone());
    let detail = m.thread_detail(tid).unwrap();
    assert!(
        !detail
            .messages
            .iter()
            .find(|m| m.message_id == aid)
            .unwrap()
            .document
            .has_blocked_remote_images(),
    );
}

/// A canned [`LinkPreviewRpc`] for the manager test: answers every URL with a
/// `Resolved` carrying an og:image hash, so we can assert the manager folds the
/// terminal state onto the bubble's `LinkPreview` block and gates the og:image
/// behind the per-message reveal — the conversations twin of the feed D4 work.
struct StubLinkPreview {
    image_hash: Option<String>,
}

#[async_trait::async_trait]
impl LinkPreviewRpc for StubLinkPreview {
    async fn link_preview_resolve(
        &self,
        url: String,
    ) -> Result<LinkPreviewResolution, ConvRpcError> {
        Ok(LinkPreviewResolution::Resolved {
            title: format!("T:{url}"),
            description: "D".into(),
            image_hash: self.image_hash.clone(),
        })
    }
}

/// The `PreviewState` of the (single) `LinkPreview` block carrying `url` in `doc`,
/// or `None` if the bubble has no such block.
fn link_preview_state<'a>(
    doc: &'a fauna_core::render::RenderDocument,
    url: &str,
) -> Option<&'a fauna_core::render::PreviewState> {
    doc.blocks.iter().find_map(|b| match b {
        fauna_core::render::RenderBlock::LinkPreview { url: u, state } if u == url => Some(state),
        _ => None,
    })
}

#[tokio::test]
async fn resolve_link_preview_folds_resolved_state_and_gates_ogimage_per_message() {
    use fauna_core::render::PreviewState;
    // D4 (render-model.md § D4), the conversations twin of `FeedManager::resolve_link_preview`:
    // a bubble whose body is a bare URL carries a producer-emitted `LinkPreview { Resolving }`
    // block; `resolve_link_preview` records the terminal state by URL, and `thread_detail` folds
    // it onto the bubble — with the og:image blocked-by-default and gated behind the SAME
    // per-message D3 reveal walk conversations already runs.
    let m = ConversationsManager::new();
    m.register_backend(Arc::new(MockRailBackend::new(Rail::Smtp)));
    m.set_link_preview_rpc(Arc::new(StubLinkPreview {
        image_hash: Some("img-hash-1".into()),
    }));

    // A markdown message whose body is a standalone bare URL — the `[url](url)` form (link
    // label == href) the shared producer (`inject_link_previews`) recognises, appending a
    // `LinkPreview { Resolving }` after the link. (`smtp_inbound` defaults to PlainText, which
    // never emits a link-preview — flip to Markdown, exactly as FaunaMls inbound messages arrive.)
    let url = "https://example.com/article";
    let body = format!("[{url}]({url})");
    let mut a = smtp_inbound("alice@host.test", Some("links"), &body, None);
    a.body_format = BodyFormat::Markdown;
    let aid = a.message_id.clone();
    m.ingest_inbound(a).unwrap();

    let tid = m.snapshot().threads[0].thread_id.clone();

    // Before resolve: the bubble carries the block in `Resolving` (the producer's
    // inline link still shows; no card yet — render-model.md § D4 "Resolving → no card").
    let detail = m.thread_detail(tid.clone()).unwrap();
    assert!(
        matches!(
            link_preview_state(&detail.messages[0].document, url),
            Some(PreviewState::Resolving)
        ),
        "producer emits a Resolving preview for the bare-url bubble",
    );

    // Resolve once.
    m.resolve_link_preview(url.to_string()).await;

    // After resolve: `thread_detail` folds the terminal state onto the bubble block,
    // with the og:image blocked-by-default (`revealed:false`), and the SHARED
    // `has_blocked_remote_images` counts the og:image so the reveal button shows.
    let detail = m.thread_detail(tid.clone()).unwrap();
    match link_preview_state(&detail.messages[0].document, url) {
        Some(PreviewState::Resolved {
            title,
            image_hash,
            revealed,
            ..
        }) => {
            assert_eq!(title, &format!("T:{url}"));
            assert_eq!(image_hash.as_deref(), Some("img-hash-1"));
            assert!(!revealed, "og:image blocked-by-default at resolution");
        }
        other => panic!("expected Resolved, got {other:?}"),
    }
    assert!(
        detail.messages[0].document.has_blocked_remote_images(),
        "an un-revealed og:image surfaces the message's reveal button (shared predicate)",
    );

    // Reveal this message → the existing per-message D3 walk flips the og:image revealed,
    // for free (the fold runs BEFORE the reveal walk in `thread_detail`).
    m.reveal_remote_images(aid.clone());
    let detail = m.thread_detail(tid.clone()).unwrap();
    match link_preview_state(&detail.messages[0].document, url) {
        Some(PreviewState::Resolved { revealed, .. }) => {
            assert!(revealed, "reveal flips the og:image revealed flag")
        }
        other => panic!("expected Resolved, got {other:?}"),
    }
    assert!(
        !detail.messages[0].document.has_blocked_remote_images(),
        "the revealed og:image is no longer blocked remote content",
    );

    // Idempotent: a repeat resolve for an already-resolved URL is a no-op (cache hit),
    // the same render-loop-safe discipline as the feed manager.
    m.resolve_link_preview(url.to_string()).await;
    let detail = m.thread_detail(tid).unwrap();
    assert!(matches!(
        link_preview_state(&detail.messages[0].document, url),
        Some(PreviewState::Resolved { .. })
    ));
}

#[test]
fn smtp_subject_creates_subject_keyed_thread() {
    let m = ConversationsManager::new();
    m.register_backend(Arc::new(MockRailBackend::new(Rail::Smtp)));
    m.ingest_inbound(smtp_inbound(
        "alice@host.test",
        Some("Q4 budget"),
        "numbers",
        None,
    ))
    .unwrap();
    let snap = m.snapshot();
    assert_eq!(snap.threads.len(), 1);
    // Label preserves the original-case subject for display; the key
    // still uses normalize_subject for matching.
    assert_eq!(snap.threads[0].label, "Q4 budget");
}

#[test]
fn re_prefix_merges_threads() {
    let m = ConversationsManager::new();
    m.register_backend(Arc::new(MockRailBackend::new(Rail::Smtp)));
    m.ingest_inbound(smtp_inbound(
        "alice@host.test",
        Some("Q4 budget"),
        "first",
        None,
    ))
    .unwrap();
    m.ingest_inbound(smtp_inbound(
        "alice@host.test",
        Some("Re: Q4 budget"),
        "second",
        None,
    ))
    .unwrap();
    let snap = m.snapshot();
    assert_eq!(snap.threads.len(), 1);
}

#[test]
fn in_reply_to_overrides_different_subject() {
    let m = ConversationsManager::new();
    m.register_backend(Arc::new(MockRailBackend::new(Rail::Smtp)));
    m.ingest_inbound(smtp_inbound(
        "alice@host.test",
        Some("Q4 budget"),
        "first",
        None,
    ))
    .unwrap();
    let parent = MessageId("msg-first".into());
    m.ingest_inbound(smtp_inbound(
        "alice@host.test",
        Some("Re: completely different"),
        "second",
        Some(parent),
    ))
    .unwrap();
    let snap = m.snapshot();
    assert_eq!(
        snap.threads.len(),
        1,
        "second message must merge into parent's thread"
    );
}

#[test]
fn no_subject_creates_participant_keyed_thread() {
    let m = ConversationsManager::new();
    m.register_backend(Arc::new(MockRailBackend::new(Rail::Smtp)));
    m.ingest_inbound(smtp_inbound("alice@host.test", None, "first", None))
        .unwrap();
    m.ingest_inbound(smtp_inbound("alice@host.test", None, "second", None))
        .unwrap();
    let snap = m.snapshot();
    assert_eq!(snap.threads.len(), 1);
    let detail = m.thread_detail(snap.threads[0].thread_id.clone()).unwrap();
    assert_eq!(detail.flavor, ThreadFlavor::OneToOne);
    assert_eq!(detail.messages.len(), 2);
}

#[test]
fn reply_with_unresolved_in_reply_to_merges_by_subject() {
    // The "original message missing" bug (linux manual testing): a reply's
    // In-Reply-To points at a Message-ID we never stored (the original was sent
    // from another MUA / its id differs), but the subject matches. It must merge
    // into the original's subject thread, not strand itself in a separate thread.
    let m = ConversationsManager::new();
    m.register_backend(Arc::new(MockRailBackend::new(Rail::Smtp)));
    // Original (no in_reply_to) → SubjectKeyed thread; stored message_id "msg-original".
    m.ingest_inbound(smtp_inbound(
        "alice@host.test",
        Some("Invoice 42"),
        "original",
        None,
    ))
    .unwrap();
    // Reply references an UNKNOWN parent (not "msg-original") but the same subject.
    m.ingest_inbound(smtp_inbound(
        "alice@host.test",
        Some("Re: Invoice 42"),
        "the reply",
        Some(MessageId("<unknown-elsewhere@other.host>".into())),
    ))
    .unwrap();
    let snap = m.snapshot();
    assert_eq!(
        snap.threads.len(),
        1,
        "a reply with an unresolved in_reply_to must merge with the original by \
         subject, not create a separate thread"
    );
    let detail = m.thread_detail(snap.threads[0].thread_id.clone()).unwrap();
    assert_eq!(
        detail.messages.len(),
        2,
        "both the original and the reply belong to the one thread"
    );
}

#[test]
fn distinct_subjects_create_distinct_threads() {
    let m = ConversationsManager::new();
    m.register_backend(Arc::new(MockRailBackend::new(Rail::Smtp)));
    m.ingest_inbound(smtp_inbound(
        "alice@host.test",
        Some("Q4 budget"),
        "A",
        None,
    ))
    .unwrap();
    m.ingest_inbound(smtp_inbound("alice@host.test", Some("Lunch?"), "B", None))
        .unwrap();
    let snap = m.snapshot();
    assert_eq!(snap.threads.len(), 2);
    let mut labels: Vec<&str> = snap.threads.iter().map(|t| t.label.as_str()).collect();
    labels.sort();
    assert_eq!(labels, vec!["Lunch?", "Q4 budget"]);
}

#[test]
fn subject_divider_set_on_subject_change() {
    let m = ConversationsManager::new();
    m.register_backend(Arc::new(MockRailBackend::new(Rail::Smtp)));
    let parent_msg = MessageId("msg-first".into());
    m.ingest_inbound(smtp_inbound(
        "alice@host.test",
        Some("Q4 budget"),
        "first",
        None,
    ))
    .unwrap();
    m.ingest_inbound(smtp_inbound(
        "alice@host.test",
        Some("lunch?"),
        "second",
        Some(parent_msg),
    ))
    .unwrap();
    let snap = m.snapshot();
    assert_eq!(snap.threads.len(), 1, "second must merge via in_reply_to");
    let detail = m.thread_detail(snap.threads[0].thread_id.clone()).unwrap();
    assert_eq!(detail.messages.len(), 2);
    assert!(
        detail.messages[1].subject_line.is_some(),
        "subject change must populate subject_line"
    );
}

#[test]
fn open_and_cancel_add_participant_round_trips_the_slot() {
    let m = ConversationsManager::new();
    m.install_mock_backends_for_test();
    let tid = m.create_mls_group(vec![
        TypedAddress::Email {
            email_address: "bob@host.test".into(),
        },
        TypedAddress::Email {
            email_address: "alice@host.test".into(),
        },
    ]);
    assert!(m.snapshot().add_participant.is_none());

    m.open_add_participant(tid.clone());
    let ap = m.snapshot().add_participant.expect("slot open");
    assert_eq!(ap.target_thread_id, tid);
    assert_eq!(ap.picker.resolve_state, ResolveState::Idle);
    assert!(ap.picker.raw_input.is_empty());

    m.cancel_add_participant();
    assert!(m.snapshot().add_participant.is_none());
}

/// The overlay carries its own offline-gate discriminant, for each of the
/// three thread shapes an app can open it on. Without this the apps have no
/// way to tell "confirming needs a nest" (a bound MLS group, whose add commit
/// opens by fetching the newcomer's key package) from "confirming issues
/// nothing" (a 1:1 fork, any non-FaunaMls rail) — and a blanket gate on
/// `fauna.conversations.keypackage.fetch` would grey a gesture that works
/// perfectly offline. `docs/goal/architecture/account-data-plane.md` § The
/// offline-mutation contract → *How a surface asks*.
#[test]
fn the_add_participant_overlay_carries_its_offline_gate_discriminant() {
    let m = ConversationsManager::new();
    m.install_mock_backends_for_test();

    // (1) A bound FaunaMls group — the one shape that reaches the wire.
    let group = m.create_mls_group(vec![
        TypedAddress::Email {
            email_address: "bob@host.test".into(),
        },
        TypedAddress::Email {
            email_address: "alice@host.test".into(),
        },
    ]);
    m.open_add_participant(group.clone());
    assert!(
        m.snapshot().add_participant.unwrap().in_place_mls_group,
        "adding to a bound MLS group posts the Commit + Welcome — it needs a nest"
    );
    m.cancel_add_participant();

    // (2) A FaunaMls 1:1 — confirming FORKS a new group locally; the fork's
    // first send is what bootstraps it, so this gesture issues nothing.
    m.ingest_inbound(fauna_inbound("carol@host.test", "hi"))
        .unwrap();
    let fauna_1v1 = m
        .snapshot()
        .threads
        .into_iter()
        .find(|t| t.flavor == ThreadFlavor::OneToOne)
        .expect("the inbound fauna message keys a 1:1")
        .thread_id;
    m.open_add_participant(fauna_1v1);
    assert!(
        !m.snapshot().add_participant.unwrap().in_place_mls_group,
        "a 1:1 fork is snapshot-only — greying it offline would be an over-claim"
    );
    m.cancel_add_participant();

    // (3) A non-FaunaMls rail — no wire membership op exists at all.
    m.ingest_inbound(smtp_inbound("dave@host.test", None, "hello", None))
        .unwrap();
    let smtp = m
        .snapshot()
        .threads
        .into_iter()
        .find(|t| t.rail == Rail::Smtp)
        .expect("the inbound smtp message keys a thread")
        .thread_id;
    m.open_add_participant(smtp);
    assert!(
        !m.snapshot().add_participant.unwrap().in_place_mls_group,
        "SMTP adds a recipient in place with no wire membership op"
    );
}

/// Typing owes a probe: any non-empty input is `Resolving` until the async
/// `resolve_recipient` reports, so the status never claims "Resolved" for an
/// address no rail has vouched for (`docs/goal/ui/conversations.md` § Errors &
/// edge cases → *The picker tells the truth*). Empty input is `Idle`.
#[test]
fn add_participant_recipient_input_is_resolving_until_probed() {
    let m = ConversationsManager::new();
    m.install_mock_backends_for_test();
    let tid = m.create_mls_group(vec![TypedAddress::Email {
        email_address: "bob@host.test".into(),
    }]);
    m.open_add_participant(tid);

    m.set_add_participant_recipient_input("not an address".into());
    assert_eq!(
        m.snapshot().add_participant.unwrap().picker.resolve_state,
        ResolveState::Resolving,
        "junk is not judged by shape either — the probe reports NotFound"
    );

    m.set_add_participant_recipient_input("alice@host.test".into());
    let ap = m.snapshot().add_participant.unwrap();
    assert_eq!(ap.picker.resolve_state, ResolveState::Resolving);
    assert_eq!(ap.picker.raw_input, "alice@host.test");

    m.set_add_participant_recipient_input(String::new());
    assert_eq!(
        m.snapshot().add_participant.unwrap().picker.resolve_state,
        ResolveState::Idle
    );
}

#[test]
fn accept_add_participant_chip_pushes_and_clears_input() {
    let m = ConversationsManager::new();
    m.install_mock_backends_for_test();
    let tid = m.create_mls_group(vec![TypedAddress::Email {
        email_address: "bob@host.test".into(),
    }]);
    m.open_add_participant(tid);
    m.set_add_participant_recipient_input("alice@host.test".into());
    m.accept_add_participant_chip(TypedAddress::Email {
        email_address: "alice@host.test".into(),
    });

    let ap = m.snapshot().add_participant.unwrap();
    assert_eq!(ap.picker.chips.len(), 1);
    assert!(ap.picker.raw_input.is_empty());
    assert_eq!(ap.picker.resolve_state, ResolveState::Resolved);
}

#[test]
fn clear_for_test_clears_add_participant_slot() {
    let m = ConversationsManager::new();
    m.install_mock_backends_for_test();
    let tid = m.create_mls_group(vec![TypedAddress::Email {
        email_address: "bob@host.test".into(),
    }]);
    m.open_add_participant(tid);
    assert!(m.snapshot().add_participant.is_some());
    m.clear_for_test();
    assert!(m.snapshot().add_participant.is_none());
}

fn fauna_inbound(sender: &str, body: &str) -> RailInboundMessage {
    RailInboundMessage {
        rail: Rail::FaunaMls,
        sender: TypedAddress::Email {
            email_address: sender.to_string(),
        },
        recipients: vec![TypedAddress::Email {
            email_address: "me@host.test".into(),
        }],
        subject: None,
        body: body.to_string(),
        body_format: BodyFormat::PlainText,
        timestamp_ms: 0,
        message_id: MessageId(format!("msg-{body}")),
        in_reply_to: None,
        attachments: vec![],
        badges: Default::default(),
        legal_takedown_ref: None,
        plane_ref: None,
    }
}

#[tokio::test]
async fn confirm_add_participant_forks_fauna_oneonone_and_selects_new_thread() {
    let m = ConversationsManager::new();
    m.install_mock_backends_for_test();
    m.ingest_inbound(fauna_inbound("bob@host.test", "hi"))
        .unwrap();
    let before = m.snapshot();
    assert_eq!(before.threads.len(), 1);
    let bob_thread = before.threads[0].thread_id.clone();
    assert_eq!(before.threads[0].flavor, ThreadFlavor::OneToOne);

    m.select_thread(bob_thread.clone());
    m.open_add_participant(bob_thread.clone());
    m.set_add_participant_recipient_input("alice@host.test".into());
    m.accept_add_participant_chip(TypedAddress::Email {
        email_address: "alice@host.test".into(),
    });
    let new_id = m
        .confirm_add_participant()
        .await
        .expect("returns the resulting thread id");

    assert_ne!(new_id, bob_thread, "1:1 fork creates a new thread");
    let after = m.snapshot();
    assert_eq!(after.threads.len(), 2, "fork added one thread");
    let bob_now = after
        .threads
        .iter()
        .find(|t| t.thread_id == bob_thread)
        .unwrap();
    assert_eq!(bob_now.participant_count, 2, "original 1:1 untouched");
    assert_eq!(
        after.selected_thread_id,
        Some(new_id),
        "navigated to the new group"
    );
    assert!(after.add_participant.is_none(), "overlay closed");
}

#[tokio::test]
async fn confirm_add_participant_adds_in_place_on_mls_group() {
    let m = ConversationsManager::new();
    m.install_mock_backends_for_test();
    let tid = m.create_mls_group(vec![
        TypedAddress::Email {
            email_address: "bob@host.test".into(),
        },
        TypedAddress::Email {
            email_address: "alice@host.test".into(),
        },
    ]);
    let before = m.snapshot();
    m.select_thread(tid.clone());
    m.open_add_participant(tid.clone());
    m.accept_add_participant_chip(TypedAddress::Email {
        email_address: "carol@host.test".into(),
    });
    let result = m
        .confirm_add_participant()
        .await
        .expect("returns a thread id");

    assert_eq!(result, tid, "group add does not fork");
    let after = m.snapshot();
    assert_eq!(after.threads.len(), before.threads.len(), "no new thread");
    let group = after.threads.iter().find(|t| t.thread_id == tid).unwrap();
    assert_eq!(
        group.participant_count,
        before.threads[0].participant_count + 1
    );
    assert_eq!(after.selected_thread_id, Some(tid), "selection unchanged");
}

#[tokio::test]
async fn confirm_add_participant_smtp_oneonone_stays_in_place() {
    let m = ConversationsManager::new();
    m.install_mock_backends_for_test();
    // A 2-party SMTP message with no subject keys to a OneToOne thread.
    m.ingest_inbound(smtp_inbound("alice@host.test", None, "hello", None))
        .unwrap();
    let before = m.snapshot();
    assert_eq!(before.threads.len(), 1);
    let tid = before.threads[0].thread_id.clone();
    assert_eq!(before.threads[0].flavor, ThreadFlavor::OneToOne);

    m.open_add_participant(tid.clone());
    m.accept_add_participant_chip(TypedAddress::Email {
        email_address: "carol@host.test".into(),
    });
    let result = m
        .confirm_add_participant()
        .await
        .expect("returns a thread id");

    assert_eq!(result, tid, "non-FaunaMls 1:1 does not fork");
    let after = m.snapshot();
    assert_eq!(after.threads.len(), 1, "no new thread for SMTP add");
    assert_eq!(
        after.threads[0].participant_count,
        before.threads[0].participant_count + 1
    );
}

#[tokio::test]
async fn accept_current_recipient_chip_targets_the_active_picker() {
    let m = ConversationsManager::new();
    m.install_mock_backends_for_test();
    let tid = m.create_mls_group(vec![TypedAddress::Email {
        email_address: "bob@host.test".into(),
    }]);

    // New-thread picker active.
    m.start_new_conversation();
    m.set_new_thread_recipient_input("dan@host.test".into());
    m.resolve_recipient().await;
    assert!(m.accept_current_recipient_chip());
    assert_eq!(
        m.snapshot()
            .new_thread_compose
            .unwrap()
            .recipient_picker
            .unwrap()
            .chips
            .len(),
        1
    );

    // Add-participant picker active — wins over new-thread.
    m.open_add_participant(tid);
    m.set_add_participant_recipient_input("erin@host.test".into());
    m.resolve_recipient().await;
    assert!(m.accept_current_recipient_chip());
    assert_eq!(m.snapshot().add_participant.unwrap().picker.chips.len(), 1);

    // Empty add-participant input: no chip pushed.
    m.set_add_participant_recipient_input(String::new());
    assert!(!m.accept_current_recipient_chip());
}

#[tokio::test]
async fn switching_to_a_thread_preserves_the_new_thread_draft() {
    // Regression + feature (user-confirmed 2026-06-21): clicking
    // `new-conversation-button` (+) then clicking an existing conversation must
    // (a) show that conversation — the old "+ makes old conversations
    // inaccessible" bug — and (b) PRESERVE the half-written new message so
    // re-opening + restores it. conversations.md § Persistence: switching keeps
    // each half-written message; only an explicit cancel or a successful send
    // discards the new-thread draft.
    let m = ConversationsManager::new();
    m.install_mock_backends_for_test();
    let tid = m.create_mls_group(vec![TypedAddress::Email {
        email_address: "bob@host.test".into(),
    }]);

    // Click + and start writing a new message (recipient + body).
    m.start_new_conversation();
    m.set_new_thread_recipient_input("dan@host.test".into());
    m.resolve_recipient().await;
    assert!(m.accept_current_recipient_chip());
    m.set_new_thread_body("half-written new message".into());

    // Click an existing conversation: the composer VIEW deactivates (so the
    // thread shows) but the draft is kept.
    m.select_thread(tid.clone());
    let after_switch = m.snapshot();
    assert!(
        after_switch.new_thread_compose.is_none(),
        "new-thread composer view is inactive while viewing a thread (fixes 'inaccessible')"
    );
    assert_eq!(
        after_switch.selected_thread_id,
        Some(tid.clone()),
        "the clicked thread is now the shown thread"
    );

    // Click + again: the half-written new message comes back intact.
    m.start_new_conversation();
    let reopened = m
        .snapshot()
        .new_thread_compose
        .expect("composer active again after re-opening +");
    assert_eq!(
        reopened.body_draft, "half-written new message",
        "the typed body is preserved across the switch"
    );
    assert_eq!(
        reopened
            .recipient_picker
            .expect("recipient picker restored")
            .chips
            .len(),
        1,
        "the typed recipient chip is preserved"
    );
}

#[test]
fn cancelling_new_conversation_discards_the_draft() {
    // The explicit Cancel affordance (and a successful send) are the ONLY paths
    // that drop the new-thread draft — conversations.md § Persistence.
    let m = ConversationsManager::new();
    m.install_mock_backends_for_test();

    m.start_new_conversation();
    m.set_new_thread_body("scratch".into());
    m.cancel_new_conversation();
    assert!(
        m.snapshot().new_thread_compose.is_none(),
        "composer inactive after cancel"
    );

    // Re-opening starts fresh — the cancelled draft is gone.
    m.start_new_conversation();
    let fresh = m
        .snapshot()
        .new_thread_compose
        .expect("composer active after re-open");
    assert_eq!(fresh.body_draft, "", "cancel discarded the prior body");
}

#[tokio::test]
async fn deactivating_new_conversation_preserves_the_draft() {
    // Nav-back out of the new-thread composer (a mobile back button / a pane
    // dismiss) deactivates the composer VIEW but PRESERVES the half-written
    // draft so re-opening + restores it. This is distinct from
    // `cancel_new_conversation` (the explicit discard) and is the nav-back
    // equivalent of `select_thread`'s deactivate — except no thread is selected,
    // so the thread LIST (not a thread) is shown. conversations.md § Persistence:
    // only an explicit cancel or a successful send drops the new-thread draft.
    let m = ConversationsManager::new();
    m.install_mock_backends_for_test();

    m.start_new_conversation();
    m.set_new_thread_recipient_input("dan@host.test".into());
    m.resolve_recipient().await;
    assert!(m.accept_current_recipient_chip());
    m.set_new_thread_body("half-written new message".into());

    // Plain back out of the composer: the view deactivates, no thread is
    // selected, and the draft is KEPT.
    m.deactivate_new_conversation();
    let after = m.snapshot();
    assert!(
        after.new_thread_compose.is_none(),
        "composer view is inactive after a plain back"
    );
    assert!(
        after.selected_thread_id.is_none(),
        "a plain back selects no thread (unlike select_thread)"
    );

    // Re-opening + brings the half-written message back intact.
    m.start_new_conversation();
    let reopened = m
        .snapshot()
        .new_thread_compose
        .expect("composer active again after re-opening +");
    assert_eq!(
        reopened.body_draft, "half-written new message",
        "the typed body is preserved across the deactivate"
    );
    assert_eq!(
        reopened
            .recipient_picker
            .expect("recipient picker restored")
            .chips
            .len(),
        1,
        "the typed recipient chip is preserved"
    );
}

#[test]
fn per_thread_drafts_survive_switching_between_conversations() {
    // conversations.md § Persistence: each existing conversation keeps its own
    // half-written reply across switches — switching never disturbs another
    // thread's draft (one draft per old conversation).
    let m = ConversationsManager::new();
    m.install_mock_backends_for_test();
    let a = m.create_mls_group(vec![TypedAddress::Email {
        email_address: "a@host.test".into(),
    }]);
    let b = m.create_mls_group(vec![TypedAddress::Email {
        email_address: "b@host.test".into(),
    }]);

    m.select_thread(a.clone());
    m.set_compose_body(a.clone(), "draft for A".into());
    m.select_thread(b.clone());
    m.set_compose_body(b.clone(), "draft for B".into());

    // Switch back to A: its draft is intact and B's is untouched.
    m.select_thread(a.clone());
    assert_eq!(
        m.thread_detail(a).unwrap().compose.body_draft,
        "draft for A"
    );
    assert_eq!(
        m.thread_detail(b).unwrap().compose.body_draft,
        "draft for B"
    );
}

// ── Selected message (SearchNav::Mail's second half) ───────────────────
//
// `search.md` § State & data shape: `Mail`'s contract is "open the thread AND
// select this message in it". The selection is manager state; `thread_detail`
// resolves it against the fetched window on every emit. These four tests pin
// the properties that read-time resolve buys — each fails against a design that
// stores the flag on the message at selection time.

/// The contract itself: the named message, and only it, comes back selected.
#[test]
fn select_thread_and_message_marks_the_message_the_caller_named() {
    let m = ConversationsManager::new();
    m.install_mock_backends_for_test();
    m.ingest_inbound(smtp_inbound("alice@host.test", Some("s"), "first", None))
        .unwrap();
    m.ingest_inbound(smtp_inbound("alice@host.test", Some("s"), "second", None))
        .unwrap();
    let tid = m.snapshot().threads[0].thread_id.clone();

    m.select_thread_and_message(tid.clone(), MessageId("msg-second".into()));

    let detail = m.thread_detail(tid.clone()).unwrap();
    assert_eq!(
        detail.selected_message_id,
        Some(MessageId("msg-second".into())),
        "the named message is the selected one"
    );
    assert_eq!(m.snapshot().selected_thread_id, Some(tid), "thread too");
    // The marker is a single-message concept: exactly one message answers to it.
    assert_eq!(
        detail
            .messages
            .iter()
            .filter(|msg| Some(&msg.message_id) == detail.selected_message_id.as_ref())
            .count(),
        1
    );
}

/// The guard. A selection this thread cannot place resolves to `None` rather
/// than reaching an app that would have to paint a marker with nowhere to put
/// it. This is the state `send_new_thread` / the 1:1 participant fork leave
/// behind when they select a *different* thread without clearing the message.
#[test]
fn a_selection_this_thread_does_not_hold_resolves_to_none() {
    let m = ConversationsManager::new();
    m.install_mock_backends_for_test();
    m.ingest_inbound(smtp_inbound("alice@host.test", Some("s"), "first", None))
        .unwrap();
    let tid = m.snapshot().threads[0].thread_id.clone();

    m.select_thread_and_message(tid.clone(), MessageId("msg-not-in-this-thread".into()));

    assert_eq!(
        m.thread_detail(tid).unwrap().selected_message_id,
        None,
        "an unplaceable selection is not handed to the view"
    );
}

/// The late arrival — the property that makes read-time resolve worth its
/// keep. A hit on a message the thread has not fetched yet is not discarded at
/// selection time; it lights up by itself on the emit that ingests it, with no
/// retry plumbing in any of the 7 apps.
#[test]
fn a_selection_lights_up_when_its_message_arrives_later() {
    let m = ConversationsManager::new();
    m.install_mock_backends_for_test();
    m.ingest_inbound(smtp_inbound("alice@host.test", Some("s"), "first", None))
        .unwrap();
    let tid = m.snapshot().threads[0].thread_id.clone();

    m.select_thread_and_message(tid.clone(), MessageId("msg-late".into()));
    assert_eq!(
        m.thread_detail(tid.clone()).unwrap().selected_message_id,
        None,
        "not yet fetched — nothing to mark"
    );

    m.ingest_inbound(smtp_inbound("alice@host.test", Some("s"), "late", None))
        .unwrap();

    assert_eq!(
        m.thread_detail(tid).unwrap().selected_message_id,
        Some(MessageId("msg-late".into())),
        "the same selection now resolves, with no second call"
    );
}

/// Picking a thread by hand is not a message selection: the marker from an
/// earlier search must not survive onto a thread the user chose themselves.
#[test]
fn selecting_a_thread_by_hand_clears_an_earlier_message_marker() {
    let m = ConversationsManager::new();
    m.install_mock_backends_for_test();
    m.ingest_inbound(smtp_inbound("alice@host.test", Some("s"), "first", None))
        .unwrap();
    let tid = m.snapshot().threads[0].thread_id.clone();

    m.select_thread_and_message(tid.clone(), MessageId("msg-first".into()));
    assert!(
        m.thread_detail(tid.clone())
            .unwrap()
            .selected_message_id
            .is_some()
    );

    m.select_thread(tid.clone());

    assert_eq!(
        m.thread_detail(tid).unwrap().selected_message_id,
        None,
        "a hand-picked thread opens with nothing selected"
    );
}

// ── Outbound send (new-thread compose → SmtpBackend → sink) ─────────────

use async_trait::async_trait;
use fauna_conversations::backend::{BackendError, OutboundMailSink};
use fauna_conversations::backends::smtp::SmtpBackend;
use std::sync::Mutex;

#[derive(Default)]
struct CapturingSink {
    calls: Mutex<Vec<(Vec<String>, Vec<u8>)>>,
}

#[async_trait]
impl OutboundMailSink for CapturingSink {
    async fn submit(&self, recipients: Vec<String>, raw: Vec<u8>) -> Result<(), String> {
        self.calls.lock().unwrap().push((recipients, raw));
        Ok(())
    }
}

#[tokio::test]
async fn send_new_thread_materializes_thread_and_submits() {
    let sink = Arc::new(CapturingSink::default());
    let m = ConversationsManager::new();
    m.register_backend(Arc::new(SmtpBackend::new(sink.clone(), "alice@localhost")));

    // Drive the new-thread compose exactly as the UI does.
    m.start_new_conversation();
    m.set_new_thread_recipient_input("bob@external.test".into());
    m.resolve_recipient().await;
    assert!(m.accept_current_recipient_chip(), "chip should be accepted");
    m.set_new_thread_subject(Some("Client-driven send".into()));
    m.set_new_thread_body("Hello from the conversations page".into());

    let new_id = m
        .send_new_thread()
        .await
        .expect("send ok")
        .expect("a thread id");

    // The sink saw exactly one submission to the external recipient.
    let calls = sink.calls.lock().unwrap();
    assert_eq!(calls.len(), 1);
    assert_eq!(calls[0].0, vec!["bob@external.test".to_string()]);
    let raw = String::from_utf8(calls[0].1.clone()).unwrap();
    assert!(raw.contains("From: alice@localhost\r\n"), "{raw:?}");
    assert!(raw.contains("To: bob@external.test\r\n"));
    assert!(raw.contains("Subject: Client-driven send\r\n"));
    assert!(raw.contains("Hello from the conversations page"));

    // The thread is materialized with the sent message; new-thread compose closed.
    let snap = m.snapshot();
    assert!(
        snap.new_thread_compose.is_none(),
        "compose cleared after send"
    );
    let detail = m.thread_detail(new_id).expect("thread exists");
    assert_eq!(detail.messages.len(), 1, "the Sent copy is appended");
    assert_eq!(
        detail.messages[0].sender,
        TypedAddress::Email {
            email_address: "alice@localhost".into()
        }
    );
}

#[tokio::test]
async fn send_to_unregistered_rail_is_not_supported() {
    let m = ConversationsManager::new();
    // No backend registered for Smtp.
    m.start_new_conversation();
    m.set_new_thread_recipient_input("bob@external.test".into());
    m.resolve_recipient().await;
    assert!(m.accept_current_recipient_chip());
    m.set_new_thread_body("hi".into());
    let err = m.send_new_thread().await.expect_err("no backend → error");
    assert!(matches!(err, BackendError::NotSupported));
}

#[tokio::test]
async fn send_new_thread_flushes_uncommitted_recipient() {
    // Repro for the live "I click Send and nothing happens" bug: a user types an
    // email recipient and clicks Send WITHOUT first pressing Enter / clicking a
    // suggestion to commit a chip. Before the fix, `send_new_thread` saw
    // `picker.chips.is_empty()` and returned `Ok(None)` — a silent no-op the
    // linux `on_send` handler then swallowed (`eprintln!` only). `send_new_thread`
    // must flush the pending recipient input into a chip (the same
    // resolve→accept the Enter handler runs) and relay, so the primary Send
    // action never silently drops the message.
    let sink = Arc::new(CapturingSink::default());
    let m = ConversationsManager::new();
    m.register_backend(Arc::new(SmtpBackend::new(sink.clone(), "alice@localhost")));

    m.start_new_conversation();
    m.set_new_thread_recipient_input("bob@external.test".into());
    // NB: deliberately NO accept_current_recipient_chip() — the user never
    // committed the chip; they just clicked Send.
    m.set_new_thread_body("Hello with no committed chip".into());

    let new_id = m
        .send_new_thread()
        .await
        .expect("send ok")
        .expect("a thread id — the pending recipient must be flushed, not dropped");

    {
        let calls = sink.calls.lock().unwrap();
        assert_eq!(
            calls.len(),
            1,
            "the flushed recipient must relay exactly once"
        );
        assert_eq!(calls[0].0, vec!["bob@external.test".to_string()]);
    }

    // Compose closed; the typed recipient became the thread's participant.
    let snap = m.snapshot();
    assert!(
        snap.new_thread_compose.is_none(),
        "compose cleared after send"
    );
    let detail = m.thread_detail(new_id).expect("thread exists");
    assert_eq!(
        detail.participants,
        vec![TypedAddress::Email {
            email_address: "bob@external.test".into()
        }]
    );
}

/// A sink that always rejects — models the nest refusing `fauna.email.send`
/// (e.g. mail not provisioned, so the actor lacks the `fauna.email.send`
/// permission — `bins/fauna-nest/src/email_handlers.rs`).
struct RejectingSink;

#[async_trait]
impl OutboundMailSink for RejectingSink {
    async fn submit(&self, _recipients: Vec<String>, _raw: Vec<u8>) -> Result<(), String> {
        Err("nest rejected fauna.email.send".to_string())
    }
}

#[tokio::test]
async fn failed_send_stamps_send_state_failed_for_surfacing() {
    // A failed compose-send must leave the materialized thread's
    // `ComposeState.send_state = Failed { reason }`, so clients render the
    // reason in the page `error-message` element rather than swallowing it
    // (`docs/goal/ui/conversations.md` § Errors & edge cases:
    // `ComposeState.send_state` is `Idle | Sending | Failed { reason }`;
    // `error-message` is the page-level surface). This is the shared half the
    // observer-driven clients read — no per-app error plumbing.
    let m = ConversationsManager::new();
    m.register_backend(Arc::new(SmtpBackend::new(
        Arc::new(RejectingSink),
        "alice@localhost",
    )));

    m.start_new_conversation();
    m.set_new_thread_recipient_input("bob@external.test".into());
    m.resolve_recipient().await;
    assert!(m.accept_current_recipient_chip());
    m.set_new_thread_body("body the send will fail to relay".into());

    let err = m
        .send_new_thread()
        .await
        .expect_err("the rejecting sink must fail the send");
    assert!(
        matches!(err, BackendError::Transport(_)),
        "submit failure surfaces as Transport, got {err:?}"
    );

    // The new-thread compose closed and the thread materialized + was selected
    // before the (failing) send, so its draft carries the Failed state.
    let snap = m.snapshot();
    assert!(
        snap.new_thread_compose.is_none(),
        "new-thread compose closes once the thread materializes"
    );
    let selected = snap
        .selected_thread_id
        .expect("the materialized thread is selected even when its send fails");
    let detail = m
        .thread_detail(selected)
        .expect("materialized thread exists");
    match &detail.compose.send_state {
        SendState::Failed { reason } => {
            // The reason is a `LocalizedText`, not a raw English string: rule 3
            // of `conversations.md` § Architectural rules ("never hardcode
            // English") applies to this surface exactly as it does to the page
            // error the same element renders. One key for every send failure
            // (sends have a single producer, unlike the page error's three), the
            // backend's own detail carried as `{message}` so an unrecoverable
            // refusal still tells the user what happened.
            assert_eq!(reason.key, "conversations.unified.error_send");
            let message = reason
                .args
                .get("message")
                .expect("the backend detail rides {message} so error-message can show why");
            assert!(
                !message.is_empty(),
                "the backend detail must be non-empty so error-message has more than a template"
            );
            // The slot is stamped through `BackendError::user_detail`, never
            // `Display` (send-slot taxonomy, conversations.md § Errors & edge
            // cases): `Transport`'s Display carries a "transport failure: " tag
            // for logs, and that tag must never reach the user-rendered slot.
            assert!(
                !message.starts_with("transport failure"),
                "the slot must carry user_detail (payload alone), not the Display tag: {message:?}"
            );
        }
        other => panic!("expected send_state == Failed after a failed send, got {other:?}"),
    }
}

#[tokio::test]
async fn a_refusal_carrying_user_facing_text_rides_message_with_no_variant_tag() {
    // The sibling above pins that `{message}` is *populated*; this pins WHAT it
    // holds, which is the half a non-empty check cannot see.
    //
    // `conversations.md` § Errors & edge cases: the send failure is always the
    // key `conversations.unified.error_send` "with the backend's own rejection as
    // `{message}`" — so whatever `{message}` carries is rendered verbatim inside
    // a localized template on all 7 apps, and § Architectural rules 3 ("never
    // hardcode English") governs the whole `error-message` element. A
    // `#[error("<variant tag>: {0}")]` therefore lands its tag in front of a
    // user-facing sentence, on every app at once.
    //
    // The inline-ceiling pre-check (`SmtpBackend::send`, smtp-server.md
    // § Message size limits) is the one refusal whose payload is already a
    // localized product string, so it is the exact case that makes the tag
    // visible: it must reach `{message}` as itself and nothing else.
    let m = ConversationsManager::new();
    let sink = Arc::new(CapturingSink::default());
    m.register_backend(Arc::new(SmtpBackend::new(sink.clone(), "alice@localhost")));

    m.start_new_conversation();
    m.set_new_thread_recipient_input("bob@external.test".into());
    m.resolve_recipient().await;
    assert!(m.accept_current_recipient_chip());
    m.set_new_thread_body(
        "x".repeat(fauna_mail::transport_limits::MAX_INLINE_RAW_MESSAGE_BYTES as usize),
    );

    m.send_new_thread()
        .await
        .expect_err("an over-inline-ceiling send must be refused");
    assert!(
        sink.calls.lock().unwrap().is_empty(),
        "the refusal is local — the sink must never see an over-ceiling send"
    );

    let selected = m
        .snapshot()
        .selected_thread_id
        .expect("the materialized thread is selected even when its send fails");
    let detail = m
        .thread_detail(selected)
        .expect("materialized thread exists");
    match &detail.compose.send_state {
        SendState::Failed { reason } => {
            assert_eq!(reason.key, "conversations.unified.error_send");
            let message = reason
                .args
                .get("message")
                .expect("the backend detail rides {message}");
            assert_eq!(
                message,
                fauna_i18n::strings::error::email::TOO_LARGE,
                "the localized refusal must reach {{message}} verbatim — a Display \
                 that prefixes its own enum-variant name (e.g. \"other: \") renders \
                 that tag to the user inside the error_send template, on every app"
            );
        }
        other => panic!("expected send_state == Failed after a refused send, got {other:?}"),
    }
}

/// The sender's own "Sent" copy's render `document` matches the rail's markdown
/// capability, so the bubble renders a `**bold**` body the same way the peer's
/// received copy does — formatted, not raw source (`conversations.md` § Layout:
/// `dm-message-text`). The `body_format` discriminant is consumed *inside* the
/// producer (render-model.md § D1) — the snapshot carries no such field — so this
/// asserts the produced `document` is the markdown render, not a sibling flag.
/// Regression for the linux UI-review finding that *every* message rendered as
/// raw markdown because the Sent copy (and the received copies) were hardcoded
/// `PlainText`.
#[tokio::test]
async fn sent_copy_document_is_markdown_render_on_markdown_capable_rail() {
    let m = ConversationsManager::new();

    // SMTP IS markdown-capable now (`docs/goal/behavior/html-mail.md`: email uses
    // markdown both ways) → the Sent copy is flagged Markdown so `**text**`
    // renders formatted, matching the peer's received copy (inbound HTML is also
    // converted to markdown).
    let sink = Arc::new(CapturingSink::default());
    m.register_backend(Arc::new(SmtpBackend::new(sink, "alice@localhost")));
    m.start_new_conversation();
    m.set_new_thread_recipient_input("bob@external.test".into());
    m.resolve_recipient().await;
    assert!(m.accept_current_recipient_chip());
    m.set_new_thread_body("plain **text**".into());
    let smtp_id = m
        .send_new_thread()
        .await
        .expect("send ok")
        .expect("a thread id");
    let smtp_detail = m.thread_detail(smtp_id).expect("smtp thread");
    assert_eq!(
        smtp_detail.messages[0].document.blocks,
        fauna_core::render::markdown_to_document("plain **text**").blocks,
        "mail composes markdown (html-mail.md) → Sent copy's document is the markdown render (formatted, not raw source)"
    );

    // FaunaMls IS markdown-capable → Sent copy is flagged Markdown. (The thread
    // rail is FaunaMls regardless of the participant address shape — the key
    // sets it — so an Email-typed participant still yields a markdown thread.)
    m.register_backend(Arc::new(MockRailBackend::new(Rail::FaunaMls)));
    let grp = m.create_mls_group(vec![TypedAddress::Email {
        email_address: "bob@host.test".into(),
    }]);
    assert!(
        m.thread_detail(grp.clone())
            .unwrap()
            .capabilities
            .supports_markdown,
        "a FaunaMls group is markdown-capable"
    );
    m.set_compose_body(grp.clone(), "**bold**".into());
    m.send(grp.clone()).await.expect("send ok");
    let last = m
        .thread_detail(grp)
        .expect("group thread")
        .messages
        .pop()
        .expect("the Sent copy is appended");
    assert_eq!(
        last.document.blocks,
        fauna_core::render::markdown_to_document("**bold**").blocks,
        "markdown-capable rail renders the Sent copy's document formatted (markdown)"
    );
    assert_eq!(
        last.body, "**bold**",
        "the retained `body` source field stays the raw markdown source"
    );
}

/// The e2e injection seam (`inject_send_failure_for_test`) must produce the
/// SAME observable state a real failed send leaves — the thread's draft
/// `Failed { reason }` **and** that thread selected — so the linux
/// `error-message` render path (`views/conversations/detail.rs`, the
/// `active_detail` branch) can be exercised deterministically. There is no
/// *product* path that fails a send on demand (a mail-OFF nest enqueues +
/// returns `Ok`, so the client send succeeds into a void and never reaches
/// `Failed`), so the cross-app e2e drives this seam instead of a
/// fail-on-demand backend. Mirror of
/// `failed_send_stamps_send_state_failed_for_surfacing` (the real-failure
/// characterization), but via the seam the action layer calls.
#[test]
fn inject_send_failure_for_test_stamps_failed_and_selects() {
    let m = ConversationsManager::new();
    m.install_mock_backends_for_test();
    let id = m.create_mls_group(vec![TypedAddress::Fauna {
        handle: "bob".into(),
        actor_id: ActorId([7u8; 32]),
    }]);

    m.inject_send_failure_for_test(&id, "nest rejected fauna.email.send".into());

    let snap = m.snapshot();
    assert_eq!(
        snap.selected_thread_id.as_ref(),
        Some(&id),
        "the injected thread must be selected so the active-detail error surface reads it"
    );
    let detail = m.thread_detail(id).expect("thread exists");
    match &detail.compose.send_state {
        SendState::Failed { reason } => {
            // The seam keeps its `String` parameter — the *key* is not a caller's
            // choice for a send (one producer, one key), so it wraps the caller's
            // detail into the same `LocalizedText` the real producer emits. That
            // is what keeps the cross-app `conversations_inject_send_failure`
            // command contract (flat `{thread_id, reason?}`) unchanged while the
            // rendered text goes through each app's i18n pipeline.
            assert_eq!(reason.key, "conversations.unified.error_send");
            assert_eq!(
                reason.args.get("message").map(String::as_str),
                Some("nest rejected fauna.email.send")
            );
        }
        other => panic!("expected send_state == Failed after injection, got {other:?}"),
    }
}

// ── Recipient resolution (manager async probe → picker state) ───────────
//
// `resolve_recipient` is the async probe behind `recipient-picker-input` Enter
// (`docs/goal/ui/conversations.md` § User actions: `resolve_recipient` then
// `accept_recipient_chip`). These exercise the manager's probe + state-mapping
// in isolation via `MockRailBackend::override_resolve`; the real FaunaMls
// key-package probe is covered in `fauna_mls_backend_tests::resolve_address_*`.

use fauna_conversations::backend::ResolveResult;
use fauna_conversations::compose::ResolveState;
use fauna_core::identity::ActorId;

/// A 64-hex actor id the FaunaMls rail resolves to a `Fauna` address promotes
/// the picker to `Resolved`, stashes the resolved address, and
/// `accept_current_recipient_chip` commits *that* address (not a format parse,
/// which cannot produce `Fauna`).
#[tokio::test]
async fn resolve_recipient_promotes_actor_id_to_fauna_chip() {
    let m = ConversationsManager::new();
    let fauna = Arc::new(MockRailBackend::new(Rail::FaunaMls));
    let hex = "ab".repeat(32); // 64 hex chars
    let resolved = TypedAddress::Fauna {
        handle: hex.clone(),
        actor_id: ActorId([0xab; 32]),
    };
    fauna.override_resolve(&hex, ResolveResult::Resolved(resolved.clone()));
    m.register_backend(fauna);

    m.start_new_conversation();
    m.set_new_thread_recipient_input(hex.clone());
    m.resolve_recipient().await;

    let picker = m
        .snapshot()
        .new_thread_compose
        .unwrap()
        .recipient_picker
        .unwrap();
    assert_eq!(picker.resolve_state, ResolveState::Resolved);
    assert_eq!(picker.resolved, Some(resolved.clone()));

    assert!(m.accept_current_recipient_chip());
    let picker = m
        .snapshot()
        .new_thread_compose
        .unwrap()
        .recipient_picker
        .unwrap();
    assert_eq!(
        picker.chips,
        vec![resolved],
        "the resolved Fauna chip committed"
    );
    assert_eq!(picker.resolved, None, "resolved cleared after commit");
}

/// A 64-hex actor id no rail can resolve lands in `NotFound` (distinct from
/// `Error`), with no resolved address, and `accept_current_recipient_chip`
/// refuses to commit a chip.
#[tokio::test]
async fn resolve_recipient_unreachable_actor_is_not_found() {
    let m = ConversationsManager::new();
    let fauna = Arc::new(MockRailBackend::new(Rail::FaunaMls));
    let hex = "00".repeat(32);
    fauna.override_resolve(&hex, ResolveResult::NotFound);
    m.register_backend(fauna);

    m.start_new_conversation();
    m.set_new_thread_recipient_input(hex.clone());
    m.resolve_recipient().await;

    let picker = m
        .snapshot()
        .new_thread_compose
        .unwrap()
        .recipient_picker
        .unwrap();
    assert_eq!(picker.resolve_state, ResolveState::NotFound);
    assert_eq!(picker.resolved, None);
    assert!(
        !m.accept_current_recipient_chip(),
        "a not-found recipient commits no chip"
    );
}

/// FaunaMls is probed first, so a string both rails could claim resolves on the
/// Fauna rail — the precedence that lets a Fauna handle win over the Email
/// fallback once handle→actor lookup lands.
#[tokio::test]
async fn resolve_recipient_prefers_fauna_over_other_rails() {
    let m = ConversationsManager::new();
    let ambiguous = "alice@fauna.test";
    let fauna = Arc::new(MockRailBackend::new(Rail::FaunaMls));
    let fauna_addr = TypedAddress::Fauna {
        handle: ambiguous.into(),
        actor_id: ActorId([1u8; 32]),
    };
    fauna.override_resolve(ambiguous, ResolveResult::Resolved(fauna_addr.clone()));
    m.register_backend(fauna);
    // SMTP would also resolve `alice@fauna.test` (its default mock returns Email).
    m.register_backend(Arc::new(MockRailBackend::new(Rail::Smtp)));

    m.start_new_conversation();
    m.set_new_thread_recipient_input(ambiguous.into());
    m.resolve_recipient().await;

    let picker = m
        .snapshot()
        .new_thread_compose
        .unwrap()
        .recipient_picker
        .unwrap();
    assert_eq!(
        picker.resolved,
        Some(fauna_addr),
        "FaunaMls wins the probe order"
    );
}

/// The disambiguation complement of `prefers_fauna_over_other_rails`: when the
/// FaunaMls probe *declines* an email-shaped string (e.g. the handle's domain
/// isn't this nest — `bob@example.com`), the chain falls through and the Email
/// rail claims it. This is "only falls back to Email when no Fauna actor
/// exists" (`docs/goal/ui/conversations.md` § Where logic lives → Fauna→…→Email
/// disambiguation).
#[tokio::test]
async fn resolve_recipient_falls_through_to_email_when_fauna_declines() {
    let m = ConversationsManager::new();
    let foreign = "bob@example.com";
    let fauna = Arc::new(MockRailBackend::new(Rail::FaunaMls));
    // FaunaMls declines a foreign-domain handle (mirrors the real backend's
    // domain-match guard in `resolve_address_handle_at_foreign_domain_not_promoted`).
    fauna.override_resolve(foreign, ResolveResult::NotFound);
    m.register_backend(fauna);
    // SMTP's default mock resolves any string to Email.
    m.register_backend(Arc::new(MockRailBackend::new(Rail::Smtp)));

    m.start_new_conversation();
    m.set_new_thread_recipient_input(foreign.into());
    m.resolve_recipient().await;

    let picker = m
        .snapshot()
        .new_thread_compose
        .unwrap()
        .recipient_picker
        .unwrap();
    assert_eq!(picker.resolve_state, ResolveState::Resolved);
    assert_eq!(
        picker.resolved,
        Some(TypedAddress::Email {
            email_address: foreign.into()
        }),
        "FaunaMls declined → the Email rail claims the foreign-domain address"
    );
}

// ── The picker tells the truth (`docs/goal/ui/conversations.md` § Errors & edge
// cases; `federation.md` § Peer-auth model → *Discovery-failure semantics*,
// ratified 2026-08-29). Four collapse points used to turn "the Fauna peer did
// not answer" into a silently-accepted email chip; these pin the three that
// live in the manager (the fourth is the backend's, pinned in
// `fauna_mls_backend_tests.rs`).

/// A rail's `Error` is TERMINAL for the probe: the FaunaMls rail saying "this
/// is a Fauna address I cannot confirm right now" is the answer, and the SMTP
/// rail's syntactic `Resolved(Email)` for the same string must never override
/// it. The picker lands in `Error`, holds no resolved address, and refuses the
/// chip.
#[tokio::test]
async fn resolve_recipient_rail_error_is_terminal_over_smtp() {
    let m = ConversationsManager::new();
    let known_peer = "bob@peer.test";
    let fauna = Arc::new(MockRailBackend::new(Rail::FaunaMls));
    fauna.override_resolve(
        known_peer,
        ResolveResult::Error("connect https://peer.test: connection refused".into()),
    );
    m.register_backend(fauna);
    // SMTP's default mock resolves any string to Email — it must not be asked.
    m.register_backend(Arc::new(MockRailBackend::new(Rail::Smtp)));

    m.start_new_conversation();
    m.set_new_thread_recipient_input(known_peer.into());
    m.resolve_recipient().await;

    let picker = m
        .snapshot()
        .new_thread_compose
        .unwrap()
        .recipient_picker
        .unwrap();
    assert_eq!(picker.resolve_state, ResolveState::Error);
    assert_eq!(picker.resolved, None, "no rail vouched for an address");
    assert!(
        !m.accept_current_recipient_chip(),
        "an errored resolve commits no chip — not even by shape"
    );
    assert!(
        m.snapshot()
            .new_thread_compose
            .unwrap()
            .recipient_picker
            .unwrap()
            .chips
            .is_empty()
    );
}

/// Typing owes a probe and Enter waits for it: the sync input write lands in
/// `Resolving` (not a shape-derived `Resolved`), and `accept_current_recipient_chip`
/// commits nothing until the async probe has stamped a resolved address — then
/// commits exactly that address. This closes the race where Enter beat the
/// Fauna lookup and the shape parse committed an email chip.
#[tokio::test]
async fn recipient_input_is_resolving_until_probed_and_enter_waits_for_the_probe() {
    let m = ConversationsManager::new();
    let raw = "bob@example.com";
    let fauna = Arc::new(MockRailBackend::new(Rail::FaunaMls));
    fauna.override_resolve(raw, ResolveResult::NotFound);
    m.register_backend(fauna);
    m.register_backend(Arc::new(MockRailBackend::new(Rail::Smtp)));

    m.start_new_conversation();
    m.set_new_thread_recipient_input(raw.into());
    let picker = m
        .snapshot()
        .new_thread_compose
        .unwrap()
        .recipient_picker
        .unwrap();
    assert_eq!(picker.resolve_state, ResolveState::Resolving);
    assert_eq!(picker.resolved, None);
    assert!(
        !m.accept_current_recipient_chip(),
        "Enter before the probe commits nothing"
    );

    m.resolve_recipient().await;
    let picker = m
        .snapshot()
        .new_thread_compose
        .unwrap()
        .recipient_picker
        .unwrap();
    assert_eq!(picker.resolve_state, ResolveState::Resolved);
    assert!(
        m.accept_current_recipient_chip(),
        "Enter after the probe commits"
    );
    assert_eq!(
        m.snapshot()
            .new_thread_compose
            .unwrap()
            .recipient_picker
            .unwrap()
            .chips,
        vec![TypedAddress::Email {
            email_address: raw.into()
        }]
    );

    // Clearing the field is the one `Idle`.
    m.set_new_thread_recipient_input(String::new());
    assert_eq!(
        m.snapshot()
            .new_thread_compose
            .unwrap()
            .recipient_picker
            .unwrap()
            .resolve_state,
        ResolveState::Idle
    );
}

/// **A restore owes the picker a probe** — the dead state this closes.
///
/// § Errors & edge cases rule 1 ratifies that a non-empty recipient input is
/// never `Idle`: typing stamps `Resolving`, and only the async probe moves it
/// on. § Persistence rests `raw_input` (it is draft content) while resetting
/// `resolve_state` to `Idle` (a probe does not survive a relaunch). Put
/// together, a *restored* draft used to land in a state nothing could ever
/// leave: the field holds an address, the status sits at `idle`, and no
/// keystroke is coming — the app shells suppress the echo of an unchanged field,
/// correctly, so the user's own re-typing of the same address does not re-arm it
/// either. The restore therefore issues the probe itself, with no user gesture,
/// and the restored recipient becomes committable (`federation.md` § Peer-auth
/// model → *Discovery-failure semantics*: a chip is only ever a probed address).
#[tokio::test]
async fn a_restored_recipient_is_probed_with_no_user_gesture() {
    let raw = "bob@example.com";

    // Device A: the user opens `+`, types a recipient, and the draft rests.
    let a = ConversationsManager::new();
    a.register_backend(Arc::new(MockRailBackend::new(Rail::Smtp)));
    a.start_new_conversation();
    a.set_new_thread_recipient_input(raw.into());
    let blob = a.drafts_snapshot_bytes();

    // Device B restores it. Nothing is typed here, ever.
    let b = ConversationsManager::new();
    let fauna = Arc::new(MockRailBackend::new(Rail::FaunaMls));
    fauna.override_resolve(raw, ResolveResult::NotFound);
    b.register_backend(fauna);
    b.register_backend(Arc::new(MockRailBackend::new(Rail::Smtp)));
    b.restore_drafts(blob).await;
    b.start_new_conversation(); // the user opens the composer the restore filled

    let picker = b
        .snapshot()
        .new_thread_compose
        .expect("the restored composer")
        .recipient_picker
        .expect("carrying the restored picker");
    assert_eq!(
        picker.raw_input, raw,
        "the typed address rests and comes back"
    );
    assert_eq!(
        picker.resolve_state,
        ResolveState::Resolved,
        "a restored non-empty input reaches a terminal state with no user \
         gesture (it used to sit at Idle with nothing left to probe it)"
    );
    assert!(
        b.accept_current_recipient_chip(),
        "and Enter commits it, because the address was probed"
    );
}

/// The other half of the same rule: a restore that fills *nothing* probes
/// nothing. An empty input is the one legitimate `Idle` (rule 1), so the restore
/// must not stamp a state onto a picker the user has not authored into.
#[tokio::test]
async fn a_restore_with_no_typed_recipient_probes_nothing() {
    // Device A rests a new-thread draft with a body but an untouched picker.
    let a = ConversationsManager::new();
    a.start_new_conversation();
    a.set_new_thread_body("half a thought".into());
    let blob = a.drafts_snapshot_bytes();

    let b = ConversationsManager::new();
    let fauna = Arc::new(MockRailBackend::new(Rail::FaunaMls));
    // Any probe at all would stamp `Error` and be loud in the assertion below.
    fauna.override_resolve("", ResolveResult::Error("must not be probed".into()));
    b.register_backend(fauna);
    b.restore_drafts(blob).await;
    b.start_new_conversation();

    let compose = b
        .snapshot()
        .new_thread_compose
        .expect("the restored composer");
    assert_eq!(compose.body_draft, "half a thought");
    assert_eq!(
        compose.recipient_picker.expect("the picker").resolve_state,
        ResolveState::Idle,
        "an empty input is the one legitimate Idle — no probe is owed"
    );
}

/// The restore's probe targets the **new-thread** picker specifically, not
/// "whichever picker is active". The blob only ever carries the new-thread slot
/// (§ Persistence), so an add-participant overlay the user happens to have open
/// when the late `__drafts` fetch lands is left exactly as it is — a restore is
/// not a gesture on the overlay.
#[tokio::test]
async fn a_restore_does_not_probe_an_open_add_participant_overlay() {
    let a = ConversationsManager::new();
    a.register_backend(Arc::new(MockRailBackend::new(Rail::Smtp)));
    a.start_new_conversation();
    a.set_new_thread_recipient_input("bob@example.com".into());
    let blob = a.drafts_snapshot_bytes();

    let b = ConversationsManager::new();
    b.register_backend(Arc::new(MockRailBackend::new(Rail::Smtp)));
    let t = b.create_mls_group(vec![TypedAddress::Fauna {
        actor_id: ActorId([7u8; 32]),
        handle: "carol@nest".into(),
    }]);
    b.open_add_participant(t.clone());
    b.set_add_participant_recipient_input("dave@example.com".into());

    b.restore_drafts(blob).await;

    let overlay = b
        .snapshot()
        .add_participant
        .expect("the overlay is still open");
    assert_eq!(overlay.picker.raw_input, "dave@example.com");
    assert_eq!(
        overlay.picker.resolve_state,
        ResolveState::Resolving,
        "the overlay keeps the state its own typing left it in — the restore \
         resolved the new-thread picker, not this one"
    );
}

/// **A launch restore the OUTGOING account started writes nothing into the
/// manager the INCOMING account uses** (`account-scoping.md` § The scoping
/// taxonomy: the loops that write account-scoped state are retired by the same
/// drop, keyed on the identity).
///
/// The shells that keep one manager across a switch (apple, android, linux)
/// start account A's `__drafts` fetch, wipe the manager at the switch, and let
/// the fetch run on. Because a restore FILLS (`conversations.md` § Persistence),
/// A's reply landing late would shadow B's own resting draft, be re-sealed into
/// B's plane by B's next autosave, and — since the restore probes a restored
/// recipient — hand A's private recipient to B's rails. The epoch the shell
/// captured before the fetch is what refuses it, and it must refuse **before
/// the fill**, so no rail is ever asked.
#[tokio::test]
async fn a_restore_the_outgoing_account_started_writes_nothing_after_the_identity_change() {
    let a_recipient = "a-private-contact@example.com";

    // Account A's resting draft: a typed recipient and a body.
    let a = ConversationsManager::new();
    a.register_backend(Arc::new(MockRailBackend::new(Rail::Smtp)));
    a.start_new_conversation();
    a.set_new_thread_recipient_input(a_recipient.into());
    a.set_new_thread_body("A's private draft".into());
    let a_blob = a.drafts_snapshot_bytes();

    // Account B's resting draft: a body only (so B's own restore owes no probe).
    let b = ConversationsManager::new();
    b.start_new_conversation();
    b.set_new_thread_body("B's own draft".into());
    let b_blob = b.drafts_snapshot_bytes();

    // Both orders of the late reply against B's own restore.
    for a_lands_first in [true, false] {
        let m = ConversationsManager::new();
        let rail = Arc::new(MockRailBackend::new(Rail::FaunaMls));
        rail.override_resolve(a_recipient, ResolveResult::Error("B's rail".into()));
        m.register_backend(rail.clone());

        // A's shell captures the epoch and starts its fetch …
        let a_epoch = m.identity_epoch();
        // … the switch wipes the manager and B's session activates …
        m.clear_for_identity_change();
        assert_ne!(
            m.identity_epoch(),
            a_epoch,
            "the identity change moves the epoch"
        );
        let b_epoch = m.identity_epoch();

        // … and A's reply lands, before or after B's own.
        if a_lands_first {
            m.restore_drafts_at(a_epoch, a_blob.clone()).await;
            m.start_new_conversation();
            let compose = m.snapshot().new_thread_compose.expect("the composer");
            assert_eq!(
                compose.body_draft, "",
                "A's stale restore filled nothing into the wiped manager"
            );
            m.cancel_new_conversation();
            m.restore_drafts_at(b_epoch, b_blob.clone()).await;
        } else {
            m.restore_drafts_at(b_epoch, b_blob.clone()).await;
            m.restore_drafts_at(a_epoch, a_blob.clone()).await;
        }

        m.start_new_conversation();
        let compose = m.snapshot().new_thread_compose.expect("the composer");
        assert_eq!(
            compose.body_draft, "B's own draft",
            "B's own restore still fills, and A's never shadows it \
             (a_lands_first = {a_lands_first})"
        );
        let raw = compose
            .recipient_picker
            .map(|p| p.raw_input)
            .unwrap_or_default();
        assert!(
            !raw.contains(a_recipient),
            "A's recipient never reaches B's composer (a_lands_first = {a_lands_first})"
        );
        assert!(
            !String::from_utf8_lossy(&m.drafts_snapshot_bytes()).contains("A's private draft"),
            "nothing of A's rides B's next autosave (a_lands_first = {a_lands_first})"
        );
        assert_eq!(
            rail.resolve_calls(),
            Vec::<String>::new(),
            "a stale-epoch restore is refused before the fill, so no rail is \
             asked to resolve A's recipient (a_lands_first = {a_lands_first})"
        );
    }
}

/// The epoch is a guard on the stale writer, not a new precondition on the
/// ordinary one: a restore at the current epoch fills and probes exactly as
/// `restore_drafts` does, and `restore_drafts` itself is the current-epoch
/// restore.
#[tokio::test]
async fn a_current_epoch_restore_fills_and_probes_as_before() {
    let raw = "bob@example.com";
    let a = ConversationsManager::new();
    a.register_backend(Arc::new(MockRailBackend::new(Rail::Smtp)));
    a.start_new_conversation();
    a.set_new_thread_recipient_input(raw.into());
    let blob = a.drafts_snapshot_bytes();

    let m = ConversationsManager::new();
    let rail = Arc::new(MockRailBackend::new(Rail::FaunaMls));
    m.register_backend(rail.clone());
    m.clear_for_identity_change(); // an earlier switch moved the epoch
    m.restore_drafts_at(m.identity_epoch(), blob).await;
    m.start_new_conversation();

    let picker = m
        .snapshot()
        .new_thread_compose
        .expect("the composer")
        .recipient_picker
        .expect("the restored picker");
    assert_eq!(picker.raw_input, raw);
    assert_eq!(rail.resolve_calls(), vec![raw.to_string()]);
}

/// The probe hands every rail the participants of every thread first, so a
/// rail can recognise a domain this account already converses with (the
/// FaunaMls backend's known-domain evidence). Pinned at the manager seam with
/// the mock rail's recorder; the FaunaMls consumer is pinned in its own tests.
#[tokio::test]
async fn probe_address_observes_thread_participants_before_resolving() {
    let m = ConversationsManager::new();
    let fauna = Arc::new(MockRailBackend::new(Rail::FaunaMls));
    m.register_backend(fauna.clone());
    m.register_backend(Arc::new(MockRailBackend::new(Rail::Smtp)));
    let carol = TypedAddress::Fauna {
        handle: "carol@peer.test".into(),
        actor_id: ActorId([9u8; 32]),
    };
    m.create_mls_group(vec![carol.clone()]);

    m.start_new_conversation();
    m.set_new_thread_recipient_input("bob@peer.test".into());
    m.resolve_recipient().await;

    let observed = fauna.observed_participants();
    assert!(
        observed.contains(&carol),
        "the FaunaMls rail was shown carol before the probe, got {observed:?}"
    );
}

/// The async resolve targets the add-participant overlay's picker when it is
/// open, mirroring `accept_current_recipient_chip`'s precedence.
#[tokio::test]
async fn resolve_recipient_targets_add_participant_overlay() {
    let m = ConversationsManager::new();
    m.install_mock_backends_for_test();
    let fauna = Arc::new(MockRailBackend::new(Rail::FaunaMls));
    let hex = "cd".repeat(32);
    let resolved = TypedAddress::Fauna {
        handle: hex.clone(),
        actor_id: ActorId([0xcd; 32]),
    };
    fauna.override_resolve(&hex, ResolveResult::Resolved(resolved.clone()));
    m.register_backend(fauna);

    let tid = m.create_mls_group(vec![TypedAddress::Email {
        email_address: "bob@host.test".into(),
    }]);
    m.open_add_participant(tid);
    m.set_add_participant_recipient_input(hex.clone());
    m.resolve_recipient().await;

    let picker = m.snapshot().add_participant.unwrap().picker;
    assert_eq!(picker.resolve_state, ResolveState::Resolved);
    assert_eq!(picker.resolved, Some(resolved));
}

// ── Sort & search (conversations.md § User actions: `conversation-sort` →
// `manager.set_sort(order)`, `conversation-search-box` →
// `manager.set_search_query(text)`). `set_sort` reorders the thread list;
// `set_search_query` is plumbed through the snapshot but global thread-list
// filtering is a deferred follow-on (tracked internally:
// "global search across threads is not in this slice"). ──

fn smtp_inbound_at(sender: &str, subject: &str, body: &str, ts_ms: i64) -> RailInboundMessage {
    RailInboundMessage {
        rail: Rail::Smtp,
        sender: TypedAddress::Email {
            email_address: sender.to_string(),
        },
        recipients: vec![TypedAddress::Email {
            email_address: "me@host.test".into(),
        }],
        subject: Some(subject.to_string()),
        body: body.to_string(),
        body_format: BodyFormat::PlainText,
        timestamp_ms: ts_ms,
        message_id: MessageId(format!("msg-{}", body)),
        in_reply_to: None,
        attachments: vec![],
        badges: Default::default(),
        legal_takedown_ref: None,
        plane_ref: None,
    }
}

#[test]
fn set_sort_oldest_first_reverses_thread_order() {
    let m = ConversationsManager::new();
    m.register_backend(Arc::new(MockRailBackend::new(Rail::Smtp)));
    // Two distinct subject-keyed threads; the "newer" one has later activity.
    m.ingest_inbound(smtp_inbound_at("a@host.test", "older", "o", 100))
        .unwrap();
    m.ingest_inbound(smtp_inbound_at("a@host.test", "newer", "n", 200))
        .unwrap();

    // Default sort is latest-activity-first.
    let snap = m.snapshot();
    assert_eq!(snap.sort, SortOrder::LatestActivity);
    assert_eq!(snap.threads[0].label, "newer");
    assert_eq!(snap.threads[1].label, "older");

    // Oldest-first reverses the order and is reflected in the snapshot field.
    m.set_sort(SortOrder::OldestFirst);
    let snap = m.snapshot();
    assert_eq!(snap.sort, SortOrder::OldestFirst);
    assert_eq!(snap.threads[0].label, "older");
    assert_eq!(snap.threads[1].label, "newer");

    // Switching back restores latest-activity-first.
    m.set_sort(SortOrder::LatestActivity);
    assert_eq!(m.snapshot().threads[0].label, "newer");
}

#[test]
fn set_search_query_plumbs_snapshot_field() {
    let m = ConversationsManager::new();
    assert_eq!(m.snapshot().search_query, None);

    // The setter stores the query (round-trips through the snapshot) so the
    // search box binds to a real field. The actual thread filtering it drives is
    // covered by `set_search_query_filters_thread_list_by_label_and_snippet`.
    m.set_search_query(Some("budget".to_string()));
    assert_eq!(m.snapshot().search_query, Some("budget".to_string()));

    m.set_search_query(None);
    assert_eq!(m.snapshot().search_query, None);
}

// ── Reply recipients — the editable "To" line (conversations.md §
//    Participants vs reply recipients) ──────────────────────────────────

/// Inbound mail with an explicit recipient list, so reply-all has > 1 peer.
fn smtp_inbound_multi(sender: &str, recipients: &[&str], body: &str) -> RailInboundMessage {
    RailInboundMessage {
        rail: Rail::Smtp,
        sender: TypedAddress::Email {
            email_address: sender.to_string(),
        },
        recipients: recipients
            .iter()
            .map(|r| TypedAddress::Email {
                email_address: r.to_string(),
            })
            .collect(),
        subject: Some("Lunch".into()),
        body: body.to_string(),
        body_format: BodyFormat::PlainText,
        timestamp_ms: 0,
        message_id: MessageId(format!("msg-{body}")),
        in_reply_to: None,
        attachments: vec![],
        badges: Default::default(),
        legal_takedown_ref: None,
        plane_ref: None,
    }
}

/// Build a manager with a real SMTP backend (self = `me@host.test`) and one
/// ingested mail from `alice` addressed to `me` + `carol`, returning the
/// thread + message ids. Uses the real `SmtpBackend` because reply-all needs
/// its `self_address()` override to drop self.
fn manager_with_mail_thread() -> (Arc<ConversationsManager>, ThreadId, MessageId) {
    let sink = Arc::new(CapturingSink::default());
    let m = ConversationsManager::new();
    m.register_backend(Arc::new(SmtpBackend::new(sink, "me@host.test")));
    m.ingest_inbound(smtp_inbound_multi(
        "alice@host.test",
        &["me@host.test", "carol@host.test"],
        "hi all",
    ))
    .unwrap();
    let tid = m.snapshot().threads[0].thread_id.clone();
    let mid = m.thread_detail(tid.clone()).unwrap().messages[0]
        .message_id
        .clone();
    (m, tid, mid)
}

fn reply_recipient_displays(m: &ConversationsManager, tid: &ThreadId) -> Vec<String> {
    let mut v: Vec<String> = m
        .thread_detail(tid.clone())
        .unwrap()
        .compose
        .reply_recipients
        .iter()
        .map(|a| a.display())
        .collect();
    v.sort();
    v
}

#[test]
fn start_reply_seeds_sender_only() {
    let (m, tid, mid) = manager_with_mail_thread();
    m.start_reply(tid.clone(), mid.clone(), false);
    let detail = m.thread_detail(tid.clone()).unwrap();
    assert_eq!(detail.compose.reply_to, Some(mid));
    // Reply (not reply-all) addresses just the message's sender.
    assert_eq!(reply_recipient_displays(&m, &tid), vec!["alice@host.test"]);
}

#[test]
fn start_reply_all_seeds_participants_minus_self() {
    let (m, tid, mid) = manager_with_mail_thread();
    m.start_reply(tid.clone(), mid, true);
    // Reply-all = alice + carol, but NOT me@host.test (self dropped via
    // SmtpBackend::self_address()).
    assert_eq!(
        reply_recipient_displays(&m, &tid),
        vec!["alice@host.test", "carol@host.test"]
    );
}

#[test]
fn remove_reply_recipient_drops_only_that_recipient() {
    let (m, tid, mid) = manager_with_mail_thread();
    m.start_reply(tid.clone(), mid, true);
    m.remove_reply_recipient(
        tid.clone(),
        TypedAddress::Email {
            email_address: "carol@host.test".into(),
        },
    );
    // Carol gone; alice stays; thread participants untouched.
    assert_eq!(reply_recipient_displays(&m, &tid), vec!["alice@host.test"]);
    assert_eq!(
        m.thread_detail(tid).unwrap().participants.len(),
        3,
        "removing a reply recipient must not mutate thread membership"
    );
}

#[test]
fn add_reply_recipient_is_deduped_case_insensitively() {
    let (m, tid, mid) = manager_with_mail_thread();
    m.start_reply(tid.clone(), mid, false); // seeds [alice]
    // Re-adding alice (different case) is a no-op; dave is appended.
    m.add_reply_recipient(
        tid.clone(),
        TypedAddress::Email {
            email_address: "ALICE@host.test".into(),
        },
    );
    m.add_reply_recipient(
        tid.clone(),
        TypedAddress::Email {
            email_address: "dave@host.test".into(),
        },
    );
    assert_eq!(
        reply_recipient_displays(&m, &tid),
        vec!["alice@host.test", "dave@host.test"]
    );
}

/// The compose bar's reply preview is one shared derivation: the answered
/// message's sender and its plain-text excerpt while a reply is armed, and
/// nothing once it is cancelled.
#[test]
fn the_reply_preview_names_the_answered_message_until_cancelled() {
    let (m, tid, mid) = manager_with_mail_thread();
    assert_eq!(
        m.reply_preview(tid.clone()),
        None,
        "no reply armed, nothing to preview"
    );
    m.start_reply(tid.clone(), mid, false);
    let preview = m.reply_preview(tid.clone()).expect("a reply is armed");
    assert_eq!(preview.excerpt, "hi all");
    assert!(
        preview.sender_display.contains("alice"),
        "the preview names who wrote the answered message: {preview:?}"
    );
    m.set_reply_to(tid.clone(), None);
    assert_eq!(
        m.reply_preview(tid),
        None,
        "cancelling the reply clears its preview"
    );
}

#[test]
fn cancel_reply_clears_the_to_line() {
    let (m, tid, mid) = manager_with_mail_thread();
    m.start_reply(tid.clone(), mid, true);
    assert!(!reply_recipient_displays(&m, &tid).is_empty());
    // dm-reply-cancel → set_reply_to(None) clears both reply_to and the To line.
    m.set_reply_to(tid.clone(), None);
    let detail = m.thread_detail(tid.clone()).unwrap();
    assert_eq!(detail.compose.reply_to, None);
    assert!(detail.compose.reply_recipients.is_empty());
}

/// The e2e seam's mail rail knows who "you" are, and an injected mail can name
/// more than one recipient. Every app's e2e mode runs
/// `install_mock_backends_for_test`, whose injected mail is addressed to
/// [`fauna_conversations::manager::TEST_SEAM_SELF_ADDRESS`] by default — but
/// until 2026-09-21 the mock mail rail had no `self_address`, so reply-all on
/// an injected mail seeded the user as their own recipient, and a payload
/// could not name a second recipient at all. The app-driving reply-all witness
/// therefore could not assert its sentence, "every participant but yourself"
/// (`conversations.md` § Participants vs. reply recipients).
#[test]
fn e2e_seam_reply_all_on_an_injected_mail_seeds_everyone_but_the_injected_self() {
    use fauna_conversations::manager::TEST_SEAM_SELF_ADDRESS;

    let m = ConversationsManager::new();
    m.install_mock_backends_for_test();
    let payload = serde_json::json!({
        "rail": "Smtp",
        "sender": "alice@host.test",
        "recipients": [TEST_SEAM_SELF_ADDRESS, "carol@host.test"],
        "subject": "Lunch",
        "body": "hi all",
    });
    m.inject_inbound_from_test_payload(payload.as_object().unwrap())
        .unwrap();
    let tid = m.snapshot().threads[0].thread_id.clone();
    let detail = m.thread_detail(tid.clone()).unwrap();
    let mut participants: Vec<String> = detail.participants.iter().map(|p| p.display()).collect();
    participants.sort();
    assert_eq!(
        participants,
        vec!["alice@host.test", "carol@host.test", TEST_SEAM_SELF_ADDRESS],
        "the payload's recipients list is the message's whole To/Cc set"
    );

    m.start_reply(tid.clone(), detail.messages[0].message_id.clone(), true);
    assert_eq!(
        reply_recipient_displays(&m, &tid),
        vec!["alice@host.test", "carol@host.test"],
        "reply-all on the seam's mail must drop the seam's own self address"
    );
}

/// The JSON-string face every app's inject handler can hand the payload to is
/// the same parser, not a second one: a payload's `recipients` land exactly as
/// through `inject_inbound_from_test_payload`, and a payload that is not a JSON
/// object is refused rather than injected as an empty message.
#[test]
fn the_json_inject_face_is_the_shared_payload_parser() {
    use fauna_conversations::manager::TEST_SEAM_SELF_ADDRESS;

    let m = ConversationsManager::new();
    m.install_mock_backends_for_test();
    let payload = serde_json::json!({
        "rail": "Smtp",
        "sender": "alice@host.test",
        "recipients": [TEST_SEAM_SELF_ADDRESS, "carol@host.test"],
        "subject": "Lunch",
        "body": "hi all",
    });
    m.inject_inbound_from_test_json(payload.to_string())
        .unwrap();
    let tid = m.snapshot().threads[0].thread_id.clone();
    assert_eq!(
        m.thread_detail(tid).unwrap().participants.len(),
        3,
        "the JSON face honours `recipients` exactly as the in-process parser does"
    );
    assert!(m.inject_inbound_from_test_json("[]".into()).is_err());
    assert!(m.inject_inbound_from_test_json("not json".into()).is_err());
}

/// Where the app's mail rail is the REAL one (a signed-in session registers
/// `SmtpBackend` over the e2e mocks), "you" is the account's own address, not
/// the seam's placeholder — so the seam addresses an injected mail's
/// placeholder recipient to the rail's real self, and reply-all still drops
/// you. Found by the tui run of the reply-all witness (2026-09-21): the
/// placeholder reached the real rail as a stranger and reply-all seeded it.
#[test]
fn e2e_seam_addresses_its_self_placeholder_to_the_mail_rails_real_self() {
    use fauna_conversations::manager::TEST_SEAM_SELF_ADDRESS;

    let m = ConversationsManager::new();
    m.install_mock_backends_for_test();
    let real_mail_rail = MockRailBackend::new(Rail::Smtp);
    real_mail_rail.set_self_address(TypedAddress::Email {
        email_address: "me@host.test".into(),
    });
    m.register_backend(Arc::new(real_mail_rail));
    let payload = serde_json::json!({
        "rail": "Smtp",
        "sender": "alice@host.test",
        "recipients": [TEST_SEAM_SELF_ADDRESS, "carol@host.test"],
        "subject": "Lunch",
        "body": "hi all",
    });
    m.inject_inbound_from_test_payload(payload.as_object().unwrap())
        .unwrap();
    let tid = m.snapshot().threads[0].thread_id.clone();
    let detail = m.thread_detail(tid.clone()).unwrap();
    let mut participants: Vec<String> = detail.participants.iter().map(|p| p.display()).collect();
    participants.sort();
    assert_eq!(
        participants,
        vec!["alice@host.test", "carol@host.test", "me@host.test"],
        "the placeholder is the rail's real self"
    );

    m.start_reply(tid.clone(), detail.messages[0].message_id.clone(), true);
    assert_eq!(
        reply_recipient_displays(&m, &tid),
        vec!["alice@host.test", "carol@host.test"],
        "reply-all drops the real self the mail was addressed to"
    );
}

// ── Attachments: staging on the per-thread compose draft ────────────────

#[test]
fn add_and_remove_attachment_stage_on_compose_draft() {
    let m = ConversationsManager::new();
    m.register_backend(Arc::new(MockRailBackend::new(Rail::Smtp)));
    m.ingest_inbound(smtp_inbound("alice@host.test", Some("Files"), "body", None))
        .unwrap();
    let tid = m.snapshot().threads[0].thread_id.clone();

    let h_img = m.add_attachment(
        tid.clone(),
        "a.png".into(),
        "image/png".into(),
        b"img-bytes".to_vec(),
    );
    let h_doc = m.add_attachment(
        tid.clone(),
        "b.txt".into(),
        "text/plain".into(),
        b"doc".to_vec(),
    );

    // Light metadata staged on the snapshot's compose draft.
    let compose = m.thread_detail(tid.clone()).unwrap().compose;
    assert_eq!(compose.attachments.len(), 2);
    assert_eq!(compose.attachments[0].blob_hash, h_img);
    assert_eq!(compose.attachments[0].filename, "a.png");
    assert_eq!(compose.attachments[0].size_bytes, 9);
    assert!(compose.attachments[0].is_image);
    assert!(
        !compose.attachments[1].is_image,
        "text/plain is not an image"
    );

    // Bytes live in the store, not the snapshot — loadable via the handle.
    assert_eq!(
        m.attachment_bytes(h_img).as_deref(),
        Some(&b"img-bytes"[..])
    );
    assert_eq!(m.attachment_bytes(h_doc).as_deref(), Some(&b"doc"[..]));

    // Remove the first; the second shifts into index 0.
    m.remove_attachment(tid.clone(), 0);
    let compose = m.thread_detail(tid.clone()).unwrap().compose;
    assert_eq!(compose.attachments.len(), 1);
    assert_eq!(compose.attachments[0].filename, "b.txt");

    // Out-of-range remove is a no-op.
    m.remove_attachment(tid.clone(), 9);
    assert_eq!(m.thread_detail(tid).unwrap().compose.attachments.len(), 1);
}

#[test]
fn identical_attachment_bytes_share_one_content_hash() {
    let m = ConversationsManager::new();
    m.register_backend(Arc::new(MockRailBackend::new(Rail::Smtp)));
    m.ingest_inbound(smtp_inbound("alice@host.test", Some("Dup"), "body", None))
        .unwrap();
    let tid = m.snapshot().threads[0].thread_id.clone();
    let h1 = m.add_attachment(
        tid.clone(),
        "x.bin".into(),
        "application/octet-stream".into(),
        b"same".to_vec(),
    );
    let h2 = m.add_attachment(
        tid.clone(),
        "y.bin".into(),
        "application/octet-stream".into(),
        b"same".to_vec(),
    );
    assert_eq!(
        h1, h2,
        "content-addressed: identical bytes → identical hash"
    );
}

// ── Attachments: privacy metadata is stripped at the staging seam ───────
//
// `add_attachment` is the single ingress every app's file picker calls
// (`docs/goal/ui/conversations.md:621`). Stripping EXIF/GPS here — rather than
// in each app's picker glue — is what makes the privacy guarantee
// structural: windows + android hand-rolled a client-side stripper and got it
// right, linux + web landed 2026-07-20 without one and shipped GPS
// coordinates. A seam half the apps forget is the wrong seam. `fauna_media::
// strip_metadata` is lossless (segment/chunk removal, no decode/re-encode) and
// passes non-image bytes through untouched, so this is safe for arbitrary
// attachments and idempotent for the two apps that already strip.

/// Minimal structurally-valid PNG carrying a `tEXt` chunk holding `canary`.
/// Hand-rolled rather than pulled from a codec dep: `strip_metadata` parses
/// chunk structure, never pixel data, so the IDAT payload can be arbitrary.
fn png_with_text_chunk(canary: &[u8]) -> Vec<u8> {
    fn crc32(bytes: &[u8]) -> u32 {
        let mut table = [0u32; 256];
        for (i, e) in table.iter_mut().enumerate() {
            let mut c = i as u32;
            for _ in 0..8 {
                c = if c & 1 != 0 {
                    0xEDB8_8320 ^ (c >> 1)
                } else {
                    c >> 1
                };
            }
            *e = c;
        }
        let mut crc = 0xFFFF_FFFFu32;
        for &b in bytes {
            crc = table[((crc ^ b as u32) & 0xFF) as usize] ^ (crc >> 8);
        }
        crc ^ 0xFFFF_FFFF
    }
    fn chunk(out: &mut Vec<u8>, kind: &[u8; 4], data: &[u8]) {
        out.extend_from_slice(&(data.len() as u32).to_be_bytes());
        let mut typed = kind.to_vec();
        typed.extend_from_slice(data);
        out.extend_from_slice(&typed);
        out.extend_from_slice(&crc32(&typed).to_be_bytes());
    }
    let mut png = vec![0x89, b'P', b'N', b'G', 0x0D, 0x0A, 0x1A, 0x0A];
    let mut ihdr = Vec::new();
    ihdr.extend_from_slice(&1u32.to_be_bytes()); // width
    ihdr.extend_from_slice(&1u32.to_be_bytes()); // height
    ihdr.extend_from_slice(&[8, 0, 0, 0, 0]); // bit depth, colour, comp, filter, interlace
    chunk(&mut png, b"IHDR", &ihdr);
    let mut text = b"Comment\0".to_vec();
    text.extend_from_slice(canary);
    chunk(&mut png, b"tEXt", &text);
    chunk(
        &mut png,
        b"IDAT",
        &[0x78, 0x9C, 0x63, 0x00, 0x00, 0x00, 0x02, 0x00, 0x01],
    );
    chunk(&mut png, b"IEND", &[]);
    png
}

#[test]
fn staging_an_attachment_strips_privacy_metadata_before_hashing() {
    const CANARY: &[u8] = b"GPS-51.5074N-0.1278W";
    let m = ConversationsManager::new();
    m.register_backend(Arc::new(MockRailBackend::new(Rail::Smtp)));
    m.ingest_inbound(smtp_inbound("alice@host.test", Some("Photo"), "body", None))
        .unwrap();
    let tid = m.snapshot().threads[0].thread_id.clone();

    let raw = png_with_text_chunk(CANARY);
    assert!(
        raw.windows(CANARY.len()).any(|w| w == CANARY),
        "fixture must carry the canary before staging"
    );

    let hash = m.add_attachment(tid.clone(), "holiday.png".into(), "image/png".into(), raw);

    // The staged bytes — what `send` re-resolves and puts on the wire — must
    // no longer carry the location metadata.
    let staged = m.attachment_bytes(hash.clone()).expect("bytes staged");
    assert!(
        !staged.windows(CANARY.len()).any(|w| w == CANARY),
        "add_attachment must strip EXIF/tEXt metadata before staging"
    );

    // The returned handle must address the *stripped* bytes: the blob_hash is
    // both the render handle and the wire reference, so hashing pre-strip
    // would make every receiver's content-address disagree with the payload.
    assert_eq!(
        hash,
        blake3::hash(&staged).to_hex().to_string(),
        "blob_hash must address the stripped bytes actually sent"
    );

    // And the light draft's size must describe the stripped payload.
    let compose = m.thread_detail(tid).unwrap().compose;
    assert_eq!(compose.attachments[0].size_bytes, staged.len() as u64);
}

#[test]
fn staging_a_non_image_attachment_passes_bytes_through_unchanged() {
    // The stripper must not corrupt arbitrary files — a conversations
    // attachment is any file, not just a photo.
    let m = ConversationsManager::new();
    m.register_backend(Arc::new(MockRailBackend::new(Rail::Smtp)));
    m.ingest_inbound(smtp_inbound("alice@host.test", Some("Doc"), "body", None))
        .unwrap();
    let tid = m.snapshot().threads[0].thread_id.clone();

    let raw = b"\x00\x01binary\xffpayload".to_vec();
    let hash = m.add_attachment(
        tid,
        "notes.bin".into(),
        "application/octet-stream".into(),
        raw.clone(),
    );
    assert_eq!(
        m.attachment_bytes(hash).as_deref(),
        Some(&raw[..]),
        "non-image bytes must pass through byte-identical"
    );
}

/// The list filter keeps finding text deep inside a long latest message even though
/// the thread-list snippet is a bounded preview (conversations.md § Where logic lives
/// → *Thread-list sort + search filtering*): the bound caps what a row carries and
/// paints, never what the filter can match.
#[test]
fn search_query_matches_text_past_the_bounded_snippet_preview() {
    let m = ConversationsManager::new();
    m.register_backend(Arc::new(MockRailBackend::new(Rail::Smtp)));
    // ~54 KB of filler, far past any preview bound, then the needle.
    let filler = "lorem ipsum dolor sit amet ".repeat(2_000);
    let body = format!("{filler}the kumquat clause is on page nine");
    m.ingest_inbound(RailInboundMessage {
        message_id: MessageId("msg-long-latest".into()),
        ..smtp_inbound("alice@host.test", Some("Contract"), &body, None)
    })
    .unwrap();
    m.ingest_inbound(smtp_inbound(
        "bob@host.test",
        Some("Lunch plans"),
        "tacos at noon",
        None,
    ))
    .unwrap();

    m.set_search_query(Some("KUMQUAT".into()));
    assert_eq!(sorted_labels(&m.snapshot()), vec!["Contract"]);
}

#[test]
fn set_search_query_filters_thread_list_by_label_and_snippet() {
    // conversations.md § User actions: `conversation-search-box` → "Filter list."
    // via `manager.set_search_query(text)`. The manager owns the filter so every
    // app is a dumb renderer of the already-filtered `snapshot().threads`
    // (priority #3) — case-insensitive substring over label + snippet (the
    // richest existing client pattern, windows' `ConversationsPage` `.Where()`).
    let m = ConversationsManager::new();
    m.register_backend(Arc::new(MockRailBackend::new(Rail::Smtp)));
    m.ingest_inbound(smtp_inbound(
        "alice@host.test",
        Some("Q4 budget"),
        "spreadsheet attached",
        None,
    ))
    .unwrap();
    m.ingest_inbound(smtp_inbound(
        "bob@host.test",
        Some("Lunch plans"),
        "tacos at noon",
        None,
    ))
    .unwrap();
    m.ingest_inbound(smtp_inbound(
        "carol@host.test",
        Some("Vacation"),
        "see the budget doc",
        None,
    ))
    .unwrap();

    assert_eq!(m.snapshot().threads.len(), 3, "no query → all threads");

    // Matches a LABEL, case-insensitively: only "Q4 budget".
    m.set_search_query(Some("q4".into()));
    assert_eq!(sorted_labels(&m.snapshot()), vec!["Q4 budget"]);

    // Matches across LABEL and SNIPPET: "budget" hits the "Q4 budget" label and
    // the "see the budget doc" snippet (the "Vacation" thread).
    m.set_search_query(Some("BUDGET".into()));
    assert_eq!(sorted_labels(&m.snapshot()), vec!["Q4 budget", "Vacation"]);

    // Matches a SNIPPET only: "tacos" is in the "Lunch plans" thread's preview.
    m.set_search_query(Some("tacos".into()));
    assert_eq!(sorted_labels(&m.snapshot()), vec!["Lunch plans"]);

    // No match → empty list.
    m.set_search_query(Some("zzz-nope".into()));
    assert!(
        m.snapshot().threads.is_empty(),
        "non-matching query → no threads"
    );

    // A blank / whitespace-only query is treated as no filter (empty box = all).
    m.set_search_query(Some("   ".into()));
    assert_eq!(
        m.snapshot().threads.len(),
        3,
        "blank/whitespace query → all threads"
    );

    // Clearing the query restores all threads; the field round-trips as None.
    m.set_search_query(None);
    let snap = m.snapshot();
    assert_eq!(snap.threads.len(), 3, "cleared query → all threads");
    assert_eq!(snap.search_query, None);
}

// ── Reactions + sender-only delete (B3) ─────────────────────────────────────
//
// `toggle_reaction` / `delete_message` are manager-local optimistic actions
// (docs/goal/behavior/conversations.md § Reactions & message delete).
// The receive/ingest half (B4) is a separate task — these tests cover only the
// local-action half.

use fauna_conversations::reactions::ReactionGroup;
use fauna_mls::types::ReactionOp;

/// Helper: snapshot one message from `thread` by id.
fn snap_msg(
    m: &ConversationsManager,
    thread: &ThreadId,
    id: &MessageId,
) -> crate::message::MessageSnapshot {
    m.thread_detail(thread.clone())
        .unwrap()
        .messages
        .into_iter()
        .find(|x| x.message_id == *id)
        .unwrap()
}

/// `toggle_reaction` adds a 👍 pill (reacted_by_me=true), then toggles it off.
/// `delete_message` is rejected on a peer's message but accepted on own.
#[tokio::test]
async fn toggle_reaction_and_sender_only_delete() {
    let m = ConversationsManager::new();

    // Register a MockRailBackend for FaunaMls with a known self_address so
    // me_actor() can identify the local actor (ActorId([1u8; 32])).
    let mock = Arc::new(MockRailBackend::new(Rail::FaunaMls));
    mock.set_self_address(TypedAddress::Fauna {
        actor_id: ActorId([1u8; 32]),
        handle: "me@nest".into(),
    });
    m.register_backend(mock);

    // Create a FaunaMls group thread.
    let peer_addr = TypedAddress::Fauna {
        actor_id: ActorId([2u8; 32]),
        handle: "peer@nest".into(),
    };
    let t = m.create_mls_group(vec![peer_addr.clone()]);

    // Inject a peer (is_own=false) message directly into the created thread
    // (ingest_inbound_to_thread bypasses participant-key routing, which avoids
    // a thread-key mismatch when the group thread was created with a partial
    // participant list via create_mls_group).
    let msg_b = MessageId("msg-peer".into());
    m.ingest_inbound_to_thread(
        t.clone(),
        RailInboundMessage {
            rail: Rail::FaunaMls,
            sender: TypedAddress::Fauna {
                actor_id: ActorId([2u8; 32]),
                handle: "peer@nest".into(),
            },
            recipients: vec![TypedAddress::Fauna {
                actor_id: ActorId([1u8; 32]),
                handle: "me@nest".into(),
            }],
            subject: None,
            body: "hello".into(),
            body_format: BodyFormat::PlainText,
            timestamp_ms: 0,
            message_id: msg_b.clone(),
            in_reply_to: None,
            attachments: vec![],
            badges: Default::default(),
            legal_takedown_ref: None,
            plane_ref: None,
        },
    )
    .unwrap();

    // Send own message → the send-echo appends with is_own=true.
    m.set_compose_body(t.clone(), "my reply".into());
    m.send(t.clone()).await.expect("send ok");
    let msg_a = m
        .thread_detail(t.clone())
        .unwrap()
        .messages
        .into_iter()
        .find(|x| x.is_own)
        .expect("own message is the send echo")
        .message_id;

    // --- toggle_reaction: add 👍 on peer's message ---
    m.toggle_reaction(t.clone(), msg_b.clone(), "👍".into())
        .await;
    let r = snap_msg(&m, &t, &msg_b).reactions;
    assert_eq!(
        r,
        vec![ReactionGroup {
            emoji: "👍".into(),
            count: 1,
            reacted_by_me: true,
        }],
        "after add: one 👍 pill with reacted_by_me=true"
    );

    // toggle off
    m.toggle_reaction(t.clone(), msg_b.clone(), "👍".into())
        .await;
    assert!(
        snap_msg(&m, &t, &msg_b).reactions.is_empty(),
        "after second toggle: 👍 pill removed"
    );

    // --- delete_message: rejected on peer's message ---
    m.delete_message(t.clone(), msg_b.clone()).await;
    assert!(
        !snap_msg(&m, &t, &msg_b).deleted,
        "delete on peer's message is rejected (sender-only)"
    );

    // --- delete_message: accepted on own message ---
    m.delete_message(t.clone(), msg_a.clone()).await;
    assert!(
        snap_msg(&m, &t, &msg_a).deleted,
        "delete on own message sets deleted=true"
    );
}

/// The FFI-reachable twin of the hand-registered mock above: a foreign test
/// (android's Robolectric pins) cannot construct a `MockRailBackend` across the
/// FFI, so `install_mock_backend_knowing_self_for_test` registers one that knows
/// who the user is — the one thing a FaunaMls-only gesture needs before
/// `toggle_reaction` will attribute it.
#[tokio::test]
async fn a_mock_rail_that_knows_self_lets_a_reaction_land() {
    let m = ConversationsManager::new();
    m.install_mock_backends_for_test();
    let peer = TypedAddress::Fauna {
        actor_id: ActorId([2u8; 32]),
        handle: "peer@nest".into(),
    };
    let t = m.create_mls_group(vec![peer]);
    m.set_compose_body(t.clone(), "react to me".into());
    m.send(t.clone()).await.expect("send ok");
    let own = m
        .thread_detail(t.clone())
        .unwrap()
        .messages
        .into_iter()
        .find(|x| x.is_own)
        .expect("the send echo")
        .message_id;

    // The bulk install leaves FaunaMls self-less on purpose, so the gesture is
    // dropped — the gap this seam exists to close.
    m.toggle_reaction(t.clone(), own.clone(), "🦊".into()).await;
    assert!(snap_msg(&m, &t, &own).reactions.is_empty());

    m.install_mock_backend_knowing_self_for_test(
        Rail::FaunaMls,
        TypedAddress::Fauna {
            actor_id: ActorId([1u8; 32]),
            handle: "me@nest".into(),
        },
    );
    m.toggle_reaction(t.clone(), own.clone(), "🦊".into()).await;
    assert_eq!(
        snap_msg(&m, &t, &own).reactions,
        vec![ReactionGroup {
            emoji: "🦊".into(),
            count: 1,
            reacted_by_me: true,
        }],
    );
}

// ── B4: ingest reactions / deletes (apply_inbound_reaction / apply_inbound_delete) ──
//
// Tests for the receive half: inbound reactions fold onto the pill,
// forged-delete (claimed-sender ≠ actual sender) is ignored, and
// out-of-order reactions are applied once the target message arrives.

/// Build a FaunaMls RailInboundMessage with a known MessageId and sender actor.
fn fauna_inbound_actor(actor_id: ActorId, body: &str, message_id: MessageId) -> RailInboundMessage {
    RailInboundMessage {
        rail: Rail::FaunaMls,
        sender: TypedAddress::Fauna {
            actor_id,
            handle: format!("peer{:02x}@nest", actor_id.0[0]),
        },
        recipients: vec![TypedAddress::Fauna {
            actor_id: ActorId([1u8; 32]),
            handle: "me@nest".into(),
        }],
        subject: None,
        body: body.into(),
        body_format: BodyFormat::PlainText,
        timestamp_ms: 0,
        message_id,
        in_reply_to: None,
        attachments: vec![],
        badges: Default::default(),
        legal_takedown_ref: None,
        plane_ref: None,
    }
}

/// Build a manager + FaunaMls mock with me=actor[1]. Returns (manager, thread_id).
fn manager_with_fauna_thread() -> (Arc<ConversationsManager>, ThreadId) {
    let m = ConversationsManager::new();
    let mock = Arc::new(MockRailBackend::new(Rail::FaunaMls));
    mock.set_self_address(TypedAddress::Fauna {
        actor_id: ActorId([1u8; 32]),
        handle: "me@nest".into(),
    });
    m.register_backend(mock);
    let peer_addr = TypedAddress::Fauna {
        actor_id: ActorId([2u8; 32]),
        handle: "peer@nest".into(),
    };
    let t = m.create_mls_group(vec![peer_addr]);
    (m, t)
}

/// A membership commit another member authored reaches this device's roster
/// through `apply_inbound_roster`: the actor who left the agreed roster leaves
/// the participant list; a recorded succession's predecessor stays for the
/// re-point; a roster that changes nothing does not notify; and the successor
/// that predecessor stands in for is **not** seated as a newcomer beside it
/// (one member, one row — the re-point would otherwise land on a duplicate).
/// The add arm proper is
/// `an_inbound_roster_seats_a_member_another_device_added`.
#[test]
fn an_inbound_roster_drops_who_left_but_keeps_a_superseded_predecessor() {
    let (m, t) = manager_with_fauna_thread();
    let third = TypedAddress::Fauna {
        actor_id: ActorId([3u8; 32]),
        handle: "third@nest".into(),
    };
    let fourth = TypedAddress::Fauna {
        actor_id: ActorId([4u8; 32]),
        handle: "fourth@nest".into(),
    };
    assert!(m.add_participant(t.clone(), third).is_some());
    assert!(m.add_participant(t.clone(), fourth).is_some());
    let ids = |m: &ConversationsManager| -> Vec<ActorId> {
        m.thread_detail(t.clone())
            .unwrap()
            .participants
            .iter()
            .filter_map(|p| p.person_actor_id())
            .collect()
    };
    assert_eq!(
        ids(&m),
        vec![ActorId([2u8; 32]), ActorId([3u8; 32]), ActorId([4u8; 32])]
    );

    struct Ticks(Arc<std::sync::atomic::AtomicUsize>);
    impl SnapshotObserver for Ticks {
        fn on_changed(&self) {
            self.0.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        }
    }
    let notified = Arc::new(std::sync::atomic::AtomicUsize::new(0));
    m.add_observer(Arc::new(Ticks(Arc::clone(&notified))));

    // Nobody left: no change, no notify.
    m.apply_inbound_roster(
        t.clone(),
        &[
            ActorId([1u8; 32]),
            ActorId([2u8; 32]),
            ActorId([3u8; 32]),
            ActorId([4u8; 32]),
        ],
        &[],
    );
    assert_eq!(notified.load(std::sync::atomic::Ordering::SeqCst), 0);

    // Someone else removed actor 3, and actor 4 is the predecessor of a
    // recorded succession whose successor 9 is in the agreed roster. Actor 4's
    // row is 9's seat until the verified statement re-points it, so 9 must not
    // arrive beside it as a newcomer.
    m.apply_inbound_roster(
        t.clone(),
        &[ActorId([1u8; 32]), ActorId([2u8; 32]), ActorId([9u8; 32])],
        &[(ActorId([4u8; 32]), ActorId([9u8; 32]))],
    );
    assert_eq!(ids(&m), vec![ActorId([2u8; 32]), ActorId([4u8; 32])]);
    assert_eq!(notified.load(std::sync::atomic::Ordering::SeqCst), 1);

    // And the re-point lands on that one row, not on a duplicate.
    m.apply_inbound_succession(t.clone(), &ActorId([4u8; 32]), ActorId([9u8; 32]));
    assert_eq!(ids(&m), vec![ActorId([2u8; 32]), ActorId([9u8; 32])]);
    let detail = m.thread_detail(t.clone()).unwrap();
    assert_eq!(
        detail.participant_displays,
        detail
            .participants
            .iter()
            .map(|p| p.display())
            .collect::<Vec<_>>(),
        "the display column follows the participants"
    );
}

/// A recorded succession's predecessor is retained ONLY while its
/// successor (the pair's chain-resolved second element) is actually in the
/// agreed roster — never unconditionally. Without this, an ordinary member
/// could self-record `{old: self, new: <an invented actor id>}` (`judge_commit`
/// admits a self-record with no check that `new` exists or is ever seated —
/// `libs/fauna-mls/src/room_policy.rs`), and after an admin removed them,
/// `superseded.iter().any(|(old, _)| *old == actor)` would put them straight
/// back into every honest member's rendered roster, permanently: the
/// successor that would let `apply_inbound_succession` re-point the row (and
/// so retire it) never arrives.
/// The sibling of `an_inbound_roster_drops_who_left_but_keeps_a_superseded_predecessor`
/// above: same setup, but the recorded successor is absent from the roster.
#[test]
fn an_inbound_roster_drops_a_predecessor_whose_recorded_successor_never_arrived() {
    let (m, t) = manager_with_fauna_thread();
    let third = TypedAddress::Fauna {
        actor_id: ActorId([3u8; 32]),
        handle: "third@nest".into(),
    };
    let fourth = TypedAddress::Fauna {
        actor_id: ActorId([4u8; 32]),
        handle: "fourth@nest".into(),
    };
    assert!(m.add_participant(t.clone(), third).is_some());
    assert!(m.add_participant(t.clone(), fourth).is_some());
    let ids = |m: &ConversationsManager| -> Vec<ActorId> {
        m.thread_detail(t.clone())
            .unwrap()
            .participants
            .iter()
            .filter_map(|p| p.person_actor_id())
            .collect()
    };
    assert_eq!(
        ids(&m),
        vec![ActorId([2u8; 32]), ActorId([3u8; 32]), ActorId([4u8; 32])]
    );

    // Someone else removed actor 3 AND actor 4. Actor 4 self-recorded a
    // succession naming actor 9 as its successor, but 9 never joined — the
    // agreed roster carries no such actor. Unlike the sibling test above,
    // there is nothing here for a later `apply_inbound_succession` to ever
    // re-point onto, so actor 4's row must drop like actor 3's did, not
    // linger forever.
    m.apply_inbound_roster(
        t.clone(),
        &[ActorId([1u8; 32]), ActorId([2u8; 32])],
        &[(ActorId([4u8; 32]), ActorId([9u8; 32]))],
    );
    assert_eq!(
        ids(&m),
        vec![ActorId([2u8; 32])],
        "a predecessor whose recorded successor never joined the agreed roster \
         must not be retained — retention exists only to give a genuinely \
         parked succession somewhere to re-point onto"
    );
}

/// The **add arm** of the same seam (`conversation-rooms.md` § The floor
/// roster: the roster every member renders is the one the MLS group agrees
/// on, never what this device last did itself). A member some *other* device
/// added joins this device's participant list from the agreed roster alone —
/// seated handle-less, exactly as `ingest_welcome` seats the members a Welcome
/// brings, because the engine roster carries actor ids and nothing else. The
/// local user is never seated (participants are everyone else), the seat is
/// idempotent, and it appends — the row order every open editor indexes into
/// does not shift under it.
#[test]
fn an_inbound_roster_seats_a_member_another_device_added() {
    let (m, t) = manager_with_fauna_thread();
    let ids = |m: &ConversationsManager| -> Vec<ActorId> {
        m.thread_detail(t.clone())
            .unwrap()
            .participants
            .iter()
            .filter_map(|p| p.person_actor_id())
            .collect()
    };
    assert_eq!(ids(&m), vec![ActorId([2u8; 32])]);

    struct Ticks(Arc<std::sync::atomic::AtomicUsize>);
    impl SnapshotObserver for Ticks {
        fn on_changed(&self) {
            self.0.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        }
    }
    let notified = Arc::new(std::sync::atomic::AtomicUsize::new(0));
    m.add_observer(Arc::new(Ticks(Arc::clone(&notified))));

    // Another member's commit seated actor 5. Self (actor 1) is in the agreed
    // roster too and must NOT become a participant row.
    m.apply_inbound_roster(
        t.clone(),
        &[ActorId([1u8; 32]), ActorId([2u8; 32]), ActorId([5u8; 32])],
        &[],
    );
    assert_eq!(
        ids(&m),
        vec![ActorId([2u8; 32]), ActorId([5u8; 32])],
        "the newcomer joins at the end of the list; the local user does not"
    );
    assert_eq!(notified.load(std::sync::atomic::Ordering::SeqCst), 1);

    let detail = m.thread_detail(t.clone()).unwrap();
    assert_eq!(
        detail.participant_displays,
        detail
            .participants
            .iter()
            .map(|p| p.display())
            .collect::<Vec<_>>(),
        "the display column follows the participants"
    );
    assert_eq!(
        detail.participants[1].person_handle(),
        None,
        "the engine roster carries no handle and this device has never met \
         actor 5, so the seat rests handle-less"
    );
    assert_eq!(
        detail.participants[1].display(),
        fauna_core::format::short_id(&ActorId([5u8; 32]).to_hex()),
        "an unresolved member renders as its elided actor id, NOT as a blank \
         row - `value-formatting.md` § Account display label, the same rule \
         the folders roster row uses for its own unset-handle case"
    );

    // Idempotent: the same agreed roster a second time changes nothing.
    m.apply_inbound_roster(
        t.clone(),
        &[ActorId([1u8; 32]), ActorId([2u8; 32]), ActorId([5u8; 32])],
        &[],
    );
    assert_eq!(ids(&m), vec![ActorId([2u8; 32]), ActorId([5u8; 32])]);
    assert_eq!(notified.load(std::sync::atomic::Ordering::SeqCst), 1);
}

/// **The add arm resolves through the shared seat path.** A member another
/// device added is named by the engine roster, which carries actor ids and
/// nothing else - but this device may already be rendering that same person, by
/// name, in another thread. `ConversationsManager::seat_address_for` is the one
/// path both roster seating sites use to reach that knowledge
/// (`conversation-rooms.md` § Implementation status today).
///
/// Asserting the *display* rather than only the handle is the load-bearing
/// half: the row the user reads is `participant_displays`, and a version that
/// resolved the handle but left the display column stale would pass a
/// handle-only assertion.
#[test]
fn an_inbound_roster_seat_takes_a_handle_this_device_already_knows() {
    let m = ConversationsManager::new();
    let mock = Arc::new(MockRailBackend::new(Rail::FaunaMls));
    mock.set_self_address(TypedAddress::Fauna {
        actor_id: ActorId([1u8; 32]),
        handle: "me@nest".into(),
    });
    m.register_backend(mock.clone());
    let t = m.create_mls_group(vec![TypedAddress::Fauna {
        actor_id: ActorId([2u8; 32]),
        handle: "peer@nest".into(),
    }]);
    let newcomer = ActorId([5u8; 32]);

    // This device met the newcomer in a different thread, by name — a thread
    // whose group has PROVEN that id (the paint gate: only a seat the rail's
    // roster vouches for lends its handle, `contacts.md` § The private overlay
    // → *The paint gate*).
    let other = m.materialize_conv_thread(
        "some-other-channel".to_string(),
        vec![TypedAddress::Fauna {
            handle: "dave@nest".into(),
            actor_id: newcomer,
        }],
    );
    mock.set_engine_roster(other, vec![ActorId([1u8; 32]), newcomer]);

    m.apply_inbound_roster(
        t.clone(),
        &[ActorId([1u8; 32]), ActorId([2u8; 32]), newcomer],
        &[],
    );

    let detail = m.thread_detail(t.clone()).unwrap();
    assert_eq!(
        detail.participants[1].person_handle(),
        Some("dave@nest"),
        "the seat reaches what this device already knows rather than resting nameless"
    );
    assert_eq!(
        detail.participant_displays[1], "dave@nest",
        "and the display column the user actually reads follows it"
    );
}

/// The same resolution, at the OTHER seating site: the members a Welcome
/// brings. `ingest_welcome` builds its participants from the joined group's
/// engine roster, which has no handles to give, and shares
/// `seat_address_for` with the add arm rather than keeping a second copy
/// (priority #2 - one resolution path, both sites).
///
/// This is the manager-level half; `fauna_mls_backend_tests.rs` drives the same
/// property through a real `ingest_welcome` over a real MLS engine.
#[test]
fn a_welcome_style_seat_takes_a_handle_this_device_already_knows() {
    let m = ConversationsManager::new();
    let mock = Arc::new(MockRailBackend::new(Rail::FaunaMls));
    m.register_backend(mock.clone());
    let member = ActorId([7u8; 32]);
    let stranger = ActorId([8u8; 32]);

    // A thread whose group has proven the member's id — the only kind of seat
    // that lends its handle (`contacts.md` § The private overlay → *The paint
    // gate*).
    let had = m.materialize_conv_thread(
        "a-thread-we-already-had".to_string(),
        vec![TypedAddress::Fauna {
            handle: "erin@nest".into(),
            actor_id: member,
        }],
    );
    mock.set_engine_roster(had, vec![member]);

    assert_eq!(
        m.seat_address_for(member),
        TypedAddress::Fauna {
            handle: "erin@nest".into(),
            actor_id: member,
        },
        "a member this device already renders by name is seated by that name"
    );

    let seat = m.seat_address_for(stranger);
    assert_eq!(
        seat.person_handle(),
        None,
        "a member this device has never met is seated handle-less - honestly, \
         not with a guess"
    );
    assert_eq!(
        seat.display(),
        fauna_core::format::short_id(&stranger.to_hex()),
        "and reads as its elided actor id"
    );
}

/// **The id-keyed handle read names a member this device has NEVER met** —
/// the leg `seat_address_for`'s device-local scan structurally cannot reach
/// (`conversation-rooms.md` § Implementation status today, the roster bullet).
///
/// `nameless_participants` picks out exactly the rows still rendering as an
/// elided actor id — the set worth a network read — and
/// `apply_resolved_handles` names them **in place**: same list position, so
/// the `thread-member-chip[i]` those indices render keeps standing for the
/// same person, with `participant_displays` following. Re-seating instead
/// would slide every later index and present as the *wrong person*, which is
/// why this mirrors `apply_inbound_succession`'s re-point rather than
/// `add_participant`.
///
/// The two negatives are the load-bearing half. A member the home nest served
/// no handle for — one homed on another nest, which the floor roster
/// deliberately elides (§ The floor roster) — must keep its elided id rather
/// than acquire a neighbour's name; and an answer must never *overwrite* a
/// handle this device already had, so a stale or slower read cannot undo
/// richer local knowledge.
#[test]
fn an_id_keyed_handle_read_names_a_member_this_device_has_never_met() {
    let (m, t) = manager_with_fauna_thread();
    let met = ActorId([2u8; 32]); // seated by the fixture, already named
    let local = ActorId([5u8; 32]); // never met, homed on this nest
    let foreign = ActorId([6u8; 32]); // never met, homed elsewhere

    // Another member's commit seats both strangers, handle-less.
    m.apply_inbound_roster(t.clone(), &[ActorId([1u8; 32]), met, local, foreign], &[]);
    assert_eq!(
        m.nameless_participants(&t),
        vec![local, foreign],
        "only the rows rendering as an elided id are worth a read — the \
         member already rendered by name is not asked about"
    );

    struct Ticks(Arc<std::sync::atomic::AtomicUsize>);
    impl SnapshotObserver for Ticks {
        fn on_changed(&self) {
            self.0.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        }
    }
    let notified = Arc::new(std::sync::atomic::AtomicUsize::new(0));
    m.add_observer(Arc::new(Ticks(Arc::clone(&notified))));

    // The home nest answered: a handle for its own user, nothing for the
    // foreign principal, and a name for the member we already knew (which
    // must not overwrite what we hold).
    m.apply_resolved_handles(
        t.clone(),
        &[
            (local, "frank@nest".to_string()),
            (met, "a-different-name@nest".to_string()),
        ],
    );
    assert_eq!(notified.load(std::sync::atomic::Ordering::SeqCst), 1);

    let detail = m.thread_detail(t.clone()).unwrap();
    assert_eq!(
        detail
            .participants
            .iter()
            .filter_map(|p| p.person_actor_id())
            .collect::<Vec<_>>(),
        vec![met, local, foreign],
        "named in place — the read adds no row and moves none"
    );
    assert_eq!(
        detail.participants[1].person_handle(),
        Some("frank@nest"),
        "the member this device had never met now has a name"
    );
    assert_eq!(
        detail.participants[0].person_handle(),
        Some("peer@nest"),
        "and an answer never overwrites a handle this device already held"
    );
    assert_eq!(
        detail.participants[2].person_handle(),
        None,
        "a member the home nest served no handle for stays unresolved — the \
         floor roster elides a principal homed on another nest"
    );
    assert_eq!(
        detail.participants[2].display(),
        fauna_core::format::short_id(&foreign.to_hex()),
        "and keeps rendering as its elided actor id, never blank and never a \
         neighbour's name"
    );
    assert_eq!(
        detail.participant_displays,
        detail
            .participants
            .iter()
            .map(|p| p.display())
            .collect::<Vec<_>>(),
        "the display column follows every row the read named"
    );
    assert_eq!(
        m.nameless_participants(&t),
        vec![foreign],
        "the resolved member drops out of the read set; the elided one stays"
    );

    // Idempotent, and silent: the same answer again changes nothing, so a
    // re-read that resolves nothing new does not tick the snapshot.
    m.apply_resolved_handles(t.clone(), &[(local, "frank@nest".to_string())]);
    assert_eq!(notified.load(std::sync::atomic::Ordering::SeqCst), 1);
}

/// **A refused add must not evict the member it was about.** The rollback only
/// undoes what *this gesture* added — "a duplicate gesture on an existing
/// participant must not evict them from the list", as `add_participant`'s own
/// comment puts it — and `was_already_listed` is what separates the two cases.
///
/// It compared the *rendered* names. A member seated off an MLS roster has no
/// handle, so the seated row and the picker-resolved address the user typed
/// rendered differently for **the same person**: the gesture read as "we added
/// them", and a refusal then removed a member who was in the group the whole
/// time — via `remove_participant_from`, which keys on the actor id and so
/// found them without trouble. The likeliest way to reach it is also the most
/// galling: the add is refused *because* they are already a member.
///
/// The predicate now keys on [`TypedAddress::same_participant`] like every
/// other roster edit. Two Fauna addresses are the same participant when their
/// actor ids agree, whatever handles they wear.
#[tokio::test]
async fn a_refused_add_does_not_evict_a_member_who_was_already_seated() {
    let m = ConversationsManager::new();
    let mock = Arc::new(MockRailBackend::new(Rail::FaunaMls));
    mock.set_self_address(TypedAddress::Fauna {
        actor_id: ActorId([1u8; 32]),
        handle: "me@nest".into(),
    });
    m.register_backend(mock.clone());

    let t = m.create_mls_group(vec![
        TypedAddress::Fauna {
            actor_id: ActorId([2u8; 32]),
            handle: "peer@nest".into(),
        },
        TypedAddress::Fauna {
            actor_id: ActorId([4u8; 32]),
            handle: "other@nest".into(),
        },
    ]);
    // Seated off the agreed roster, so nameless — exactly what both seating
    // sites write for a member this device has never met.
    let nameless = ActorId([3u8; 32]);
    m.apply_inbound_roster(
        t.clone(),
        &[
            ActorId([1u8; 32]),
            ActorId([2u8; 32]),
            ActorId([4u8; 32]),
            nameless,
        ],
        &[],
    );
    let seated = |m: &ConversationsManager| -> bool {
        m.thread_detail(t.clone())
            .unwrap()
            .participants
            .iter()
            .any(|p| p.person_actor_id() == Some(nameless))
    };
    assert!(seated(&m), "the nameless member is seated to begin with");

    // The user types that same person; the picker resolves them BY NAME, so
    // the chip and the seated row render differently for one identity. The
    // rail then refuses — the likeliest refusal being that they are already in.
    mock.fail_add_on(t.clone(), "already a member of this group");
    m.select_thread(t.clone());
    m.open_add_participant(t.clone());
    m.accept_add_participant_chip(TypedAddress::Fauna {
        actor_id: nameless,
        handle: "carol@nest".into(),
    });
    m.confirm_add_participant().await;

    assert!(
        seated(&m),
        "the refusal must not evict a member this gesture did not add — they \
         were seated off the agreed roster before the user ever typed them"
    );
    assert!(
        m.page_error_diagnostic().is_some(),
        "and the refusal is still surfaced, not swallowed"
    );
}

#[test]
fn inbound_reaction_from_peer_shows_pill() {
    // B4: an inbound reaction from a peer (actor[2]) on a peer message causes
    // thread_detail to fold a pill: count=1, reacted_by_me=false (me=actor[1]).
    let (m, t) = manager_with_fauna_thread();
    let mid = MessageId("msg-peer-rxn".into());
    m.ingest_inbound_to_thread(
        t.clone(),
        fauna_inbound_actor(ActorId([2u8; 32]), "hello", mid.clone()),
    )
    .unwrap();

    m.apply_inbound_reaction(
        mid.clone(),
        ActorId([2u8; 32]),
        "👍".into(),
        ReactionOp::Add,
        1_000,
    );

    assert_eq!(
        snap_msg(&m, &t, &mid).reactions,
        vec![ReactionGroup {
            emoji: "👍".into(),
            count: 1,
            reacted_by_me: false,
        }],
        "peer reaction shows pill with reacted_by_me=false"
    );
}

#[test]
fn forged_delete_from_non_sender_is_ignored() {
    // B4: a delete claim from actor[3] (a non-sender) on actor[2]'s message must be
    // ignored; a claim from actor[2] (the real sender) must be honored.
    let (m, t) = manager_with_fauna_thread();
    let mid = MessageId("msg-peer-del".into());
    m.ingest_inbound_to_thread(
        t.clone(),
        fauna_inbound_actor(ActorId([2u8; 32]), "hello", mid.clone()),
    )
    .unwrap();

    // Forged delete (claimer ≠ message sender) — must be ignored.
    m.apply_inbound_delete(mid.clone(), ActorId([3u8; 32]));
    assert!(
        !snap_msg(&m, &t, &mid).deleted,
        "forged delete (claimer ≠ sender) must be ignored"
    );

    // Real delete (claimer == message sender) — must be honored.
    m.apply_inbound_delete(mid.clone(), ActorId([2u8; 32]));
    assert!(
        snap_msg(&m, &t, &mid).deleted,
        "real delete (claimer == sender) must mark deleted"
    );
}

/// `conversation-rooms.md` § Roles and authorization → *Delete any message —
/// the mechanism*: a cross-sender delete is a SECOND admission beside the
/// sender match. It is honoured only on the role the backend recorded for the
/// claimant when it folded the delete (owner or admin under the policy the
/// delete was made under); a plain member's, and a claim with no role to vouch
/// for it, stay the forged delete they always were.
#[test]
fn a_cross_sender_delete_is_honoured_only_on_a_governing_role_recorded_at_fold() {
    use fauna_conversations::{DeleteClaim, RoomRole};
    let (m, t) = manager_with_fauna_thread();
    let claim = |actor: u8, seq: u64, role: Option<RoomRole>| DeleteClaim {
        claimant: ActorId([actor; 32]),
        delete_seq: Some(seq),
        role,
    };
    for (i, (role, honoured)) in [
        (None, false),
        (Some(RoomRole::Member), false),
        (Some(RoomRole::Admin), true),
        (Some(RoomRole::Owner), true),
    ]
    .into_iter()
    .enumerate()
    {
        let mid = MessageId(format!("conv:testch:{i}"));
        m.ingest_inbound_to_thread(
            t.clone(),
            fauna_inbound_actor(ActorId([2u8; 32]), "hello", mid.clone()),
        )
        .unwrap();
        m.apply_inbound_delete_claim(mid.clone(), claim(3, 100 + i as u64, role));
        assert_eq!(
            snap_msg(&m, &t, &mid).deleted,
            honoured,
            "a cross-sender delete whose claimant held {role:?} when it was folded"
        );
    }
}

/// One claim per message was the as-built map, so whichever delete arrived
/// last decided the tombstone. Every claim is judged on its own: a forged
/// claim that follows an honoured one must not lift the tombstone, and an
/// honoured one that follows a forged one must still land.
#[test]
fn a_forged_delete_claim_never_displaces_an_honoured_one() {
    use fauna_conversations::{DeleteClaim, RoomRole};
    let (m, t) = manager_with_fauna_thread();
    let admin = DeleteClaim {
        claimant: ActorId([3u8; 32]),
        delete_seq: Some(10),
        role: Some(RoomRole::Admin),
    };
    let forged = DeleteClaim {
        claimant: ActorId([4u8; 32]),
        delete_seq: Some(11),
        role: Some(RoomRole::Member),
    };
    for (n, order) in [[admin.clone(), forged.clone()], [forged, admin]]
        .into_iter()
        .enumerate()
    {
        let mid = MessageId(format!("conv:testch:{}", 50 + n));
        m.ingest_inbound_to_thread(
            t.clone(),
            fauna_inbound_actor(ActorId([2u8; 32]), "hello", mid.clone()),
        )
        .unwrap();
        for c in order {
            m.apply_inbound_delete_claim(mid.clone(), c);
        }
        assert!(
            snap_msg(&m, &t, &mid).deleted,
            "the admin's claim decides, whichever order the two arrived in"
        );
    }
}

/// **`devices.md` § Durability rules, rule 3** — *"state only this device can
/// produce must reach the replica before the user can believe it saved"* —
/// applied to the two derived states `conversations.md` § Reactions & message
/// delete → *At rest* describes: the tombstone and the reaction aggregate.
///
/// Stream replay cannot reconstruct either on a restored device. Both live in
/// manager memory (`reactions`, `deleted`, `delete_claims`), and the records
/// that produced them sit **below** the resumed poll's watermark — an own
/// `Reaction`/`Delete` is MLS-opaque to its own author off the log at that, so
/// no re-walk can re-derive it even in principle. So the slice has to carry
/// them, and this is the pin that says it does.
///
/// Four legs, because each fails differently:
/// 1. the sender's **own** delete,
/// 2. an **owner/admin's** cross-sender delete, admitted on the role the rail
///    recorded at fold (`conversation-rooms.md` § Roles and authorization →
///    *Delete any message — the mechanism*),
/// 3. the reaction **pills**, mine and a peer's, and
/// 4. — the one an aggregate alone could never satisfy — a reaction **toggled
///    after the restore**: `toggle_reaction` resolves Add-vs-Remove by folding
///    the event log, so unless the raw per-actor events rode the slice, my
///    restored 👍 would re-Add instead of retracting, and the projection's fold
///    of that one post-restore event would wipe every restored pill it did not
///    itself produce.
#[tokio::test]
async fn a_tombstone_and_its_reaction_pills_survive_a_history_slice_restore() {
    use fauna_conversations::store::history::ChannelHistorySlice;
    const CH: &str = "5b1d0f4a3c2e1908776655443322110ffeeddccbbaa99887766554433221100ab";

    let (m, t) = manager_with_fauna_thread();

    // A peer message an admin deletes across senders, and a peer message that
    // collects reactions.
    let admin_deleted = MessageId(format!("conv:{CH}:1"));
    let reacted = MessageId(format!("conv:{CH}:2"));
    for id in [&admin_deleted, &reacted] {
        m.ingest_inbound_to_thread(
            t.clone(),
            fauna_inbound_actor(ActorId([2u8; 32]), "hello", id.clone()),
        )
        .unwrap();
    }

    // Leg 2: a third actor's delete, admitted on the Admin role the rail
    // recorded when it folded the delete.
    m.apply_inbound_delete_claim(
        admin_deleted.clone(),
        DeleteClaim {
            claimant: ActorId([3u8; 32]),
            delete_seq: Some(10),
            role: Some(RoomRole::Admin),
        },
    );

    // Leg 3: a peer's 👍 and ❤️, and my own 👍 on top.
    m.apply_inbound_reaction(
        reacted.clone(),
        ActorId([2u8; 32]),
        "👍".into(),
        ReactionOp::Add,
        1_000,
    );
    m.apply_inbound_reaction(
        reacted.clone(),
        ActorId([2u8; 32]),
        "❤️".into(),
        ReactionOp::Add,
        1_001,
    );
    m.toggle_reaction(t.clone(), reacted.clone(), "👍".into())
        .await;

    // Leg 1: my own message, deleted by me.
    m.set_compose_body(t.clone(), "my reply".into());
    m.send(t.clone()).await.expect("send ok");
    let own = m
        .thread_detail(t.clone())
        .unwrap()
        .messages
        .into_iter()
        .find(|x| x.is_own)
        .expect("own message is the send echo")
        .message_id;
    m.delete_message(t.clone(), own.clone()).await;

    // The capture both `history/<ch>` writers take, through the at-rest bytes
    // the restoring device decodes.
    let captured = m
        .snapshot_channel_slice(&t, CH, 7)
        .expect("a bound thread snapshots");
    let restored = ChannelHistorySlice::from_bytes(&captured.to_bytes().unwrap()).unwrap();

    // A fresh manager for the same owner — the RAM-only thread store re-seeded
    // from the replica alone, exactly as a relaunch does.
    let device_two = ConversationsManager::new();
    let mock = Arc::new(MockRailBackend::new(Rail::FaunaMls));
    mock.set_self_address(TypedAddress::Fauna {
        actor_id: ActorId([1u8; 32]),
        handle: "me@nest".into(),
    });
    device_two.register_backend(mock);
    let t2 = device_two.restore_channel_slice(&restored);

    assert!(
        snap_msg(&device_two, &t2, &own).deleted,
        "leg 1: the sender's own tombstone survives the restore"
    );
    assert!(
        snap_msg(&device_two, &t2, &admin_deleted).deleted,
        "leg 2: the admin's cross-sender tombstone survives exactly as it was folded"
    );
    assert_eq!(
        snap_msg(&device_two, &t2, &reacted).reactions,
        vec![
            ReactionGroup {
                emoji: "👍".into(),
                count: 2,
                reacted_by_me: true,
            },
            ReactionGroup {
                emoji: "❤️".into(),
                count: 1,
                reacted_by_me: false,
            },
        ],
        "leg 3: both pills, their counts and my own membership survive"
    );

    // Leg 4: a reaction toggled AFTER the restore. My restored 👍 must retract
    // (not double-add), and the peer's pills must not be wiped by the fold.
    device_two
        .toggle_reaction(t2.clone(), reacted.clone(), "👍".into())
        .await;
    assert_eq!(
        snap_msg(&device_two, &t2, &reacted).reactions,
        vec![
            ReactionGroup {
                emoji: "👍".into(),
                count: 1,
                reacted_by_me: false,
            },
            ReactionGroup {
                emoji: "❤️".into(),
                count: 1,
                reacted_by_me: false,
            },
        ],
        "leg 4: the post-restore toggle retracts MY 👍 off the restored log and \
         leaves the peer's pills standing"
    );
}

/// **A restored slice's derived state may not reach a message the slice does
/// not carry.** The manager's `deleted` set and reaction log are keyed by
/// `MessageId` *globally*, across every thread — but a slice belongs to one
/// channel, and one of its readers is the history-for-joiners path, where the
/// writer is another room MEMBER rather than the owner's own device
/// (`conversation-rooms.md` § History for joiners). Unbounded, a slice naming
/// channel A would be a forged delete that reaches a message of channel B —
/// the exact act `DeleteClaim::admits` refuses on the live path, arriving by a
/// door that has no claimant to judge.
///
/// Both halves are asserted against the same tampered slice, so the test also
/// shows the bound is a *filter* and not a blanket refusal: the id the slice
/// carries still lands.
#[tokio::test]
async fn a_restored_slice_cannot_tombstone_or_react_to_a_message_it_does_not_carry() {
    use fauna_conversations::store::history::ChannelHistorySlice;
    const CH: &str = "77aa1b2c3d4e5f60718293a4b5c6d7e8f9012345678998765432100fedcba9876";

    // Device one: two peer messages, one tombstoned by an admin and one
    // carrying a pill — then captured as a slice.
    let (one, t_one) = manager_with_fauna_thread();
    let reaches = MessageId(format!("conv:{CH}:1"));
    let carried = MessageId(format!("conv:{CH}:2"));
    for id in [&reaches, &carried] {
        one.ingest_inbound_to_thread(
            t_one.clone(),
            fauna_inbound_actor(ActorId([2u8; 32]), "hello", id.clone()),
        )
        .unwrap();
        one.apply_inbound_delete_claim(
            id.clone(),
            DeleteClaim {
                claimant: ActorId([3u8; 32]),
                delete_seq: Some(10),
                role: Some(RoomRole::Owner),
            },
        );
        one.apply_inbound_reaction(
            id.clone(),
            ActorId([2u8; 32]),
            "👍".into(),
            ReactionOp::Add,
            1_000,
        );
    }
    let mut slice = one
        .snapshot_channel_slice(&t_one, CH, 2)
        .expect("a bound thread snapshots");
    assert_eq!(slice.deleted_messages.len(), 2);
    assert_eq!(slice.reaction_log.len(), 2);

    // A writer that is not this owner's own device drops the message but keeps
    // the derived state that named it — a trimmed slice, or a forged one.
    slice.messages.retain(|m| m.message_id == carried);
    let slice = ChannelHistorySlice::from_bytes(&slice.to_bytes().unwrap()).unwrap();

    // Device two holds BOTH messages first-hand, neither deleted.
    let (two, t_two) = manager_with_fauna_thread();
    for id in [&reaches, &carried] {
        two.ingest_inbound_to_thread(
            t_two.clone(),
            fauna_inbound_actor(ActorId([2u8; 32]), "hello", id.clone()),
        )
        .unwrap();
    }
    two.restore_channel_slice(&slice);

    assert!(
        !snap_msg(&two, &t_two, &reaches).deleted,
        "the slice dropped this message, so its tombstone must not reach it"
    );
    assert!(
        snap_msg(&two, &t_two, &reaches).reactions.is_empty(),
        "nor may its reaction log paint a pill on it"
    );
    assert!(
        snap_msg(&two, &t_two, &carried).deleted,
        "the message the slice DOES carry keeps its tombstone — a filter, not a refusal"
    );
}

/// **The stamp-less `reaction_events` field is neither written nor read.** A
/// slice once carried it beside `reaction_log` — a downgrade mirror for a
/// build predating the stamp — and a restore fell back to it, folding its
/// events by log order. Both halves were retired by the compat-remnant sweep
/// (`version-compatibility.md` § Dimension 2, program 4): a written slice
/// carries no such key, and a slice carrying ONLY that key restores no
/// reactions at all.
#[tokio::test]
async fn a_slice_carrying_only_the_retired_unstamped_reaction_field_restores_none() {
    use fauna_conversations::store::history::ChannelHistorySlice;
    use std::collections::BTreeMap;
    const CH: &str = "1199aabbccddeeff00112233445566778899aabbccddeeff0011223344556677";

    let (one, t_one) = manager_with_fauna_thread();
    let target = MessageId(format!("conv:{CH}:1"));
    one.ingest_inbound_to_thread(
        t_one.clone(),
        fauna_inbound_actor(ActorId([2u8; 32]), "hello", target.clone()),
    )
    .unwrap();
    one.apply_inbound_reaction(
        target.clone(),
        ActorId([2u8; 32]),
        "🙏".into(),
        ReactionOp::Add,
        1_000,
    );
    let mut slice = one
        .snapshot_channel_slice(&t_one, CH, 1)
        .expect("a bound thread snapshots");
    // Blank the folded aggregate too, so the only reaction state left on the
    // slice is whatever the event log re-seeds.
    for m in &mut slice.messages {
        m.reactions.clear();
    }
    let mut payload: BTreeMap<String, fauna_cbor::Value> =
        fauna_cbor::decode_strict(&slice.to_bytes().expect("encode")).expect("a map");
    assert!(
        !payload.contains_key("reaction_events"),
        "the stamp-less mirror is no longer written"
    );
    assert!(payload.contains_key("reaction_log"));

    // Rewrite it as the retired shape: the stamped log gone, the stamp-less
    // `(actor, emoji, op)` field in its place.
    payload.remove("reaction_log");
    let legacy: BTreeMap<String, Vec<(ActorId, String, ReactionOp)>> = BTreeMap::from([(
        target.0.clone(),
        vec![(ActorId([2u8; 32]), "🙏".to_string(), ReactionOp::Add)],
    )]);
    payload.insert(
        "reaction_events".into(),
        fauna_cbor::decode_strict(&fauna_cbor::encode_canonical(&legacy).expect("encode"))
            .expect("as a value"),
    );
    let slice =
        ChannelHistorySlice::from_bytes(&fauna_cbor::encode_canonical(&payload).expect("encode"))
            .expect("an unknown key does not fail the decode");
    assert!(slice.reaction_log.is_empty());

    let (two, _) = manager_with_fauna_thread();
    let t_two = two.restore_channel_slice(&slice);
    assert!(
        snap_msg(&two, &t_two, &target).reactions.is_empty(),
        "the retired stamp-less field must not re-seed any reaction"
    );
}

/// The affordance is the snapshot's, never an app's role branch: each message
/// says whether THIS viewer may delete it — its own always; another member's
/// only when the viewer governs a governed room of a class that carries the
/// act (end-to-end, or community).
#[test]
fn can_delete_marks_own_messages_and_every_message_for_a_governing_viewer() {
    use fauna_conversations::{
        HistoryPolicy, JoinRule, PrincipalKind, RoomClass, RoomMemberSnapshot, RoomPolicySnapshot,
        RoomRole, RoomSnapshot,
    };
    fn room(class: RoomClass, my_role: RoomRole, governed: bool) -> RoomSnapshot {
        RoomSnapshot {
            class,
            members: vec![
                RoomMemberSnapshot {
                    kind: PrincipalKind::User,
                    role: Some(my_role),
                },
                RoomMemberSnapshot {
                    kind: PrincipalKind::User,
                    role: Some(RoomRole::Member),
                },
            ],
            policy: governed.then_some(RoomPolicySnapshot {
                version: 1,
                name: None,
                join_rule: JoinRule::Invite,
                history_policy: HistoryPolicy::None,
            }),
            my_role: Some(my_role),
            nest_read: None,
            labelers: None,
            awaiting_key: false,
            moderation_unverified: false,
            pending_invites: None,
        }
    }
    let m = ConversationsManager::new();
    let mock = Arc::new(MockRailBackend::new(Rail::FaunaMls));
    mock.set_self_address(TypedAddress::Fauna {
        actor_id: ActorId([1u8; 32]),
        handle: "me@nest".into(),
    });
    m.register_backend(mock.clone());
    let t = m.create_mls_group(vec![TypedAddress::Fauna {
        actor_id: ActorId([2u8; 32]),
        handle: "peer@nest".into(),
    }]);
    let peers = MessageId("conv:testch:1".into());
    m.ingest_inbound_to_thread(
        t.clone(),
        fauna_inbound_actor(ActorId([2u8; 32]), "theirs", peers.clone()),
    )
    .unwrap();

    // No room at all (a policy-less group): sender-only.
    assert!(!snap_msg(&m, &t, &peers).can_delete);
    for (class, role, governed, expected) in [
        (RoomClass::EndToEnd, RoomRole::Member, true, false),
        (RoomClass::EndToEnd, RoomRole::Admin, true, true),
        (RoomClass::EndToEnd, RoomRole::Owner, true, true),
        // A role with no policy behind it governs nothing.
        (RoomClass::EndToEnd, RoomRole::Owner, false, false),
        // The community class carries the act as a floor delete record, so
        // the same affordance holds there — and for nobody it does not hold
        // for in an end-to-end room.
        (RoomClass::Community, RoomRole::Owner, true, true),
        (RoomClass::Community, RoomRole::Admin, true, true),
        (RoomClass::Community, RoomRole::Member, true, false),
        (RoomClass::Community, RoomRole::Owner, false, false),
        // A transport-only room has no such act.
        (RoomClass::TransportOnly, RoomRole::Owner, true, false),
    ] {
        mock.set_room(t.clone(), room(class, role, governed));
        assert_eq!(
            snap_msg(&m, &t, &peers).can_delete,
            expected,
            "another member's message, viewer {role:?} in a {class:?} room (governed: {governed})"
        );
    }
}

#[test]
fn reaction_before_its_target_is_buffered_then_applied() {
    // B4 out-of-order: a reaction arrives BEFORE its target message.
    // The reaction log holds it; once the target is injected, thread_detail
    // folds the pill onto the now-present message.
    let (m, t) = manager_with_fauna_thread();
    // Use a MessageId that matches "conv:ch:7" style — for the test we
    // use a direct manager call so the MessageId is whatever we choose.
    let mid = MessageId("conv:testch:7".into());

    // Reaction arrives before its target — target not in thread yet.
    m.apply_inbound_reaction(
        mid.clone(),
        ActorId([2u8; 32]),
        "👍".into(),
        ReactionOp::Add,
        1_000,
    );

    // Target message has not arrived — thread has no such message, so
    // thread_detail returns None for snap_msg (message absent = no pill visible yet).
    // Don't assert on the absent message, just confirm no panic.
    let detail = m.thread_detail(t.clone()).unwrap();
    assert!(
        detail.messages.iter().all(|x| x.message_id != mid),
        "target message not yet in thread"
    );

    // Now inject the target message with that exact id.
    m.ingest_inbound_to_thread(
        t.clone(),
        fauna_inbound_actor(ActorId([2u8; 32]), "late target", mid.clone()),
    )
    .unwrap();

    // After arrival, thread_detail should fold the buffered reaction.
    assert_eq!(
        snap_msg(&m, &t, &mid).reactions,
        vec![ReactionGroup {
            emoji: "👍".into(),
            count: 1,
            reacted_by_me: false,
        }],
        "buffered reaction applied once target message arrives"
    );
}

// ── D2b — in-bubble reply-quote (RenderBlock::QuotedMessage) ──────────────────

/// Return the `(author_display, snippet)` of the first `QuotedMessage` block in
/// a rendered message document, or `None` if it carries no reply-quote.
fn quoted_message_block(doc: &fauna_core::render::RenderDocument) -> Option<(String, String)> {
    doc.blocks.iter().find_map(|b| match b {
        fauna_core::render::RenderBlock::QuotedMessage {
            author_display,
            snippet,
        } => Some((author_display.clone(), snippet.clone())),
        _ => None,
    })
}

#[test]
fn reply_renders_quoted_message_block_of_parent() {
    // A message that replies to a loaded parent renders an in-bubble reply-quote
    // (render-model.md § D2 QuotedMessage): the manager folds a
    // `RenderBlock::QuotedMessage{author_display, snippet}` into the reply's
    // document at read time (`thread_detail`), resolving the parent from the same
    // thread. The quote is prepended (first block) so the in-order client walkers
    // paint it above the body.
    let m = ConversationsManager::new();
    m.register_backend(Arc::new(MockRailBackend::new(Rail::Smtp)));
    // Parent → stored message_id "msg-original" (smtp_inbound keys id on body).
    m.ingest_inbound(smtp_inbound(
        "alice@host.test",
        Some("Invoice 42"),
        "original about the fountain",
        None,
    ))
    .unwrap();
    // Reply references the parent's id → ByMessageReference keying merges them.
    m.ingest_inbound(smtp_inbound(
        "bob@host.test",
        Some("Re: Invoice 42"),
        "sounds good",
        Some(MessageId("msg-original about the fountain".into())),
    ))
    .unwrap();

    let snap = m.snapshot();
    let detail = m.thread_detail(snap.threads[0].thread_id.clone()).unwrap();
    assert_eq!(detail.messages.len(), 2, "parent + reply in one thread");

    // The parent (a non-reply) carries no quote.
    assert!(
        quoted_message_block(&detail.messages[0].document).is_none(),
        "the parent message must not show a reply-quote",
    );
    // The reply carries a QuotedMessage of the parent — prepended as the FIRST
    // block so the in-order walkers render it above the reply body.
    assert!(
        matches!(
            detail.messages[1].document.blocks.first(),
            Some(fauna_core::render::RenderBlock::QuotedMessage { .. })
        ),
        "the reply-quote must be the first block (rendered above the body)",
    );
    let (author, snippet) =
        quoted_message_block(&detail.messages[1].document).expect("reply shows a quote");
    assert_eq!(
        author, "alice@host.test",
        "quote author is the parent sender's display",
    );
    assert!(
        snippet.contains("fountain"),
        "quote snippet {snippet:?} must carry the parent body text",
    );
}

#[test]
fn non_reply_has_no_quoted_message_block() {
    let m = ConversationsManager::new();
    m.register_backend(Arc::new(MockRailBackend::new(Rail::Smtp)));
    m.ingest_inbound(smtp_inbound(
        "alice@host.test",
        Some("hi"),
        "no reply here",
        None,
    ))
    .unwrap();
    let snap = m.snapshot();
    let detail = m.thread_detail(snap.threads[0].thread_id.clone()).unwrap();
    assert!(
        quoted_message_block(&detail.messages[0].document).is_none(),
        "a standalone (non-reply) message must not show a reply-quote",
    );
}

#[test]
fn reply_with_unloaded_parent_hides_quote() {
    // A reply whose In-Reply-To points at a parent we never stored: we hold only
    // the bare id, no author/snippet, so the quote is HIDDEN (user-approved
    // 2026-06-23). It still merges into the subject thread (existing behaviour).
    let m = ConversationsManager::new();
    m.register_backend(Arc::new(MockRailBackend::new(Rail::Smtp)));
    m.ingest_inbound(smtp_inbound(
        "alice@host.test",
        Some("Invoice 42"),
        "original",
        None,
    ))
    .unwrap();
    m.ingest_inbound(smtp_inbound(
        "alice@host.test",
        Some("Re: Invoice 42"),
        "the reply",
        Some(MessageId("<unknown-elsewhere@other.host>".into())),
    ))
    .unwrap();
    let snap = m.snapshot();
    let detail = m.thread_detail(snap.threads[0].thread_id.clone()).unwrap();
    let reply = detail
        .messages
        .iter()
        .find(|msg| msg.message_id == MessageId("msg-the reply".into()))
        .expect("reply is in the thread");
    assert!(
        quoted_message_block(&reply.document).is_none(),
        "a reply whose parent is not loaded must hide the quote",
    );
}

/// **Notify-on-mint contract** (`devices.md` § Cross-device MLS group-state
/// sync): a key-package mint writes fresh private init keys into the engine's
/// provider storage only, so the manager must tick its `SnapshotObserver`s —
/// that is what schedules the debounced replica autosave that makes the keys
/// durable. No tick when the pool is already at target (nothing new to save);
/// the last-resort publish mints on every call, so it always ticks.
#[tokio::test]
async fn keypackage_mint_ticks_snapshot_observers() {
    use std::sync::atomic::{AtomicUsize, Ordering};

    struct CountingObserver(Arc<AtomicUsize>);
    impl SnapshotObserver for CountingObserver {
        fn on_changed(&self) {
            self.0.fetch_add(1, Ordering::SeqCst);
        }
    }

    let m = ConversationsManager::new();
    let mock = Arc::new(MockRailBackend::new(Rail::FaunaMls));
    m.register_backend(mock.clone());
    let ticks = Arc::new(AtomicUsize::new(0));
    m.add_observer(Arc::new(CountingObserver(Arc::clone(&ticks))));

    mock.set_mint_on_ensure(3);
    assert_eq!(m.ensure_keypackages(20).await.unwrap(), 3);
    assert_eq!(ticks.load(Ordering::SeqCst), 1, "a mint ticks the autosave");

    mock.set_mint_on_ensure(0);
    assert_eq!(m.ensure_keypackages(20).await.unwrap(), 0);
    assert_eq!(
        ticks.load(Ordering::SeqCst),
        1,
        "at-target replenish is quiet"
    );

    m.ensure_last_resort_keypackage().await.unwrap();
    assert_eq!(
        ticks.load(Ordering::SeqCst),
        2,
        "the last-resort publish mints every call, so it always ticks"
    );
}

/// **Observer-count diagnostic**: `observer_count` must
/// track exactly what `add_observer`/`clear_observers` do to the registered set. The
/// windows e2e state protocol surfaces it as `diagnostics.conversations_observer_count`
/// specifically so a client-side accumulation bug (registering a fresh observer on
/// every page re-navigation instead of reusing one for the manager's lifetime) is
/// assertable as a headless count instead of only a downstream retention/dispatch-storm
/// symptom.
#[test]
fn observer_count_tracks_add_and_clear() {
    struct NoopObserver;
    impl SnapshotObserver for NoopObserver {
        fn on_changed(&self) {}
    }

    let m = ConversationsManager::new();
    assert_eq!(
        m.observer_count(),
        0,
        "a fresh manager registers no observers"
    );

    m.add_observer(Arc::new(NoopObserver));
    assert_eq!(m.observer_count(), 1);

    m.add_observer(Arc::new(NoopObserver));
    assert_eq!(
        m.observer_count(),
        2,
        "each add_observer call registers one more"
    );

    m.clear_observers();
    assert_eq!(
        m.observer_count(),
        0,
        "clear_observers drops every registration"
    );
}

// ── Cross-group eviction (the review surfaces' "Remove", `src/eviction.rs`) ──
//
// The three arms below are the three questions `identity-succession.md`
// § Propagation left to the implementer, and each is written so it can only pass
// for its own reason. Note in particular that the *rails* are chosen, not
// incidental: an SMTP thread cannot exercise a membership gate, because mail's
// `supports_membership_change` is false and its chips never reach the branch.

/// The **1:1 thread** here is the control, and it is as much the point of the
/// test as the two groups are. The gate under test is `flavor == MlsGroup`, and
/// a DM is the only fixture that reaches it: a `TypedAddress::Fauna` participant
/// appears on FaunaMls threads and nowhere else, so a mail/Bluesky/Nostr
/// "control" is excluded one branch earlier by having no actor id at all and
/// asserts nothing about this gate (that draft passed with the gate deleted).
/// Evicting from a DM would leave the owner alone in a thread that is not a
/// group, which is not what *Remove* means here.
#[tokio::test]
async fn evicting_a_person_clears_every_group_and_leaves_a_one_to_one_alone() {
    let m = ConversationsManager::new();
    m.install_mock_backends_for_test();
    let person = ActorId([9u8; 32]);
    let bystander = TypedAddress::Fauna {
        handle: "carol".into(),
        actor_id: ActorId([3u8; 32]),
    };
    let target = TypedAddress::Fauna {
        handle: "mallory".into(),
        actor_id: person,
    };

    let g1 = m.create_mls_group(vec![target.clone(), bystander.clone()]);
    let g2 = m.create_mls_group(vec![target.clone()]);
    // The same human, in a 1:1 rather than a group.
    let dm = m.materialize_conv_thread("dm-channel".into(), vec![target.clone()]);

    let outcome = m.evict_person_everywhere(&person).await;

    assert!(
        outcome.is_complete(),
        "no removal failed, so the eviction is complete; got {outcome:?}"
    );
    assert_eq!(
        outcome.evicted.len(),
        2,
        "exactly the two groups; the 1:1 must be skipped, neither evicted nor \
         failed. got {outcome:?}"
    );
    for g in [&g1, &g2] {
        let detail = m.thread_detail(g.clone()).expect("group still exists");
        assert!(
            !detail.participants.iter().any(|p| matches!(
                p,
                TypedAddress::Fauna { actor_id, .. } if *actor_id == person
            )),
            "the flagged person must be gone from {g:?}"
        );
    }
    assert!(
        m.thread_detail(g1)
            .expect("group still exists")
            .participants
            .iter()
            .any(|p| p.display() == bystander.display()),
        "evicting one person must not disturb anyone else in the group"
    );
    assert!(
        m.thread_detail(dm)
            .expect("thread exists")
            .participants
            .iter()
            .any(|p| matches!(p, TypedAddress::Fauna { actor_id, .. } if *actor_id == person)),
        "a 1:1 thread is not a group — the flagged person must still be in it"
    );
    assert_eq!(
        outcome.earned_verdict(),
        Some(UnattestedVerdict::Removed),
        "a complete eviction is what earns the Removed verdict"
    );
}

/// The permanent review view holds a postponed backlog, so a flagged person may
/// have left every group before the owner gets to them. That *Remove* evicts
/// nothing and must still close the item — refusing the verdict would strand it
/// forever.
#[tokio::test]
async fn an_eviction_that_frees_nobody_still_earns_the_removed_verdict() {
    let m = ConversationsManager::new();
    m.install_mock_backends_for_test();
    let person = ActorId([9u8; 32]);
    m.create_mls_group(vec![TypedAddress::Fauna {
        handle: "carol".into(),
        actor_id: ActorId([3u8; 32]),
    }]);

    let outcome = m.evict_person_everywhere(&person).await;

    assert!(outcome.evicted.is_empty() && outcome.failed.is_empty());
    assert_eq!(
        outcome.earned_verdict(),
        Some(UnattestedVerdict::Removed),
        "'removed from every group they were in' is vacuously true of zero groups, \
         and the owner's judgment about the person is just as real"
    );
}

/// The arm that decides whether the surface is honest. A partial eviction must
/// NOT earn a verdict: `Removed` means the person *was* removed, an adjudicated
/// item stops being rendered, and the person is still seated in the group that
/// failed. It must also leave the successful removal durable, so the retry
/// re-derives a roster holding only the remainder.
#[tokio::test]
async fn a_partial_eviction_earns_no_verdict_and_a_retry_targets_only_the_remainder() {
    let m = ConversationsManager::new();
    let backend = Arc::new(MockRailBackend::new(Rail::FaunaMls));
    m.register_backend(backend.clone());
    let person = ActorId([9u8; 32]);
    let target = TypedAddress::Fauna {
        handle: "mallory".into(),
        actor_id: person,
    };
    let ok_group = m.create_mls_group(vec![target.clone()]);
    let stuck_group = m.create_mls_group(vec![
        target.clone(),
        TypedAddress::Fauna {
            handle: "carol".into(),
            actor_id: ActorId([3u8; 32]),
        },
    ]);
    backend.fail_remove_on(stuck_group.clone(), "the nest is unreachable");

    let outcome = m.evict_person_everywhere(&person).await;

    assert_eq!(outcome.evicted, vec![ok_group.clone()], "got {outcome:?}");
    assert_eq!(outcome.failed.len(), 1, "got {outcome:?}");
    assert_eq!(outcome.failed[0].thread, stuck_group);
    assert!(
        !outcome.is_complete(),
        "3-of-5 is the ordinary outcome under a flaky link, not a success"
    );
    assert_eq!(
        outcome.earned_verdict(),
        None,
        "no verdict may be written while the person is still seated somewhere — \
         recording Removed here is what would make the surface go quiet on a live problem"
    );
    // The rollback put them back in the group that refused, and only there.
    assert!(
        m.thread_detail(stuck_group.clone())
            .expect("group exists")
            .participants
            .iter()
            .any(|p| matches!(p, TypedAddress::Fauna { actor_id, .. } if *actor_id == person)),
        "a failed removal must render membership truthfully — they ARE still in the group"
    );

    // The retry: the same call, with no cursor and no stored group list.
    backend.clear_remove_failures();
    let retry = m.evict_person_everywhere(&person).await;
    assert_eq!(
        retry.evicted,
        vec![stuck_group],
        "the retry re-derives live membership, so it targets ONLY the group that failed — \
         the one already evicted is not revisited and not re-asked about"
    );
    assert_eq!(
        retry.earned_verdict(),
        Some(UnattestedVerdict::Removed),
        "and now the eviction is complete, so the verdict is earned"
    );
}

/// **The roster source is the security property, not a detail of it**. The flag these surfaces render is raised off the MLS
/// **engine** roster (`SweepReport::unattested_members` → `group_members`), while
/// `ThreadDetail::participants` is a local view written at join and by the
/// owner's own gestures — `apply_inbound_commit` advances the engine and never
/// reconciles it. So a **foreign-authored** membership Commit seats someone in
/// the engine and not in the snapshot, and that is the adversarial case, not a
/// corner: a thief's planted second identity (§ Propagation, Residual 1) is
/// added by the *thief's* commit, which is precisely the membership least likely
/// to be in the local snapshot.
///
/// Deciding membership from the snapshot there does not fail loudly — it
/// `continue`s, records no failure, and so reports a **complete** eviction that
/// earns `Removed` while the person is still seated. That is the same
/// "surface goes quiet on a live problem" harm as a partial eviction writing a
/// verdict, arriving through the enumeration door instead, which is why this pin
/// asserts the attempt and not merely the verdict.
#[tokio::test]
async fn a_member_the_engine_seats_and_the_snapshot_omits_is_evicted_not_silently_skipped() {
    let m = ConversationsManager::new();
    let backend = Arc::new(MockRailBackend::new(Rail::FaunaMls));
    m.register_backend(backend.clone());
    let person = ActorId([9u8; 32]);
    let bystander = ActorId([3u8; 32]);

    // The snapshot carries only the bystander — what a foreign-authored Commit
    // that seated the flagged person leaves behind on this device.
    let group = m.create_mls_group(vec![TypedAddress::Fauna {
        handle: "carol".into(),
        actor_id: bystander,
    }]);
    // ...while the roster the flag was raised off seats them.
    backend.set_engine_roster(group.clone(), vec![bystander, person]);

    let outcome = m.evict_person_everywhere(&person).await;

    assert_eq!(
        outcome.evicted,
        vec![group],
        "the authoritative roster seats the flagged person in this group, so the \
         eviction must ATTEMPT it; reading membership off the snapshot skips it \
         with no failure recorded. got {outcome:?}"
    );
    assert_eq!(
        outcome.earned_verdict(),
        Some(UnattestedVerdict::Removed),
        "and the verdict is earned because the group was really attempted — not \
         because it was never looked at"
    );
}

/// The converse divergence, and it is load-bearing rather than symmetry for its
/// own sake: when the authoritative roster says the person has **left** a group
/// the snapshot still lists them in, the group must be *skipped*, not attempted.
/// Attempting it fails at the real backend (`find_leaf_by_identity` → "is not a
/// member of this group"), which records a failure, which blocks the verdict —
/// permanently, since a stale chip does not heal itself. That would strand an
/// item nobody could ever close, which § Propagation rule (4) refuses.
#[tokio::test]
async fn a_group_the_authoritative_roster_says_they_left_is_skipped_not_failed_forever() {
    let m = ConversationsManager::new();
    let backend = Arc::new(MockRailBackend::new(Rail::FaunaMls));
    m.register_backend(backend.clone());
    let person = ActorId([9u8; 32]);
    let bystander = ActorId([3u8; 32]);

    let group = m.create_mls_group(vec![
        TypedAddress::Fauna {
            handle: "mallory".into(),
            actor_id: person,
        },
        TypedAddress::Fauna {
            handle: "carol".into(),
            actor_id: bystander,
        },
    ]);
    // The engine holds the group and the flagged person is not in it.
    backend.set_engine_roster(group.clone(), vec![bystander]);

    let outcome = m.evict_person_everywhere(&person).await;

    assert!(
        outcome.evicted.is_empty() && outcome.failed.is_empty(),
        "a group the authority says they are not in is neither evicted nor \
         failed — it is not one of their groups. got {outcome:?}"
    );
    assert_eq!(
        outcome.earned_verdict(),
        Some(UnattestedVerdict::Removed),
        "so the item can still close: the person is in none of the owner's groups"
    );
}

/// **A handle is not an identity — including on the way out.** `is_person`
/// selects the participant by actor id, and then the store used to drop them by
/// `display()`, which for a Fauna address is the **handle**. Fauna rosters carry
/// empty handles by construction (`ingest_welcome` builds them from
/// `group_members`, which has none to give) and attacker-chosen handles by this
/// threat model, so an identity-precise selection was executed as a
/// handle-shaped delete: evicting one flagged person also dropped every
/// co-member wearing the same handle from the local view — hiding an honest
/// same-handle member, or hiding the thief's *second* identity behind a
/// collided handle at the exact moment the owner is trying to see it.
///
/// The two participants here differ **only** in actor id, which is the whole
/// construction: the three pre-existing eviction pins all give their bystander a
/// distinct handle, so none of them can fail for this reason.
#[tokio::test]
async fn evicting_one_person_leaves_a_co_member_who_shares_their_handle() {
    let m = ConversationsManager::new();
    m.install_mock_backends_for_test();
    let person = ActorId([9u8; 32]);
    let twin = ActorId([4u8; 32]);

    let group = m.create_mls_group(vec![
        TypedAddress::Fauna {
            handle: "mallory".into(),
            actor_id: person,
        },
        // Same handle, different identity.
        TypedAddress::Fauna {
            handle: "mallory".into(),
            actor_id: twin,
        },
    ]);

    let outcome = m.evict_person_everywhere(&person).await;
    assert_eq!(outcome.evicted, vec![group.clone()], "got {outcome:?}");

    let detail = m.thread_detail(group).expect("group still exists");
    assert!(
        !detail
            .participants
            .iter()
            .any(|p| matches!(p, TypedAddress::Fauna { actor_id, .. } if *actor_id == person)),
        "the flagged identity is gone"
    );
    assert!(
        detail
            .participants
            .iter()
            .any(|p| matches!(p, TypedAddress::Fauna { actor_id, .. } if *actor_id == twin)),
        "and the co-member who merely shares their handle is still seated — \
         removing by handle drops both, which is how the review surface would \
         hide the very identity it exists to show"
    );
    assert_eq!(
        detail.participant_displays.len(),
        detail.participants.len(),
        "the display list stays index-parallel with the participant list"
    );
}

// ── Rule (5): what "every group of mine" spans across channel classes ──
//
// `identity-succession.md` § Propagation → *Removing a flagged member* (5),
// ratified 2026-08-10. The raise's span is every engine group; each class below
// pins what Remove does about a seat there, and each is written so exactly one
// wrong dispatch arm reds it.

/// A seat in a **folder channel** blocks the verdict as a typed fact. Its
/// removal is the folder plane's (the set owner's key-rotating remove, or the
/// member leaving the set) — but `Removed` earned while the seat stands would
/// be the surface going quiet on live shared-file access, the worst class to go
/// quiet on. Dropping the seat (treating folder like scheduling) reds the
/// verdict assertion; dropping the class reds the match.
#[tokio::test]
async fn a_folder_seat_blocks_the_verdict_as_a_typed_fact() {
    let m = ConversationsManager::new();
    let backend = Arc::new(MockRailBackend::new(Rail::FaunaMls));
    m.register_backend(backend.clone());
    let person = ActorId([9u8; 32]);

    backend.set_unbound_seats(
        person,
        vec![fauna_conversations::backend::UnboundSeat {
            channel_hex: "aa11".into(),
            class: fauna_conversations::backend::UnboundChannelClass::Folder,
        }],
    );

    let outcome = m.evict_person_everywhere(&person).await;

    assert_eq!(
        outcome.unreachable,
        vec![fauna_conversations::eviction::UnreachableSeat {
            channel_hex: "aa11".into(),
            class: fauna_conversations::eviction::UnreachableSeatClass::FolderChannel,
        }],
        "the seat is reported, typed, with the channel named; got {outcome:?}"
    );
    assert_eq!(
        outcome.earned_verdict(),
        None,
        "a raised seat still standing may not earn Removed — rule (3)'s harm \
         through the class door"
    );
}

/// A seat in a **scheduling channel** never blocks: no Commit is ever applied
/// to one (`poll_inbound_scheduling` skips them), so nothing can be planted
/// there in the owner's own copy and nothing needs removing — the honest
/// one-off peer must not strand the item. A dispatch that blocks on scheduling
/// reds this; one that blocks on nothing reds the folder pin above.
#[tokio::test]
async fn a_scheduling_seat_never_blocks_and_the_vacuous_verdict_stands() {
    let m = ConversationsManager::new();
    let backend = Arc::new(MockRailBackend::new(Rail::FaunaMls));
    m.register_backend(backend.clone());
    let person = ActorId([9u8; 32]);

    backend.set_unbound_seats(
        person,
        vec![fauna_conversations::backend::UnboundSeat {
            channel_hex: "bb22".into(),
            class: fauna_conversations::backend::UnboundChannelClass::Scheduling,
        }],
    );

    let outcome = m.evict_person_everywhere(&person).await;

    assert!(
        outcome.unreachable.is_empty(),
        "a scheduling one-off is not a group of mine — nothing to remove, \
         nothing to block on; got {outcome:?}"
    );
    assert_eq!(
        outcome.earned_verdict(),
        Some(UnattestedVerdict::Removed),
        "so a person seated only in a scheduling channel still closes vacuously"
    );
}

/// A seat in a **chat group whose thread is not on this device** blocks, typed
/// as such: the group is one of mine (the durable chat marker says so), the
/// removal is thread-keyed, and there is no thread — the closure is the restore
/// completing here, or Remove running from a device that holds the thread.
/// Treating the class as skippable earns a false `Removed`; this pin is what
/// reds it.
#[tokio::test]
async fn a_chat_group_with_no_thread_on_this_device_blocks_not_closes() {
    let m = ConversationsManager::new();
    let backend = Arc::new(MockRailBackend::new(Rail::FaunaMls));
    m.register_backend(backend.clone());
    let person = ActorId([9u8; 32]);

    backend.set_unbound_seats(
        person,
        vec![fauna_conversations::backend::UnboundSeat {
            channel_hex: "cc33".into(),
            class: fauna_conversations::backend::UnboundChannelClass::Chat,
        }],
    );

    let outcome = m.evict_person_everywhere(&person).await;

    assert_eq!(
        outcome.unreachable,
        vec![fauna_conversations::eviction::UnreachableSeat {
            channel_hex: "cc33".into(),
            class: fauna_conversations::eviction::UnreachableSeatClass::ChatGroupNoThreadHere,
        }],
        "got {outcome:?}"
    );
    assert_eq!(outcome.earned_verdict(), None);
}

/// **An engine seat a 1:1 never promised is evicted like a group seat.** The
/// chat poll applies membership Commits with no flavor gate, so a thief's
/// Commit can seat a third identity in a DM channel — engine-visible, snapshot-
/// invisible. Rule (2) protects the *honest peer* (the snapshot-listed
/// participant), not the flavor: its rationale — don't strand the owner alone
/// in a thread the delete already handles — says nothing about a third seat.
/// Restoring the old unconditional `flavor != MlsGroup` skip reds exactly this.
#[tokio::test]
async fn an_extra_seat_on_a_one_to_one_is_evicted_like_any_group_seat() {
    let m = ConversationsManager::new();
    let backend = Arc::new(MockRailBackend::new(Rail::FaunaMls));
    m.register_backend(backend.clone());
    let person = ActorId([9u8; 32]);
    let honest_peer = ActorId([3u8; 32]);

    let dm = m.materialize_conv_thread(
        "dm-channel".into(),
        vec![TypedAddress::Fauna {
            handle: "carol".into(),
            actor_id: honest_peer,
        }],
    );
    // The engine seats three where the 1:1 promised two — the planted seat is
    // exactly the membership least likely to be in the local snapshot.
    backend.set_engine_roster(dm.clone(), vec![honest_peer, person]);

    let outcome = m.evict_person_everywhere(&person).await;

    assert_eq!(
        outcome.evicted,
        vec![dm.clone()],
        "the planted third seat is evicted; got {outcome:?}"
    );
    assert!(
        m.thread_detail(dm)
            .expect("thread exists")
            .participants
            .iter()
            .any(|p| matches!(p, TypedAddress::Fauna { actor_id, .. } if *actor_id == honest_peer)),
        "and the honest peer is untouched"
    );
    assert_eq!(outcome.earned_verdict(), Some(UnattestedVerdict::Removed));
}

/// The rule-(2) control for the arm above, sharpened: the flagged person IS the
/// snapshot-listed peer of the 1:1, **and the authoritative roster seats them**
/// — so an extra-seat arm keyed on the roster alone (dropping the
/// snapshot-listed guard) would evict the honest peer of every DM with the
/// flagged person, minting the second removal mechanism rule (2) refuses. The
/// original 1:1 control (`evicting_a_person_clears_every_group_and_leaves_a_one_to_one_alone`)
/// cannot catch that mutation: its mock claims no authoritative roster for the
/// DM, so the arm never fires there.
#[tokio::test]
async fn the_honest_peer_of_a_one_to_one_is_never_removes_business() {
    let m = ConversationsManager::new();
    let backend = Arc::new(MockRailBackend::new(Rail::FaunaMls));
    m.register_backend(backend.clone());
    let person = ActorId([9u8; 32]);

    let dm = m.materialize_conv_thread(
        "dm-channel".into(),
        vec![TypedAddress::Fauna {
            handle: "mallory".into(),
            actor_id: person,
        }],
    );
    backend.set_engine_roster(dm.clone(), vec![person]);

    let outcome = m.evict_person_everywhere(&person).await;

    assert!(
        outcome.evicted.is_empty() && outcome.failed.is_empty(),
        "the honest peer of a 1:1 is rule (2)'s business, not Remove's; \
         got {outcome:?}"
    );
    assert!(
        m.thread_detail(dm)
            .expect("thread exists")
            .participants
            .iter()
            .any(|p| matches!(p, TypedAddress::Fauna { actor_id, .. } if *actor_id == person)),
        "they are still seated in the DM"
    );
    assert_eq!(
        outcome.earned_verdict(),
        Some(UnattestedVerdict::Removed),
        "and the item still closes — vacuously, over zero groups"
    );
}

// ── Display resolution for a person rendered outside any one group ──

/// The bystander is listed **first and in the same thread** as the target, which
/// is the whole construction: the lookup walks participants in order, so a
/// version that dropped the actor-id match would return `carol` here whatever
/// the thread iteration order happens to be. Asserting only "is Some" — or
/// putting the two in separate threads — would let that mutation live.
#[tokio::test]
async fn a_person_outside_any_group_is_named_by_actor_id_not_by_position() {
    let m = ConversationsManager::new();
    let mock = Arc::new(MockRailBackend::new(Rail::FaunaMls));
    m.register_backend(mock.clone());
    let person = ActorId([9u8; 32]);
    let bystander = TypedAddress::Fauna {
        handle: "carol".into(),
        actor_id: ActorId([3u8; 32]),
    };
    let target = TypedAddress::Fauna {
        handle: "mallory".into(),
        actor_id: person,
    };

    let group = m.create_mls_group(vec![bystander, target]);
    // Both seats proven by the group's roster: the lookup is gated on proof
    // (`contacts.md` § The private overlay → *The paint gate*), and this pin
    // is about which proven row it names.
    mock.set_engine_roster(group, vec![ActorId([3u8; 32]), person]);

    assert_eq!(
        m.handle_for_person(&person).as_deref(),
        Some("mallory"),
        "the row must name the person the review is about, not whoever the roster lists first"
    );
}

/// The permanent view's ordinary case, and the reason `None` is an answer rather
/// than a failure: it holds a backlog someone postponed, and a flagged person
/// may have left every group since. The row still has to render — an item nobody
/// can name is an item nobody can close.
#[tokio::test]
async fn a_person_in_no_group_of_ours_has_no_handle_to_show() {
    let m = ConversationsManager::new();
    let mock = Arc::new(MockRailBackend::new(Rail::FaunaMls));
    m.register_backend(mock.clone());
    let person = ActorId([9u8; 32]);
    let target = TypedAddress::Fauna {
        handle: "mallory".into(),
        actor_id: person,
    };
    let group = m.create_mls_group(vec![target.clone()]);
    mock.set_engine_roster(group, vec![person]);
    assert!(
        m.handle_for_person(&person).is_some(),
        "seated to begin with"
    );

    m.evict_person_everywhere(&person).await;

    assert_eq!(
        m.handle_for_person(&person),
        None,
        "once they are in none of our groups there is no live handle to read"
    );
}

/// **A member seated with no handle has no name to show — the same `None` as
/// not being seated at all.** This is the *ordinary* roster for any group the
/// owner joined rather than created: `ingest_welcome` builds participants from
/// `MlsEngine::group_members`, and a leaf credential carries no handle, so every
/// row rests with `handle: String::new()`. Answering `Some("")` renders a review
/// row with a blank where the person goes, because it walks past the no-name
/// fallback each surface already has (tui's `REVIEW_UNKNOWN_PERSON`).
#[tokio::test]
async fn a_member_seated_without_a_handle_has_no_name_to_show() {
    let m = ConversationsManager::new();
    let person = ActorId([9u8; 32]);

    // Exactly what `backends::fauna_mls::ingest_welcome` writes on the joiner.
    m.materialize_conv_thread(
        "c0ffee".to_string(),
        vec![TypedAddress::Fauna {
            handle: String::new(),
            actor_id: person,
        }],
    );

    assert_eq!(
        m.handle_for_person(&person),
        None,
        "a Welcome-joined roster names nobody, and an empty string is not a name"
    );
}

/// And the scan must not STOP at the nameless row. One person is commonly seated
/// in several threads — a group they were welcomed into and a thread whose
/// address the owner resolved — so a scan that takes the first *thread* holding
/// them, then judges only that answer, reports no name while a perfectly good
/// handle sits one thread over.
///
/// ⚠ **The fixture has to force the nameless thread to be scanned FIRST, and
/// this test asserts that it did.** `list_summaries` orders by descending last
/// activity, and two message-less threads both sit at `0` — a tie broken by
/// `HashMap` iteration order, i.e. by nothing. Built that way this pin passes
/// under the wrong implementation about half the time and pins the fixture
/// rather than the property (it did, on the first draft: filtering the *result*
/// of the scan instead of the scan itself left the whole suite green). Giving
/// the nameless thread the later message makes its position a fact rather than
/// a coin toss — and is also the case a real user hits, since the group you were
/// welcomed into is usually the busier one.
///
/// That the ordering matters at all is the second finding here: judged on the
/// result, whether the owner sees a name for this person would depend on which
/// thread happened to sort first.
#[tokio::test]
async fn a_nameless_seat_does_not_shadow_a_named_one_in_another_thread() {
    let m = ConversationsManager::new();
    let mock = Arc::new(MockRailBackend::new(Rail::FaunaMls));
    m.register_backend(mock.clone());
    let person = ActorId([9u8; 32]);

    let named = m.materialize_conv_thread(
        "decaf0".to_string(),
        vec![TypedAddress::Fauna {
            handle: "mallory@example.test".into(),
            actor_id: person,
        }],
    );
    let nameless = m.materialize_conv_thread(
        "c0ffee".to_string(),
        vec![TypedAddress::Fauna {
            handle: String::new(),
            actor_id: person,
        }],
    );
    // Both threads' groups have proven the person (the paint gate); what is
    // under test is the scan, not the proof.
    mock.set_engine_roster(named.clone(), vec![person]);
    mock.set_engine_roster(nameless.clone(), vec![person]);

    // The later message is what puts the nameless thread at the top of the scan.
    // Sent by a third party so nothing here touches the person's own rows.
    let mut chatter = fauna_inbound_actor(
        ActorId([7u8; 32]),
        "the welcomed group is the busy one",
        MessageId("m-shadow-1".into()),
    );
    chatter.timestamp_ms = 9_000;
    m.ingest_inbound_to_thread(nameless.clone(), chatter)
        .unwrap();

    // Fixture check, not decoration: without this the assertion below can pass
    // against an implementation that never skips anything.
    let scan_order: Vec<_> = m
        .snapshot()
        .threads
        .into_iter()
        .map(|s| s.thread_id)
        .collect();
    assert_eq!(
        scan_order.first(),
        Some(&nameless),
        "the nameless thread must be scanned first or this pin proves nothing; \
         order was {scan_order:?} (named={named:?})"
    );

    assert_eq!(
        m.handle_for_person(&person).as_deref(),
        Some("mallory@example.test"),
        "a nameless Welcome seat must be skipped, not treated as the answer"
    );
}

/// **The conversations-engine-role refusal flag** (`account-data-plane.md` §
/// Multi-instance concurrency, W5.6 (account-data-plane.md § Workstreams)): defaults false, flips + ticks observers
/// on a real change, and is a no-op (no tick) when set to its current value —
/// the same optimization `clear_page_error` uses for `page_error`. Each app's
/// engine-construction site sets it directly (never through a producer's
/// `clear_page_error`), which is the whole reason it is a field of its own
/// rather than folded into `page_error`.
#[test]
fn engine_served_elsewhere_flips_and_ticks_only_on_change() {
    use std::sync::atomic::{AtomicUsize, Ordering};

    struct CountingObserver(Arc<AtomicUsize>);
    impl SnapshotObserver for CountingObserver {
        fn on_changed(&self) {
            self.0.fetch_add(1, Ordering::SeqCst);
        }
    }

    let m = ConversationsManager::new();
    let ticks = Arc::new(AtomicUsize::new(0));
    m.add_observer(Arc::new(CountingObserver(Arc::clone(&ticks))));

    assert!(
        !m.engine_served_elsewhere(),
        "must default false — most processes hold the role"
    );

    m.set_engine_served_elsewhere(true);
    assert!(m.engine_served_elsewhere());
    assert_eq!(ticks.load(Ordering::SeqCst), 1, "a real flip must tick");

    m.set_engine_served_elsewhere(true);
    assert_eq!(
        ticks.load(Ordering::SeqCst),
        1,
        "setting the same value again must not tick"
    );

    m.set_engine_served_elsewhere(false);
    assert!(!m.engine_served_elsewhere());
    assert_eq!(
        ticks.load(Ordering::SeqCst),
        2,
        "clearing it is a real flip too"
    );
}

// ── The attachment store's key is a CLAIM until it is verified ──
//
// `conversations.md` § Attachments states attachments are "content-addressed by
// `blob_hash` (lowercase-hex BLAKE3 of the plaintext bytes)". Nothing enforced it
// on receive: `blob_hash` is sender-authored, carried inside the sealed
// `ChannelAttachment`, and the store is ONE `HashMap` shared across every channel
// and room whose `insert` overwrites. So a co-member who knew a hash could
// replace anyone's bytes for it, in any conversation.
//
// These pin the door. The send-time arm — `resolve_attachments` re-reading
// `store.peek(&d.blob_hash)` at send, letting a staged outgoing attachment be
// swapped between staging and send — is closed by the SAME invariant rather than
// by its own check: that attack is an overwrite, and an overwrite that must hash
// to its own key is a BLAKE3 preimage. The second test drives exactly that.

#[test]
fn bytes_that_do_not_hash_to_their_key_never_enter_the_store() {
    let m = ConversationsManager::new();

    // An honest attachment, cached under its true hash.
    let honest = b"the real document bytes".to_vec();
    let hash = blake3::hash(&honest).to_hex().to_string();
    m.cache_attachment_bytes(hash.clone(), honest.clone());
    assert_eq!(
        m.attachment_bytes(hash.clone()).as_deref(),
        Some(honest.as_slice()),
        "a truthful hash/bytes pair caches normally"
    );

    // A hostile inbound attachment DECLARES that same hash and carries different
    // bytes — the render-poisoning arm.
    m.cache_attachment_bytes(hash.clone(), b"attacker's substituted bytes".to_vec());

    assert_eq!(
        m.attachment_bytes(hash.clone()).as_deref(),
        Some(honest.as_slice()),
        "a declared hash that is not BLAKE3 of its bytes must be refused, leaving \
         the honest bytes in place — every bubble in every conversation resolves \
         this one shared map"
    );

    // And it must not be able to CREATE an entry either, or the lie simply lands
    // on a key nobody had claimed yet.
    let unclaimed = blake3::hash(b"some other document").to_hex().to_string();
    m.cache_attachment_bytes(unclaimed.clone(), b"not those bytes".to_vec());
    assert_eq!(
        m.attachment_bytes(unclaimed),
        None,
        "an unverified pair must not populate a fresh key either"
    );
}

/// The sharper arm, driven end-to-end through `send` rather than inferred from
/// a direct store read. `send` re-resolves attachment bytes from the store by
/// hash via `resolve_attachments` (`conversations.md` § Attachments), so if
/// the store could be poisoned the victim would seal and sign the attacker's
/// bytes under their own filename. The precondition for that attack is an
/// overwrite of the staged entry.
///
/// This test previously asserted `m.attachment_bytes(att.blob_hash)` — a
/// direct store read — under a comment naming `send`, but never built a
/// compose or called it: `send` calls `resolve_attachments`, not
/// `attachment_bytes`. Measured (a security review finding): corrupting
/// `resolve_attachments` to return substituted bytes for every attachment
/// left this test, and 82+132 siblings across this crate's two integration
/// suites, green. Asserting on [`MockRailBackend::last_sent_attachments`] —
/// what the backend actually received — closes that gap; re-run the same
/// mutation against `resolve_attachments` to confirm this test now catches it.
#[tokio::test]
async fn a_staged_outgoing_attachment_cannot_be_substituted_before_send() {
    let m = ConversationsManager::new();
    let mock = Arc::new(MockRailBackend::new(Rail::FaunaMls));
    mock.set_self_address(TypedAddress::Fauna {
        actor_id: ActorId([1u8; 32]),
        handle: "me@nest".into(),
    });
    m.register_backend(mock.clone());
    let peer_addr = TypedAddress::Fauna {
        actor_id: ActorId([2u8; 32]),
        handle: "peer@nest".into(),
    };
    let t = m.create_mls_group(vec![peer_addr]);

    // The user stages a document they are forwarding — a publicly known one, so
    // its hash is predictable to an attacker, which is what makes this reachable.
    let staged = b"quarterly-report.pdf contents".to_vec();
    m.set_compose_body(t.clone(), "see attached".into());
    let blob_hash = m.add_attachment(
        t.clone(),
        "quarterly-report.pdf".into(),
        "application/pdf".into(),
        staged.clone(),
    );

    // An inbound attachment arrives declaring the staged hash with other bytes.
    m.cache_attachment_bytes(blob_hash.clone(), b"attacker's replacement".to_vec());

    m.send(t.clone()).await.expect("send ok");

    let sent = mock.last_sent_attachments();
    assert_eq!(sent.len(), 1, "the one staged attachment reached send");
    assert_eq!(sent[0].blob_hash, blob_hash);
    assert_eq!(
        sent[0].bytes, staged,
        "the bytes send actually put on the wire must still be the ones the \
         user staged"
    );
}

/// `conversations.md` § Persistence: a draft's attachments rest by content
/// address, but their bytes live only in the in-memory attachment store — so a
/// draft restored after a relaunch, or synced to the user's other device, can
/// list a file no store on that device holds. The send either puts it on the
/// wire or refuses naming it; it never goes out without it, and the Sent echo
/// lists exactly what the backend received.
///
/// Before this, `resolve_attachments` dropped the missing attachment by
/// `filter_map` and the echo was built from the compose: the backend received
/// no attachment while the sender's own bubble showed one.
#[tokio::test]
async fn a_restored_drafts_attachment_either_reaches_the_backend_or_refuses_the_send() {
    fn device() -> (Arc<ConversationsManager>, Arc<MockRailBackend>, ThreadId) {
        let m = ConversationsManager::new();
        let mock = Arc::new(MockRailBackend::new(Rail::FaunaMls));
        mock.set_self_address(TypedAddress::Fauna {
            actor_id: ActorId([1u8; 32]),
            handle: "me@nest".into(),
        });
        m.register_backend(mock.clone());
        let t = m.create_mls_group(vec![TypedAddress::Fauna {
            actor_id: ActorId([2u8; 32]),
            handle: "peer@nest".into(),
        }]);
        (m, mock, t)
    }
    fn echoed(m: &ConversationsManager, t: &ThreadId) -> usize {
        m.thread_detail(t.clone())
            .expect("thread exists")
            .messages
            .iter()
            .filter(|msg| msg.is_own)
            .map(|msg| fauna_conversations::message::attachment_blocks(&msg.document).len())
            .sum()
    }

    // Device A stages a file on its draft; the drafts blob rests and syncs.
    let (a, _, t) = device();
    let file = b"quarterly-report.pdf contents".to_vec();
    a.set_compose_body(t.clone(), "see attached".into());
    let blob_hash = a.add_attachment(
        t.clone(),
        "quarterly-report.pdf".into(),
        "application/pdf".into(),
        file.clone(),
    );
    let drafts = a.drafts_snapshot_bytes();

    // Device B restores that blob. Its store has never held the bytes.
    let (b, mock, t_b) = device();
    assert_eq!(t_b, t, "a fresh manager mints the same first thread id");
    b.restore_drafts(drafts).await;
    assert_eq!(
        b.thread_detail(t.clone())
            .unwrap()
            .compose
            .attachments
            .len(),
        1,
        "the restored draft lists the attachment"
    );
    assert_eq!(
        b.attachment_bytes(blob_hash.clone()),
        None,
        "whose bytes this device does not hold"
    );

    let err = b
        .send(t.clone())
        .await
        .expect_err("a send whose attachment has no bytes must refuse, never drop it");
    let BackendError::Refusal(sentence) = &err else {
        panic!("a product refusal the user can act on, got {err:?}");
    };
    assert!(
        sentence.contains("quarterly-report.pdf"),
        "the refusal names the file to attach again: {sentence}"
    );
    assert_eq!(mock.last_sent_attachments().len(), 0);
    assert_eq!(echoed(&b, &t), 0, "nothing went, and nothing says it did");
    let detail = b.thread_detail(t.clone()).unwrap();
    match &detail.compose.send_state {
        SendState::Failed { reason } => assert_eq!(
            reason.args.get("message"),
            Some(sentence),
            "the refusal reaches error-message through send_state"
        ),
        other => panic!("a refused send stamps Failed, got {other:?}"),
    }
    assert_eq!(
        detail.compose.attachments.len(),
        1,
        "the draft is kept, so the user can attach the file again and send"
    );

    // With the bytes present, the same draft sends, and the echo agrees with
    // the wire.
    b.cache_attachment_bytes(blob_hash.clone(), file.clone());
    b.send(t.clone())
        .await
        .expect("with its bytes present the draft sends");
    let sent = mock.last_sent_attachments();
    assert_eq!(sent.len(), 1);
    assert_eq!(sent[0].bytes, file);
    assert_eq!(
        echoed(&b, &t),
        sent.len(),
        "the Sent echo lists exactly what the backend received"
    );
}

// ── Room-restricted posts: the rooms the composer offers ─────────────────

/// `ui/feed.md` § Encryption at rest → *Room-restricted — the app half* → *The
/// rooms offered*: a room this device has been removed from — and has seen the
/// removal of — is not offered for a room-restricted post, while a live member's
/// rooms still are, of either member-keyed class. The offer is the shared
/// seam's, so this is the filter every app's `compose-gate-tier-select` inherits.
#[tokio::test]
async fn a_room_this_device_was_removed_from_is_not_offered_for_a_post() {
    use fauna_conversations::{
        PrincipalKind, RoomClass, RoomMemberSnapshot, RoomRole, RoomSnapshot,
    };
    fn room(class: RoomClass) -> RoomSnapshot {
        RoomSnapshot {
            class,
            members: vec![
                RoomMemberSnapshot {
                    kind: PrincipalKind::User,
                    role: Some(RoomRole::Owner),
                },
                RoomMemberSnapshot {
                    kind: PrincipalKind::User,
                    role: Some(RoomRole::Member),
                },
            ],
            policy: None,
            my_role: Some(RoomRole::Member),
            nest_read: None,
            labelers: None,
            awaiting_key: false,
            moderation_unverified: false,
            pending_invites: None,
        }
    }
    let owner = TypedAddress::Fauna {
        handle: "owner".into(),
        actor_id: ActorId([0x11; 32]),
    };
    let other = TypedAddress::Fauna {
        handle: "other".into(),
        actor_id: ActorId([0x22; 32]),
    };
    let m = ConversationsManager::new();
    let fauna = Arc::new(MockRailBackend::new(Rail::FaunaMls));
    m.register_backend(fauna.clone());

    let kept = m.materialize_conv_thread("aa".repeat(32), vec![owner.clone(), other.clone()]);
    let left = m.materialize_conv_thread("bb".repeat(32), vec![owner.clone(), other.clone()]);
    let community = m.materialize_conv_thread("cc".repeat(32), vec![owner, other]);
    fauna.set_room(kept.clone(), room(RoomClass::EndToEnd));
    fauna.set_room(left.clone(), room(RoomClass::EndToEnd));
    fauna.set_room(community.clone(), room(RoomClass::Community));

    let offered = |m: &ConversationsManager| -> Vec<[u8; 32]> {
        m.room_post_rooms().into_iter().map(|r| r.room).collect()
    };
    let mut all = offered(&m);
    all.sort();
    assert_eq!(
        all,
        vec![[0xaa; 32], [0xbb; 32], [0xcc; 32]],
        "a seated member is offered every member-keyed room"
    );

    // This device is removed from one end-to-end room and has processed it.
    fauna.unseat(left);

    let mut after = offered(&m);
    after.sort();
    assert_eq!(
        after,
        vec![[0xaa; 32], [0xcc; 32]],
        "the room this device was removed from is no longer offered; \
         the other end-to-end room and the community room still are"
    );
}

// ── The attachment store is a bounded cache, never the home of the bytes ──
//
// `conversations.md` § Attachments → *Retention*: the per-record reader bound
// caps what one message costs, not what the app HOLDS — before this the store
// kept every opened attachment for the manager's lifetime, so N messages kept
// N × 640 MiB in every member's process. The store now holds at most
// `ATTACHMENT_STORE_BUDGET_BYTES`, evicting the least recently read entry first;
// a staged outgoing draft is pinned (send re-resolves its bytes from here); an
// evicted FaunaMls attachment is fetched again on the next receive cycle
// (`fauna_mls_backend_tests.rs`), an SMTP one is declared.

#[test]
fn the_attachment_store_holds_at_most_its_budget_evicting_least_recently_read_first() {
    let m = ConversationsManager::new();
    m.set_attachment_store_budget_for_test(100);
    let cache = |bytes: Vec<u8>| {
        let hash = blake3::hash(&bytes).to_hex().to_string();
        m.cache_attachment_bytes(hash.clone(), bytes);
        hash
    };
    let a = cache(vec![b'a'; 40]);
    let b = cache(vec![b'b'; 40]);
    assert_eq!(m.attachment_store_resident_bytes(), 80);

    // Reading `a` makes `b` the least recently read.
    assert!(m.attachment_bytes(a.clone()).is_some());
    let c = cache(vec![b'c'; 40]);

    assert!(
        m.attachment_store_resident_bytes() <= 100,
        "the store never holds more than its budget"
    );
    assert!(
        m.attachment_bytes(a).is_some(),
        "a recently read entry stays"
    );
    assert!(
        m.attachment_bytes(c).is_some(),
        "the newest entry is resident"
    );
    assert!(
        m.attachment_bytes(b).is_none(),
        "the least recently read entry is the one evicted"
    );
}

#[test]
fn a_staged_outgoing_attachment_is_pinned_until_it_is_unstaged() {
    let m = ConversationsManager::new();
    m.set_attachment_store_budget_for_test(100);
    let thread = fauna_conversations::thread::ThreadId("t-1".into());
    let staged = m.add_attachment(
        thread.clone(),
        "report.pdf".into(),
        "application/pdf".into(),
        vec![b's'; 60],
    );

    // Two inbound attachments arrive; the budget forces eviction, but the
    // staged draft is what `send` will re-resolve, so it must survive.
    let x = vec![b'x'; 60];
    let x_hash = blake3::hash(&x).to_hex().to_string();
    m.cache_attachment_bytes(x_hash.clone(), x);
    let y = vec![b'y'; 60];
    let y_hash = blake3::hash(&y).to_hex().to_string();
    m.cache_attachment_bytes(y_hash.clone(), y);
    assert!(
        m.attachment_bytes(staged.clone()).is_some(),
        "a staged draft's bytes are pinned while the draft references them"
    );
    assert!(
        m.attachment_bytes(y_hash).is_some(),
        "the newest inbound entry is resident"
    );
    assert!(
        m.attachment_bytes(x_hash).is_none(),
        "the unpinned inbound entry is what made room"
    );

    // Unstaging lifts the pin: the next insert may evict it like any other.
    m.remove_attachment(thread, 0);
    let z = vec![b'z'; 60];
    m.cache_attachment_bytes(blake3::hash(&z).to_hex().to_string(), z);
    assert!(
        m.attachment_bytes(staged).is_none(),
        "once unstaged the bytes are evictable — they were the least recently read"
    );
    assert!(m.attachment_store_resident_bytes() <= 100);
}

// ── A message the user sends reaches the content index at send time ─────────
// `content-index-ingest.md` § Ingest triggers, v1 → *The Conversation kind's
// catch-up*. The receive poll never re-presents a sender's own record on the
// device that sent it, so without an offer at send time the only path into the
// index was the NEXT launch's store walk: a message stayed unsearchable from
// the moment it was sent until the app restarted.

/// What one `observe_indexable_message` call carried.
#[derive(Debug, Clone, PartialEq, Eq)]
struct Offered {
    kind: fauna_conversations::index_sink::IndexableKind,
    message_id: String,
    body: String,
    is_own: bool,
    nest_message_id: Option<Vec<u8>>,
}

#[derive(Default)]
struct IndexRecorder {
    offered: std::sync::Mutex<Vec<Offered>>,
}

impl fauna_conversations::index_sink::MessageIndexObserver for IndexRecorder {
    fn observe_indexable_message(
        &self,
        msg: fauna_conversations::index_sink::IndexableMessage<'_>,
    ) {
        self.offered.lock().unwrap().push(Offered {
            kind: msg.kind,
            message_id: msg.message_id.0.clone(),
            body: msg.body.to_string(),
            is_own: msg.is_own,
            nest_message_id: msg.nest_message_id.map(<[u8]>::to_vec),
        });
    }
}

impl IndexRecorder {
    fn offered(&self) -> Vec<Offered> {
        self.offered.lock().unwrap().clone()
    }
}

#[tokio::test]
async fn a_message_sent_in_a_fauna_native_group_is_offered_to_the_index_at_send_time() {
    use fauna_conversations::index_sink::IndexableKind;

    let m = ConversationsManager::new();
    m.register_backend(Arc::new(MockRailBackend::new(Rail::FaunaMls)));
    let grp = m.create_mls_group(vec![TypedAddress::Email {
        email_address: "bob@host.test".into(),
    }]);
    let recorder = Arc::new(IndexRecorder::default());
    m.set_index_observer(recorder.clone());

    m.set_compose_body(grp.clone(), "meet at the ferry at six".into());
    m.send(grp).await.expect("send ok");

    // Exactly the copy the thread store now holds, as the launch walk would
    // offer it — no subject, no nest record id — so the next launch's re-offer
    // of the same `(kind, content_id)` is dropped by the builder's guard rather
    // than indexed twice.
    assert_eq!(
        recorder.offered(),
        vec![Offered {
            kind: IndexableKind::Conversation,
            message_id: "mock-msg-0".into(),
            body: "meet at the ferry at six".into(),
            is_own: true,
            nest_message_id: None,
        }],
        "a sent message must reach the index when it is sent, not at the next launch"
    );
}

/// The bridge rails decided explicitly: a message sent on a bridged DM rail is
/// the user's own content exactly as a received one is (received bridge DMs
/// are already indexed as `Conversation` at the ingest chokepoint), and the
/// launch walk skips non-native threads, so without this offer it would never
/// be searchable at all.
#[tokio::test]
async fn a_message_sent_on_a_bridged_rail_is_offered_to_the_index_at_send_time() {
    use fauna_conversations::index_sink::IndexableKind;

    let m = ConversationsManager::new();
    m.register_backend(Arc::new(MockRailBackend::new(Rail::Bridged)));
    let mut inbound = smtp_inbound("npub-peer@nostr.test", None, "hello from the relay", None);
    inbound.rail = Rail::Bridged;
    m.ingest_inbound(inbound).expect("ingest");
    let tid = m.snapshot().threads[0].thread_id.clone();

    let recorder = Arc::new(IndexRecorder::default());
    m.set_index_observer(recorder.clone());
    m.set_compose_body(tid.clone(), "see you on the relay".into());
    m.send(tid).await.expect("send ok");

    let offered = recorder.offered();
    assert_eq!(offered.len(), 1, "exactly the sent message: {offered:?}");
    assert_eq!(offered[0].kind, IndexableKind::Conversation);
    assert_eq!(offered[0].body, "see you on the relay");
    assert!(
        offered[0].is_own,
        "a sent message is the account's own copy"
    );
}

/// The viewer's own nickname for a fauna-native sender paints as the
/// bubble's sender and the reply preview's — and clearing it restores the
/// address fallback (`contacts.md` § The private overlay → *Where the
/// nickname paints*).
#[test]
fn a_contact_overlay_nickname_paints_as_the_message_sender() {
    use fauna_core::contact_overlay::{ContactOverlay, Register, Stamp};
    let m = ConversationsManager::new();
    let mock = Arc::new(MockRailBackend::new(Rail::FaunaMls));
    mock.set_self_address(TypedAddress::Fauna {
        actor_id: ActorId([1u8; 32]),
        handle: "me@nest".into(),
    });
    m.register_backend(mock.clone());
    let peer = TypedAddress::Fauna {
        actor_id: ActorId([2u8; 32]),
        handle: "peer@nest".into(),
    };
    let t = m.create_mls_group(vec![peer.clone()]);
    // The peer's id is a verified leaf of the thread's group — the paint gate
    // (`contacts.md` § The private overlay → *The paint gate*) paints a chip
    // only on a proven id; the bubble's sender is proven by the rail.
    mock.set_engine_roster(t.clone(), vec![ActorId([2u8; 32])]);
    let mid = MessageId("msg-peer".into());
    m.ingest_inbound_to_thread(
        t.clone(),
        RailInboundMessage {
            rail: Rail::FaunaMls,
            sender: peer.clone(),
            recipients: vec![],
            subject: None,
            body: "hello".into(),
            body_format: BodyFormat::PlainText,
            timestamp_ms: 0,
            message_id: mid.clone(),
            in_reply_to: None,
            attachments: vec![],
            badges: Default::default(),
            legal_takedown_ref: None,
            plane_ref: None,
        },
    )
    .unwrap();
    assert_eq!(snap_msg(&m, &t, &mid).sender_display, "", "no overlay yet");

    let mut overlays = std::collections::BTreeMap::new();
    overlays.insert(
        ActorId([2u8; 32]).to_hex(),
        ContactOverlay {
            nickname: Register {
                stamp: Stamp::new(1, [1; 32]),
                value: Some("Mum".into()),
            },
            ..Default::default()
        },
    );
    let generation = m.register_contact_overlays(None);
    let chips = |m: &ConversationsManager| m.thread_detail(t.clone()).unwrap().participant_displays;
    assert_eq!(
        chips(&m),
        vec![peer.display()],
        "no overlay: the address display"
    );
    assert!(m.apply_contact_overlays(generation, overlays));
    assert_eq!(snap_msg(&m, &t, &mid).sender_display, "Mum");
    assert_eq!(chips(&m), vec!["Mum".to_string()], "the member chip too");
    assert_eq!(
        m.thread_detail(t.clone()).unwrap().participants,
        vec![peer.clone()],
        "painted, never authored: the address keeps the public handle"
    );
    m.start_reply(t.clone(), mid.clone(), false);
    assert_eq!(m.reply_preview(t.clone()).unwrap().sender_display, "Mum");

    assert!(m.apply_contact_overlays(generation, Default::default()));
    assert_eq!(snap_msg(&m, &t, &mid).sender_display, "", "cleared again");
}

/// Records every fold the manager asks its seam for, in order.
#[derive(Default)]
struct RecordingFolds(std::sync::Mutex<Vec<(String, String)>>);

impl RecordingFolds {
    fn take(&self) -> Vec<(String, String)> {
        std::mem::take(&mut *self.0.lock().unwrap())
    }
}

impl fauna_conversations::backend::ContactOverlayFolds for RecordingFolds {
    fn fold(&self, predecessor_hex: &str, successor_hex: &str) {
        self.0
            .lock()
            .unwrap()
            .push((predecessor_hex.to_string(), successor_hex.to_string()));
    }
}

fn nicknamed(nick: &str) -> fauna_core::contact_overlay::ContactOverlay {
    use fauna_core::contact_overlay::{ContactOverlay, Register, Stamp};
    ContactOverlay {
        nickname: Register {
            stamp: Stamp::new(1, [1; 32]),
            value: Some(nick.into()),
        },
        ..Default::default()
    }
}

/// **The succession fold is a reconcile over witness verdicts
/// (`contacts.md` § The private overlay → *When a person's identity
/// succeeds*).** An overlay under an identity with no verified successor
/// folds nothing; the verified re-point folds it onto the successor; a
/// straggler item that reappears under the old key is folded again at the
/// next projection load, with no statement; a twice-succeeded person folds
/// onto the terminal hop; and an identity change retires both the verdicts
/// and the projection, so the outgoing account's never reach the incoming
/// one's.
#[test]
fn a_verified_succession_folds_the_overlay_and_every_load_reconciles_it() {
    let m = ConversationsManager::new();
    let folds = Arc::new(RecordingFolds::default());
    let generation = m.register_contact_overlays(Some(folds.clone()));
    let (old, new, newer) = (ActorId([4u8; 32]), ActorId([9u8; 32]), ActorId([7u8; 32]));
    let load = |entries: &[(ActorId, &str)]| {
        entries
            .iter()
            .map(|(a, n)| (a.to_hex(), nicknamed(n)))
            .collect::<std::collections::BTreeMap<_, _>>()
    };
    let t = m.create_mls_group(vec![TypedAddress::Fauna {
        actor_id: old,
        handle: "mum@nest".into(),
    }]);

    assert!(m.apply_contact_overlays(generation, load(&[(old, "Mum")])));
    assert!(
        folds.take().is_empty(),
        "no verdict names a successor for this person: nothing folds"
    );

    m.apply_inbound_succession(t.clone(), &old, new);
    assert_eq!(folds.take(), vec![(old.to_hex(), new.to_hex())]);

    // The fold's own reload: the predecessor says nothing any more.
    m.apply_contact_overlays(generation, load(&[(new, "Mum")]));
    assert!(folds.take().is_empty(), "the reconcile's fixed point");

    // A sibling device that had not seen the fold edited the old identity.
    m.apply_contact_overlays(generation, load(&[(new, "Mum"), (old, "Mother")]));
    assert_eq!(
        folds.take(),
        vec![(old.to_hex(), new.to_hex())],
        "the projection load folds the straggler from the verdict already given"
    );

    // The successor succeeds again: both keys fold onto the terminal hop.
    m.apply_contact_overlays(generation, load(&[(new, "Mum"), (old, "Mother")]));
    folds.take();
    m.apply_inbound_succession(t.clone(), &new, newer);
    let mut asked = folds.take();
    asked.sort();
    let mut expected = vec![
        (old.to_hex(), newer.to_hex()),
        (new.to_hex(), newer.to_hex()),
    ];
    expected.sort();
    assert_eq!(asked, expected);

    // An identity change retires the verdicts, the seam and the projection.
    m.clear_for_identity_change();
    assert!(m.contacts().overlay(&old.to_hex()).is_none());
    assert!(
        !m.apply_contact_overlays(generation, load(&[(old, "Mum")])),
        "a delivery under the retired generation is refused"
    );
    let next = m.register_contact_overlays(Some(folds.clone()));
    assert!(m.apply_contact_overlays(next, load(&[(old, "Mum")])));
    assert!(
        folds.take().is_empty(),
        "the outgoing account's verdicts do not fold the incoming account's overlays"
    );
}
