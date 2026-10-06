//! `fauna.bridges.send_list_message` refuses a message carrying other than
//! exactly one From field, before any list cap is reserved
//! (`smtp-server.md` § Architectural rules → *Exactly one From field*).
//!
//! The Go outbound worker picks each fan-out copy's DKIM key by the message's
//! From domain, which `mail-parser` reads from the LAST From field, while a
//! receiver's DMARC may align against the FIRST — so a second From field would
//! leave a list send signed for one address and carrying another.

mod common;

use std::sync::Arc;

use bytes::Bytes;
use serde_bytes::ByteBuf;

use common::{TEST_DOMAIN as DOMAIN, dispatch, register_user};
use fauna_nest::bridge_list_handlers::register_bridge_list_handlers;
use fauna_nest::db::CacheDb;
use fauna_nest::routes::AppState;
use fauna_nest::rpc_router::RpcRouter;
use fauna_protocol::bridge_routing::{SendListMessageReply, SendListMessageRequest};
use fauna_protocol::{RpcError, decode_strict as decode, encode_canonical};

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

async fn send_list(
    router: &RpcRouter,
    state: &Arc<AppState>,
    owner: [u8; 32],
    list_id: [u8; 16],
    message: Vec<u8>,
) -> Result<SendListMessageReply, RpcError> {
    let req = SendListMessageRequest {
        list_id: ByteBuf::from(list_id.to_vec()),
        message: ByteBuf::from(message),
    };
    let payload = Bytes::from(encode_canonical(&req).unwrap().to_vec());
    let reply = dispatch(
        router,
        state.clone(),
        owner,
        "fauna.bridges.send_list_message",
        payload,
    )
    .await?;
    Ok(decode(&reply).expect("decode send_list_message reply"))
}

#[tokio::test]
async fn a_list_message_with_other_than_one_from_field_is_refused_before_any_cap() {
    let (router, state) = router_and_state().await;
    let owner: [u8; 32] = [0xC1; 32];
    register_user(&state, owner, "owner").await;
    let (list_id, _alias_id) = state
        .db
        .create_list(&owner, DOMAIN, "news", None, None, None, None, None)
        .await
        .expect("create the owner's list");
    state
        .db
        .add_member(&list_id, "member@external.test", "unsubscribe-token")
        .await
        .expect("subscribe one external member");

    let own = format!("owner@{DOMAIN}");
    let with_froms = |froms: &[&str]| {
        let mut m = String::new();
        for from in froms {
            m.push_str(&format!("From: {from}\r\n"));
        }
        m.push_str(&format!("To: news@{DOMAIN}\r\nSubject: weekly\r\n\r\nbody"));
        m.into_bytes()
    };
    for (label, message) in [
        ("victim first", with_froms(&["ceo@bank.test", &own])),
        ("own first", with_froms(&[&own, "ceo@bank.test"])),
        ("no From field", with_froms(&[])),
    ] {
        let err = send_list(&router, &state, owner, list_id, message)
            .await
            .expect_err(label);
        assert_eq!(err.code, "fauna.bridges.invalid_params", "{label}");
        let detail = format!("{:?}", err.details);
        assert!(detail.contains("exactly one From"), "{label}: {detail}");
    }

    // The control: one From field fans out, and the account's daily list quota
    // shows a single recipient spent — none of the three refusals above
    // reserved any of it.
    let per_account_ceiling = state
        .db
        .get_mass_mailing_policy()
        .await
        .expect("read the mass-mailing policy")
        .effective()
        .list_recipients_per_account_per_day_ceiling as u64;
    let reply = send_list(&router, &state, owner, list_id, with_froms(&[&own]))
        .await
        .expect("one From field sends");
    assert_eq!(reply.queued_count, 1);
    assert_eq!(reply.estimated_quota_remaining, per_account_ceiling - 1);
}
