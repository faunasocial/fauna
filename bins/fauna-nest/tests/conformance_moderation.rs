//! Integration round-trip for `fauna.moderation.{stats,actions,appeal,
//! train}` and the report/signal kinds — a faithful transport migration of the
//! `/api/v1/moderation/*` HTTP routes. Reaches `CacheDb` directly through the
//! handlers via `state.db.*`.
//!
//! Authority for the wire types: `libs/fauna-protocol/src/moderation.rs`.
//! Slice: tracked internally.

mod common;
use common::dispatch;

use std::sync::Arc;

use bytes::Bytes;

use fauna_core::{
    data::{Post, PostBody, Timestamp},
    encoding::canonical_encode,
    identity::ActorId,
};
use fauna_nest::{
    bridge_method_allowlist::{CallerClass, is_permitted},
    db::CacheDb,
    db::reports::{ReportKey, capture_report},
    db::signals::SignalVerdict,
    moderation_handlers,
    routes::AppState,
    rpc_router::RpcRouter,
};
use fauna_protocol::{
    RpcError, decode_strict as decode, encode_canonical,
    moderation::{
        AbuseReportMineReply, AbuseReportMineRequest, AbuseReportOutcome, AbuseReportQueueReply,
        AbuseReportQueueRequest, AbuseReportReason, AbuseReportResolveRequest, AbuseReportStatus,
        AbuseReportSubject, AbuseReportSubmitReply, AbuseReportSubmitRequest,
        AbuseReportWithdrawRequest, ModerationActionsReply, ModerationActionsRequest,
        ModerationAppealReply, ModerationAppealRequest, ModerationLegalTakedownReply,
        ModerationLegalTakedownRequest, ModerationReportShareSetReply,
        ModerationReportShareSetRequest, ModerationReportShareStatusReply,
        ModerationReportShareStatusRequest, ModerationSignalContributeReply,
        ModerationSignalContributeRequest, ModerationSignalShareSetReply,
        ModerationSignalShareSetRequest, ModerationSignalShareStatusReply,
        ModerationSignalShareStatusRequest, ModerationStatsReply, ModerationStatsRequest,
        ModerationTrainReply, ModerationTrainRequest,
    },
};

/// Seed a native text post authored by `author` into the test DB (also creates
/// the `content_meta` row so quarantine flags apply), returning its body bytes.
/// Used by the read-authz tests below.
async fn seed_post(state: &Arc<AppState>, post_id: &[u8; 32], author: [u8; 32], text: &str) {
    let post = Post {
        author: ActorId(author),
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
    };
    state
        .db
        .put_post(post_id, &canonical_encode(&post).unwrap(), None)
        .await
        .unwrap();
}

/// The **User|Admin** moderation surface. `fauna.moderation.legal_takedown` is
/// deliberately NOT here — it is **Admin-only** (asserted separately below).
const KINDS: [&str; 4] = [
    "fauna.moderation.stats",
    "fauna.moderation.actions",
    "fauna.moderation.appeal",
    "fauna.moderation.train",
];

async fn router_with_db_only() -> (RpcRouter, Arc<AppState>) {
    let db = Arc::new(CacheDb::open_in_memory().unwrap());
    let state = Arc::new(AppState::for_test(db));
    let mut b = RpcRouter::builder();
    moderation_handlers::register_moderation_handlers(&mut b);
    (b.build(), state)
}

fn pack<T: serde::Serialize>(req: &T) -> Bytes {
    Bytes::from(encode_canonical(req).unwrap().to_vec())
}

#[tokio::test]
async fn stats_empty_db_returns_empty_labels() {
    let (router, state) = router_with_db_only().await;
    let bytes = dispatch(
        &router,
        state,
        [1u8; 32],
        "fauna.moderation.stats",
        pack(&ModerationStatsRequest::default()),
    )
    .await
    .expect("stats ok");
    let reply: ModerationStatsReply = decode(&bytes).unwrap();
    assert!(reply.labels.is_empty(), "fresh db has no labels");
}

#[tokio::test]
async fn actions_for_fresh_actor_is_empty() {
    let (router, state) = router_with_db_only().await;
    let bytes = dispatch(
        &router,
        state,
        [2u8; 32],
        "fauna.moderation.actions",
        pack(&ModerationActionsRequest::default()),
    )
    .await
    .expect("actions ok");
    let reply: ModerationActionsReply = decode(&bytes).unwrap();
    assert!(reply.actions.is_empty(), "fresh actor has no actions");
}

#[tokio::test]
async fn appeal_records_and_echoes_content_id() {
    // `appeal_subject` (moderation_handlers.rs's appeal gate) requires a real
    // enforcement record — so the appeal here follows a real takedown, the
    // same setup `legal_takedown_and_restore_flow` below uses — and scopes a
    // post's appeal to its author, who files it.
    let (router, state) = router_with_db_only().await;
    let admin = [0x77u8; 32];
    state.db.add_admin_actor(&admin).await.unwrap();
    let author = [0x78u8; 32];
    let (post_id, content_id) = post_id_and_hex(0x7a);
    seed_post(&state, &post_id, author, "content under appeal").await;
    dispatch(
        &router,
        state.clone(),
        admin,
        "fauna.moderation.legal_takedown",
        pack_takedown(&content_id, "EU-DSA-2024/99999", false),
    )
    .await
    .expect("admin takedown ok");

    let bytes = dispatch(
        &router,
        state.clone(),
        author,
        "fauna.moderation.appeal",
        pack(&ModerationAppealRequest {
            content_id: content_id.clone(),
            reason: "false positive".into(),
            extra: Default::default(),
        }),
    )
    .await
    .expect("appeal ok");
    let reply: ModerationAppealReply = decode(&bytes).unwrap();
    assert_eq!(reply.status, "appeal_recorded");
    assert_eq!(reply.content_id, content_id);
}

fn appeal_req(content_id: &str, reason: String) -> Bytes {
    pack(&ModerationAppealRequest {
        content_id: content_id.to_string(),
        reason,
        extra: Default::default(),
    })
}

async fn appeal_rows(state: &Arc<AppState>) -> usize {
    state
        .db
        .list_audit(1000, None)
        .await
        .unwrap()
        .into_iter()
        .filter(|r| r.action == "moderation:appeal")
        .count()
}

/// The residue `moderation.md` § Errors & edge cases closes on the appeal
/// gate: an unbounded reason, a non-author filing against someone else's
/// post, and one caller appending a row per call for ever. Each refusal or
/// collapse writes NO audit row; the author's own appeal still records, once
/// per decision.
#[tokio::test]
async fn appeal_is_bounded_and_author_scoped_on_posts() {
    let (router, state) = router_with_db_only().await;
    let admin = [0x87u8; 32];
    state.db.add_admin_actor(&admin).await.unwrap();
    let author = [0x88u8; 32];
    let stranger = [0x89u8; 32];
    let (post_id, content_id) = post_id_and_hex(0x8a);
    seed_post(&state, &post_id, author, "content under appeal").await;
    dispatch(
        &router,
        state.clone(),
        admin,
        "fauna.moderation.legal_takedown",
        pack_takedown(&content_id, "EU-DSA-2024/99998", false),
    )
    .await
    .expect("admin takedown ok");

    // An over-long reason is refused before any write — even the author's.
    let err = dispatch(
        &router,
        state.clone(),
        author,
        "fauna.moderation.appeal",
        appeal_req(
            &content_id,
            "x".repeat(fauna_protocol::moderation::MAX_APPEAL_REASON_BYTES + 1),
        ),
    )
    .await
    .expect_err("over-long reason refused");
    assert_eq!(err.code, "fauna.moderation.invalid_params");
    assert_eq!(appeal_rows(&state).await, 0);

    // Someone else's post is not theirs to appeal.
    let err = dispatch(
        &router,
        state.clone(),
        stranger,
        "fauna.moderation.appeal",
        appeal_req(&content_id, "not mine".into()),
    )
    .await
    .expect_err("non-author refused");
    assert_eq!(err.code, "fauna.moderation.permission_denied");
    assert_eq!(appeal_rows(&state).await, 0);

    // The author's appeal — exactly at the bound — records.
    let reply: ModerationAppealReply = decode(
        &dispatch(
            &router,
            state.clone(),
            author,
            "fauna.moderation.appeal",
            appeal_req(
                &content_id,
                "x".repeat(fauna_protocol::moderation::MAX_APPEAL_REASON_BYTES),
            ),
        )
        .await
        .expect("author's appeal ok"),
    )
    .unwrap();
    assert_eq!(reply.status, "appeal_recorded");
    assert_eq!(appeal_rows(&state).await, 1);

    // A repeat — even re-spelled in upper case — collapses onto the pending
    // appeal and writes nothing.
    let reply: ModerationAppealReply = decode(
        &dispatch(
            &router,
            state.clone(),
            author,
            "fauna.moderation.appeal",
            appeal_req(&content_id.to_uppercase(), "again".into()),
        )
        .await
        .expect("repeat collapses, not an error"),
    )
    .unwrap();
    assert_eq!(reply.status, "appeal_already_recorded");
    assert_eq!(appeal_rows(&state).await, 1);

    // The overturn is a decision: it re-opens the author's handle.
    dispatch(
        &router,
        state.clone(),
        admin,
        "fauna.moderation.legal_takedown",
        pack_takedown(&content_id, "", true),
    )
    .await
    .expect("admin restore ok");
    let reply: ModerationAppealReply = decode(
        &dispatch(
            &router,
            state.clone(),
            author,
            "fauna.moderation.appeal",
            appeal_req(&content_id, "after the overturn".into()),
        )
        .await
        .expect("appeal after a decision ok"),
    )
    .unwrap();
    assert_eq!(reply.status, "appeal_recorded");
    assert_eq!(appeal_rows(&state).await, 2);
}

/// The conversation arm stays unscoped (`moderation.md` § Errors & edge
/// cases): a sealed record persists no sender, so each caller holding its id
/// appeals it — each once per decision.
#[tokio::test]
async fn a_conversation_appeal_is_any_callers_once_each() {
    let (router, state) = router_with_db_only().await;
    let record_id = [0x9au8; 32];
    let content_id = hex::encode(record_id);
    let record_cid = fauna_cbor::Cid::from_digest_dag_cbor(record_id);
    state
        .db
        .segment_records_insert_conv(&[0x9bu8; 32], 0, &record_cid, "b", 1_000, 1, &[], None)
        .await
        .unwrap();
    assert_eq!(
        state
            .db
            .set_conv_legal_takedown(&record_cid, Some("EU-DSA-2024/99997"))
            .await
            .unwrap(),
        1
    );

    for (member, want) in [
        ([0x9cu8; 32], "appeal_recorded"),
        ([0x9du8; 32], "appeal_recorded"),
        ([0x9cu8; 32], "appeal_already_recorded"),
    ] {
        let reply: ModerationAppealReply = decode(
            &dispatch(
                &router,
                state.clone(),
                member,
                "fauna.moderation.appeal",
                appeal_req(&content_id, "the message was lawful".into()),
            )
            .await
            .expect("a conversation appeal is any caller's"),
        )
        .unwrap();
        assert_eq!(reply.status, want);
    }
    assert_eq!(appeal_rows(&state).await, 2);
}

/// A decision re-opens the appeal handle whatever spelling the admin sent the
/// id in: the appeal is keyed lowercase, and `record_appeal` finds the last
/// decision by exact `target`, so a decision recorded in the admin's upper-case
/// spelling used to leave the author's appeal pending for ever.
#[tokio::test]
async fn a_re_spelled_overturn_still_reopens_the_appeal() {
    let (router, state) = router_with_db_only().await;
    let admin = [0xa7u8; 32];
    state.db.add_admin_actor(&admin).await.unwrap();
    let author = [0xa8u8; 32];
    let (post_id, content_id) = post_id_and_hex(0xaa);
    seed_post(&state, &post_id, author, "content under appeal").await;
    dispatch(
        &router,
        state.clone(),
        admin,
        "fauna.moderation.legal_takedown",
        pack_takedown(&content_id, "EU-DSA-2024/99996", false),
    )
    .await
    .expect("admin takedown ok");
    let appeal = |reason: &str| {
        dispatch(
            &router,
            state.clone(),
            author,
            "fauna.moderation.appeal",
            appeal_req(&content_id, reason.into()),
        )
    };
    let reply: ModerationAppealReply = decode(&appeal("first").await.expect("appeal ok")).unwrap();
    assert_eq!(reply.status, "appeal_recorded");

    dispatch(
        &router,
        state.clone(),
        admin,
        "fauna.moderation.legal_takedown",
        pack_takedown(&content_id.to_uppercase(), "", true),
    )
    .await
    .expect("admin restore in upper case ok");
    let reply: ModerationAppealReply =
        decode(&appeal("after the overturn").await.expect("appeal ok")).unwrap();
    assert_eq!(reply.status, "appeal_recorded");
    assert_eq!(appeal_rows(&state).await, 2);
}

#[tokio::test]
async fn appeal_rejects_empty_fields() {
    let (router, state) = router_with_db_only().await;
    let err = dispatch(
        &router,
        state,
        [3u8; 32],
        "fauna.moderation.appeal",
        pack(&ModerationAppealRequest {
            content_id: String::new(),
            reason: "x".into(),
            extra: Default::default(),
        }),
    )
    .await
    .expect_err("empty content_id rejected");
    assert_eq!(err.code, "fauna.moderation.invalid_params");
}

#[tokio::test]
async fn train_rejects_bad_verdict() {
    let (router, state) = router_with_db_only().await;
    let err = dispatch(
        &router,
        state,
        [5u8; 32],
        "fauna.moderation.train",
        pack(&ModerationTrainRequest {
            content_id: "00".repeat(32),
            verdict: "maybe".into(),
            extra: Default::default(),
        }),
    )
    .await
    .expect_err("bad verdict rejected");
    assert_eq!(err.code, "fauna.moderation.invalid_params");
}

#[tokio::test]
async fn train_rejects_non_hex_content_id() {
    let (router, state) = router_with_db_only().await;
    let err = dispatch(
        &router,
        state,
        [5u8; 32],
        "fauna.moderation.train",
        pack(&ModerationTrainRequest {
            content_id: "not-hex".into(),
            verdict: "spam".into(),
            extra: Default::default(),
        }),
    )
    .await
    .expect_err("non-hex content_id rejected");
    assert_eq!(err.code, "fauna.moderation.invalid_params");
}

#[tokio::test]
async fn train_missing_post_is_not_found() {
    let (router, state) = router_with_db_only().await;
    let err = dispatch(
        &router,
        state,
        [5u8; 32],
        "fauna.moderation.train",
        pack(&ModerationTrainRequest {
            content_id: "11".repeat(32), // valid 32-byte hex, no such post
            verdict: "spam".into(),
            extra: Default::default(),
        }),
    )
    .await
    .expect_err("missing post is not_found");
    assert_eq!(err.code, "fauna.moderation.not_found");
}

/// A `(post_id, content_id-hex)` pair from one fill byte, so the seeded post id
/// and the wire `content_id` hex string always agree.
fn post_id_and_hex(fill: u8) -> ([u8; 32], String) {
    ([fill; 32], format!("{fill:02x}").repeat(32))
}

async fn dispatch_train(
    router: &RpcRouter,
    state: Arc<AppState>,
    caller: [u8; 32],
    content_id: String,
) -> Result<Bytes, RpcError> {
    dispatch(
        router,
        state,
        caller,
        "fauna.moderation.train",
        pack(&ModerationTrainRequest {
            content_id,
            verdict: "spam".into(),
            extra: Default::default(),
        }),
    )
    .await
}

/// A stranger may not train on a **quarantined** post they
/// can't read — reported as the SAME `not_found` as an absent post (no
/// existence oracle). This is the read-leg of the content-reconstruction
/// oracle, now closed.
#[tokio::test]
async fn train_on_quarantined_post_by_stranger_is_denied() {
    let (router, state) = router_with_db_only().await;
    let (post_id, content_id) = post_id_and_hex(0x21);
    let author = [0x22u8; 32];
    let stranger = [0x23u8; 32];
    seed_post(&state, &post_id, author, "free crypto click now").await;
    state.db.set_post_quarantined(&post_id, true).await.unwrap();

    let err = dispatch_train(&router, state, stranger, content_id)
        .await
        .expect_err("a stranger may not train on a quarantined post");
    assert_eq!(err.code, "fauna.moderation.not_found");
}

/// The author may still train on their **own** quarantined post (the gate
/// passes ⇒ the body loads ⇒ `trained`), proving the gate distinguishes
/// allow from deny and preserves the legitimate feature.
#[tokio::test]
async fn train_on_quarantined_post_by_author_succeeds() {
    let (router, state) = router_with_db_only().await;
    let (post_id, content_id) = post_id_and_hex(0x24);
    let author = [0x25u8; 32];
    seed_post(&state, &post_id, author, "free crypto click now").await;
    state.db.set_post_quarantined(&post_id, true).await.unwrap();

    let bytes = dispatch_train(&router, state, author, content_id)
        .await
        .expect("the author may train on their own quarantined post");
    let reply: ModerationTrainReply = decode(&bytes).unwrap();
    assert_eq!(reply.status, "trained");
}

/// An admin may train on any quarantined post (admins can read quarantined
/// content — the same author-or-admin gate `get_post_core` applies).
#[tokio::test]
async fn train_on_quarantined_post_by_admin_succeeds() {
    let (router, state) = router_with_db_only().await;
    let (post_id, content_id) = post_id_and_hex(0x26);
    let author = [0x27u8; 32];
    let admin = [0x28u8; 32];
    seed_post(&state, &post_id, author, "free crypto click now").await;
    state.db.set_post_quarantined(&post_id, true).await.unwrap();
    state.db.add_admin_actor(&admin).await.unwrap();

    let bytes = dispatch_train(&router, state, admin, content_id)
        .await
        .expect("an admin may train on a quarantined post");
    let reply: ModerationTrainReply = decode(&bytes).unwrap();
    assert_eq!(reply.status, "trained");
}

/// A **taken-down** post takes no verdict from anybody — not a stranger, not an
/// admin, not even its author (`moderation.md` § Legal takedown → *Posts*: the
/// post is withheld from every viewer). `train` reads through `get_post_core`,
/// inheriting the takedown gate ahead of quarantine, so each caller gets the
/// same `not_found` as an absent post, no report is captured, and no model is
/// written. (Until 2026-09-10 the handler checked quarantine alone and trained
/// on the body, so anyone could pull a taken-down post's withheld n-grams out
/// of their own model; the nest no longer trains at all.) The quarantine arm
/// of the same core is pinned by the `train_on_quarantined_*` tests above.
#[tokio::test]
async fn train_on_taken_down_post_is_withheld_from_every_caller() {
    let (router, state) = router_with_db_only().await;
    let (post_id, content_id) = post_id_and_hex(0x2c);
    let author = [0x2du8; 32];
    let admin = [0x2eu8; 32];
    let stranger = [0x2fu8; 32];
    seed_post(&state, &post_id, author, "free crypto click now").await;
    state.db.add_admin_actor(&admin).await.unwrap();
    state
        .db
        .set_post_legal_takedown(&post_id, Some("EU-DSA-2024/901"))
        .await
        .unwrap();

    for caller in [author, admin, stranger] {
        state.db.set_share_reports(&caller, true).await.unwrap();
        let err = dispatch_train(&router, state.clone(), caller, content_id.clone())
            .await
            .expect_err("a taken-down post takes a verdict from nobody");
        assert_eq!(err.code, "fauna.moderation.not_found");
        assert!(
            state.db.get_spam_model(&caller).await.unwrap().is_none(),
            "no model was written for the caller"
        );
        assert!(
            !state
                .db
                .delete_content_report(&post_report_key(post_id), &caller)
                .await
                .unwrap(),
            "no report was captured"
        );
    }
}

/// The `report:spam` key a post's verdict captures under — a post's
/// content-addressed id IS its report-hash.
fn post_report_key(post_id: [u8; 32]) -> ReportKey {
    ReportKey {
        content_hash: post_id,
        factor: fauna_core::scoring::factor::REPORT_SPAM.to_string(),
        content_kind: "post".into(),
    }
}

/// The feature is preserved for the common case: a non-author may train on a
/// **non-quarantined** (world-readable) post — marking a post you can see as
/// spam is exactly the intended flow.
#[tokio::test]
async fn train_on_non_quarantined_post_by_non_author_succeeds() {
    let (router, state) = router_with_db_only().await;
    let (post_id, content_id) = post_id_and_hex(0x29);
    let author = [0x2au8; 32];
    let reader = [0x2bu8; 32];
    seed_post(&state, &post_id, author, "free crypto click now").await;
    // Not quarantined ⇒ world-readable.

    let bytes = dispatch_train(&router, state, reader, content_id)
        .await
        .expect("anyone may train on a world-readable post");
    let reply: ModerationTrainReply = decode(&bytes).unwrap();
    assert_eq!(reply.status, "trained");
}

/// `fauna.moderation.train` is the NEST half of a training correction only:
/// on a post the caller may read, an opted-in caller's spam verdict captures
/// the report (and a ham verdict withdraws it), the reply acknowledges the
/// correction — and `spam_models` is never touched. The per-user model rests
/// sealed; its half of the correction is the client's own sealed
/// `put_spam_model`.
#[tokio::test]
async fn train_captures_the_report_and_writes_no_model() {
    let (router, state) = router_with_db_only().await;
    let (post_id, content_id) = post_id_and_hex(0x30);
    let author = [0x31u8; 32];
    let reader = [0x32u8; 32];
    seed_post(&state, &post_id, author, "free crypto click now").await;
    state.db.set_share_reports(&reader, true).await.unwrap();

    let bytes = dispatch_train(&router, state.clone(), reader, content_id.clone())
        .await
        .expect("a readable post takes the verdict");
    let reply: ModerationTrainReply = decode(&bytes).unwrap();
    assert_eq!(reply.status, "trained");
    assert_eq!(reply.verdict, "spam");
    assert!(
        state.db.get_spam_model(&reader).await.unwrap().is_none(),
        "the nest wrote no model"
    );
    // The spam verdict captured the reader's report: withdrawing it finds a row.
    assert!(
        state
            .db
            .delete_content_report(&post_report_key(post_id), &reader)
            .await
            .unwrap(),
        "the spam verdict captured a report"
    );

    // Re-capture, then a ham verdict withdraws it through the handler.
    dispatch_train(&router, state.clone(), reader, content_id.clone())
        .await
        .expect("spam again");
    dispatch(
        &router,
        state.clone(),
        reader,
        "fauna.moderation.train",
        pack(&ModerationTrainRequest {
            content_id,
            verdict: "ham".into(),
            extra: Default::default(),
        }),
    )
    .await
    .expect("ham verdict");
    assert!(
        !state
            .db
            .delete_content_report(&post_report_key(post_id), &reader)
            .await
            .unwrap(),
        "the ham verdict withdrew the report"
    );
    assert!(state.db.get_spam_model(&reader).await.unwrap().is_none());
}

/// `train` is per-actor rate-limited. Pre-exhaust the
/// actor's bucket directly (the same key the handler uses), then the next
/// handler call is throttled with `fauna.moderation.rate_limited`.
#[tokio::test]
async fn train_is_per_actor_rate_limited() {
    let (router, state) = router_with_db_only().await;
    let actor = [13u8; 32];

    // Drain the bucket: `.check` returns true until the window is full, then
    // false. After the loop the bucket is exhausted, so the handler's own
    // `.check` rejects the request.
    while state
        .spam_train_rate_limit
        .check(&actor, &actor, "moderation.train")
    {}

    let err = dispatch_train(&router, state, actor, "11".repeat(32))
        .await
        .expect_err("over the per-actor limit");
    assert_eq!(err.code, "fauna.moderation.rate_limited");
}

#[tokio::test]
async fn moderation_kinds_are_user_admin_at_allowlist_layer() {
    for kind in KINDS {
        for class in [CallerClass::User, CallerClass::Admin] {
            assert!(
                is_permitted(class, kind),
                "{kind} should be permitted for {class:?}"
            );
        }
        for class in [CallerClass::BridgeMta, CallerClass::BridgeMda] {
            assert!(
                !is_permitted(class, kind),
                "{kind} should be denied for {class:?}"
            );
        }
    }
}

// ── fauna.moderation.legal_takedown ────────────────────────────────────
//
// The narrow legal-compulsion social-takedown carve-out (moderation.md
// § Categories & enforcement item 1). Admin-only; requires a legal reference;
// tombstone-not-delete + appealable + audited.

fn pack_takedown(content_id: &str, legal_reference: &str, restore: bool) -> Bytes {
    pack(&ModerationLegalTakedownRequest {
        content_id: content_id.to_string(),
        content_type: "post".into(),
        legal_reference: legal_reference.to_string(),
        restore,
        extra: Default::default(),
    })
}

#[test]
fn legal_takedown_is_admin_only_at_allowlist_layer() {
    assert!(
        is_permitted(CallerClass::Admin, "fauna.moderation.legal_takedown"),
        "admin may take down under legal obligation"
    );
    for class in [
        CallerClass::User,
        CallerClass::BridgeMta,
        CallerClass::BridgeMda,
        CallerClass::ContentProcessor,
    ] {
        assert!(
            !is_permitted(class, "fauna.moderation.legal_takedown"),
            "legal_takedown must be denied for {class:?} (no operator; not a User lever)"
        );
    }
}

/// A non-admin User cannot take down a post — `permission_denied` (the handler
/// gates on the Admin-only allowlist arm). This is the structural guard that a
/// User can never remove another user's content.
#[tokio::test]
async fn legal_takedown_denied_for_non_admin() {
    let (router, state) = router_with_db_only().await;
    let (post_id, content_id) = post_id_and_hex(0x41);
    seed_post(&state, &post_id, [0x42u8; 32], "hello").await;
    // Caller is an unknown actor → classified User.
    let err = dispatch(
        &router,
        state,
        [0x43u8; 32],
        "fauna.moderation.legal_takedown",
        pack_takedown(&content_id, "EU-DSA-2024/1", false),
    )
    .await
    .expect_err("a non-admin cannot take down content");
    assert_eq!(err.code, "fauna.moderation.permission_denied");
}

/// A takedown REQUIRES a non-empty legal reference — the structural guard that
/// this is compulsion, not policy/opinion. An admin with a blank reference is
/// rejected `invalid_params`.
#[tokio::test]
async fn legal_takedown_requires_legal_reference() {
    let (router, state) = router_with_db_only().await;
    let admin = [0x51u8; 32];
    state.db.add_admin_actor(&admin).await.unwrap();
    let (post_id, content_id) = post_id_and_hex(0x52);
    seed_post(&state, &post_id, [0x53u8; 32], "hello").await;
    let err = dispatch(
        &router,
        state,
        admin,
        "fauna.moderation.legal_takedown",
        pack_takedown(&content_id, "   ", false),
    )
    .await
    .expect_err("a takedown with no legal reference is rejected");
    assert_eq!(err.code, "fauna.moderation.invalid_params");
}

/// Taking down an absent post is `not_found` (nothing to withhold).
#[tokio::test]
async fn legal_takedown_missing_post_is_not_found() {
    let (router, state) = router_with_db_only().await;
    let admin = [0x61u8; 32];
    state.db.add_admin_actor(&admin).await.unwrap();
    let (_pid, content_id) = post_id_and_hex(0x62); // never seeded
    let err = dispatch(
        &router,
        state,
        admin,
        "fauna.moderation.legal_takedown",
        pack_takedown(&content_id, "EU-DSA-2024/1", false),
    )
    .await
    .expect_err("takedown of an absent post is not_found");
    assert_eq!(err.code, "fauna.moderation.not_found");
}

/// End-to-end: admin takes down a seeded post → the `content_meta` flag carries
/// the reference; an `ObligationAction::TakenDown` (7) row surfaces to the
/// AUTHOR's `fauna.moderation.actions` queue; `restore` clears the flag
/// (tombstone-not-delete — the content row survives). Never a silent removal.
#[tokio::test]
async fn legal_takedown_and_restore_flow() {
    let (router, state) = router_with_db_only().await;
    let admin = [0x71u8; 32];
    state.db.add_admin_actor(&admin).await.unwrap();
    let author = [0x72u8; 32];
    let (post_id, content_id) = post_id_and_hex(0x73);
    seed_post(&state, &post_id, author, "genuinely illegal content").await;

    // Live by default.
    assert_eq!(
        state.db.get_post_legal_takedown(&post_id).await.unwrap(),
        None
    );

    // Take it down.
    let bytes = dispatch(
        &router,
        state.clone(),
        admin,
        "fauna.moderation.legal_takedown",
        pack_takedown(&content_id, "EU-DSA-2024/12345", false),
    )
    .await
    .expect("admin takedown ok");
    let reply: ModerationLegalTakedownReply = decode(&bytes).unwrap();
    assert_eq!(reply.status, "taken_down");
    assert_eq!(reply.content_id, content_id);

    // The flag now carries the legal reference.
    assert_eq!(
        state
            .db
            .get_post_legal_takedown(&post_id)
            .await
            .unwrap()
            .as_deref(),
        Some("EU-DSA-2024/12345")
    );

    // The AUTHOR sees a TakenDown (7) obligation row in their queue — never
    // silent: the author learns of the removal and can appeal.
    let bytes = dispatch(
        &router,
        state.clone(),
        author,
        "fauna.moderation.actions",
        pack(&ModerationActionsRequest::default()),
    )
    .await
    .expect("author reads their queue");
    let actions: ModerationActionsReply = decode(&bytes).unwrap();
    let row = actions
        .actions
        .iter()
        .find(|a| a.content_id == content_id)
        .expect("the author's queue carries the takedown row");
    assert_eq!(
        row.action,
        fauna_core::obligation::ObligationAction::TakenDown as u8
    );

    // Restore (overturned appeal) clears the flag; the content row survives.
    let bytes = dispatch(
        &router,
        state.clone(),
        admin,
        "fauna.moderation.legal_takedown",
        pack_takedown(&content_id, "appeal upheld", true),
    )
    .await
    .expect("admin restore ok");
    let reply: ModerationLegalTakedownReply = decode(&bytes).unwrap();
    assert_eq!(reply.status, "restored");
    assert_eq!(
        state.db.get_post_legal_takedown(&post_id).await.unwrap(),
        None
    );
    // Tombstone-not-delete: the post is still stored (re-serves after restore).
    assert!(state.db.get_post(&post_id).await.unwrap().is_some());
}

/// A router over an `AppState` whose web-content service is wired (a real disk
/// blob store in a tempdir), so the takedown handler's web re-render runs.
async fn router_with_web_site() -> (RpcRouter, Arc<AppState>, tempfile::TempDir) {
    let db = Arc::new(CacheDb::open_in_memory().unwrap());
    let blobs = tempfile::tempdir().unwrap();
    let store = Arc::new(fauna_nest::blob_store::DiskBlobStore::new(blobs.path()).unwrap());
    let state = Arc::new(AppState {
        web_content_service: Some(Arc::new(
            fauna_nest::web_content::service::WebContentService::new(db.clone(), store),
        )),
        ..AppState::for_test(db)
    });
    let mut b = RpcRouter::builder();
    moderation_handlers::register_moderation_handlers(&mut b);
    (b.build(), state, blobs)
}

/// **A takedown shuts the author's web site before it replies, and an overturn
/// reopens it** (`moderation.md` § Legal takedown → *Posts*).
///
/// A published post is rendered once into static pages — `post/{slug}.html`,
/// the index, `feed.xml` — served to the open internet, and nothing re-renders
/// them on a schedule. So gating the render's enumeration alone is not enough:
/// the page rendered BEFORE the takedown would keep serving the withheld body
/// until some unrelated publish happened to re-render. The handler therefore
/// re-renders synchronously, the way it rebuilds the blob withhold, and this
/// pins the whole flow: rendered → taken down → page gone (and absent from the
/// index and feed) → restored → page back.
#[tokio::test]
async fn a_takedown_shuts_the_authors_web_page_and_a_restore_reopens_it() {
    let (router, state, _blobs) = router_with_web_site().await;
    let admin = [0x74u8; 32];
    state.db.add_admin_actor(&admin).await.unwrap();
    let author = [0x75u8; 32];
    let (post_id, content_id) = post_id_and_hex(0x76);
    seed_post(&state, &post_id, author, "a-published-body-8d4c").await;
    state
        .db
        .publish_web_post(&author, &post_id, "the-post")
        .await
        .unwrap();
    let wcs = state.web_content_service.clone().unwrap();
    wcs.render_published_posts(&author).await.unwrap();

    async fn rendered(state: &AppState, author: &[u8; 32], path: &str) -> bool {
        state
            .db
            .get_web_rendered(author, path)
            .await
            .unwrap()
            .is_some()
    }
    const PAGE: &str = "post/the-post.html";
    assert!(
        rendered(&state, &author, PAGE).await,
        "precondition: the published post rendered a page"
    );

    let bytes = dispatch(
        &router,
        state.clone(),
        admin,
        "fauna.moderation.legal_takedown",
        pack_takedown(&content_id, "EU-DSA-2024/902", false),
    )
    .await
    .expect("admin takedown ok");
    let reply: ModerationLegalTakedownReply = decode(&bytes).unwrap();
    assert_eq!(reply.status, "taken_down");
    assert!(
        !rendered(&state, &author, PAGE).await,
        "\"taken_down\" must mean the post's web page is already gone, not that it will go \
         at some future render"
    );
    for listing in ["index.html", "feed.xml"] {
        assert!(
            !rendered(&state, &author, listing).await,
            "{listing} listed only the taken-down post, so it must be gone with it"
        );
    }

    let bytes = dispatch(
        &router,
        state.clone(),
        admin,
        "fauna.moderation.legal_takedown",
        pack_takedown(&content_id, "appeal upheld", true),
    )
    .await
    .expect("admin restore ok");
    let reply: ModerationLegalTakedownReply = decode(&bytes).unwrap();
    assert_eq!(reply.status, "restored");
    assert!(
        rendered(&state, &author, PAGE).await,
        "an overturn re-renders: the author's publish link still stands, so the page returns"
    );
}

/// F1 (review 2026-07-06, false-compliance guard): a post whose `content` row
/// exists but whose `content_meta` row is missing must FAIL the takedown
/// loudly — never reply `"taken_down"` (nor audit / enqueue an obligation row)
/// for a withhold flag that was not actually written. The state is reachable:
/// the existence check reads `content.author` while the flag lives on
/// `content_meta`, and `put_post`'s meta co-write is best-effort (its
/// raw/undecodable branch skips `write_post_index` entirely — used here).
#[tokio::test]
async fn legal_takedown_without_content_meta_row_fails_loudly() {
    let (router, state) = router_with_db_only().await;
    let admin = [0x81u8; 32];
    state.db.add_admin_actor(&admin).await.unwrap();
    let (post_id, content_id) = post_id_and_hex(0x82);
    // Undecodable bytes take `put_post`'s raw branch: a `content` row lands
    // (the existence check passes) with NO `content_meta` row (the flag
    // UPDATE matches nothing).
    state
        .db
        .put_post(&post_id, &[0xff, 0xff, 0xff], None)
        .await
        .unwrap();

    let err = dispatch(
        &router,
        state.clone(),
        admin,
        "fauna.moderation.legal_takedown",
        pack_takedown(&content_id, "EU-DSA-2024/1", false),
    )
    .await
    .expect_err("a flag write that matches no content_meta row must fail, not report taken_down");
    assert_eq!(err.code, "fauna.protocol.internal");

    // The transaction rolled back: no false "taken_down" audit row and no
    // obligation row surfacing a takedown that withheld nothing.
    let audits = state.db.list_audit(50, None).await.unwrap();
    assert!(
        audits
            .iter()
            .all(|a| a.action != "moderation:legal-takedown"),
        "a failed takedown must not audit taken_down"
    );
    assert!(
        state
            .db
            .get_obligation_actions("post", &content_id)
            .await
            .unwrap()
            .is_empty(),
        "a failed takedown must not enqueue an obligation row"
    );
    // And the post is (still) not flagged.
    assert_eq!(
        state.db.get_post_legal_takedown(&post_id).await.unwrap(),
        None
    );
}

// ── fauna.moderation.report_share.{set,status} ─────────────────────────
//
// The distributed-report-sharing opt-in + transparency surface
// (`report-sharing.md` § Client wire + transparency surface). These prove the
// wire pair over the real handlers + `CacheDb` seams: caller-scoping, the
// `published == k-gated export` identity, and the opt-out sweep through the
// wire.

fn report_key(hash: [u8; 32]) -> ReportKey {
    ReportKey {
        content_hash: hash,
        factor: fauna_core::scoring::factor::REPORT_SPAM.to_string(),
        content_kind: "mail".into(),
    }
}

async fn get_status(
    router: &RpcRouter,
    state: &Arc<AppState>,
    actor: [u8; 32],
) -> ModerationReportShareStatusReply {
    let bytes = dispatch(
        router,
        state.clone(),
        actor,
        "fauna.moderation.report_share.status",
        pack(&ModerationReportShareStatusRequest::default()),
    )
    .await
    .expect("status dispatch");
    decode(&bytes).expect("decode status reply")
}

async fn set_share(
    router: &RpcRouter,
    state: &Arc<AppState>,
    actor: [u8; 32],
    share: bool,
) -> ModerationReportShareSetReply {
    let bytes = dispatch(
        router,
        state.clone(),
        actor,
        "fauna.moderation.report_share.set",
        pack(&ModerationReportShareSetRequest {
            share,
            extra: Default::default(),
        }),
    )
    .await
    .expect("set dispatch");
    decode(&bytes).expect("decode set reply")
}

#[tokio::test]
async fn report_share_status_fresh_actor_is_off_with_empty_published() {
    let (router, state) = router_with_db_only().await;
    let reply = get_status(&router, &state, [7u8; 32]).await;
    assert!(!reply.share, "default opt-in is off");
    assert!(reply.published.is_empty(), "a fresh nest publishes nothing");
}

#[tokio::test]
async fn report_share_set_opt_in_echoes_and_status_reflects_it() {
    let (router, state) = router_with_db_only().await;
    let actor = [3u8; 32];
    let set_reply = set_share(&router, &state, actor, true).await;
    assert!(set_reply.share, "set echoes the new state");
    let status = get_status(&router, &state, actor).await;
    assert!(status.share, "status reflects the opt-in");
    // Opting in alone publishes nothing (no reports captured yet).
    assert!(status.published.is_empty());
    // And the DB seam agrees.
    assert!(state.db.share_reports_enabled(&actor).await.unwrap());
}

#[tokio::test]
async fn report_share_set_is_caller_scoped() {
    let (router, state) = router_with_db_only().await;
    let a = [10u8; 32];
    let b = [20u8; 32];
    // A opts in.
    assert!(set_share(&router, &state, a, true).await.share);
    // A sees itself opted in; B — who never called set — stays off. A's set
    // wrote only A's own row (no `actor_id` on the wire — an admin couldn't
    // flip B's either).
    assert!(get_status(&router, &state, a).await.share);
    assert!(
        !get_status(&router, &state, b).await.share,
        "one actor's opt-in must not touch another's"
    );
    assert!(!state.db.share_reports_enabled(&b).await.unwrap());
}

#[tokio::test]
async fn report_share_published_is_the_k_gated_export_view() {
    let (router, state) = router_with_db_only().await;
    let hash = [0x42u8; 32];
    let key = report_key(hash);
    let reporters = [[31u8; 32], [32u8; 32], [33u8; 32]];

    // Two opted-in reporters flag the same content: below k=3 → nothing
    // readable anywhere, including this transparency surface.
    for r in &reporters[..2] {
        state.db.set_share_reports(r, true).await.unwrap();
        capture_report(&state.db, r, &key, true).await.unwrap();
    }
    let status = get_status(&router, &state, reporters[0]).await;
    assert!(
        status.published.is_empty(),
        "below k the aggregate is invisible on the transparency surface"
    );

    // The third opted-in reporter crosses k → the aggregate becomes readable.
    state
        .db
        .set_share_reports(&reporters[2], true)
        .await
        .unwrap();
    capture_report(&state.db, &reporters[2], &key, true)
        .await
        .unwrap();
    let status = get_status(&router, &state, reporters[0]).await;
    assert_eq!(status.published.len(), 1, "one aggregate crossed k");
    let entry = &status.published[0];
    assert_eq!(entry.content_hash, hex::encode(hash));
    assert_eq!(entry.factor, "report:spam");
    assert_eq!(
        entry.count, 3,
        "a local reporter count, not a below-k value"
    );

    // The published list is BYTE-IDENTICAL to the federation export view —
    // the same ≥k list a peer nest would receive (the transparency guarantee).
    let export: Vec<(String, String, u32)> = state
        .db
        .export_report_aggregates()
        .await
        .unwrap()
        .into_iter()
        .map(|(h, f, c)| (hex::encode(h), f, c))
        .collect();
    let published: Vec<(String, String, u32)> = status
        .published
        .iter()
        .map(|e| (e.content_hash.clone(), e.factor.clone(), e.count))
        .collect();
    assert_eq!(
        published, export,
        "status.published must equal the federation export exactly"
    );
}

#[tokio::test]
async fn report_share_opt_out_withdraws_through_the_wire() {
    let (router, state) = router_with_db_only().await;
    let hash = [0x55u8; 32];
    let key = report_key(hash);
    let reporters = [[41u8; 32], [42u8; 32], [43u8; 32]];
    for r in &reporters {
        state.db.set_share_reports(r, true).await.unwrap();
        capture_report(&state.db, r, &key, true).await.unwrap();
    }
    // k=3 reached → published shows the aggregate.
    assert_eq!(
        get_status(&router, &state, reporters[0])
            .await
            .published
            .len(),
        1
    );
    // One reporter opts out through the wire: their row is swept and the
    // aggregate recomputes below k → it is withdrawn everywhere, including here.
    let set_reply = set_share(&router, &state, reporters[0], false).await;
    assert!(!set_reply.share);
    assert!(
        get_status(&router, &state, reporters[1])
            .await
            .published
            .is_empty(),
        "opt-out drops the aggregate below k and withdraws it"
    );
}

#[tokio::test]
async fn report_share_kinds_are_user_and_admin_gated() {
    for kind in [
        "fauna.moderation.report_share.set",
        "fauna.moderation.report_share.status",
    ] {
        for class in [CallerClass::User, CallerClass::Admin] {
            assert!(is_permitted(class, kind), "{kind} permitted for {class:?}");
        }
        for class in [CallerClass::BridgeMta, CallerClass::BridgeMda] {
            assert!(
                !is_permitted(class, kind),
                "{kind} denied for bridge class {class:?}"
            );
        }
    }
}

// ── fauna.moderation.signal_share.{set,status} + signal_contribute ─────
//
// The Layer-B engagement-cue surface (`engagement-cues.md` § Layer B nest
// legs) over the real handlers + `CacheDb` seams: caller-scoping, the
// `published == export` identity (the SAME export as `report_share.status`),
// the public-post write gate on `signal_contribute`, and the INDEPENDENCE of
// the two opt-in families (`share_reports` / `share_signals`).

async fn signal_get_status(
    router: &RpcRouter,
    state: &Arc<AppState>,
    actor: [u8; 32],
) -> ModerationSignalShareStatusReply {
    let bytes = dispatch(
        router,
        state.clone(),
        actor,
        "fauna.moderation.signal_share.status",
        pack(&ModerationSignalShareStatusRequest::default()),
    )
    .await
    .expect("signal status dispatch");
    decode(&bytes).expect("decode signal status reply")
}

async fn signal_set_share(
    router: &RpcRouter,
    state: &Arc<AppState>,
    actor: [u8; 32],
    share: bool,
) -> ModerationSignalShareSetReply {
    let bytes = dispatch(
        router,
        state.clone(),
        actor,
        "fauna.moderation.signal_share.set",
        pack(&ModerationSignalShareSetRequest {
            share,
            extra: Default::default(),
        }),
    )
    .await
    .expect("signal set dispatch");
    decode(&bytes).expect("decode signal set reply")
}

async fn signal_contribute(
    router: &RpcRouter,
    state: &Arc<AppState>,
    actor: [u8; 32],
    content_id_hex: &str,
    signal: &str,
) -> Result<ModerationSignalContributeReply, RpcError> {
    let bytes = dispatch(
        router,
        state.clone(),
        actor,
        "fauna.moderation.signal_contribute",
        pack(&ModerationSignalContributeRequest {
            content_id: content_id_hex.into(),
            signal: signal.into(),
            extra: Default::default(),
        }),
    )
    .await?;
    Ok(decode(&bytes).expect("decode signal contribute reply"))
}

#[tokio::test]
async fn signal_share_status_fresh_actor_is_off_with_empty_published() {
    let (router, state) = router_with_db_only().await;
    let reply = signal_get_status(&router, &state, [8u8; 32]).await;
    assert!(!reply.share, "default signal opt-in is off");
    assert!(reply.published.is_empty(), "a fresh nest publishes nothing");
}

#[tokio::test]
async fn signal_share_set_opt_in_echoes_and_is_caller_scoped() {
    let (router, state) = router_with_db_only().await;
    let a = [11u8; 32];
    let b = [21u8; 32];
    assert!(signal_set_share(&router, &state, a, true).await.share);
    assert!(signal_get_status(&router, &state, a).await.share);
    // The DB seam agrees, and B — who never called set — stays off (caller-scoped).
    assert!(state.db.share_signals_enabled(&a).await.unwrap());
    assert!(
        !signal_get_status(&router, &state, b).await.share,
        "one actor's signal opt-in must not touch another's"
    );
    assert!(!state.db.share_signals_enabled(&b).await.unwrap());
}

#[tokio::test]
async fn signal_published_is_the_shared_k_gated_export_view() {
    // `signal_share.status.published` is the SAME export as
    // `report_share.status` — one function, byte-identical to the peer export.
    let (router, state) = router_with_db_only().await;
    let hash = [0x71u8; 32];
    let contributors = [[51u8; 32], [52u8; 32], [53u8; 32]];
    for c in &contributors {
        state.db.set_share_signals(c, true).await.unwrap();
        state
            .db
            .capture_signal(c, &hash, SignalVerdict::WatchComplete)
            .await
            .unwrap();
    }
    let status = signal_get_status(&router, &state, contributors[0]).await;
    assert_eq!(status.published.len(), 1, "one signal aggregate crossed k");
    let entry = &status.published[0];
    assert_eq!(entry.content_hash, hex::encode(hash));
    assert_eq!(entry.factor, "signal:watch-complete");
    assert_eq!(entry.count, 3);
    // Byte-identical to the federation export (the transparency guarantee).
    let export: Vec<(String, String, u32)> = state
        .db
        .export_report_aggregates()
        .await
        .unwrap()
        .into_iter()
        .map(|(h, f, c)| (hex::encode(h), f, c))
        .collect();
    let published: Vec<(String, String, u32)> = status
        .published
        .iter()
        .map(|e| (e.content_hash.clone(), e.factor.clone(), e.count))
        .collect();
    assert_eq!(published, export);
}

#[tokio::test]
async fn signal_opt_out_is_factor_scoped_reports_survive() {
    // THE independence guarantee over the wire: opting out of SIGNAL sharing
    // withdraws only the `signal:*` aggregate — the same actors' `report:spam`
    // aggregate is untouched.
    //
    // Wire-level sibling of the lib pin
    // `db::signals::tests::opt_out_is_factor_scoped_reports_and_signals_are_independent`
    // (`src/db/signals.rs`) — a future session retiring either should know the
    // other exists ().
    let (router, state) = router_with_db_only().await;
    let hash = [0x72u8; 32];
    let report_key = report_key(hash); // content_kind irrelevant to the export view
    let actors = [[61u8; 32], [62u8; 32], [63u8; 32]];
    for a in &actors {
        state.db.set_share_reports(a, true).await.unwrap();
        state.db.set_share_signals(a, true).await.unwrap();
        capture_report(&state.db, a, &report_key, true)
            .await
            .unwrap();
        state
            .db
            .capture_signal(a, &hash, SignalVerdict::WatchComplete)
            .await
            .unwrap();
    }
    // Both aggregates published at k=3 (the export carries both families).
    let factors: Vec<String> = signal_get_status(&router, &state, actors[0])
        .await
        .published
        .into_iter()
        .map(|e| e.factor)
        .collect();
    assert!(factors.contains(&"report:spam".to_string()));
    assert!(factors.contains(&"signal:watch-complete".to_string()));

    // One actor opts out of signals through the wire → signal aggregate drops
    // below k (withdrawn); report:spam stays at 3.
    assert!(
        !signal_set_share(&router, &state, actors[0], false)
            .await
            .share
    );
    let factors: Vec<String> = signal_get_status(&router, &state, actors[1])
        .await
        .published
        .into_iter()
        .map(|e| e.factor)
        .collect();
    assert!(
        factors.contains(&"report:spam".to_string()),
        "a signal opt-out must NOT withdraw the report aggregate"
    );
    assert!(
        !factors.contains(&"signal:watch-complete".to_string()),
        "the signal aggregate falls below k after one opt-out"
    );
}

#[tokio::test]
async fn signal_contribute_rejects_non_public_but_withdraw_is_exempt() {
    let (router, state) = router_with_db_only().await;
    let contributor = [71u8; 32];
    state
        .db
        .set_share_signals(&contributor, true)
        .await
        .unwrap();
    let unseen = "ab".repeat(32); // no content_meta row → not public

    // A NEW verdict about a non-public (here unseen) post is rejected at the
    // handler — its aggregate would leak readership.
    let err = signal_contribute(&router, &state, contributor, &unseen, "watch-complete")
        .await
        .expect_err("a verdict about a non-public post is rejected");
    assert_eq!(err.code, "fauna.moderation.invalid_params");

    // A withdraw is exempt from the public gate (a retraction reveals nothing
    // and must always succeed).
    let ok = signal_contribute(&router, &state, contributor, &unseen, "withdraw")
        .await
        .expect("withdraw is exempt from the public gate");
    assert_eq!(ok.status, "recorded");

    // An unrecognised verdict is rejected.
    let err = signal_contribute(&router, &state, contributor, &unseen, "bogus")
        .await
        .expect_err("unknown verdict rejected");
    assert_eq!(err.code, "fauna.moderation.invalid_params");
}

#[tokio::test]
async fn signal_kinds_are_user_and_admin_gated() {
    for kind in [
        "fauna.moderation.signal_share.set",
        "fauna.moderation.signal_share.status",
        "fauna.moderation.signal_contribute",
    ] {
        for class in [CallerClass::User, CallerClass::Admin] {
            assert!(is_permitted(class, kind), "{kind} permitted for {class:?}");
        }
        for class in [CallerClass::BridgeMta, CallerClass::BridgeMda] {
            assert!(
                !is_permitted(class, kind),
                "{kind} denied for bridge class {class:?}"
            );
        }
    }
}

// ── fauna.moderation.abuse_report.* (moderation.md § User-initiated reporting) ──

fn submit_req(cid: &str, note: Option<&str>) -> Bytes {
    pack(&AbuseReportSubmitRequest {
        subject: AbuseReportSubject::Post { cid: cid.into() },
        reason: AbuseReportReason::Harassment,
        note: note.map(str::to_string),
        excerpt: None,
        block_author: false,
        subject_actor: None,
        extra: Default::default(),
    })
}

async fn notif_types(state: &Arc<AppState>, actor: &[u8; 32]) -> Vec<String> {
    state
        .db
        .list_notifications(actor, None, 100)
        .await
        .unwrap()
        .into_iter()
        .map(|n| n.notif_type.to_string())
        .collect()
}

async fn submit(
    router: &RpcRouter,
    state: &Arc<AppState>,
    reporter: [u8; 32],
    cid: &str,
) -> AbuseReportSubmitReply {
    let bytes = dispatch(
        router,
        state.clone(),
        reporter,
        "fauna.moderation.abuse_report.submit",
        submit_req(cid, Some("  repeated slurs ")),
    )
    .await
    .expect("submit ok");
    decode(&bytes).unwrap()
}

async fn queue(
    router: &RpcRouter,
    state: &Arc<AppState>,
    admin: [u8; 32],
) -> AbuseReportQueueReply {
    let bytes = dispatch(
        router,
        state.clone(),
        admin,
        "fauna.moderation.abuse_report.queue",
        pack(&AbuseReportQueueRequest::default()),
    )
    .await
    .expect("queue ok");
    decode(&bytes).unwrap()
}

async fn mine(
    router: &RpcRouter,
    state: &Arc<AppState>,
    reporter: [u8; 32],
) -> AbuseReportMineReply {
    let bytes = dispatch(
        router,
        state.clone(),
        reporter,
        "fauna.moderation.abuse_report.mine",
        pack(&AbuseReportMineRequest::default()),
    )
    .await
    .expect("mine ok");
    decode(&bytes).unwrap()
}

fn resolve_req(report_id: &str, outcome: AbuseReportOutcome) -> Bytes {
    pack(&AbuseReportResolveRequest {
        report_id: report_id.into(),
        outcome,
        extra: Default::default(),
    })
}

/// The local loop: report → the admin's doorbell → the queue row (with the
/// author the nest resolved, and the reporter named) → resolve → the reporter
/// is told the outcome only. The author hears nothing, and no other plane is
/// written (no label on the post).
#[tokio::test]
async fn abuse_report_local_loop_reaches_the_admin_and_returns_the_outcome() {
    let (router, state) = router_with_db_only().await;
    let admin = [0xa1u8; 32];
    state.db.add_admin_actor(&admin).await.unwrap();
    let author = [0xa2u8; 32];
    let reporter = [0xa3u8; 32];
    let (post_id, cid) = post_id_and_hex(0xa4);
    seed_post(&state, &post_id, author, "something abusive").await;

    let reply = submit(&router, &state, reporter, &cid).await;
    assert_eq!(reply.routed_to, vec![state.handle_domain()]);
    assert_eq!(
        notif_types(&state, &admin).await,
        vec!["abuse_report.received"]
    );

    let q = queue(&router, &state, admin).await;
    assert_eq!(q.reports.len(), 1);
    let row = &q.reports[0];
    assert_eq!(row.report_id, reply.report_id);
    assert_eq!(row.subject, AbuseReportSubject::Post { cid: cid.clone() });
    assert_eq!(
        row.subject_actor.as_deref(),
        Some(hex::encode(author).as_str())
    );
    assert_eq!(row.note.as_deref(), Some("repeated slurs"));
    assert!(row.reporter_handle.is_some(), "a local reporter is named");
    assert!(row.origin_nest.is_none());

    dispatch(
        &router,
        state.clone(),
        admin,
        "fauna.moderation.abuse_report.resolve",
        resolve_req(&reply.report_id, AbuseReportOutcome::Acted),
    )
    .await
    .expect("resolve ok");

    assert_eq!(
        notif_types(&state, &reporter).await,
        vec!["abuse_report.resolved"]
    );
    let ledger = mine(&router, &state, reporter).await;
    assert_eq!(ledger.reports.len(), 1);
    assert_eq!(ledger.reports[0].status, AbuseReportStatus::Resolved);
    assert_eq!(ledger.reports[0].outcome, Some(AbuseReportOutcome::Acted));
    assert!(queue(&router, &state, admin).await.reports.is_empty());

    // The author is told nothing by a report, and the report wrote no label.
    assert!(notif_types(&state, &author).await.is_empty());
    assert!(
        state
            .db
            .get_content_labels("post", &cid)
            .await
            .unwrap()
            .is_empty()
    );

    // A resolve retry with the same outcome is a no-op; a different one conflicts.
    dispatch(
        &router,
        state.clone(),
        admin,
        "fauna.moderation.abuse_report.resolve",
        resolve_req(&reply.report_id, AbuseReportOutcome::Acted),
    )
    .await
    .expect("same-outcome retry is a no-op");
    assert_eq!(notif_types(&state, &reporter).await.len(), 1, "rung once");
    let err = dispatch(
        &router,
        state.clone(),
        admin,
        "fauna.moderation.abuse_report.resolve",
        resolve_req(&reply.report_id, AbuseReportOutcome::Dismissed),
    )
    .await
    .expect_err("a resolved record stands");
    assert_eq!(err.code, "fauna.moderation.conflict");
}

/// One open report per (reporter, subject); a pile-on by several reporters is
/// one doorbell per subject per day, while the queue carries every report.
#[tokio::test]
async fn abuse_report_dedupes_reports_and_doorbells() {
    let (router, state) = router_with_db_only().await;
    let admin = [0xb1u8; 32];
    state.db.add_admin_actor(&admin).await.unwrap();
    let (_, cid) = post_id_and_hex(0xb4);

    let first = submit(&router, &state, [0xb2; 32], &cid).await;
    let retry = submit(&router, &state, [0xb2; 32], &cid).await;
    assert_eq!(first.report_id, retry.report_id);
    submit(&router, &state, [0xb3; 32], &cid).await;

    assert_eq!(queue(&router, &state, admin).await.reports.len(), 2);
    assert_eq!(notif_types(&state, &admin).await.len(), 1, "one doorbell");
}

#[tokio::test]
async fn abuse_report_withdraw_deletes_note_and_leaves_the_queue() {
    let (router, state) = router_with_db_only().await;
    let admin = [0xc1u8; 32];
    state.db.add_admin_actor(&admin).await.unwrap();
    let reporter = [0xc2u8; 32];
    let (_, cid) = post_id_and_hex(0xc4);
    let reply = submit(&router, &state, reporter, &cid).await;

    let withdraw = pack(&AbuseReportWithdrawRequest {
        report_id: reply.report_id.clone(),
        extra: Default::default(),
    });
    // Nobody else can withdraw it.
    let err = dispatch(
        &router,
        state.clone(),
        [0xc3; 32],
        "fauna.moderation.abuse_report.withdraw",
        withdraw.clone(),
    )
    .await
    .expect_err("not theirs");
    assert_eq!(err.code, "fauna.moderation.not_found");
    for _ in 0..2 {
        dispatch(
            &router,
            state.clone(),
            reporter,
            "fauna.moderation.abuse_report.withdraw",
            withdraw.clone(),
        )
        .await
        .expect("withdraw is idempotent");
    }
    assert!(queue(&router, &state, admin).await.reports.is_empty());
    let ledger = mine(&router, &state, reporter).await;
    assert_eq!(ledger.reports[0].status, AbuseReportStatus::Withdrawn);
}

#[tokio::test]
async fn abuse_report_refuses_bad_input() {
    let (router, state) = router_with_db_only().await;
    let (_, cid) = post_id_and_hex(0xd4);
    let long = "x".repeat(fauna_protocol::moderation::MAX_ABUSE_REPORT_NOTE_BYTES + 1);
    for payload in [submit_req("not-hex", None), submit_req(&cid, Some(&long))] {
        let err = dispatch(
            &router,
            state.clone(),
            [0xd1; 32],
            "fauna.moderation.abuse_report.submit",
            payload,
        )
        .await
        .expect_err("refused");
        assert_eq!(err.code, "fauna.moderation.invalid_params");
    }
}

#[tokio::test]
async fn abuse_report_per_hour_cap_refuses_rate_limited() {
    let (router, state) = router_with_db_only().await;
    let reporter = [0xe1u8; 32];
    let cap = fauna_core::scoring::reports::ABUSE_REPORTS_PER_HOUR as u8;
    for i in 0..cap {
        submit(&router, &state, reporter, &format!("{i:02x}").repeat(32)).await;
    }
    let err = dispatch(
        &router,
        state.clone(),
        reporter,
        "fauna.moderation.abuse_report.submit",
        submit_req(&"ff".repeat(32), None),
    )
    .await
    .expect_err("capped");
    assert_eq!(err.code, "fauna.moderation.rate_limited");
}

#[test]
fn abuse_report_queue_and_resolve_are_admin_only() {
    for kind in [
        "fauna.moderation.abuse_report.submit",
        "fauna.moderation.abuse_report.mine",
        "fauna.moderation.abuse_report.withdraw",
    ] {
        assert!(is_permitted(CallerClass::User, kind), "{kind}");
        assert!(is_permitted(CallerClass::Admin, kind), "{kind}");
        assert!(!is_permitted(CallerClass::BridgeMta, kind), "{kind}");
    }
    for kind in [
        "fauna.moderation.abuse_report.queue",
        "fauna.moderation.abuse_report.resolve",
    ] {
        assert!(!is_permitted(CallerClass::User, kind), "{kind}");
        assert!(is_permitted(CallerClass::Admin, kind), "{kind}");
    }
}
