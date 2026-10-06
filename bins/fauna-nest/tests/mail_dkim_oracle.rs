//! Integration test — the nest holds each mail domain's DKIM key and signs at
//! the outbound spool's hand-out (`mail-bridge-lifecycle.md` § DKIM
//! provisioning (automatic) → *Custody moves to the nest*;
//! `key-material-hierarchy.md` § Audience: deployment infrastructure → *The
//! oracle* → *The DKIM class is the outbound spool's own door*).
//!
//! Five claims, each driven through the production kinds:
//!
//! 1. a spool row whose one From field is on an active local domain leaves
//!    `fauna.bridges.fetch_outbound_due` carrying a `DKIM-Signature` that an
//!    independent parse of the published record verifies;
//! 2. a row with a foreign From, and a row with two From fields, leave
//!    byte-identical to what was enqueued;
//! 3. revoking the MTA refuses its next hand-out and touches neither the key
//!    nor the published record;
//! 4. a deployment-seed rotation leaves the key signing under the same
//!    published record (the satellite walk re-wraps it);
//! 5. no kind hands the key to a bridge: the router serves neither
//!    `fauna.bridges.fetch_dkim_blob` nor `fauna.bridges.provision_dkim_blob`.
//!
//! Run with: cargo test -p fauna-nest --test mail_dkim_oracle

mod common;
use common::{admin_actor, approve_bridge};

use std::borrow::Borrow;
use std::collections::HashMap;
use std::hash::Hash;
use std::sync::{Arc, Mutex};
use std::time::Instant;

use bytes::Bytes;
use mail_auth::common::parse::TxtRecordParser;
use mail_auth::common::verify::DomainKey;
use mail_auth::{
    AuthenticatedMessage, DkimResult, MessageAuthenticator, Parameters, ResolverCache, Txt,
};
use zeroize::Zeroizing;

use fauna_nest::bridge_blob_handlers::register_bridge_blob_handlers;
use fauna_nest::bridge_routing_handlers::register_bridge_routing_handlers;
use fauna_nest::db::CacheDb;
use fauna_nest::db::bridge_service_users::BridgeRole;
use fauna_nest::db::outbound::{InboundVerdictsSnapshot, NewOutbound};
use fauna_nest::routes::AppState;
use fauna_nest::rpc_router::RpcRouter;
use fauna_protocol::bridge_routing::{
    FetchOutboundDueReply, FetchOutboundDueRequest, OutboundUnit,
};
use fauna_protocol::wrapped_blob::RevokeServiceUserRequest;
use fauna_protocol::{decode_strict as decode, encode_canonical};

const DOMAIN: &str = "example.test";
const MTA: [u8; 32] = [0x0A; 32];

/// A resolver cache holding exactly the records a test publishes, so the
/// verifier never reaches the network (the shape
/// `libs/fauna-mail/tests/dkim_tests.rs` uses).
#[derive(Default)]
struct StaticTxtCache {
    inner: Mutex<HashMap<Box<str>, Txt>>,
}

impl ResolverCache<Box<str>, Txt> for StaticTxtCache {
    fn get<Q>(&self, name: &Q) -> Option<Txt>
    where
        Box<str>: Borrow<Q>,
        Q: Hash + Eq + ?Sized,
    {
        self.inner.lock().unwrap().get(name).cloned()
    }

    fn remove<Q>(&self, name: &Q) -> Option<Txt>
    where
        Box<str>: Borrow<Q>,
        Q: Hash + Eq + ?Sized,
    {
        self.inner.lock().unwrap().remove(name)
    }

    fn insert(&self, key: Box<str>, value: Txt, _valid_until: Instant) {
        self.inner.lock().unwrap().insert(key, value);
    }
}

fn seed(byte: u8) -> Zeroizing<[u8; 32]> {
    Zeroizing::new([byte; 32])
}

/// A nest holding `deployment_seed`, with [`DOMAIN`] added through the door
/// that activates a mail domain and an approved MTA.
async fn nest(deployment_seed: &[u8; 32]) -> (RpcRouter, Arc<AppState>) {
    let db = Arc::new(CacheDb::open_in_memory().expect("open in-memory db"));
    let public = ed25519_dalek::SigningKey::from_bytes(deployment_seed)
        .verifying_key()
        .to_bytes();
    db.set_nest_keypair(deployment_seed, &public)
        .await
        .expect("seat the deployment seed");
    db.add_mail_domain(DOMAIN, true, "testing", "none", None, None)
        .await
        .expect("add the mail domain");
    let state = Arc::new(AppState::for_test(db));
    approve_bridge(&state.db, &MTA, BridgeRole::Mta, &[0x99u8; 32]).await;
    let mut b = RpcRouter::builder();
    register_bridge_routing_handlers(&mut b);
    register_bridge_blob_handlers(&mut b);
    (b.build(), state)
}

fn message(from_lines: &str, tag: &str) -> Vec<u8> {
    format!(
        "{from_lines}To: bob@external.test\r\nSubject: oracle {tag}\r\n\
         Date: Mon, 1 Jan 2024 00:00:00 +0000\r\nMessage-ID: <{tag}@{DOMAIN}>\r\n\r\n\
         body of {tag}\r\n"
    )
    .into_bytes()
}

async fn enqueue(db: &CacheDb, msgid: &str, raw: &[u8]) -> i64 {
    let ids = db
        .enqueue_outbound(NewOutbound {
            original_msgid: msgid,
            original_sender: "alice@example.test",
            recipients: &["bob@external.test"],
            raw_message: raw,
            inbound_verdicts: InboundVerdictsSnapshot {
                spf: String::new(),
                dmarc: String::new(),
                dmarc_policy: String::new(),
            },
            is_forwarded: false,
            forward_actor_id: None,
            forward_rule_id: None,
            forward_copy_mode: None,
            submit_actor_id: None,
        })
        .await
        .expect("enqueue");
    assert_eq!(ids.len(), 1);
    ids[0]
}

async fn call(
    router: &RpcRouter,
    state: &Arc<AppState>,
    kind: &str,
    caller: [u8; 32],
    payload: Vec<u8>,
) -> Result<Bytes, fauna_protocol::RpcError> {
    let meta = router.kind_meta(kind).expect("kind registered");
    (meta.handler)(state.clone(), caller, Bytes::from(payload)).await
}

async fn try_fetch_due(
    router: &RpcRouter,
    state: &Arc<AppState>,
) -> Result<FetchOutboundDueReply, fauna_protocol::RpcError> {
    let payload = encode_canonical(&FetchOutboundDueRequest {
        max: 10,
        lease_seconds: 60,
    })
    .expect("encode req")
    .to_vec();
    let reply = call(
        router,
        state,
        "fauna.bridges.fetch_outbound_due",
        MTA,
        payload,
    )
    .await?;
    Ok(decode(&reply).expect("decode reply"))
}

async fn fetch_unit(router: &RpcRouter, state: &Arc<AppState>, id: i64) -> OutboundUnit {
    try_fetch_due(router, state)
        .await
        .expect("fetch_outbound_due ok")
        .units
        .into_iter()
        .find(|u| u.id == id)
        .expect("the enqueued row is due")
}

/// The `(selector, public_dns_value)` the nest reports for [`DOMAIN`] — what
/// the DNS page publishes.
async fn published(state: &Arc<AppState>) -> (String, String) {
    let rows = state
        .db
        .list_dkim_selectors(Some(DOMAIN))
        .await
        .expect("list selectors");
    assert_eq!(
        rows.len(),
        1,
        "the domain has exactly one selector — minted when the domain was added"
    );
    (rows[0].selector.clone(), rows[0].public_dns_value.clone())
}

/// Whether `signed` carries a DKIM signature that verifies against `record`
/// published at `<selector>._domainkey.<DOMAIN>`.
async fn verifies(signed: &[u8], selector: &str, record: &str) -> bool {
    let cache = StaticTxtCache::default();
    let key = DomainKey::parse(record.as_bytes()).expect("parse the published record");
    cache.insert(
        format!("{selector}._domainkey.{DOMAIN}.").into(),
        Txt::DomainKey(Arc::new(key)),
        Instant::now(),
    );
    let message = AuthenticatedMessage::parse(signed).expect("parse the handed-out message");
    let resolver = MessageAuthenticator::new_system_conf().expect("resolver init");
    let results = resolver
        .verify_dkim(Parameters::new(&message).with_txt_cache(&cache))
        .await;
    results
        .iter()
        .any(|r| matches!(r.result(), DkimResult::Pass))
}

/// (a) A row whose one From field is on an active local domain leaves signed,
/// the signature verifies against the record the nest publishes, and the unit
/// tells the worker there is nothing left for it to sign.
#[tokio::test]
async fn a_local_from_leaves_signed_and_verifies_against_the_published_record() {
    let (router, state) = nest(&seed(0xa1)).await;
    let raw = message("From: Alice <alice@example.test>\r\n", "local");
    let id = enqueue(&state.db, "<local@example.test>", &raw).await;

    let unit = fetch_unit(&router, &state, id).await;

    assert!(
        unit.raw_message.starts_with(b"DKIM-Signature:"),
        "the hand-out prepends the signature; got: {}",
        String::from_utf8_lossy(&unit.raw_message[..unit.raw_message.len().min(120)])
    );
    assert!(
        unit.raw_message.ends_with(&raw),
        "the message itself leaves untouched behind the signature"
    );
    let (selector, record) = published(&state).await;
    assert!(
        verifies(&unit.raw_message, &selector, &record).await,
        "the signature must verify against the published `{selector}` record {record}"
    );
}

/// (b) A From the deployment does not sign for, and a message carrying two
/// From fields, leave exactly as they rest.
#[tokio::test]
async fn a_foreign_from_and_a_doubled_from_leave_as_they_rest() {
    let (router, state) = nest(&seed(0xa1)).await;
    let foreign = message("From: mallory@elsewhere.test\r\n", "foreign");
    let doubled = message(
        "From: mallory@elsewhere.test\r\nFrom: alice@example.test\r\n",
        "doubled",
    );
    let foreign_id = enqueue(&state.db, "<foreign@example.test>", &foreign).await;
    let doubled_id = enqueue(&state.db, "<doubled@example.test>", &doubled).await;

    let reply = try_fetch_due(&router, &state).await.expect("fetch ok");
    let unit = |id| {
        reply
            .units
            .iter()
            .find(|u| u.id == id)
            .expect("the enqueued row is due")
    };

    assert_eq!(unit(foreign_id).raw_message, foreign);
    assert_eq!(unit(doubled_id).raw_message, doubled);
}

/// (c) Revoking the MTA cuts its hand-out and leaves the deployment's DKIM
/// identity alone: the bridge's key and the domain's key are orthogonal.
#[tokio::test]
async fn revoking_the_mta_refuses_its_hand_out_and_leaves_the_key_alone() {
    let (router, state) = nest(&seed(0xa1)).await;
    let before = published(&state).await;
    let raw = message("From: alice@example.test\r\n", "revoke");
    let id = enqueue(&state.db, "<revoke@example.test>", &raw).await;

    let admin = admin_actor(&state).await;
    let payload = encode_canonical(&RevokeServiceUserRequest {
        bridge_actor_id: MTA.to_vec(),
        extra: Default::default(),
    })
    .expect("encode revoke")
    .to_vec();
    call(
        &router,
        &state,
        "fauna.bridges.revoke_service_user",
        admin,
        payload,
    )
    .await
    .expect("the admin revokes the MTA");

    assert!(
        try_fetch_due(&router, &state).await.is_err(),
        "a revoked MTA's next hand-out is refused"
    );
    assert_eq!(
        published(&state).await,
        before,
        "an MTA revoke touches neither the selector nor its published record"
    );

    // The successor MTA is handed the same message, signed under the same key.
    let successor: [u8; 32] = [0x0B; 32];
    common::approve_bridge_as(
        &state.db,
        &successor,
        BridgeRole::Mta,
        "mta-successor",
        &[0x98u8; 32],
    )
    .await;
    let payload = encode_canonical(&FetchOutboundDueRequest {
        max: 10,
        lease_seconds: 60,
    })
    .unwrap()
    .to_vec();
    let reply: FetchOutboundDueReply = decode(
        &call(
            &router,
            &state,
            "fauna.bridges.fetch_outbound_due",
            successor,
            payload,
        )
        .await
        .expect("the successor's hand-out"),
    )
    .unwrap();
    let unit = reply.units.into_iter().find(|u| u.id == id).expect("due");
    assert!(verifies(&unit.raw_message, &before.0, &before.1).await);
}

/// (d) A deployment-seed rotation re-wraps the key: the nest keeps signing,
/// and the record the admin published stays valid.
#[tokio::test]
async fn a_deployment_seed_rotation_leaves_the_key_signing() {
    let (a, b) = (seed(0xa1), seed(0xb2));
    let (router, state) = nest(&a).await;
    let before = published(&state).await;

    let outcome = state
        .db
        .rotate_deployment_seed(&a, &b)
        .await
        .expect("rotation runs")
        .expect("rotation commits");
    assert!(
        outcome.satellites_rekeyed >= 1,
        "the DKIM key rides the satellite walk"
    );

    let raw = message("From: alice@example.test\r\n", "rotated");
    let id = enqueue(&state.db, "<rotated@example.test>", &raw).await;
    let unit = fetch_unit(&router, &state, id).await;

    assert_eq!(published(&state).await, before);
    assert!(
        verifies(&unit.raw_message, &before.0, &before.1).await,
        "after the rotation the hand-out still signs under the published key"
    );
}

/// (e) The key never leaves the nest. The two kinds that once carried it
/// sealed — out to the MTA, and in from an admin — are not served: a bridge
/// asking for the blob reaches no handler, and the shared registry names
/// neither kind.
#[tokio::test]
async fn a_bridge_asking_for_the_dkim_key_is_refused() {
    let (router, _state) = nest(&seed(0xa1)).await;
    let registry = fauna_protocol::kind::KindRegistry::full();
    for kind in [
        "fauna.bridges.fetch_dkim_blob",
        "fauna.bridges.provision_dkim_blob",
    ] {
        assert!(
            router.kind_meta(kind).is_none(),
            "`{kind}` must reach no handler"
        );
        assert!(!registry.contains(kind), "`{kind}` must not be a kind");
    }
    assert!(
        router
            .kind_meta("fauna.bridges.fetch_outbound_due")
            .is_some(),
        "the hand-out — the one sign site — is served"
    );
}
