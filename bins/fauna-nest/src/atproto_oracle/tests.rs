//! Tier_1: the `atproto.service_auth` door, in process — every policy arm,
//! the correlated round trip (a captured push answered through the bridge's
//! deliver handler), and the no-bridge, bridge-refusal and deadline arms. The
//! mint itself is the Go bridge's, proven tier_3 by
//! `tests/e2e-unified/tests/api/test_atproto_oracle.py`.

use std::sync::Arc;
use std::time::Duration;

use bytes::Bytes;
use fauna_core::identity_op::{IdentityOpClass, OPS_PER_WINDOW};
use fauna_mls::wrapped_blob::GrantWindow;
use fauna_protocol::atproto_oracle::{KIND_SERVICE_AUTH, ServiceAuthReply, ServiceAuthRequest};
use fauna_protocol::atproto_pds::{
    BridgeAtprotoServiceAuthRequestedPush, DeliverServiceAuthReply, DeliverServiceAuthRequest,
};
use fauna_protocol::kind_manifest::ServiceAuthEntry;
use fauna_protocol::{RpcError, decode_strict, encode_canonical};

use super::*;
use crate::db::third_party_principals::{
    AttestedKeys, ExecutionForm, PrincipalAttestation, PrincipalManifest,
};
use crate::principal_handlers::{PrincipalBinding, dispatch_principal};

const ACCOUNT: [u8; 32] = [0xA7; 32];
const BRIDGE: [u8; 32] = [0x7B; 32];
const HOLDER: [u8; 32] = [0x77; 32];
const GRANT_ID: [u8; 16] = [0x0D; 16];
const CLIENT: &str = "https://oracle.example/client.json";
const SCOPE: &str = "fauna:identity:op:atproto.service_auth";
const DID: &str = "did:plc:oracleaccount0000000000";
const AUD: &str = "did:web:feed.example#bsky_fg";
const LXM: &str = "app.bsky.feed.getFeedSkeleton";
const DENIED: &str = "com.atproto.identity.submitPlcOperation";

fn now_secs() -> i64 {
    crate::db::now_epoch_secs()
}

/// A nest with the account, its active ATProto identity, an approved PDS
/// bridge, the owner's live grant and the consented principal declaring
/// `(AUD, LXM)`. Returns the state and the principal's binding.
async fn seeded() -> (Arc<AppState>, PrincipalBinding) {
    let state = crate::test_support::fixture_state();
    state
        .db
        .create_user(&ACCOUNT, "free", "test")
        .await
        .unwrap();
    state
        .db
        .upsert_atproto_identity_intent(&ACCOUNT, "plc", "did:key:zDnaeROTATION")
        .await
        .unwrap();
    state
        .db
        .record_atproto_minted(&ACCOUNT, DID, None)
        .await
        .unwrap();
    crate::bridge_approval_test_support::approve_bridge(&state.db, &BRIDGE, BridgeRole::AtprotoPds)
        .await;
    mint_grant(&state, IdentityOpClass::AtprotoServiceAuth).await;
    state
        .db
        .record_atproto_oauth_grant(
            &ACCOUNT,
            b"family-1",
            CLIENT,
            Some("Oracle App"),
            SCOPE,
            &[],
            "jkt",
            i64::MAX,
            None,
            crate::db::atproto_pds::OAUTH_GRANT_ISSUER_NEST,
            &PrincipalAttestation {
                keys: AttestedKeys {
                    holder_x25519: Some(HOLDER),
                    writer_ed25519: None,
                },
                execution_form: ExecutionForm::Remote,
                manifest: Some(PrincipalManifest {
                    publisher_key: [0x9B; 32],
                    declared_service_auth: vec![ServiceAuthEntry {
                        aud: AUD.into(),
                        lxm: vec![LXM.into()],
                        extra: Default::default(),
                    }],
                    ..Default::default()
                }),
            },
        )
        .await
        .unwrap();
    let principal_id = state
        .db
        .list_third_party_principals(&ACCOUNT)
        .await
        .unwrap()[0]
        .principal_id
        .clone();
    (
        state,
        PrincipalBinding {
            account: ACCOUNT,
            principal_id,
            token_scopes: vec![SCOPE.to_string()],
            family_id: None,
        },
    )
}

/// The owner's device mints the keyless `identity.op` grant to the
/// principal's attested key.
async fn mint_grant(state: &AppState, class: IdentityOpClass) {
    let now = now_secs() as u64;
    let blob = fauna_mls::wrapped_blob::build_grant_blob(
        &ACCOUNT,
        &GRANT_ID,
        &HOLDER,
        None,
        GrantWindow(now - 1, now + 3600),
        &[(fauna_client_capabilities::identity_op_scope(class), None)],
    )
    .unwrap()
    .to_canonical_bytes()
    .unwrap();
    state
        .db
        .put_capability_grant(&ACCOUNT, &GRANT_ID, &HOLDER, now as i64 + 3600, &blob)
        .await
        .unwrap();
}

fn request(aud: &str, lxm: &str, exp: Option<i64>) -> ServiceAuthRequest {
    ServiceAuthRequest {
        aud: aud.into(),
        lxm: lxm.into(),
        exp,
        extra: Default::default(),
    }
}

async fn call(
    state: &Arc<AppState>,
    binding: &PrincipalBinding,
    req: &ServiceAuthRequest,
) -> Result<ServiceAuthReply, RpcError> {
    let payload = encode_canonical(req).unwrap();
    let reply: Bytes =
        dispatch_principal(state.clone(), binding, KIND_SERVICE_AUTH, payload).await?;
    Ok(decode_strict(&reply).unwrap())
}

/// Read the next push off the bridge's connection as the service-auth
/// request it must be.
async fn next_request(
    rx: &mut tokio::sync::mpsc::Receiver<Bytes>,
) -> BridgeAtprotoServiceAuthRequestedPush {
    let frame = tokio::time::timeout(Duration::from_secs(5), rx.recv())
        .await
        .expect("the door pushes to the connected bridge")
        .expect("connection open");
    let fauna_protocol::Frame::Push(push) = fauna_protocol::decode_frame(&frame).unwrap() else {
        panic!("expected a Push frame")
    };
    assert_eq!(push.kind, "fauna.bridges.atproto.service_auth_requested");
    let fauna_protocol::PushEvent::BridgeAtprotoServiceAuthRequested(asked) =
        fauna_protocol::PushEvent::from_push(&push.kind, push.payload)
    else {
        panic!("the request must classify as its own variant")
    };
    asked
}

async fn deliver(state: &Arc<AppState>, request_id: Vec<u8>, token: Option<&str>) -> bool {
    let reply = crate::bridge_atproto_handlers::deliver_service_auth_handler()(
        state.clone(),
        BRIDGE,
        Bytes::from(
            encode_canonical(&DeliverServiceAuthRequest {
                request_id,
                token: token.map(str::to_string),
                extra: Default::default(),
            })
            .unwrap()
            .to_vec(),
        ),
    )
    .await
    .unwrap();
    let reply: DeliverServiceAuthReply = decode_strict(&reply).unwrap();
    reply.accepted
}

async fn count(state: &AppState, sql: &str) -> i64 {
    let conn = state.db.conn().await;
    conn.query_row(sql, [], |r| r.get(0)).unwrap()
}

fn assert_refused(outcome: Result<ServiceAuthReply, RpcError>, why: &str) {
    let err = outcome.expect_err(why);
    assert_eq!(err.code, "fauna.atproto.permission_denied", "{why}");
}

/// The round trip, both legs, over a real subscribed bridge connection: the
/// door pushes the admitted request — the account, its DID as `iss`, the
/// pair, the clamped `exp` — the bridge's deliver releases the waiter, the
/// door hands back exactly the token delivered, and one operation is
/// recorded and served to the owner through the roster reads.
#[tokio::test]
async fn a_declared_pair_round_trips_through_the_bridge_and_is_recorded() {
    let (state, binding) = seeded().await;
    let (_conn, mut rx) = state.ws.subscribe(BRIDGE);

    let before = now_secs();
    // spawn-ok(test): the door parks on the bridge's answer, so the test
    // drives the bridge leg while it runs; the join below reaps it.
    let calling = tokio::spawn({
        let state = state.clone();
        let binding = binding.clone();
        async move { call(&state, &binding, &request(AUD, LXM, None)).await }
    });
    let asked = next_request(&mut rx).await;
    let after = now_secs();
    assert_eq!(asked.request_id.len(), 16);
    assert_eq!(asked.actor_id, ACCOUNT.to_vec());
    assert_eq!(asked.did, DID);
    assert_eq!((asked.aud.as_str(), asked.lxm.as_str()), (AUD, LXM));
    assert!(
        (before + 60..=after + 60).contains(&asked.exp),
        "an absent exp is the cap"
    );

    assert!(deliver(&state, asked.request_id.clone(), Some("h.c.s")).await);
    let reply = calling.await.unwrap().expect("the minted token comes back");
    assert_eq!(reply.token, "h.c.s");

    assert_eq!(
        count(
            &state,
            "SELECT COUNT(*) FROM atproto_oracle_ops
              WHERE class = 'atproto.service_auth'
                AND detail = 'did:web:feed.example#bsky_fg app.bsky.feed.getFeedSkeleton'"
        )
        .await,
        1
    );
    let summaries = state
        .db
        .principal_oracle_summaries(&ACCOUNT, now_secs() as u64)
        .await
        .unwrap();
    let summary = &summaries[&binding.principal_id];
    assert_eq!(summary.requests_served, 1);
    assert_eq!(summary.ops_in_window, 1);
    let ops = state
        .db
        .list_principal_oracle_ops(&ACCOUNT, &binding.principal_id, 10)
        .await
        .unwrap();
    assert_eq!(ops.len(), 1);
    assert_eq!(ops[0].class, "atproto.service_auth");

    // A second answer under the same id matches nothing.
    assert!(!deliver(&state, asked.request_id, Some("again")).await);
    assert_eq!(state.atproto_service_auth.pending(), 0);
}

/// A requested `exp` inside the cap rides as asked; one past it is clamped.
#[tokio::test]
async fn the_requested_exp_is_clamped_to_the_cap() {
    let (state, binding) = seeded().await;
    let (_conn, mut rx) = state.ws.subscribe(BRIDGE);
    for (ask, want_offset) in [(30, 30), (3600, 60)] {
        let before = now_secs();
        // spawn-ok(test): as above — the test answers for the bridge.
        let calling = tokio::spawn({
            let state = state.clone();
            let binding = binding.clone();
            async move { call(&state, &binding, &request(AUD, LXM, Some(before + ask))).await }
        });
        let asked = next_request(&mut rx).await;
        let after = now_secs();
        if ask <= 60 {
            assert_eq!(asked.exp, before + want_offset);
        } else {
            assert!((before + want_offset..=after + want_offset).contains(&asked.exp));
        }
        assert!(deliver(&state, asked.request_id, Some("t")).await);
        calling.await.unwrap().unwrap();
    }
}

/// Every policy arm refuses with the one coarse answer, before any push goes
/// out and without consuming the window: an undeclared audience, an
/// undeclared method, a method on the class's deny list, an empty pair, a
/// grant for another class only, no grant at all, and an account whose
/// ATProto identity is not active.
#[tokio::test]
async fn each_policy_arm_refuses_before_any_push() {
    let (state, binding) = seeded().await;
    let (_conn, mut rx) = state.ws.subscribe(BRIDGE);

    for (req, why) in [
        (
            request("did:web:other.example", LXM, None),
            "an undeclared aud",
        ),
        (
            request("did:web:feed.example", LXM, None),
            "the declared aud without its fragment",
        ),
        (
            request(AUD, "app.bsky.feed.getTimeline", None),
            "an undeclared lxm",
        ),
        (request(AUD, DENIED, None), "a denied lxm"),
        (request("", LXM, None), "an empty aud"),
        (request(AUD, "", None), "an empty lxm"),
    ] {
        assert_refused(call(&state, &binding, &req).await, why);
        assert!(rx.try_recv().is_err(), "{why}: nothing may be pushed");
    }

    // The grant names another custodian's class only.
    {
        let conn = state.db.conn().await;
        conn.execute("DELETE FROM capability_grants", []).unwrap();
    }
    mint_grant(&state, IdentityOpClass::ActivityPubHttpSig).await;
    assert_refused(
        call(&state, &binding, &request(AUD, LXM, None)).await,
        "a grant for another class",
    );
    // No grant at all — the owner's revoke, read on the very next call.
    {
        let conn = state.db.conn().await;
        conn.execute("DELETE FROM capability_grants", []).unwrap();
    }
    assert_refused(
        call(&state, &binding, &request(AUD, LXM, None)).await,
        "no live grant",
    );
    // The grant back, the identity stepped down.
    mint_grant(&state, IdentityOpClass::AtprotoServiceAuth).await;
    state
        .db
        .set_atproto_identity_active(&ACCOUNT, false)
        .await
        .unwrap();
    assert_refused(
        call(&state, &binding, &request(AUD, LXM, None)).await,
        "no active ATProto identity",
    );

    assert!(rx.try_recv().is_err(), "no arm may push");
    assert_eq!(
        count(&state, "SELECT COUNT(*) FROM atproto_oracle_principals").await,
        0,
        "a refused call consumes no window"
    );
    assert_eq!(state.atproto_service_auth.pending(), 0);
}

/// With no approved PDS bridge connected the door refuses at once — no
/// window consumed, nothing registered.
#[tokio::test]
async fn with_no_connected_bridge_the_refusal_is_immediate() {
    let (state, binding) = seeded().await;
    let started = std::time::Instant::now();
    assert_refused(
        call(&state, &binding, &request(AUD, LXM, None)).await,
        "no bridge",
    );
    assert!(started.elapsed() < Duration::from_secs(5));
    assert_eq!(state.atproto_service_auth.pending(), 0);
    assert_eq!(
        count(&state, "SELECT COUNT(*) FROM atproto_oracle_principals").await,
        0
    );
}

/// The bridge answering "could not mint" refuses at once, and nothing is
/// recorded.
#[tokio::test]
async fn a_bridge_refusal_refuses_without_waiting_out_the_deadline() {
    let (state, binding) = seeded().await;
    let (_conn, mut rx) = state.ws.subscribe(BRIDGE);
    // spawn-ok(test): as above — the test answers for the bridge.
    let calling = tokio::spawn({
        let state = state.clone();
        let binding = binding.clone();
        async move { call(&state, &binding, &request(AUD, LXM, None)).await }
    });
    let asked = next_request(&mut rx).await;
    let started = std::time::Instant::now();
    assert!(deliver(&state, asked.request_id, None).await);
    assert_refused(calling.await.unwrap(), "the bridge could not mint");
    assert!(started.elapsed() < Duration::from_secs(5));
    assert_eq!(
        count(&state, "SELECT COUNT(*) FROM atproto_oracle_ops").await,
        0
    );
}

/// A connected bridge that never answers is the deadline's to refuse, and
/// the refusal leaves nothing registered.
#[tokio::test]
async fn a_silent_bridge_is_refused_at_the_deadline() {
    let (state, binding) = seeded().await;
    let (_conn, mut rx) = state.ws.subscribe(BRIDGE);
    let outcome = service_auth(
        &state,
        &binding.account,
        &binding.principal_id,
        Some(HOLDER),
        &request(AUD, LXM, None),
        Duration::from_millis(200),
    )
    .await;
    assert_refused(outcome, "a silent bridge");
    assert!(rx.try_recv().is_ok(), "the push did go out first");
    assert_eq!(state.atproto_service_auth.pending(), 0);
    assert_eq!(
        count(&state, "SELECT COUNT(*) FROM atproto_oracle_ops").await,
        0
    );
}

/// A full window refuses with the ceiling's own answer, before any push.
#[tokio::test]
async fn the_ceiling_refuses_a_full_window() {
    let (state, binding) = seeded().await;
    let (_conn, mut rx) = state.ws.subscribe(BRIDGE);
    {
        let conn = state.db.conn().await;
        conn.execute(
            "INSERT INTO atproto_oracle_principals
                (actor_id, principal_id, window_start, window_count)
             VALUES (?1, ?2, ?3, ?4)",
            params![
                hex::encode(ACCOUNT),
                binding.principal_id,
                now_secs(),
                OPS_PER_WINDOW
            ],
        )
        .unwrap();
    }
    let err = call(&state, &binding, &request(AUD, LXM, None))
        .await
        .expect_err("the ceiling");
    assert_eq!(err.code, "fauna.atproto.rate_limited");
    assert!(rx.try_recv().is_err());
}

/// Revoking the principal deletes its record with it.
#[tokio::test]
async fn revoking_the_principal_deletes_its_record() {
    let (state, binding) = seeded().await;
    let (_conn, mut rx) = state.ws.subscribe(BRIDGE);
    // spawn-ok(test): as above — the test answers for the bridge.
    let calling = tokio::spawn({
        let state = state.clone();
        let binding = binding.clone();
        async move { call(&state, &binding, &request(AUD, LXM, None)).await }
    });
    let asked = next_request(&mut rx).await;
    assert!(deliver(&state, asked.request_id, Some("t")).await);
    calling.await.unwrap().unwrap();
    assert_eq!(
        count(&state, "SELECT COUNT(*) FROM atproto_oracle_ops").await,
        1
    );

    state
        .db
        .revoke_third_party_principal(&ACCOUNT, &binding.principal_id)
        .await
        .unwrap()
        .expect("the principal existed");
    assert_eq!(
        count(&state, "SELECT COUNT(*) FROM atproto_oracle_ops").await,
        0
    );
    assert_eq!(
        count(&state, "SELECT COUNT(*) FROM atproto_oracle_principals").await,
        0
    );
}

/// The deliver kind is the PDS bridge's alone.
#[tokio::test]
async fn deliver_service_auth_is_bridge_only() {
    let (state, _binding) = seeded().await;
    let err = crate::bridge_atproto_handlers::deliver_service_auth_handler()(
        state.clone(),
        ACCOUNT,
        Bytes::from(
            encode_canonical(&DeliverServiceAuthRequest {
                request_id: vec![0; 16],
                token: Some("forged".into()),
                extra: Default::default(),
            })
            .unwrap()
            .to_vec(),
        ),
    )
    .await
    .expect_err("a user may not deliver a token");
    assert_eq!(err.code, "fauna.bridges.permission_denied");
}
