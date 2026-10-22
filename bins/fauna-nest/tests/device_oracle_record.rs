//! Tier_1 (nest): a device custodian's per-operation record
//! (`docs/goal/behavior/conversations-oracle.md` § The per-operation record).
//!
//! The conversations oracle's custodian is the signed-in app on the user's
//! device, so its record reaches the account's other apps through the nest:
//! `fauna.principals.ops.record` (User class, self-scoped) keeps the rows and
//! `fauna.principals.ops` / the roster summary merge them as a second
//! custodian's. Proven here, in process:
//!
//!  * the account's own principal: rows kept, merged into the ops read and the
//!    roster summary, a resubmitted `op_id` kept once;
//!  * another account's principal: refused, nothing written;
//!  * a nest custodian's class, a malformed row, an oversized batch: refused;
//!  * revoking the principal deletes the record.

use std::sync::Arc;

use fauna_nest::db::third_party_principals::{
    AttestedKeys, ExecutionForm, PrincipalAttestation, PrincipalManifest,
};
use fauna_nest::routes::AppState;
use fauna_protocol::principals::{
    ListPrincipalOpsReply, ListPrincipalOpsRequest, ListPrincipalsReply, ListPrincipalsRequest,
    PRINCIPAL_OPS_RECORD_BATCH, RecordPrincipalOpsReply, RecordPrincipalOpsRequest,
    RecordedPrincipalOp, RecordedWindow, RevokePrincipalReply, RevokePrincipalRequest,
};

const ACCOUNT: [u8; 32] = [0x11; 32];
const OTHER: [u8; 32] = [0x22; 32];
const SCOPE: &str = "fauna:identity:op:conversation.read";

async fn state() -> Arc<AppState> {
    let db = Arc::new(fauna_nest::db::CacheDb::open_in_memory().expect("open in-memory db"));
    let state = AppState::for_test(db);
    fauna_nest::test_support::seat_own_deployment_seed(&state).await;
    for (actor, name) in [(ACCOUNT, "alice"), (OTHER, "bob")] {
        state.db.create_user(&actor, "free", name).await.unwrap();
    }
    Arc::new(state)
}

/// One consented device principal of `account`; its roster id.
async fn consent(state: &AppState, account: [u8; 32], client: &str) -> Vec<u8> {
    state
        .db
        .record_atproto_oauth_grant(
            &account,
            client.as_bytes(),
            client,
            Some("Assistant"),
            SCOPE,
            &[],
            "jkt",
            i64::MAX,
            None,
            fauna_nest::db::atproto_pds::OAUTH_GRANT_ISSUER_NEST,
            &PrincipalAttestation {
                keys: AttestedKeys {
                    holder_x25519: Some([0x5A; 32]),
                    writer_ed25519: None,
                },
                execution_form: ExecutionForm::Device,
                manifest: Some(PrincipalManifest {
                    publisher_key: [0x9B; 32],
                    ..Default::default()
                }),
            },
        )
        .await
        .unwrap();
    state
        .db
        .list_third_party_principals(&account)
        .await
        .unwrap()
        .into_iter()
        .find(|p| p.client_id == client)
        .expect("the principal row")
        .principal_id
}

async fn call<Req: serde::Serialize, Reply: serde::de::DeserializeOwned>(
    state: &Arc<AppState>,
    actor: [u8; 32],
    kind: &str,
    req: &Req,
) -> Result<Reply, String> {
    let mut b = fauna_nest::rpc_router::RpcRouter::builder();
    fauna_nest::principals_handlers::register_principals_handlers(&mut b);
    let router = b.build();
    let payload = fauna_protocol::encode_canonical(req).unwrap();
    (router.kind_meta(kind).expect("registered").handler)(state.clone(), actor, payload)
        .await
        .map(|reply| fauna_protocol::decode_strict(&reply).unwrap())
        .map_err(|e| format!("{e:?}"))
}

fn op(id: u8, at: i64, class: &str, room: &str) -> RecordedPrincipalOp {
    RecordedPrincipalOp {
        op_id: vec![id; 16],
        at,
        class: class.to_string(),
        detail: room.to_string(),
        extra: Default::default(),
    }
}

fn record(principal_id: &[u8], ops: Vec<RecordedPrincipalOp>) -> RecordPrincipalOpsRequest {
    RecordPrincipalOpsRequest {
        principal_id: principal_id.to_vec(),
        ops,
        ..Default::default()
    }
}

/// `record` carrying the custodian's ceiling position.
fn record_in_window(
    principal_id: &[u8],
    ops: Vec<RecordedPrincipalOp>,
    window_start: i64,
    count: u32,
) -> RecordPrincipalOpsRequest {
    RecordPrincipalOpsRequest {
        window: Some(RecordedWindow {
            window_start: u64::try_from(window_start).unwrap(),
            count,
            extra: Default::default(),
        }),
        ..record(principal_id, ops)
    }
}

async fn ops_in_window(state: &Arc<AppState>, actor: [u8; 32]) -> u32 {
    let roster: ListPrincipalsReply = call(
        state,
        actor,
        "fauna.principals.list",
        &ListPrincipalsRequest::default(),
    )
    .await
    .unwrap();
    roster.principals[0]
        .oracle
        .clone()
        .expect("the summary")
        .ops_in_window
}

async fn ops_of(state: &Arc<AppState>, actor: [u8; 32], principal_id: &[u8]) -> Vec<(i64, String)> {
    let reply: ListPrincipalOpsReply = call(
        state,
        actor,
        "fauna.principals.ops",
        &ListPrincipalOpsRequest {
            principal_id: principal_id.to_vec(),
            extra: Default::default(),
        },
    )
    .await
    .unwrap();
    reply.ops.into_iter().map(|o| (o.at, o.detail)).collect()
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn the_device_custodians_record_merges_into_the_roster() {
    let state = state().await;
    let principal = consent(&state, ACCOUNT, "https://assistant.example/client.json").await;
    let now = fauna_core::data::Timestamp::now_secs();

    let reply: RecordPrincipalOpsReply = call(
        &state,
        ACCOUNT,
        "fauna.principals.ops.record",
        &record(
            &principal,
            vec![
                op(1, now - 20, "conversation.read", "room-a"),
                op(2, now - 10, "conversation.send", "room-b"),
            ],
        ),
    )
    .await
    .unwrap();
    assert_eq!(reply.recorded, 2);

    // A retry after a lost reply resubmits the same ids: one row each.
    let again: RecordPrincipalOpsReply = call(
        &state,
        ACCOUNT,
        "fauna.principals.ops.record",
        &record(
            &principal,
            vec![
                op(2, now - 10, "conversation.send", "room-b"),
                op(3, now - 5, "conversation.read", "room-a"),
            ],
        ),
    )
    .await
    .unwrap();
    assert_eq!(again.recorded, 1);

    assert_eq!(
        ops_of(&state, ACCOUNT, &principal).await,
        [
            (now - 5, "room-a".to_string()),
            (now - 10, "room-b".to_string()),
            (now - 20, "room-a".to_string()),
        ],
        "newest first, one row per op id"
    );

    let roster: ListPrincipalsReply = call(
        &state,
        ACCOUNT,
        "fauna.principals.list",
        &ListPrincipalsRequest::default(),
    )
    .await
    .unwrap();
    let summary = roster.principals[0].oracle.clone().expect("the summary");
    assert_eq!(summary.requests_served, 3);
    assert_eq!(
        summary.ops_in_window, 0,
        "no batch carried the device's window"
    );
    let last = summary.last_op.expect("the newest operation");
    assert_eq!(
        (last.class.as_str(), last.detail.as_str()),
        ("conversation.read", "room-a")
    );

    // Revoking the principal deletes its record with it.
    let revoked: RevokePrincipalReply = call(
        &state,
        ACCOUNT,
        "fauna.principals.revoke",
        &RevokePrincipalRequest {
            principal_id: principal.clone(),
            extra: Default::default(),
        },
    )
    .await
    .unwrap();
    assert!(revoked.revoked);
    assert!(ops_of(&state, ACCOUNT, &principal).await.is_empty());
}

/// The ceiling's window lives on the device, so the roster's count in window
/// is the one the custodian's newest batch carried: an older window arriving
/// late changes nothing, a resubmission with nothing new still moves it, a
/// rolled window reads zero, and an impossible one is refused.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_batchs_window_is_the_rosters_count_in_window() {
    let state = state().await;
    let principal = consent(&state, ACCOUNT, "https://assistant.example/client.json").await;
    let now = fauna_core::data::Timestamp::now_secs();
    let read = op(1, now - 20, "conversation.read", "room-a");

    let _: RecordPrincipalOpsReply = call(
        &state,
        ACCOUNT,
        "fauna.principals.ops.record",
        &record_in_window(&principal, vec![read.clone()], now - 30, 2),
    )
    .await
    .unwrap();
    assert_eq!(ops_in_window(&state, ACCOUNT).await, 2);

    // The same window, further on — a lost reply's resubmission included.
    let again: RecordPrincipalOpsReply = call(
        &state,
        ACCOUNT,
        "fauna.principals.ops.record",
        &record_in_window(&principal, vec![read.clone()], now - 30, 5),
    )
    .await
    .unwrap();
    assert_eq!(again.recorded, 0);
    assert_eq!(ops_in_window(&state, ACCOUNT).await, 5);

    // An older position arriving late is not the newest.
    for (start, count) in [(now - 30, 3), (now - 4000, 9)] {
        let _: RecordPrincipalOpsReply = call(
            &state,
            ACCOUNT,
            "fauna.principals.ops.record",
            &record_in_window(&principal, vec![read.clone()], start, count),
        )
        .await
        .unwrap();
    }
    assert_eq!(ops_in_window(&state, ACCOUNT).await, 5);

    // A batch without a window keeps the one held.
    let _: RecordPrincipalOpsReply = call(
        &state,
        ACCOUNT,
        "fauna.principals.ops.record",
        &record(
            &principal,
            vec![op(2, now - 5, "conversation.send", "room-b")],
        ),
    )
    .await
    .unwrap();
    assert_eq!(ops_in_window(&state, ACCOUNT).await, 5);

    // A window from the future, or over the ceiling, is no custodian's.
    let over = fauna_core::identity_op::OPS_PER_WINDOW + 1;
    for (start, count) in [(now + 3600, 1), (now - 10, over)] {
        let refused = call::<_, RecordPrincipalOpsReply>(
            &state,
            ACCOUNT,
            "fauna.principals.ops.record",
            &record_in_window(&principal, vec![read.clone()], start, count),
        )
        .await;
        assert!(refused.is_err(), "window ({start}, {count}) refused");
    }
    assert_eq!(ops_in_window(&state, ACCOUNT).await, 5);
}

/// A window the custodian last reported has rolled by the time the roster is
/// read: it counts nothing, as the ceiling itself would.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_rolled_window_counts_nothing() {
    let state = state().await;
    let principal = consent(&state, ACCOUNT, "https://assistant.example/client.json").await;
    let now = fauna_core::data::Timestamp::now_secs();
    let rolled = now - i64::try_from(fauna_core::identity_op::WINDOW_SECS).unwrap() - 1;
    let _: RecordPrincipalOpsReply = call(
        &state,
        ACCOUNT,
        "fauna.principals.ops.record",
        &record_in_window(
            &principal,
            vec![op(1, rolled + 1, "conversation.read", "room-a")],
            rolled,
            4,
        ),
    )
    .await
    .unwrap();
    assert_eq!(ops_in_window(&state, ACCOUNT).await, 0);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn another_accounts_principal_is_refused_and_nothing_is_written() {
    let state = state().await;
    let principal = consent(&state, ACCOUNT, "https://assistant.example/client.json").await;
    let now = fauna_core::data::Timestamp::now_secs();

    let refused = call::<_, RecordPrincipalOpsReply>(
        &state,
        OTHER,
        "fauna.principals.ops.record",
        &record(&principal, vec![op(1, now, "conversation.read", "room-a")]),
    )
    .await
    .unwrap_err();
    assert!(refused.contains("no such principal"), "{refused}");
    assert!(ops_of(&state, ACCOUNT, &principal).await.is_empty());
    assert!(ops_of(&state, OTHER, &principal).await.is_empty());
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn only_a_well_formed_device_class_row_is_recorded() {
    let state = state().await;
    let principal = consent(&state, ACCOUNT, "https://assistant.example/client.json").await;
    let now = fauna_core::data::Timestamp::now_secs();

    let mut short_id = op(1, now, "conversation.read", "room-a");
    short_id.op_id = vec![1; 8];
    for (bad, why) in [
        (
            op(1, now, "post.author", "room-a"),
            "not a device custodian's class",
        ),
        (op(1, now, "identity.handle", "room-a"), "sovereign"),
        (op(1, now, "conversation.read", ""), "detail"),
        (op(1, now, "conversation.read", &"r".repeat(257)), "detail"),
        (
            op(1, now + 3600, "conversation.read", "room-a"),
            "at is not",
        ),
        (short_id, "op_id"),
    ] {
        let refused = call::<_, RecordPrincipalOpsReply>(
            &state,
            ACCOUNT,
            "fauna.principals.ops.record",
            &record(&principal, vec![bad]),
        )
        .await
        .unwrap_err();
        assert!(refused.contains(why), "{why}: {refused}");
    }
    let batch = (0..=PRINCIPAL_OPS_RECORD_BATCH)
        .map(|i| op(i as u8, now, "conversation.read", "room-a"))
        .collect();
    let refused = call::<_, RecordPrincipalOpsReply>(
        &state,
        ACCOUNT,
        "fauna.principals.ops.record",
        &record(&principal, batch),
    )
    .await
    .unwrap_err();
    assert!(refused.contains("at most"), "{refused}");
    assert!(ops_of(&state, ACCOUNT, &principal).await.is_empty());
}
