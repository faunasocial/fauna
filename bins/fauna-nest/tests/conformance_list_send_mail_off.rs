//! "Disable mail" stops a list send too: `fauna.bridges.send_list_message`
//! sends as the list's owner (its From is the owner's own address or the
//! list's posting address), so while the owner has turned their mail off the
//! send is refused with `fauna.bridges.mail_disabled_by_user` before any list
//! quota is drawn and before any copy is queued, and sends again once mail is
//! back on (`mail-credentials.md` § Trigger taxonomy, the Disable-mail row).
//! Mutation pinned: dropping the `ensure_actor_mail_enabled` call from
//! `send_list_message_handler`.

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
    ListAccountListsReply, ListAccountListsRequest, SendListMessageReply, SendListMessageRequest,
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

async fn account_recipients_today(
    router: &RpcRouter,
    state: &Arc<AppState>,
    owner: [u8; 32],
) -> i64 {
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
    let lists: ListAccountListsReply = decode(&reply).expect("decode list_account_lists reply");
    lists.account_recipients_today
}

#[tokio::test]
async fn a_list_send_is_refused_and_spends_nothing_while_the_owners_mail_is_off() {
    let (router, state) = router_and_state().await;
    let owner: [u8; 32] = [0xC3; 32];
    register_user(&state, owner, "owner").await;
    let (list_id, _alias_id) = state
        .db
        .create_list(&owner, DOMAIN, "news", None, None, None, None, None)
        .await
        .expect("create the owner's list");
    state
        .db
        .add_member(&list_id, "member@external.test", "unsubscribe-token-0")
        .await
        .expect("subscribe an external member");
    let send = || {
        let message = format!("From: owner@{DOMAIN}\r\nSubject: Issue 1\r\n\r\nHello\r\n");
        let req = SendListMessageRequest {
            list_id: ByteBuf::from(list_id.to_vec()),
            message: ByteBuf::from(message.into_bytes()),
        };
        let payload = Bytes::from(encode_canonical(&req).unwrap().to_vec());
        dispatch(
            &router,
            state.clone(),
            owner,
            "fauna.bridges.send_list_message",
            payload,
        )
    };

    state
        .db
        .set_actor_mail_enabled(&owner, false)
        .await
        .unwrap();
    let err = send().await.expect_err("mail off refuses the list send");
    assert_eq!(err.code, "fauna.bridges.mail_disabled_by_user");
    assert_eq!(
        account_recipients_today(&router, &state, owner).await,
        0,
        "a refused send spends no list quota"
    );
    assert!(
        state
            .db
            .fetch_all_outbound_for_test()
            .await
            .unwrap()
            .is_empty(),
        "a refused send queues no copy"
    );

    state.db.set_actor_mail_enabled(&owner, true).await.unwrap();
    let reply = send().await.expect("sends once mail is back on");
    let sent: SendListMessageReply = decode(&reply).expect("decode send reply");
    assert_eq!(sent.queued_count, 1);
}
