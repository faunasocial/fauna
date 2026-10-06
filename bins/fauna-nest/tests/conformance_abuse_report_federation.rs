//! **The abuse-report federation triad** — `fauna.federation.abuse_report.
//! {deliver,withdraw,outcome}` between two in-process nests over the federation
//! WS-RPC channel (`docs/goal/behavior/moderation.md` § Routing, § What the
//! reporter is told).
//!
//! `O` is the reporter's nest; `H` hosts the reported author. A report filed on
//! `O` about `H`'s author lands on both queues — on `H` with no reporter
//! identity — and `H`'s admin's resolution comes back to `O`, which alone can
//! tell the reporter. A withdrawal on `O` deletes the note on `H` too. `H`
//! refuses a report about someone it does not host, and a report whose home is
//! unreachable stays on `O`'s queue and in `O`'s retry queue.
//!
//! Harness mirrors `conformance_cross_nest_post_delete.rs`: plain-http nests
//! whose routers are populated by hand (discovery for the pool's `nest.info`
//! resolve, the moderation kinds, the federation serving table).

mod common;
use common::dispatch;

use std::sync::Arc;

use bytes::Bytes;
use ed25519_dalek::SigningKey;
use fauna_core::data::{Post, PostBody, Timestamp};
use fauna_core::encoding::canonical_encode;
use fauna_core::identity::ActorId;
use fauna_nest::abuse_report_federation::drain_abuse_report_calls;
use fauna_nest::config::SubmissionPolicy;
use fauna_nest::db::CacheDb;
use fauna_nest::nest_identity::NestIdentity;
use fauna_nest::pending_actions::finalize_user_deletion;
use fauna_nest::routes::AppState;
use fauna_nest::rpc_router::RpcRouter;
use fauna_protocol::moderation::{
    AbuseReportMineReply, AbuseReportMineRequest, AbuseReportOutcome, AbuseReportQueueReply,
    AbuseReportQueueRequest, AbuseReportReason, AbuseReportResolveRequest, AbuseReportStatus,
    AbuseReportSubject, AbuseReportSubmitReply, AbuseReportSubmitRequest,
    AbuseReportWithdrawRequest,
};
use fauna_protocol::{decode_strict as decode, encode_canonical};

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

/// `H` holds `author`'s post as its own; `O` has indexed it from `H`.
async fn seed_foreign_post(o: &Nest, h: &Nest, author: [u8; 32], seed: u8) -> String {
    let post = Post {
        author: ActorId(author),
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
    let post_id = [seed; 32];
    h.state
        .db
        .put_post(&post_id, &canonical_encode(&post).unwrap(), None)
        .await
        .unwrap();
    o.state
        .db
        .insert_post_index_entry_with_origin(
            &post_id,
            &author,
            1_000_000,
            false,
            false,
            "fauna",
            &[],
            Some(&h.url),
        )
        .await
        .unwrap();
    hex::encode(post_id)
}

async fn submit(
    o: &Nest,
    reporter: [u8; 32],
    subject: AbuseReportSubject,
) -> AbuseReportSubmitReply {
    let bytes = dispatch(
        &o.router,
        o.state.clone(),
        reporter,
        "fauna.moderation.abuse_report.submit",
        pack(&AbuseReportSubmitRequest {
            subject,
            reason: AbuseReportReason::Harassment,
            note: Some("repeated slurs".into()),
            excerpt: None,
            block_author: false,
            subject_actor: None,
            extra: Default::default(),
        }),
    )
    .await
    .expect("submit ok");
    decode(&bytes).unwrap()
}

async fn queue(n: &Nest, admin: [u8; 32]) -> AbuseReportQueueReply {
    let bytes = dispatch(
        &n.router,
        n.state.clone(),
        admin,
        "fauna.moderation.abuse_report.queue",
        pack(&AbuseReportQueueRequest::default()),
    )
    .await
    .expect("queue ok");
    decode(&bytes).unwrap()
}

async fn mine(o: &Nest, reporter: [u8; 32]) -> AbuseReportMineReply {
    let bytes = dispatch(
        &o.router,
        o.state.clone(),
        reporter,
        "fauna.moderation.abuse_report.mine",
        pack(&AbuseReportMineRequest::default()),
    )
    .await
    .expect("mine ok");
    decode(&bytes).unwrap()
}

async fn notif_types(n: &Nest, actor: &[u8; 32]) -> Vec<String> {
    n.state
        .db
        .list_notifications(actor, None, 100)
        .await
        .unwrap()
        .into_iter()
        .map(|n| n.notif_type.to_string())
        .collect()
}

/// Two nests, `H` hosting `author` with an admin, and — standing in for an
/// earlier dial — `H` holding a proven address for `O`.
async fn pair() -> (Nest, Nest, [u8; 32]) {
    let o = start_nest().await;
    let h = start_nest().await;
    let author = [0xb1u8; 32];
    common::seed_user(&h.state.db, &author).await;
    h.state
        .db
        .record_nest_address(&o.nest_id(), &o.url, true)
        .await
        .unwrap();
    (o, h, author)
}

/// **The loop across nests.** Report on `O` → forwarded to `H` at submit (the
/// acknowledgement names both) → `H`'s admin is rung and sees the report with
/// no reporter identity, named as coming from `O` → resolve on `H` → the
/// outcome crosses back and `O` tells the reporter.
#[tokio::test]
async fn a_report_on_a_foreign_author_reaches_the_home_admin_and_the_outcome_returns() {
    let (o, h, author) = pair().await;
    let h_admin = [0xb2u8; 32];
    h.state.db.add_admin_actor(&h_admin).await.unwrap();
    let o_admin = [0xb3u8; 32];
    o.state.db.add_admin_actor(&o_admin).await.unwrap();
    let reporter = [0xb4u8; 32];
    let cid = seed_foreign_post(&o, &h, author, 0xc1).await;

    let reply = submit(&o, reporter, AbuseReportSubject::Post { cid: cid.clone() }).await;
    assert_eq!(
        reply.routed_to,
        vec![o.state.handle_domain(), h.host()],
        "the acknowledgement names the home nest it reached"
    );
    assert_eq!(
        queue(&o, o_admin).await.reports.len(),
        1,
        "the reporter's own admin has it"
    );

    assert_eq!(
        notif_types(&h, &h_admin).await,
        vec!["abuse_report.received"]
    );
    let q = queue(&h, h_admin).await;
    assert_eq!(q.reports.len(), 1);
    let copy = &q.reports[0];
    assert_eq!(copy.subject, AbuseReportSubject::Post { cid });
    assert_eq!(
        copy.subject_actor.as_deref(),
        Some(hex::encode(author).as_str())
    );
    assert_eq!(copy.note.as_deref(), Some("repeated slurs"));
    assert!(copy.reporter_handle.is_none(), "the reporter never crosses");
    assert_eq!(copy.origin_nest.as_deref(), Some(o.host().as_str()));

    dispatch(
        &h.router,
        h.state.clone(),
        h_admin,
        "fauna.moderation.abuse_report.resolve",
        pack(&AbuseReportResolveRequest {
            report_id: copy.report_id.clone(),
            outcome: AbuseReportOutcome::Acted,
            extra: Default::default(),
        }),
    )
    .await
    .expect("resolve ok");
    drain_abuse_report_calls(&h.state).await;

    assert_eq!(
        notif_types(&o, &reporter).await,
        vec!["abuse_report.resolved"]
    );
    let ledger = mine(&o, reporter).await;
    assert_eq!(ledger.reports[0].status, AbuseReportStatus::Resolved);
    assert_eq!(ledger.reports[0].outcome, Some(AbuseReportOutcome::Acted));
    assert!(queue(&o, o_admin).await.reports.is_empty());
    // The author is told nothing, on either nest.
    assert!(notif_types(&h, &author).await.is_empty());
}

/// The reporter withdraws after the forward landed: `H`'s copy leaves its
/// queue and its note and excerpt are deleted.
#[tokio::test]
async fn a_withdrawal_follows_the_report_to_the_home_nest() {
    let (o, h, author) = pair().await;
    let h_admin = [0xb2u8; 32];
    h.state.db.add_admin_actor(&h_admin).await.unwrap();
    let reporter = [0xb4u8; 32];
    // An account report: `O` resolves the author's home through a post of
    // theirs it holds.
    seed_foreign_post(&o, &h, author, 0xc2).await;

    let reply = submit(
        &o,
        reporter,
        AbuseReportSubject::Actor {
            actor_id: hex::encode(author),
        },
    )
    .await;
    assert_eq!(reply.routed_to.len(), 2, "forwarded: {:?}", reply.routed_to);
    assert_eq!(queue(&h, h_admin).await.reports.len(), 1);

    dispatch(
        &o.router,
        o.state.clone(),
        reporter,
        "fauna.moderation.abuse_report.withdraw",
        pack(&AbuseReportWithdrawRequest {
            report_id: reply.report_id.clone(),
            extra: Default::default(),
        }),
    )
    .await
    .expect("withdraw ok");
    drain_abuse_report_calls(&o.state).await;

    assert!(
        queue(&h, h_admin).await.reports.is_empty(),
        "withdrawn on H too"
    );
    let copies = h.state.db.list_open_abuse_reports().await.unwrap();
    assert!(copies.is_empty());
    assert!(
        o.state
            .db
            .due_abuse_report_calls(i64::MAX, 10)
            .await
            .unwrap()
            .is_empty(),
        "nothing left to send"
    );
}

/// The reporter's account is deleted after the forward landed: the deletion
/// leg withdraws the report as the reporter would have, and the withdrawal
/// rides the durable queue after the account is gone — `H`'s copy leaves
/// its queue and its note is deleted (`moderation.md` § Where it lands, the
/// deletion ruling). Through the real deletion door, so the leg's place in
/// the walk is what is proven, not the leg alone.
#[tokio::test]
async fn a_deleted_reporters_forwarded_report_is_withdrawn_on_the_home_nest() {
    let (o, h, author) = pair().await;
    let h_admin = [0xb2u8; 32];
    h.state.db.add_admin_actor(&h_admin).await.unwrap();
    let reporter = [0xb4u8; 32];
    common::seed_user(&o.state.db, &reporter).await;
    seed_foreign_post(&o, &h, author, 0xc2).await;

    let reply = submit(
        &o,
        reporter,
        AbuseReportSubject::Actor {
            actor_id: hex::encode(author),
        },
    )
    .await;
    assert_eq!(reply.routed_to.len(), 2, "forwarded: {:?}", reply.routed_to);
    assert_eq!(queue(&h, h_admin).await.reports.len(), 1);

    finalize_user_deletion(&o.state, &reporter)
        .await
        .expect("the account deletion runs");
    let row = o
        .state
        .db
        .get_abuse_report(&reply.report_id)
        .await
        .unwrap()
        .expect("the skeleton stays under the retired id");
    assert_eq!(row.status, "withdrawn");
    assert!(row.note.is_none(), "the words are gone here");

    drain_abuse_report_calls(&o.state).await;
    assert!(
        queue(&h, h_admin).await.reports.is_empty(),
        "withdrawn on H too, after the account is gone"
    );
    assert!(
        h.state
            .db
            .list_open_abuse_reports()
            .await
            .unwrap()
            .is_empty()
    );
    assert!(
        o.state
            .db
            .due_abuse_report_calls(i64::MAX, 10)
            .await
            .unwrap()
            .is_empty(),
        "nothing left to send"
    );
}

/// `H` hosts only its own: a report whose subject `H` does not host is
/// refused, and `O` keeps it to itself — no destination is claimed and
/// nothing is retried.
#[tokio::test]
async fn the_home_nest_refuses_a_subject_it_does_not_host() {
    let (o, h, _author) = pair().await;
    let reporter = [0xb4u8; 32];
    // A post `O` believes came from `H`, by an author `H` has no account for.
    let stranger = [0xb9u8; 32];
    let cid = seed_foreign_post(&o, &h, stranger, 0xc3).await;

    let reply = submit(&o, reporter, AbuseReportSubject::Post { cid }).await;
    assert_eq!(reply.routed_to, vec![o.state.handle_domain()]);
    assert!(
        h.state
            .db
            .list_open_abuse_reports()
            .await
            .unwrap()
            .is_empty()
    );
    assert!(
        o.state
            .db
            .due_abuse_report_calls(i64::MAX, 10)
            .await
            .unwrap()
            .is_empty(),
        "a refusal is final"
    );
}

/// The home nest is unreachable: the report stands on `O`, the acknowledgement
/// names `O` alone, and the delivery waits in the retry queue.
#[tokio::test]
async fn an_unreachable_home_nest_leaves_the_delivery_queued() {
    let o = start_nest().await;
    let reporter = [0xb4u8; 32];
    let author = [0xb1u8; 32];
    let dead = {
        let l = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        format!("http://{}", l.local_addr().unwrap())
    };
    let post_id = [0xc4u8; 32];
    o.state
        .db
        .insert_post_index_entry_with_origin(
            &post_id,
            &author,
            1_000_000,
            false,
            false,
            "fauna",
            &[],
            Some(&dead),
        )
        .await
        .unwrap();

    let reply = submit(
        &o,
        reporter,
        AbuseReportSubject::Post {
            cid: hex::encode(post_id),
        },
    )
    .await;
    assert_eq!(reply.routed_to, vec![o.state.handle_domain()]);
    let queued = o
        .state
        .db
        .due_abuse_report_calls(i64::MAX, 10)
        .await
        .unwrap();
    assert_eq!(queued.len(), 1);
    assert_eq!(queued[0].kind, "deliver");
    assert_eq!(queued[0].attempts, 1, "the inline attempt was counted");
    assert_eq!(queued[0].peer_url, dead);
}
