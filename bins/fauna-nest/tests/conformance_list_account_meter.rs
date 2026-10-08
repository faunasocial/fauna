//! `fauna.bridges.list_account_lists` carries the caller's per-account
//! list-recipient meter for today and the effective daily cap
//! (`mail-mass-mailing.md` § Composing a list message — the compose form's
//! "Today's quota: N / M" — and § The per-day per-account cap).

mod common;

use std::sync::Arc;

use bytes::Bytes;
use serde_bytes::ByteBuf;

use common::{TEST_DOMAIN as DOMAIN, dispatch, register_user};
use fauna_nest::bridge_list_handlers::register_bridge_list_handlers;
use fauna_nest::db::CacheDb;
use fauna_nest::routes::AppState;
use fauna_nest::rpc_router::RpcRouter;
use fauna_protocol::bridge_routing::{
    ListAccountListsReply, ListAccountListsRequest, MassMailingPolicy, SendListMessageReply,
    SendListMessageRequest,
};
use fauna_protocol::{decode_strict as decode, encode_canonical};

async fn router_and_state() -> (RpcRouter, Arc<AppState>) {
    let db = Arc::new(CacheDb::open_in_memory().expect("open in-memory db"));
    db.add_mail_domain(DOMAIN, true, "testing", "self_signed", None, None)
        .await
        .unwrap();
    let state = Arc::new(AppState::for_test(db));
    let mut b = RpcRouter::builder();
    register_bridge_list_handlers(&mut b);
    (b.build(), state)
}

async fn list_lists(
    router: &RpcRouter,
    state: &Arc<AppState>,
    owner: [u8; 32],
) -> ListAccountListsReply {
    let payload = Bytes::from(
        encode_canonical(&ListAccountListsRequest {})
            .unwrap()
            .to_vec(),
    );
    let reply = dispatch(
        router,
        state.clone(),
        owner,
        "fauna.bridges.list_account_lists",
        payload,
    )
    .await
    .expect("list_account_lists");
    decode(&reply).expect("decode list_account_lists reply")
}

#[tokio::test]
async fn the_lists_read_carries_todays_account_meter_and_the_daily_cap() {
    let (router, state) = router_and_state().await;
    let owner: [u8; 32] = [0xC2; 32];
    register_user(&state, owner, "owner").await;
    let (list_id, _alias_id) = state
        .db
        .create_list(&owner, DOMAIN, "news", None, None, None, None, None)
        .await
        .expect("create the owner's list");
    for n in 0..2 {
        state
            .db
            .add_member(
                &list_id,
                &format!("member{n}@external.test"),
                &format!("unsubscribe-token-{n}"),
            )
            .await
            .expect("subscribe an external member");
    }

    let before = list_lists(&router, &state, owner).await;
    assert_eq!(before.account_recipients_today, 0);
    assert_eq!(
        before.account_recipients_per_day,
        MassMailingPolicy::default().list_recipients_per_account_per_day_ceiling as i64,
        "with no per-account knob stored, the effective cap is the admin ceiling"
    );

    let message = format!("From: owner@{DOMAIN}\r\nSubject: Issue 1\r\n\r\nHello\r\n");
    let req = SendListMessageRequest {
        list_id: ByteBuf::from(list_id.to_vec()),
        message: ByteBuf::from(message.into_bytes()),
    };
    let payload = Bytes::from(encode_canonical(&req).unwrap().to_vec());
    let reply = dispatch(
        &router,
        state.clone(),
        owner,
        "fauna.bridges.send_list_message",
        payload,
    )
    .await
    .expect("send_list_message");
    let sent: SendListMessageReply = decode(&reply).expect("decode send reply");
    assert_eq!(sent.queued_count, 2);

    let after = list_lists(&router, &state, owner).await;
    assert_eq!(
        after.account_recipients_today, 2,
        "the meter counts the recipients the send reserved"
    );
    assert_eq!(
        after.account_recipients_per_day - after.account_recipients_today,
        sent.estimated_quota_remaining as i64,
        "the read agrees with what the send reported remaining"
    );
}
