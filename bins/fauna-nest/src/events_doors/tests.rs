//! The events doors at tier_1: the poll's filter and cursor, and the
//! filtered push on a principal session — two principals of two publishers on
//! one account, so "the other publisher's change was filtered" is observed,
//! not assumed.

use std::sync::Arc;

use bytes::Bytes;
use tokio::sync::mpsc;

use fauna_protocol::push_events::{
    EventsPollReply, EventsPollRequest, KIND_EVENTS_POLL, SyncChangedPayload,
};

use crate::db::CacheDb;
use crate::db::third_party_principals::{
    AttestedKeys, ExecutionForm, PrincipalAttestation, PrincipalManifest,
};
use crate::principal_handlers::{PrincipalBinding, dispatch_principal};
use crate::routes::AppState;

const ACCOUNT: [u8; 32] = [0xA1; 32];
const NEVER: i64 = i64::MAX;

const EVENTS: &str = "fauna:events:subscribe";
const A_RECORDS: &str = "fauna:records:rw:ext.app.example.*";
const A_NOTES: &str = "ext:ext.app.example.notes";
const A_WRITER: [u8; 32] = [0x57; 32];
const B_RECORDS: &str = "fauna:records:rw:ext.other.org.*";
const B_NOTES: &str = "ext:ext.other.org.notes";
const B_WRITER: [u8; 32] = [0x58; 32];

/// Consent one principal of `publisher` on the account, attesting `writer`,
/// declaring one `notes` kind (and a webhook when `events_uri` says so), and
/// return a binding under `token_scopes`.
async fn principal(
    state: &Arc<AppState>,
    publisher: &str,
    holder: u8,
    writer: [u8; 32],
    granted: &str,
    token_scopes: &[&str],
    events_uri: Option<&str>,
) -> PrincipalBinding {
    let client = format!("https://{publisher}/client.json");
    state
        .db
        .record_atproto_oauth_grant(
            &ACCOUNT,
            client.as_bytes(),
            &client,
            Some(publisher),
            granted,
            &[],
            "jkt",
            NEVER,
            None,
            crate::db::atproto_pds::OAUTH_GRANT_ISSUER_NEST,
            &PrincipalAttestation {
                keys: AttestedKeys {
                    holder_x25519: Some([holder; 32]),
                    writer_ed25519: Some(writer),
                },
                execution_form: ExecutionForm::Device,
                manifest: Some(PrincipalManifest {
                    publisher_key: [holder; 32],
                    declared_kinds: vec![format!("ext.{publisher}.notes")],
                    events_uri: events_uri.map(str::to_string),
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
        .unwrap()
        .into_iter()
        .find(|p| p.client_id == client)
        .expect("minted")
        .principal_id;
    PrincipalBinding {
        account: ACCOUNT,
        principal_id,
        token_scopes: token_scopes.iter().map(|s| s.to_string()).collect(),
    }
}

/// An account with principal A (`app.example`, records + events) and
/// principal B (`other.org`, records only).
async fn fixture(a_token: &[&str]) -> (Arc<AppState>, PrincipalBinding, PrincipalBinding) {
    let db = Arc::new(CacheDb::open_in_memory().unwrap());
    db.create_user(&ACCOUNT, "free", "test").await.unwrap();
    let state = Arc::new(AppState::for_test(db));
    let a = principal(
        &state,
        "app.example",
        0x71,
        A_WRITER,
        &format!("{A_RECORDS} {EVENTS}"),
        a_token,
        Some("https://app.example/fauna/events"),
    )
    .await;
    let b = principal(
        &state,
        "other.org",
        0x72,
        B_WRITER,
        B_RECORDS,
        &[B_RECORDS],
        Some("https://other.org/fauna/events"),
    )
    .await;
    (state, a, b)
}

/// The webhook walk's selection (`crate::events_webhook::targets`): A, who
/// subscribed, is reached by its own scope's change and not by B's; B, who
/// declared a webhook but never subscribed, is reached by nothing; and the
/// external-apps switch OFF silences the walk as it silences the push.
#[tokio::test]
async fn the_webhook_walk_selects_by_the_live_reach() {
    let (state, _a, _b) = fixture(&[A_RECORDS, EVENTS]).await;
    let reached = crate::events_webhook::targets(&state.db, &ACCOUNT, A_NOTES)
        .await
        .unwrap();
    assert_eq!(
        reached
            .iter()
            .map(|t| t.hook.events_uri.as_str())
            .collect::<Vec<_>>(),
        ["https://app.example/fauna/events"]
    );
    assert_eq!(reached[0].hook.client_id, "https://app.example/client.json");
    assert!(reached[0].scopes.iter().any(|s| s == EVENTS));
    assert!(
        crate::events_webhook::targets(&state.db, &ACCOUNT, B_NOTES)
            .await
            .unwrap()
            .is_empty(),
        "B's scope reaches no webhook: A's records arm does not cover it, B never subscribed"
    );
    state
        .db
        .set_atproto_external_apps_enabled(&ACCOUNT, false)
        .await
        .unwrap();
    assert!(
        crate::events_webhook::targets(&state.db, &ACCOUNT, A_NOTES)
            .await
            .unwrap()
            .is_empty()
    );
}

async fn put(
    state: &Arc<AppState>,
    who: &PrincipalBinding,
    scope: &str,
    writer: &[u8; 32],
    seq: i64,
) {
    let payload =
        fauna_protocol::encode_canonical(&fauna_protocol::account_state::AccountStatePutRequest {
            scope: scope.into(),
            writer_id: hex::encode(writer),
            writer_seq: seq,
            item_key: fauna_protocol::ByteBuf::from(vec![0x11; 32]),
            op: fauna_protocol::account_state::OP_STATE_PUT.into(),
            // The shape floor the put enforces: a gen-0 (v1) envelope.
            entry: fauna_protocol::ByteBuf::from({
                let mut envelope = vec![fauna_core::account_entry_crypto::SEALED_ENTRY_V1];
                envelope.extend_from_slice(&[0x5e; 12 + 16 + 8]);
                envelope
            }),
            ..Default::default()
        })
        .unwrap();
    dispatch_principal(state.clone(), who, "fauna.account.state.put", payload)
        .await
        .expect("its own kind, as its own writer");
}

async fn poll(
    state: &Arc<AppState>,
    who: &PrincipalBinding,
    cursor: Option<i64>,
) -> Result<EventsPollReply, fauna_protocol::RpcError> {
    let payload = fauna_protocol::encode_canonical(&EventsPollRequest {
        cursor,
        extra: Default::default(),
    })
    .unwrap();
    let reply = dispatch_principal(state.clone(), who, KIND_EVENTS_POLL, payload).await?;
    Ok(fauna_protocol::decode_strict(&reply).unwrap())
}

fn scopes(reply: &EventsPollReply) -> Vec<&str> {
    reply
        .frames
        .iter()
        .map(|f| f.scope.as_deref().unwrap())
        .collect()
}

/// The push frames a principal session's outbound queue holds right now, as
/// the nudges they carry.
fn pushed(rx: &mut mpsc::Receiver<Bytes>) -> Vec<SyncChangedPayload> {
    let mut out = Vec::new();
    while let Ok(bytes) = rx.try_recv() {
        let fauna_protocol::Frame::Push(push) = fauna_protocol::decode_frame(&bytes).unwrap()
        else {
            panic!("only pushes ride a principal's queue here");
        };
        assert_eq!(push.kind, "fauna.sync.changed");
        let raw = fauna_protocol::encode_canonical(&push.payload).unwrap();
        out.push(fauna_protocol::decode_strict(&raw).unwrap());
    }
    out
}

/// The headline: B's change lands first, A's after it; A's poll names only
/// its own scope, and the cursor walks past both.
#[tokio::test]
async fn a_poll_names_only_the_scopes_the_session_can_list() {
    let (state, a, b) = fixture(&[A_RECORDS, EVENTS]).await;
    put(&state, &b, B_NOTES, &B_WRITER, 1).await;
    put(&state, &a, A_NOTES, &A_WRITER, 1).await;

    let first = poll(&state, &a, None).await.unwrap();
    assert_eq!(scopes(&first), vec![A_NOTES]);
    assert!(first.frames[0].folder.is_empty() && first.frames[0].folder_hash.is_none());

    // Nothing moved since: no frames, and the cursor holds.
    let again = poll(&state, &a, Some(first.cursor)).await.unwrap();
    assert!(again.frames.is_empty());
    assert_eq!(again.cursor, first.cursor);

    // Only a change past the cursor is named again.
    put(&state, &b, B_NOTES, &B_WRITER, 2).await;
    let after_b = poll(&state, &a, Some(first.cursor)).await.unwrap();
    assert!(after_b.frames.is_empty(), "another publisher's change");
    assert!(
        after_b.cursor > first.cursor,
        "the cursor still walks past it"
    );
    put(&state, &a, A_NOTES, &A_WRITER, 2).await;
    let after_a = poll(&state, &a, Some(after_b.cursor)).await.unwrap();
    assert_eq!(scopes(&after_a), vec![A_NOTES]);
}

/// A token without the events arm reaches no poll — the gate's half — even
/// though the row holds the arm.
#[tokio::test]
async fn a_token_without_the_events_arm_cannot_poll() {
    let (state, a, _b) = fixture(&[A_RECORDS]).await;
    let err = poll(&state, &a, None).await.unwrap_err();
    assert!(err.code.ends_with("permission_denied"), "{}", err.code);
}

/// The WS-RPC door: A's session hears its own scope's nudge — the scope and
/// nothing else — and never B's; a session whose token did not subscribe
/// hears nothing.
#[tokio::test]
async fn the_push_reaches_only_a_subscribed_session_for_its_own_scope() {
    let (state, a, b) = fixture(&[A_RECORDS, EVENTS]).await;
    let (_a_conn, mut a_rx) = state.ws.subscribe_principal(a.clone());
    let unsubscribed = PrincipalBinding {
        token_scopes: vec![A_RECORDS.into()],
        ..a.clone()
    };
    let (_u_conn, mut u_rx) = state.ws.subscribe_principal(unsubscribed);
    let (_b_conn, mut b_rx) = state.ws.subscribe_principal(b.clone());

    put(&state, &b, B_NOTES, &B_WRITER, 1).await;
    put(&state, &a, A_NOTES, &A_WRITER, 1).await;

    assert_eq!(
        pushed(&mut a_rx),
        vec![SyncChangedPayload::scope_nudge(A_NOTES)]
    );
    assert!(pushed(&mut u_rx).is_empty(), "no events arm, no frame");
    assert!(pushed(&mut b_rx).is_empty(), "B never subscribed");
}

/// The push reads the live reach: the account's external-apps switch OFF
/// silences it, exactly as it refuses every call.
#[tokio::test]
async fn the_external_apps_switch_off_silences_the_push() {
    let (state, a, _b) = fixture(&[A_RECORDS, EVENTS]).await;
    let (_conn, mut rx) = state.ws.subscribe_principal(a.clone());
    put(&state, &a, A_NOTES, &A_WRITER, 1).await;
    assert_eq!(pushed(&mut rx).len(), 1);
    state
        .db
        .set_atproto_external_apps_enabled(&ACCOUNT, false)
        .await
        .unwrap();
    crate::events_doors::on_scope_changed(&state, &ACCOUNT, A_NOTES).await;
    assert!(pushed(&mut rx).is_empty());
}

#[test]
fn no_actor_class_holds_the_poll_kind() {
    use crate::bridge_method_allowlist::{CallerClass, is_permitted};
    for class in [CallerClass::User, CallerClass::Admin] {
        assert!(!is_permitted(class, KIND_EVENTS_POLL), "{class:?}");
    }
    assert!(is_permitted(CallerClass::ThirdParty, KIND_EVENTS_POLL));
}
