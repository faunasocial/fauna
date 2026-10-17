//! Integration test — the nest holds the `From:` header to its submitter at the
//! bridges' enqueue door (`mail-multidomain.md` § From: header ownership), so
//! the deployment's DKIM signature, applied at the outbound hand-out, is never
//! spent on a message that names another local user as its author, whatever
//! the MTA process does.
//!
//! Driven through the production kind `fauna.bridges.enqueue_outbound_mail` as
//! an approved MTA:
//!
//! 1. a From naming another local user's handle, submitted for an actor that
//!    does not own it → refused with the typed code, nothing spooled;
//! 2. the same message submitted for its owner → spooled;
//! 3. a From on a domain the deployment does not sign for → spooled unchanged;
//! 4. an alias the submitter owns → spooled;
//! 5. a subdomain of a local domain, even with the submitter's handle →
//!    refused (nothing there can be owned);
//! 6. a submission naming no submitter → refused as malformed.
//!
//! And the MTA proves the submitter it names (§ *Per door, nest-side*, the
//! enqueue door's submitter) with the user-signed submission token SMTP AUTH
//! unwrapped, checked before the From: is held to that submitter:
//!
//! 7. a token signed by another key → refused with the typed code, nothing
//!    spooled;
//! 8. a token naming another actor than the submitter → refused;
//! 9. an expired token → refused;
//! 10. a token whose credential is no longer on file (revoked in the app) →
//!     refused, where the same token passed before the revoke;
//! 11. more recipients than the token allows → refused;
//! 12. an MTA submission carrying no token → refused as malformed.
//!
//! (The MDA class carries none: `conformance_caldav_autoschedule_in_domain`
//! enqueues without one and stays green.)
//!
//! Run with: cargo test -p fauna-nest --test mail_from_ownership_enqueue

mod common;
use common::{approve_bridge, register_user};

use std::sync::Arc;

use bytes::Bytes;
use ed25519_dalek::SigningKey;

use fauna_mls::wrapped_blob::submission_token::signed_token_until;
use fauna_nest::bridge_routing_handlers::register_bridge_routing_handlers;
use fauna_nest::db::CacheDb;
use fauna_nest::db::bridge_service_users::BridgeRole;
use fauna_nest::routes::AppState;
use fauna_nest::rpc_router::RpcRouter;
use fauna_protocol::bridge_routing::{
    EnqueueOutboundMailReply, EnqueueOutboundMailRequest, SENDER_NOT_OWNED_CODE,
    SUBMITTER_NOT_PROVEN_CODE,
};
use fauna_protocol::{decode_strict as decode, encode_canonical};
use serde_bytes::ByteBuf;

const DOMAIN: &str = "example.test";
const MTA: [u8; 32] = [0x0A; 32];
const CREDENTIAL: &str = "default";

fn alice_key() -> SigningKey {
    SigningKey::from_bytes(&[0x11; 32])
}
fn bob_key() -> SigningKey {
    SigningKey::from_bytes(&[0x22; 32])
}
fn actor(key: &SigningKey) -> [u8; 32] {
    key.verifying_key().to_bytes()
}

/// A day from now, in the token's unit (epoch seconds).
fn tomorrow() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .expect("clock after the epoch")
        .as_secs()
        + 86_400
}

/// The canonical plaintext of a submission token `key` signed for its own
/// actor under [`CREDENTIAL`], valid until `expires_at`.
fn token(key: &SigningKey, expires_at: u64) -> Vec<u8> {
    signed_token_until(key, CREDENTIAL, expires_at)
        .to_canonical_bytes()
        .expect("encode token")
}

async fn nest() -> (RpcRouter, Arc<AppState>) {
    let deployment_seed = [0x5Eu8; 32];
    let db = Arc::new(CacheDb::open_in_memory().expect("open in-memory db"));
    let public = ed25519_dalek::SigningKey::from_bytes(&deployment_seed)
        .verifying_key()
        .to_bytes();
    db.set_nest_keypair(&deployment_seed, &public)
        .await
        .expect("seat the deployment seed");
    db.add_mail_domain(DOMAIN, true, "testing", "none", None, None)
        .await
        .expect("add the mail domain");
    let state = Arc::new(AppState::for_test(db));
    approve_bridge(&state.db, &MTA, BridgeRole::Mta, &[0x99u8; 32]).await;
    for (key, handle) in [(alice_key(), "alice"), (bob_key(), "bob")] {
        register_user(&state, actor(&key), handle).await;
        // The user enabled third-party-MUA submission: a wrapped token is on
        // file for the credential (its sealed bytes are not this test's).
        state
            .db
            .put_wrapped_submission_token(&actor(&key), CREDENTIAL, b"wrapped")
            .await
            .expect("file the wrapped token");
    }
    let mut b = RpcRouter::builder();
    register_bridge_routing_handlers(&mut b);
    (b.build(), state)
}

fn message(from: &str, tag: &str) -> Vec<u8> {
    format!(
        "From: {from}\r\nTo: carol@external.test\r\nSubject: {tag}\r\n\
         Date: Mon, 1 Jan 2024 00:00:00 +0000\r\nMessage-ID: <{tag}@{DOMAIN}>\r\n\r\n\
         body of {tag}\r\n"
    )
    .into_bytes()
}

/// Enqueue `raw` as the MTA, submitted for `submitter`'s actor with a valid
/// token that key signed, with the envelope sender `sender`; `None` names no
/// submitter and carries no token.
async fn enqueue(
    router: &RpcRouter,
    state: &Arc<AppState>,
    sender: &str,
    submitter: Option<&SigningKey>,
    raw: Vec<u8>,
    tag: &str,
) -> Result<EnqueueOutboundMailReply, fauna_protocol::RpcError> {
    let req = EnqueueOutboundMailRequest {
        original_msgid: format!("<{tag}@{DOMAIN}>"),
        original_sender: sender.into(),
        recipients: vec!["carol@external.test".into()],
        raw_message: raw,
        on_behalf_of_actor: submitter.map(|k| actor(k).to_vec()),
        submission_token: submitter.map(|k| ByteBuf::from(token(k, tomorrow()))),
        ..Default::default()
    };
    send(router, state, req).await
}

async fn send(
    router: &RpcRouter,
    state: &Arc<AppState>,
    req: EnqueueOutboundMailRequest,
) -> Result<EnqueueOutboundMailReply, fauna_protocol::RpcError> {
    let payload = Bytes::from(encode_canonical(&req).expect("encode").to_vec());
    let meta = router
        .kind_meta("fauna.bridges.enqueue_outbound_mail")
        .expect("kind registered");
    let reply = (meta.handler)(state.clone(), MTA, payload).await?;
    Ok(decode(&reply).expect("decode reply"))
}

async fn spooled(state: &Arc<AppState>) -> Vec<Vec<u8>> {
    state
        .db
        .fetch_due_outbound(i64::MAX, 100)
        .await
        .expect("due query")
        .into_iter()
        .map(|r| r.raw_message)
        .collect()
}

#[tokio::test]
async fn a_from_naming_another_local_users_handle_is_refused_and_nothing_spools() {
    let (router, state) = nest().await;
    let raw = message("Bob <bob@example.test>", "forged");
    let err = enqueue(
        &router,
        &state,
        "alice@example.test",
        Some(&alice_key()),
        raw,
        "forged",
    )
    .await
    .expect_err("alice may not author as bob");
    assert_eq!(err.code, SENDER_NOT_OWNED_CODE);
    assert!(
        spooled(&state).await.is_empty(),
        "a refused message never spools"
    );
}

#[tokio::test]
async fn the_same_message_submitted_for_its_owner_spools() {
    let (router, state) = nest().await;
    let raw = message("Bob <bob@example.test>", "own");
    let reply = enqueue(
        &router,
        &state,
        "bob@example.test",
        Some(&bob_key()),
        raw.clone(),
        "own",
    )
    .await
    .expect("bob may author as bob");
    assert_eq!(reply.ids.len(), 1);
    assert_eq!(spooled(&state).await, vec![raw]);
}

#[tokio::test]
async fn a_from_on_a_domain_the_deployment_does_not_sign_for_spools_unchanged() {
    let (router, state) = nest().await;
    let raw = message("Someone <someone@foreign.test>", "foreign");
    let reply = enqueue(
        &router,
        &state,
        "alice@example.test",
        Some(&alice_key()),
        raw.clone(),
        "foreign",
    )
    .await
    .expect("an off-domain From is outside the rule");
    assert_eq!(reply.ids.len(), 1);
    assert_eq!(spooled(&state).await, vec![raw]);
}

#[tokio::test]
async fn an_alias_the_submitter_owns_spools() {
    let (router, state) = nest().await;
    state
        .db
        .put_exact_alias(DOMAIN, "sales", "exact", &actor(&alice_key()))
        .await
        .expect("alias");
    let raw = message("Sales <sales@example.test>", "alias");
    enqueue(
        &router,
        &state,
        "alice@example.test",
        Some(&alice_key()),
        raw.clone(),
        "alias",
    )
    .await
    .expect("an owned alias is alice's to send from");
    assert_eq!(spooled(&state).await, vec![raw]);
}

#[tokio::test]
async fn a_subdomain_from_is_refused_even_for_the_handle() {
    let (router, state) = nest().await;
    let raw = message("alice@mail.example.test", "subdomain");
    let err = enqueue(
        &router,
        &state,
        "alice@example.test",
        Some(&alice_key()),
        raw,
        "subdomain",
    )
    .await
    .expect_err("nothing on a subdomain can be owned");
    assert_eq!(err.code, SENDER_NOT_OWNED_CODE);
    assert!(spooled(&state).await.is_empty());
}

#[tokio::test]
async fn a_submission_naming_no_submitter_is_malformed() {
    let (router, state) = nest().await;
    let raw = message("alice@example.test", "anonymous");
    let err = enqueue(
        &router,
        &state,
        "alice@example.test",
        None,
        raw,
        "anonymous",
    )
    .await
    .expect_err("the MTA must say whom it submits for");
    assert_eq!(err.code, "fauna.protocol.malformed");
    assert!(spooled(&state).await.is_empty());
}

/// An MTA request for alice's own message, submitted for `submitter` with
/// `token` — the knobs the submitter-proof tests turn.
fn alice_request(
    submitter: [u8; 32],
    token: Option<Vec<u8>>,
    tag: &str,
) -> EnqueueOutboundMailRequest {
    EnqueueOutboundMailRequest {
        original_msgid: format!("<{tag}@{DOMAIN}>"),
        original_sender: "alice@example.test".into(),
        recipients: vec!["carol@external.test".into()],
        raw_message: message("alice@example.test", tag),
        on_behalf_of_actor: Some(submitter.to_vec()),
        submission_token: token.map(ByteBuf::from),
        ..Default::default()
    }
}

#[tokio::test]
async fn a_token_signed_by_another_key_does_not_prove_the_submitter() {
    let (router, state) = nest().await;
    // Bob signs a token claiming to be alice's: the actor field says alice,
    // the signature is bob's.
    let forged = signed_token_until(&alice_key(), CREDENTIAL, tomorrow())
        .sign(&bob_key())
        .expect("sign");
    let req = alice_request(
        actor(&alice_key()),
        Some(forged.to_canonical_bytes().expect("encode")),
        "forged-sig",
    );
    let err = send(&router, &state, req)
        .await
        .expect_err("bob cannot sign for alice");
    assert_eq!(err.code, SUBMITTER_NOT_PROVEN_CODE);
    assert!(
        spooled(&state).await.is_empty(),
        "a refused message never spools"
    );
}

#[tokio::test]
async fn a_token_naming_another_actor_does_not_prove_the_submitter() {
    let (router, state) = nest().await;
    // A genuine token of bob's, replayed for alice.
    let req = alice_request(
        actor(&alice_key()),
        Some(token(&bob_key(), tomorrow())),
        "replayed",
    );
    let err = send(&router, &state, req)
        .await
        .expect_err("bob's token is not alice's");
    assert_eq!(err.code, SUBMITTER_NOT_PROVEN_CODE);
    assert!(spooled(&state).await.is_empty());
}

#[tokio::test]
async fn an_expired_token_does_not_prove_the_submitter() {
    let (router, state) = nest().await;
    let req = alice_request(
        actor(&alice_key()),
        Some(token(&alice_key(), 1_700_000_000)),
        "expired",
    );
    let err = send(&router, &state, req)
        .await
        .expect_err("an expired token has no authority");
    assert_eq!(err.code, SUBMITTER_NOT_PROVEN_CODE);
    assert!(spooled(&state).await.is_empty());
}

#[tokio::test]
async fn revoking_the_credential_ends_the_tokens_authority_at_the_nest() {
    let (router, state) = nest().await;
    let good = token(&alice_key(), tomorrow());
    send(
        &router,
        &state,
        alice_request(actor(&alice_key()), Some(good.clone()), "before"),
    )
    .await
    .expect("a valid token for a credential on file proves alice");
    assert_eq!(spooled(&state).await.len(), 1);
    // The user revokes the credential in the app.
    assert!(
        state
            .db
            .delete_wrapped_submission_token(&actor(&alice_key()), CREDENTIAL)
            .await
            .expect("revoke")
    );
    let err = send(
        &router,
        &state,
        alice_request(actor(&alice_key()), Some(good), "after"),
    )
    .await
    .expect_err("the same token stops working once its credential is gone");
    assert_eq!(err.code, SUBMITTER_NOT_PROVEN_CODE);
    assert_eq!(spooled(&state).await.len(), 1, "nothing more spooled");
}

#[tokio::test]
async fn more_recipients_than_the_token_allows_is_refused() {
    let (router, state) = nest().await;
    let mut tok = signed_token_until(&alice_key(), CREDENTIAL, tomorrow());
    tok.max_recipients = 1;
    let tok = tok.sign(&alice_key()).expect("sign");
    let mut req = alice_request(
        actor(&alice_key()),
        Some(tok.to_canonical_bytes().expect("encode")),
        "too-many",
    );
    req.recipients.push("dave@external.test".into());
    let err = send(&router, &state, req)
        .await
        .expect_err("over the token's own recipient bound");
    assert_eq!(err.code, SUBMITTER_NOT_PROVEN_CODE);
    assert!(spooled(&state).await.is_empty());
}

#[tokio::test]
async fn an_mta_submission_carrying_no_token_is_malformed() {
    let (router, state) = nest().await;
    let req = alice_request(actor(&alice_key()), None, "tokenless");
    let err = send(&router, &state, req)
        .await
        .expect_err("the MTA must prove its submitter");
    assert_eq!(err.code, "fauna.protocol.malformed");
    assert!(spooled(&state).await.is_empty());
}
