//! **The transparency triple across a nest boundary** —
//! `fauna.federation.moderation.{obligation.deliver,appeal}` between two
//! in-process nests over the federation WS-RPC channel
//! (`docs/goal/behavior/moderation.md` § Legal takedown → *The author homed on
//! another nest*).
//!
//! `A` decides: it holds a projection of a post `B`'s author wrote, or is the
//! home of a channel `B`'s author sits in. A takedown or block on `A` pushes
//! the author's queue row to `B`, where the author reads it; the author's
//! appeal on `B` is relayed to `A`, recorded and bounded on `A`'s chain, and
//! only then noted on `B`'s. Forged, over-budget and wrong-origin legs are
//! refused.
//!
//! Harness mirrors `conformance_abuse_report_federation.rs`.

mod common;
use common::dispatch;

use std::collections::BTreeMap;
use std::sync::Arc;

use bytes::Bytes;
use ed25519_dalek::SigningKey;
use fauna_core::data::{Post, PostBody, Timestamp};
use fauna_core::encoding::canonical_encode;
use fauna_core::identity::ActorId;
use fauna_core::obligation::ObligationAction as Action;
use fauna_nest::config::SubmissionPolicy;
use fauna_nest::db::CacheDb;
use fauna_nest::db::channels::RebindPower;
use fauna_nest::moderation_federation::{APPEAL_KIND, DELIVER_KIND};
use fauna_nest::nest_identity::NestIdentity;
use fauna_nest::routes::AppState;
use fauna_nest::rpc_router::RpcRouter;
use fauna_protocol::moderation::{
    AbuseReportReason, AbuseReportSubject, AbuseReportSubmitReply, AbuseReportSubmitRequest,
    ModerationActionsReply, ModerationActionsRequest, ModerationAdminBlockRequest,
    ModerationAppealReply, ModerationAppealRequest, ModerationLegalTakedownRequest,
    ObligationAction, ObligationAppealRelayRequest, ObligationDeliverRequest,
};
use fauna_protocol::{RpcError, Value, decode_strict as decode, encode_canonical};

struct Nest {
    url: String,
    router: RpcRouter,
    state: Arc<AppState>,
}

impl Nest {
    fn host(&self) -> String {
        self.url.trim_start_matches("http://").to_string()
    }

    fn nest_id(&self) -> [u8; 32] {
        self.state.nest_identity.public_key_bytes()
    }
}

fn rpc_router() -> RpcRouter {
    let mut b = RpcRouter::builder();
    fauna_nest::discovery_handlers::register_discovery_handlers(&mut b);
    fauna_nest::moderation_handlers::register_moderation_handlers(&mut b);
    b.build()
}

async fn start_nest() -> Nest {
    let mut secret = [0u8; 32];
    getrandom::fill(&mut secret).unwrap();
    let signing_key = SigningKey::from_bytes(&secret);
    let verifying_key = signing_key.verifying_key();
    let state = Arc::new(AppState {
        nest_identity: Arc::new(NestIdentity {
            signing_key,
            verifying_key,
        }),
        config: common::test_config(SubmissionPolicy::Open),
        federation_router: Arc::new({
            let mut b = fauna_nest::federation_router::FederationRouter::builder();
            fauna_nest::federation_handlers::register_federation_handlers(&mut b);
            b.build()
        }),
        rpc_router: Arc::new(rpc_router()),
        ..AppState::for_test(Arc::new(CacheDb::open_in_memory().unwrap()))
    });
    let app = fauna_nest::build_router(state.clone());
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        axum::serve(listener, app).await.ok();
    });
    Nest {
        url: format!("http://{addr}"),
        router: rpc_router(),
        state,
    }
}

fn pack<T: serde::Serialize>(req: &T) -> Bytes {
    Bytes::from(encode_canonical(req).unwrap().to_vec())
}

fn value<T: serde::Serialize>(req: &T) -> Value {
    decode(&encode_canonical(req).unwrap()).unwrap()
}

const AUTHOR: [u8; 32] = [0xd1; 32];
const STRANGER: [u8; 32] = [0xd2; 32];
const A_ADMIN: [u8; 32] = [0xd3; 32];
const REPORTER: [u8; 32] = [0xd4; 32];

/// `A` decides; `B` hosts [`AUTHOR`] and — standing in for an earlier dial —
/// holds a proven address for `A`, the one its relayed appeal dials.
async fn pair() -> (Nest, Nest) {
    let a = start_nest().await;
    let b = start_nest().await;
    a.state.db.add_admin_actor(&A_ADMIN).await.unwrap();
    common::seed_user(&b.state.db, &AUTHOR).await;
    common::seed_user(&b.state.db, &STRANGER).await;
    b.state
        .db
        .record_nest_address(&a.nest_id(), &a.url, true)
        .await
        .unwrap();
    (a, b)
}

/// `B` holds [`AUTHOR`]'s post in its own segment store; `A` has indexed a
/// projection of it, arrived from `B`.
async fn seed_foreign_post(a: &Nest, b: &Nest, seed: u8) -> String {
    let post = Post {
        author: ActorId(AUTHOR),
        created_at: Timestamp(1_000_000 + u64::from(seed)),
        body: PostBody::Text {
            content: format!("post {seed}"),
            facets: vec![],
        },
        references: vec![],
        expires_at: None,
        gated: None,
        content_warning: None,
        origin: None,
    };
    let body = canonical_encode(&post).unwrap();
    let post_id = *blake3::hash(&body).as_bytes();
    fauna_nest::segments::post::store_post(
        &b.state.post_segments,
        &b.state.db,
        &post_id,
        &body,
        None,
    )
    .await
    .unwrap();
    a.state
        .db
        .insert_post_index_entry_with_origin(
            &post_id,
            &AUTHOR,
            1_000_000,
            false,
            false,
            "fauna",
            &[],
            Some(&b.url),
        )
        .await
        .unwrap();
    hex::encode(post_id)
}

async fn take_down(a: &Nest, content_type: &str, content_id: &str) {
    dispatch(
        &a.router,
        a.state.clone(),
        A_ADMIN,
        "fauna.moderation.legal_takedown",
        pack(&ModerationLegalTakedownRequest {
            content_id: content_id.into(),
            content_type: content_type.into(),
            legal_reference: "court order 2026/17".into(),
            restore: false,
            extra: Default::default(),
        }),
    )
    .await
    .expect("takedown ok");
}

async fn queue(n: &Nest, caller: [u8; 32]) -> Vec<ObligationAction> {
    let reply: ModerationActionsReply = decode(
        &dispatch(
            &n.router,
            n.state.clone(),
            caller,
            "fauna.moderation.actions",
            pack(&ModerationActionsRequest::default()),
        )
        .await
        .expect("actions ok"),
    )
    .unwrap();
    reply.actions
}

async fn appeal(b: &Nest, caller: [u8; 32], content_id: &str) -> Result<String, RpcError> {
    let bytes = dispatch(
        &b.router,
        b.state.clone(),
        caller,
        "fauna.moderation.appeal",
        pack(&ModerationAppealRequest {
            content_id: content_id.into(),
            reason: "I hold the rights to this".into(),
            extra: Default::default(),
        }),
    )
    .await?;
    let reply: ModerationAppealReply = decode(&bytes).unwrap();
    Ok(reply.status)
}

/// `(actor_id, target, detail)` of every `action` row on `n`'s chain.
async fn audit_rows(n: &Nest, action: &str) -> Vec<(Option<Vec<u8>>, Option<String>, String)> {
    n.state
        .db
        .list_audit(1000, None)
        .await
        .unwrap()
        .into_iter()
        .filter(|r| r.action == action)
        .map(|r| (r.actor_id, r.target, r.detail.unwrap_or_default()))
        .collect()
}

/// `A`'s own row id for `content_id` — the `ref` its delivery carried.
async fn issuer_ref(a: &Nest, content_type: &str, content_id: &str) -> String {
    let rows = a
        .state
        .db
        .get_obligation_actions(content_type, content_id)
        .await
        .unwrap();
    assert_eq!(rows.len(), 1, "one row on the issuer");
    rows[0].id.to_string()
}

/// Originate `kind` from `from` to `to`, the way the queue and the relay do.
async fn originate(from: &Nest, to: &Nest, kind: &str, payload: Value) -> Result<Value, RpcError> {
    fauna_nest::federation_pool::originate_abuse_report_call(
        &from.state.federation_pool,
        &from.state,
        &to.url,
        None,
        kind,
        payload,
    )
    .await
    .expect("the channel opens")
}

fn forged_delivery(issuer_ref: &str, post_id: &str) -> ObligationDeliverRequest {
    ObligationDeliverRequest {
        issuer_ref: issuer_ref.into(),
        content_type: "post".into(),
        content_id: post_id.into(),
        author: hex::encode(AUTHOR),
        action: Action::TakenDown as u8,
        category: "illegal".into(),
        timestamp: 1_700_000_000_000_000,
        issuer_nest_id: "c.example".into(),
        issuer_nest_url: None,
        extra: BTreeMap::new(),
    }
}

/// (a) + (b): a takedown of a projected foreign post on `A` lands the author's
/// row on `B`'s queue, naming `A`; the author's appeal on `B` is recorded on
/// `A`'s chain with the author as actor, and `B` notes the relay only after;
/// a repeat collapses on `A` and `B` notes nothing more.
#[tokio::test]
async fn a_takedown_of_a_projected_post_reaches_the_authors_home_and_the_appeal_returns() {
    let (a, b) = pair().await;
    let post_id = seed_foreign_post(&a, &b, 0xe1).await;

    take_down(&a, "post", &post_id).await;

    let rows = queue(&b, AUTHOR).await;
    assert_eq!(rows.len(), 1, "the author's home serves the delivered row");
    let row = &rows[0];
    assert_eq!(row.content_type, "post");
    assert_eq!(row.content_id, post_id);
    assert_eq!(row.action, Action::TakenDown as u8);
    assert_eq!(row.category, "illegal");
    assert_eq!(row.issuer_nest.as_deref(), Some(a.host().as_str()));
    assert!(queue(&b, STRANGER).await.is_empty());
    // The issuer's own copy is its own decision: no issuer named there.
    let issuer_rows = a
        .state
        .db
        .get_obligation_actions("post", &post_id)
        .await
        .unwrap();
    assert_eq!(issuer_rows[0].issuer_nest_id, None);

    assert_eq!(
        appeal(&b, AUTHOR, &post_id).await.unwrap(),
        "appeal_recorded"
    );
    let on_a = audit_rows(&a, "moderation:appeal").await;
    assert_eq!(on_a.len(), 1, "the issuer holds the appeal");
    assert_eq!(on_a[0].0.as_deref(), Some(AUTHOR.as_slice()));
    assert_eq!(on_a[0].1.as_deref(), Some(post_id.as_str()));
    assert!(on_a[0].2.contains("I hold the rights"));
    assert!(
        audit_rows(&b, "moderation:appeal").await.is_empty(),
        "the home never records a foreign appeal as its own"
    );
    let relayed = audit_rows(&b, "moderation:appeal-relayed").await;
    assert_eq!(relayed.len(), 1);
    assert_eq!(relayed[0].0.as_deref(), Some(AUTHOR.as_slice()));
    assert_eq!(relayed[0].1.as_deref(), Some(post_id.as_str()));
    assert!(relayed[0].2.contains(&hex::encode(a.nest_id())));

    assert_eq!(
        appeal(&b, AUTHOR, &post_id).await.unwrap(),
        "appeal_already_recorded",
        "the issuer's pending bound collapses the repeat"
    );
    assert_eq!(audit_rows(&a, "moderation:appeal").await.len(), 1);
    assert_eq!(
        audit_rows(&b, "moderation:appeal-relayed").await.len(),
        1,
        "no relay row for an appeal the issuer did not newly record"
    );
}

/// (c): a non-author on the home is refused before any relay; a delivery
/// naming a post the author never wrote there is `not_hosted`; past the
/// author's budget a new delivery is `rate_limited`, while a re-delivery of a
/// stored `(origin, ref)` stays free.
#[tokio::test]
async fn the_home_refuses_forged_and_budget_exceeding_deliveries() {
    let (a, b) = pair().await;
    let post_id = seed_foreign_post(&a, &b, 0xe2).await;
    take_down(&a, "post", &post_id).await;

    let err = appeal(&b, STRANGER, &post_id).await.unwrap_err();
    assert!(err.code.ends_with("permission_denied"), "{}", err.code);
    assert!(audit_rows(&a, "moderation:appeal").await.is_empty());

    let c = start_nest().await;
    let never_written = hex::encode([0xe9u8; 32]);
    let err = originate(
        &c,
        &b,
        DELIVER_KIND,
        value(&forged_delivery("1", &never_written)),
    )
    .await
    .unwrap_err();
    assert_eq!(err.code, "fauna.federation.not_found", "{err:?}");

    // `A`'s delivery spent one of the author's ten; nine more new refs land.
    for n in 0..9 {
        originate(
            &c,
            &b,
            DELIVER_KIND,
            value(&forged_delivery(&format!("r{n}"), &post_id)),
        )
        .await
        .unwrap_or_else(|e| panic!("delivery r{n}: {e:?}"));
    }
    let err = originate(
        &c,
        &b,
        DELIVER_KIND,
        value(&forged_delivery("r9", &post_id)),
    )
    .await
    .unwrap_err();
    assert!(err.code.ends_with("rate_limited"), "{err:?}");
    originate(
        &c,
        &b,
        DELIVER_KIND,
        value(&forged_delivery("r0", &post_id)),
    )
    .await
    .expect("a re-delivery of a stored ref is free");
    assert_eq!(queue(&b, AUTHOR).await.len(), 10, "A's row and C's nine");
}

/// (d): the issuer accepts a relayed appeal only from the nest it delivered
/// the row to, for the row's own author.
#[tokio::test]
async fn the_issuer_refuses_an_appeal_from_another_nest() {
    let (a, b) = pair().await;
    let post_id = seed_foreign_post(&a, &b, 0xe3).await;
    take_down(&a, "post", &post_id).await;
    let issuer_ref = issuer_ref(&a, "post", &post_id).await;

    let relay = |author: [u8; 32]| ObligationAppealRelayRequest {
        issuer_ref: issuer_ref.clone(),
        content_id: post_id.clone(),
        author: hex::encode(author),
        reason: "forged".into(),
        extra: BTreeMap::new(),
    };
    let c = start_nest().await;
    let err = originate(&c, &a, APPEAL_KIND, value(&relay(AUTHOR)))
        .await
        .unwrap_err();
    assert_eq!(err.code, "fauna.federation.forbidden", "{err:?}");
    let err = originate(&b, &a, APPEAL_KIND, value(&relay(STRANGER)))
        .await
        .unwrap_err();
    assert_eq!(err.code, "fauna.federation.forbidden", "{err:?}");
    assert!(audit_rows(&a, "moderation:appeal").await.is_empty());

    let ok = originate(&b, &a, APPEAL_KIND, value(&relay(AUTHOR)))
        .await
        .expect("the delivered-to home relays its author");
    let reply: ModerationAppealReply = decode(&encode_canonical(&ok).unwrap()).unwrap();
    assert_eq!(reply.status, "appeal_recorded");
}

/// (e): an admin block of a projected post delivers the `Blocked` row the same
/// way, its category the report's reason.
#[tokio::test]
async fn a_block_of_a_projected_post_reaches_the_authors_home() {
    let (a, b) = pair().await;
    let post_id = seed_foreign_post(&a, &b, 0xe4).await;
    let report: AbuseReportSubmitReply = decode(
        &dispatch(
            &a.router,
            a.state.clone(),
            REPORTER,
            "fauna.moderation.abuse_report.submit",
            pack(&AbuseReportSubmitRequest {
                subject: AbuseReportSubject::Post {
                    cid: post_id.clone(),
                },
                reason: AbuseReportReason::Harassment,
                note: None,
                excerpt: None,
                block_author: false,
                subject_actor: None,
                extra: Default::default(),
            }),
        )
        .await
        .expect("submit ok"),
    )
    .unwrap();
    dispatch(
        &a.router,
        a.state.clone(),
        A_ADMIN,
        "fauna.moderation.admin_block",
        pack(&ModerationAdminBlockRequest {
            report_id: report.report_id,
            restore: false,
            extra: Default::default(),
        }),
    )
    .await
    .expect("block ok");

    let rows = queue(&b, AUTHOR).await;
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].action, Action::Blocked as u8);
    assert_eq!(rows[0].category, "harassment");
    assert_eq!(rows[0].issuer_nest.as_deref(), Some(a.host().as_str()));
    assert_eq!(
        appeal(&b, AUTHOR, &post_id).await.unwrap(),
        "appeal_recorded"
    );
    assert_eq!(audit_rows(&a, "moderation:appeal").await.len(), 1);
}

/// (f): a takedown of an attested conversation record on the channel's home
/// delivers the row to the foreign member's own home — the nest the channel
/// binds them to — and the appeal returns there.
#[tokio::test]
async fn a_conversation_takedown_reaches_the_foreign_members_home() {
    let (a, b) = pair().await;
    let channel = [0xf1u8; 32];
    let record_id = [0xf2u8; 32];
    let content_id = hex::encode(record_id);
    let record_cid = fauna_cbor::Cid::from_digest_dag_cbor(record_id);
    a.state
        .db
        .segment_records_insert_conv(&channel, 0, &record_cid, "b", 1_000, 1, &[], Some(&AUTHOR))
        .await
        .unwrap();
    a.state
        .db
        .register_foreign_channel_member(
            &channel,
            &AUTHOR,
            &b.nest_id(),
            Some(&b.url),
            RebindPower::Standing,
        )
        .await
        .unwrap();

    take_down(&a, "conversation", &content_id).await;

    let rows = queue(&b, AUTHOR).await;
    assert_eq!(rows.len(), 1, "the foreign member's home serves the row");
    assert_eq!(rows[0].content_type, "conversation");
    assert_eq!(rows[0].content_id, content_id);
    assert_eq!(rows[0].issuer_nest.as_deref(), Some(a.host().as_str()));
    assert_eq!(
        appeal(&b, AUTHOR, &content_id).await.unwrap(),
        "appeal_recorded"
    );
    let on_a = audit_rows(&a, "moderation:appeal").await;
    assert_eq!(on_a.len(), 1);
    assert_eq!(on_a[0].0.as_deref(), Some(AUTHOR.as_slice()));
}

/// A takedown whose author the deciding nest hosts delivers nothing — the
/// row is served where the author already reads it.
#[tokio::test]
async fn a_local_authors_row_stays_home() {
    let (a, b) = pair().await;
    common::seed_user(&a.state.db, &STRANGER).await;
    let post = Post {
        author: ActorId(STRANGER),
        created_at: Timestamp(1_000_000),
        body: PostBody::Text {
            content: "local".into(),
            facets: vec![],
        },
        references: vec![],
        expires_at: None,
        gated: None,
        content_warning: None,
        origin: None,
    };
    let body = canonical_encode(&post).unwrap();
    let post_id = *blake3::hash(&body).as_bytes();
    fauna_nest::segments::post::store_post(
        &a.state.post_segments,
        &a.state.db,
        &post_id,
        &body,
        None,
    )
    .await
    .unwrap();
    take_down(&a, "post", &hex::encode(post_id)).await;

    assert_eq!(queue(&a, STRANGER).await.len(), 1);
    assert!(queue(&b, STRANGER).await.is_empty());
    assert_eq!(
        appeal(&a, STRANGER, &hex::encode(post_id)).await.unwrap(),
        "appeal_recorded"
    );
    assert!(audit_rows(&a, "moderation:appeal-relayed").await.is_empty());
    assert_eq!(
        fauna_nest::moderation_federation::reconcile_undelivered_obligations(&a.state).await,
        0,
        "the boot reconcile never queues a hosted author's row"
    );
}

/// A nest that stopped between a takedown's commit and its delivery's enqueue
/// (simulated: the decision written through its transaction, the delivery
/// never queued) still delivers the row: the forwarder's boot reconcile
/// re-queues it and the drain lands it on the author's home — and, once
/// delivered, a later boot queues nothing more.
#[tokio::test]
async fn a_delivery_lost_to_a_crash_after_the_commit_is_reconciled_at_boot() {
    let (a, b) = pair().await;
    let post_id = seed_foreign_post(&a, &b, 0xe7).await;
    let post_bytes: [u8; 32] = hex::decode(&post_id).unwrap().try_into().unwrap();
    let row_id = a
        .state
        .db
        .post_legal_takedown_txn(
            &post_bytes,
            &post_id,
            Some("court order 2026/17"),
            &AUTHOR,
            &A_ADMIN,
            "kind=post",
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_micros() as i64,
        )
        .await
        .unwrap()
        .expect("the takedown writes the author's row");
    assert!(queue(&b, AUTHOR).await.is_empty(), "nothing was delivered");

    assert_eq!(
        fauna_nest::moderation_federation::reconcile_undelivered_obligations(&a.state).await,
        1,
        "the boot reconcile re-queues the lost delivery"
    );
    fauna_nest::abuse_report_federation::drain_abuse_report_calls(&a.state).await;

    let rows = queue(&b, AUTHOR).await;
    assert_eq!(rows.len(), 1, "the author's home now serves the row");
    assert_eq!(rows[0].content_id, post_id);
    assert_eq!(rows[0].issuer_nest.as_deref(), Some(a.host().as_str()));
    assert_eq!(issuer_ref(&a, "post", &post_id).await, row_id.to_string());

    assert_eq!(
        fauna_nest::moderation_federation::reconcile_undelivered_obligations(&a.state).await,
        0,
        "a delivered row is never queued again"
    );
}
