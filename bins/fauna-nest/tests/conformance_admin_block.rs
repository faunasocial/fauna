//! Conformance for `fauna.moderation.admin_block{,.list}` — the admin's block
//! of one reported item (`docs/goal/behavior/moderation.md` § Admin block of a
//! reported item): the fourth verb on a report-queue row, which withholds the
//! reported item from everyone this nest serves but the author through the
//! legal takedown's own withhold with a second ground, the transparency triple
//! mandatory (the notice, the author's appeal, the audit row), reversible,
//! never a deletion, and never an off-box retraction.
//!
//! Authority for the wire types: `libs/fauna-protocol/src/moderation.rs`.

mod common;
use common::dispatch;

use std::sync::Arc;

use bytes::Bytes;

use fauna_core::{
    data::{Post, PostBody, Timestamp},
    encoding::canonical_encode,
    identity::ActorId,
    obligation::ObligationAction,
};
use fauna_nest::{
    bridge_method_allowlist::{CallerClass, is_permitted},
    db::CacheDb,
    db::moderation::{AbuseReportInsert, NewAbuseReport},
    moderation_handlers, posts_handlers,
    routes::AppState,
    rpc_router::RpcRouter,
};
use fauna_protocol::{
    decode_strict as decode, encode_canonical,
    moderation::{
        AbuseReportReason, AbuseReportSubject, ModerationActionsReply, ModerationActionsRequest,
        ModerationAdminBlockListReply, ModerationAdminBlockListRequest, ModerationAdminBlockReply,
        ModerationAdminBlockRequest, ModerationAppealReply, ModerationAppealRequest,
    },
    posts::{PostGetReply, PostGetRequest},
};

const ADMIN: [u8; 32] = [0xA1; 32];
const AUTHOR: [u8; 32] = [0xA2; 32];
const REPORTER: [u8; 32] = [0xA3; 32];
const STRANGER: [u8; 32] = [0xA4; 32];

async fn router() -> (RpcRouter, Arc<AppState>) {
    let db = Arc::new(CacheDb::open_in_memory().unwrap());
    db.add_admin_actor(&ADMIN).await.unwrap();
    let state = Arc::new(AppState::for_test(db));
    let mut b = RpcRouter::builder();
    moderation_handlers::register_moderation_handlers(&mut b);
    posts_handlers::register_posts_handlers(&mut b);
    (b.build(), state)
}

fn pack<T: serde::Serialize>(req: &T) -> Bytes {
    Bytes::from(encode_canonical(req).unwrap().to_vec())
}

fn post_body(text: &str) -> Vec<u8> {
    canonical_encode(&Post {
        author: ActorId(AUTHOR),
        created_at: Timestamp(1_000_000),
        body: PostBody::Text {
            content: text.to_string(),
            facets: vec![],
        },
        references: vec![],
        expires_at: None,
        gated: None,
        content_warning: None,
        origin: None,
    })
    .unwrap()
}

/// Seed AUTHOR's post and REPORTER's open report on it; returns the post id
/// and the report id.
async fn seed_reported_post(state: &Arc<AppState>, fill: u8, reason: &str) -> ([u8; 32], String) {
    let post_id = [fill; 32];
    state
        .db
        .put_post(&post_id, &post_body("a reported post"), None)
        .await
        .unwrap();
    let report = file_report(state, "post", &hex::encode(post_id), None, reason).await;
    (post_id, report)
}

async fn file_report(
    state: &Arc<AppState>,
    kind: &str,
    subject_id: &str,
    channel: Option<String>,
    reason: &str,
) -> String {
    match state
        .db
        .insert_abuse_report(
            NewAbuseReport {
                reporter_actor: REPORTER,
                subject_kind: kind.into(),
                subject_id: subject_id.into(),
                subject_channel: channel,
                subject_actor: None,
                reason: reason.into(),
                note: None,
                excerpt: None,
                block_author: false,
            },
            100,
            Timestamp::now().as_i64(),
        )
        .await
        .unwrap()
    {
        AbuseReportInsert::Inserted(row) => row.id,
        other => panic!("report not inserted: {other:?}"),
    }
}

async fn block(
    router: &RpcRouter,
    state: &Arc<AppState>,
    caller: [u8; 32],
    report_id: &str,
    restore: bool,
) -> Result<ModerationAdminBlockReply, fauna_protocol::RpcError> {
    dispatch(
        router,
        state.clone(),
        caller,
        "fauna.moderation.admin_block",
        pack(&ModerationAdminBlockRequest {
            report_id: report_id.into(),
            restore,
            extra: Default::default(),
        }),
    )
    .await
    .map(|b| decode(&b).unwrap())
}

async fn get_post(
    router: &RpcRouter,
    state: &Arc<AppState>,
    caller: [u8; 32],
    post_id: &[u8; 32],
) -> PostGetReply {
    decode(
        &dispatch(
            router,
            state.clone(),
            caller,
            "fauna.posts.get",
            pack(&PostGetRequest {
                post_id: hex::encode(post_id),
                extra: Default::default(),
            }),
        )
        .await
        .expect("a blocked post still answers — with the notice"),
    )
    .unwrap()
}

async fn audit_count(state: &Arc<AppState>, action: &str, target: &str) -> i64 {
    let conn = state.db.conn().await;
    conn.query_row(
        "SELECT COUNT(*) FROM audit_log WHERE action = ?1 AND target = ?2",
        rusqlite::params![action, target],
        |r| r.get(0),
    )
    .unwrap()
}

async fn on_box_feed(state: &Arc<AppState>) -> Vec<Vec<u8>> {
    state
        .db
        .query_feed_for_authors(
            &[],
            fauna_core::scoring::FilterCombination::All,
            &[],
            &[AUTHOR.to_vec()],
            None,
            100,
            fauna_nest::db::feeds::FeedAudience::OnBox,
            fauna_nest::db::feeds::FeedLocality::NestWide,
        )
        .await
        .unwrap()
        .into_iter()
        .map(|p| p.post_id)
        .collect()
}

#[test]
fn admin_block_is_admin_only_at_the_allowlist_layer() {
    for kind in [
        "fauna.moderation.admin_block",
        "fauna.moderation.admin_block.list",
    ] {
        assert!(is_permitted(CallerClass::Admin, kind), "{kind}");
        for class in [
            CallerClass::User,
            CallerClass::BridgeMta,
            CallerClass::BridgeMda,
            CallerClass::ContentProcessor,
        ] {
            assert!(
                !is_permitted(class, kind),
                "{kind} must be denied for {class:?}"
            );
        }
    }
}

/// A user can never block another user's content.
#[tokio::test]
async fn a_non_admin_is_refused() {
    let (router, state) = router().await;
    let (_post, report) = seed_reported_post(&state, 0x11, "spam").await;
    let err = block(&router, &state, STRANGER, &report, false)
        .await
        .expect_err("a non-admin cannot block");
    assert_eq!(err.code, "fauna.moderation.permission_denied");
}

/// A block is of one item: an `actor` report is refused before anything is
/// written, and an unknown report is `not_found`.
#[tokio::test]
async fn an_account_report_and_an_unknown_report_are_refused() {
    let (router, state) = router().await;
    let report = file_report(&state, "actor", &hex::encode(AUTHOR), None, "harassment").await;
    let err = block(&router, &state, ADMIN, &report, false)
        .await
        .expect_err("an account report cannot be blocked");
    assert_eq!(err.code, "fauna.moderation.invalid_params");
    let row = state.db.get_abuse_report(&report).await.unwrap().unwrap();
    assert_eq!(row.status, "open", "a refused block leaves the report open");

    let err = block(&router, &state, ADMIN, "no-such-report", false)
        .await
        .expect_err("an unknown report");
    assert_eq!(err.code, "fauna.moderation.not_found");
}

/// The whole post loop: block → every non-author read withholds the body with
/// the notice, the author's read serves it with the notice, the on-box feed
/// excludes it, the author's queue carries the `Blocked` row, the report is
/// resolved *acted*, the audit row stands, NO retraction witness is written,
/// the author (and only the author) appeals, the blocked list names it; then
/// unblock → the same bytes serve again and the obligation row stays.
#[tokio::test]
async fn block_withholds_from_everyone_but_the_author_and_unblock_re_serves() {
    let (router, state) = router().await;
    let (post_id, report) = seed_reported_post(&state, 0x21, "harassment").await;
    let content_id = hex::encode(post_id);
    let before = get_post(&router, &state, STRANGER, &post_id).await;
    assert!(!before.body.is_empty() && before.admin_block.is_none());
    assert!(on_box_feed(&state).await.contains(&post_id.to_vec()));

    let reply = block(&router, &state, ADMIN, &report, false)
        .await
        .expect("admin blocks the reported post");
    assert_eq!(reply.status, "blocked");
    assert_eq!(reply.content_id, content_id);
    assert_eq!(reply.content_type, "post");

    // Every non-author — a stranger, the reporter, the admin — gets the notice
    // and no body.
    for viewer in [STRANGER, REPORTER, ADMIN] {
        let r = get_post(&router, &state, viewer, &post_id).await;
        assert!(r.body.is_empty(), "the body is withheld from {viewer:?}");
        assert_eq!(
            r.admin_block.expect("the notice is carried").reason,
            AbuseReportReason::Harassment
        );
        assert!(r.legal_takedown.is_none(), "a block is not a takedown");
    }
    // The author keeps their own view, with the notice beside it.
    let own = get_post(&router, &state, AUTHOR, &post_id).await;
    assert_eq!(own.body.to_vec(), before.body.to_vec());
    assert_eq!(
        own.admin_block.unwrap().reason,
        AbuseReportReason::Harassment
    );

    // `MODERATION_SERVABLE` withholds it from the feed reads.
    assert!(!on_box_feed(&state).await.contains(&post_id.to_vec()));

    // The author's queue carries the Blocked (10) row, categorised by reason.
    let actions: ModerationActionsReply = decode(
        &dispatch(
            &router,
            state.clone(),
            AUTHOR,
            "fauna.moderation.actions",
            pack(&ModerationActionsRequest::default()),
        )
        .await
        .unwrap(),
    )
    .unwrap();
    let row = actions
        .actions
        .iter()
        .find(|a| a.content_id == content_id)
        .expect("the author's queue carries the block");
    assert_eq!(row.action, ObligationAction::Blocked as u8);
    assert_eq!(row.category, "harassment");

    // The report is resolved acted, in the same act.
    let resolved = state.db.get_abuse_report(&report).await.unwrap().unwrap();
    assert_eq!(resolved.status, "resolved");
    assert_eq!(resolved.outcome.as_deref(), Some("acted"));
    assert_eq!(resolved.resolved_by.as_deref(), Some(ADMIN.as_slice()));
    assert_eq!(
        audit_count(&state, "moderation:admin-block", &content_id).await,
        1
    );

    // No off-box retraction: no `tombstone/post` witness was written.
    let witnesses: i64 = {
        let conn = state.db.conn().await;
        conn.query_row(
            "SELECT COUNT(*) FROM content WHERE schema = 'tombstone/post'",
            [],
            |r| r.get(0),
        )
        .unwrap()
    };
    assert_eq!(witnesses, 0, "a block retracts nothing already published");

    // The appeal: the author's, never a stranger's.
    let appeal = |caller| {
        dispatch(
            &router,
            state.clone(),
            caller,
            "fauna.moderation.appeal",
            pack(&ModerationAppealRequest {
                content_id: content_id.clone(),
                reason: "the post was fine".into(),
                extra: Default::default(),
            }),
        )
    };
    let reply: ModerationAppealReply =
        decode(&appeal(AUTHOR).await.expect("author appeals")).unwrap();
    assert_eq!(reply.status, "appeal_recorded");
    assert_eq!(
        appeal(STRANGER)
            .await
            .expect_err("a stranger cannot appeal")
            .code,
        "fauna.moderation.permission_denied"
    );

    // A replay writes nothing more.
    let again = block(&router, &state, ADMIN, &report, false).await.unwrap();
    assert_eq!(again.status, "blocked");
    assert_eq!(
        audit_count(&state, "moderation:admin-block", &content_id).await,
        1
    );

    // The blocked list names it.
    let list: ModerationAdminBlockListReply = decode(
        &dispatch(
            &router,
            state.clone(),
            ADMIN,
            "fauna.moderation.admin_block.list",
            pack(&ModerationAdminBlockListRequest::default()),
        )
        .await
        .unwrap(),
    )
    .unwrap();
    assert_eq!(list.items.len(), 1);
    assert_eq!(list.items[0].report_id, report);
    assert_eq!(
        list.items[0].subject,
        AbuseReportSubject::Post {
            cid: content_id.clone()
        }
    );
    assert_eq!(list.items[0].reason, AbuseReportReason::Harassment);

    // Unblock: the same bytes serve again; the block stays on record.
    let reply = block(&router, &state, ADMIN, &report, true).await.unwrap();
    assert_eq!(reply.status, "restored");
    let after = get_post(&router, &state, STRANGER, &post_id).await;
    assert_eq!(after.body.to_vec(), before.body.to_vec());
    assert!(after.admin_block.is_none());
    assert!(on_box_feed(&state).await.contains(&post_id.to_vec()));
    assert_eq!(
        audit_count(&state, "moderation:admin-block-restore", &content_id).await,
        1
    );
    let actions: ModerationActionsReply = decode(
        &dispatch(
            &router,
            state.clone(),
            AUTHOR,
            "fauna.moderation.actions",
            pack(&ModerationActionsRequest::default()),
        )
        .await
        .unwrap(),
    )
    .unwrap();
    assert!(
        actions
            .actions
            .iter()
            .any(|a| a.action == ObligationAction::Blocked as u8),
        "the obligation row survives the unblock as history"
    );
    let list: ModerationAdminBlockListReply = decode(
        &dispatch(
            &router,
            state.clone(),
            ADMIN,
            "fauna.moderation.admin_block.list",
            pack(&ModerationAdminBlockListRequest::default()),
        )
        .await
        .unwrap(),
    )
    .unwrap();
    assert!(list.items.is_empty(), "an unblocked item leaves the list");
    // A second unblock is a replay, not an error.
    assert_eq!(
        block(&router, &state, ADMIN, &report, true)
            .await
            .unwrap()
            .status,
        "restored"
    );
}

/// A block cites an OPEN report: a dismissed report blocks nothing.
#[tokio::test]
async fn a_resolved_report_cannot_be_turned_into_a_block() {
    let (router, state) = router().await;
    let (post_id, report) = seed_reported_post(&state, 0x31, "spam").await;
    state
        .db
        .resolve_abuse_report(&report, "dismissed", &ADMIN, 1)
        .await
        .unwrap();
    let err = block(&router, &state, ADMIN, &report, false)
        .await
        .expect_err("a dismissed report");
    assert_eq!(err.code, "fauna.moderation.conflict");
    assert!(
        state
            .db
            .get_post_admin_block(&post_id)
            .await
            .unwrap()
            .is_none()
    );
}

/// A conversation record blocks by its record cid without the nest reading
/// it: the relay read withholds the envelope and names the reason, and unblock
/// re-serves. This record was appended by no authenticated sender, so no
/// attestation names its author: no queue row is written and the appeal is any
/// holder's (`moderation.md` § Errors & edge cases, the appeal bullet).
#[tokio::test]
async fn a_conversation_record_blocks_by_record_cid() {
    let (router, state) = router().await;
    let channel = [0xC1u8; 32];
    let record_id = [0xC2u8; 32];
    let record_cid = fauna_cbor::Cid::from_digest_dag_cbor(record_id);
    state
        .db
        .segment_records_insert_conv(&channel, 0, &record_cid, "b", 1_000, 1, &[], None)
        .await
        .unwrap();
    let report = file_report(
        &state,
        "message",
        &hex::encode(record_id),
        Some(hex::encode(channel)),
        "violence",
    )
    .await;

    let reply = block(&router, &state, ADMIN, &report, false).await.unwrap();
    assert_eq!(reply.status, "blocked");
    assert_eq!(reply.content_type, "conversation");
    assert_eq!(reply.content_id, hex::encode(record_id));
    let rows = state
        .db
        .segment_records_list_conv_after_seq(&channel, 0, 100)
        .await
        .unwrap();
    assert_eq!(rows.len(), 1, "the record keeps its seq slot");
    assert!(rows[0].3.is_withheld());
    assert_eq!(rows[0].3.admin_block.as_deref(), Some("violence"));
    assert!(rows[0].3.legal_takedown.is_none());
    assert_eq!(
        audit_count(&state, "moderation:admin-block", &hex::encode(record_id)).await,
        1
    );

    // No attestation → no queue row for anyone.
    for caller in [AUTHOR, STRANGER] {
        assert!(
            queue_row(&router, &state, caller, &hex::encode(record_id))
                .await
                .is_none()
        );
    }

    // Any holder of the record id appeals it — there is no author to check.
    let reply: ModerationAppealReply = decode(
        &dispatch(
            &router,
            state.clone(),
            STRANGER,
            "fauna.moderation.appeal",
            pack(&ModerationAppealRequest {
                content_id: hex::encode(record_id),
                reason: "it was a quote".into(),
                extra: Default::default(),
            }),
        )
        .await
        .expect("a blocked conversation record is appealable"),
    )
    .unwrap();
    assert_eq!(reply.status, "appeal_recorded");

    let reply = block(&router, &state, ADMIN, &report, true).await.unwrap();
    assert_eq!(reply.status, "restored");
    let rows = state
        .db
        .segment_records_list_conv_after_seq(&channel, 0, 100)
        .await
        .unwrap();
    assert!(!rows[0].3.is_withheld(), "unblock re-serves the record");
}

/// `caller`'s moderation-queue row for `content_id`, if any.
async fn queue_row(
    router: &RpcRouter,
    state: &Arc<AppState>,
    caller: [u8; 32],
    content_id: &str,
) -> Option<fauna_protocol::moderation::ObligationAction> {
    let reply: ModerationActionsReply = decode(
        &dispatch(
            router,
            state.clone(),
            caller,
            "fauna.moderation.actions",
            pack(&ModerationActionsRequest::default()),
        )
        .await
        .unwrap(),
    )
    .unwrap();
    reply
        .actions
        .into_iter()
        .find(|a| a.content_id == content_id)
}

/// A conversation record whose sender the nest attested (`conv_record_authors`)
/// blocks onto that author's queue exactly as a post does (`moderation.md`
/// § Admin block of a reported item → *Mechanism*): an `ObligationAction::Blocked`
/// row, `content_type="conversation"`, `category` the report's reason. The row
/// names the author, so the appeal is theirs alone, and unblock keeps it.
#[tokio::test]
async fn an_attested_conversation_record_blocks_onto_its_authors_queue() {
    let (router, state) = router().await;
    let channel = [0xC5u8; 32];
    let record_id = [0xC6u8; 32];
    let content_id = hex::encode(record_id);
    let record_cid = fauna_cbor::Cid::from_digest_dag_cbor(record_id);
    state
        .db
        .segment_records_insert_conv(&channel, 0, &record_cid, "b", 1_000, 1, &[], Some(&AUTHOR))
        .await
        .unwrap();
    let report = file_report(
        &state,
        "message",
        &content_id,
        Some(hex::encode(channel)),
        "harassment",
    )
    .await;

    let reply = block(&router, &state, ADMIN, &report, false).await.unwrap();
    assert_eq!(reply.status, "blocked");
    let row = queue_row(&router, &state, AUTHOR, &content_id)
        .await
        .expect("the attested author's queue carries the block");
    assert_eq!(row.content_type, "conversation");
    assert_eq!(row.action, ObligationAction::Blocked as u8);
    assert_eq!(row.category, "harassment");
    assert!(
        queue_row(&router, &state, STRANGER, &content_id)
            .await
            .is_none()
    );

    let appeal = |caller: [u8; 32]| {
        dispatch(
            &router,
            state.clone(),
            caller,
            "fauna.moderation.appeal",
            pack(&ModerationAppealRequest {
                content_id: content_id.clone(),
                reason: "it was a quote".into(),
                extra: Default::default(),
            }),
        )
    };
    let reply: ModerationAppealReply =
        decode(&appeal(AUTHOR).await.expect("the author appeals")).unwrap();
    assert_eq!(reply.status, "appeal_recorded");
    let err = appeal(STRANGER)
        .await
        .expect_err("a non-author cannot appeal an attested record");
    assert_eq!(err.code, "fauna.moderation.permission_denied");

    let reply = block(&router, &state, ADMIN, &report, true).await.unwrap();
    assert_eq!(reply.status, "restored");
    assert!(
        queue_row(&router, &state, AUTHOR, &content_id)
            .await
            .is_some(),
        "unblock keeps the queue row as history"
    );
}
