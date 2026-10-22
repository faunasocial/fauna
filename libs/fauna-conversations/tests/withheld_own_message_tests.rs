//! The withheld-message badge on the author's OWN copy — `docs/goal/behavior/
//! moderation.md` § Legal takedown → *App render* (the fourth surface). The
//! author's devices keep the real copy of a record the nest withholds, so the
//! manager reads the author's queue rows when the thread opens
//! (`refresh_withheld_messages`) and `thread_detail` projects the badge onto
//! the own message whose record digest the row names — beside the body, never
//! collapsing it, and never onto a message someone else sent.

use fauna_conversations::backends::mock::MockRailBackend;
use fauna_conversations::message::{PlaneRef, WithheldGround, WithheldRecord};
use fauna_conversations::*;
use std::sync::Arc;

const DIGEST: &str = "ab12";

fn arrival(id: &str, digest: &str, admin_block_reason: Option<&str>) -> RailInboundMessage {
    RailInboundMessage {
        rail: Rail::FaunaMls,
        sender: TypedAddress::Email {
            email_address: "me@host.test".into(),
        },
        recipients: vec![TypedAddress::Email {
            email_address: "peer@host.test".into(),
        }],
        subject: None,
        body: format!("body {id}"),
        body_format: BodyFormat::PlainText,
        timestamp_ms: 1,
        message_id: MessageId(id.to_string()),
        in_reply_to: None,
        attachments: vec![],
        badges: Default::default(),
        legal_takedown_ref: None,
        admin_block_reason: admin_block_reason.map(str::to_string),
        plane_ref: Some(PlaneRef {
            scope: "content:conv:00".into(),
            record_digest: digest.into(),
        }),
    }
}

fn manager(ground: WithheldGround, issuer: Option<&str>) -> Arc<ConversationsManager> {
    let m = ConversationsManager::new();
    let backend = MockRailBackend::new(Rail::FaunaMls);
    *backend.withheld_records.lock().unwrap() = vec![WithheldRecord {
        content_id: DIGEST.into(),
        ground,
        issuer_nest: issuer.map(str::to_string),
        timestamp: 5,
    }];
    m.register_backend(Arc::new(backend));
    m
}

fn only_thread(m: &ConversationsManager) -> thread::ThreadId {
    let snap = m.snapshot();
    assert_eq!(snap.threads.len(), 1, "one thread");
    snap.threads[0].thread_id.clone()
}

fn message<'a>(detail: &'a ThreadDetail, id: &str) -> &'a MessageSnapshot {
    detail
        .messages
        .iter()
        .find(|m| m.message_id.0 == id)
        .unwrap_or_else(|| panic!("no message {id}"))
}

#[tokio::test]
async fn the_authors_copy_synced_before_the_takedown_gains_the_badge_on_open() {
    let m = manager(WithheldGround::TakenDown, Some("issuer.example"));
    m.inject_own_for_test(arrival("own", DIGEST, None)).unwrap();
    let id = only_thread(&m);
    assert!(
        message(&m.thread_detail(id.clone()).unwrap(), "own")
            .withheld
            .is_none(),
        "nothing is painted before the queue has been read"
    );

    m.refresh_withheld_messages(id.clone()).await;

    let detail = m.thread_detail(id).unwrap();
    let own = message(&detail, "own");
    let badge = own
        .withheld
        .as_ref()
        .expect("the badge on the matched copy");
    assert_eq!(badge.content_id, DIGEST);
    assert_eq!(badge.ground, WithheldGround::TakenDown);
    assert_eq!(badge.issuer_nest.as_deref(), Some("issuer.example"));
    assert_eq!(own.body, "body own", "the body stays in view");
    assert_eq!(
        own.legal_takedown_ref, None,
        "never the collapsing tombstone field"
    );
}

#[tokio::test]
async fn a_message_someone_else_sent_or_another_record_gets_no_badge() {
    let m = manager(WithheldGround::TakenDown, None);
    m.inject_inbound_for_test(arrival("theirs", DIGEST, None))
        .unwrap();
    m.inject_own_for_test(arrival("other", "cd34", None))
        .unwrap();
    let id = only_thread(&m);
    m.refresh_withheld_messages(id.clone()).await;
    let detail = m.thread_detail(id).unwrap();
    assert!(message(&detail, "theirs").withheld.is_none());
    assert!(message(&detail, "other").withheld.is_none());
}

#[tokio::test]
async fn a_blocked_copy_carrying_its_notice_gets_one_badge_with_the_reason() {
    let m = manager(WithheldGround::Blocked, None);
    m.inject_own_for_test(arrival("own", DIGEST, Some("harassment")))
        .unwrap();
    let id = only_thread(&m);
    m.refresh_withheld_messages(id.clone()).await;
    let detail = m.thread_detail(id).unwrap();
    let badge = message(&detail, "own").withheld.clone().expect("badge");
    assert!(badge.carries_block_notice());
    assert_eq!(
        badge.lines[0],
        fauna_core::obligation::admin_block_notice("harassment")
    );
}
