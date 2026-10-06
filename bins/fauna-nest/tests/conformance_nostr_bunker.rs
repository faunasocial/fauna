//! Conformance + caller-scoping for the NIP-46 bunker control plane —
//! `fauna.nostr.bunker.{create_invite,list,revoke,set_label}`
//! (`docs/goal/ui/nostr.md` § The nest as the user's NIP-46 signer, control
//! plane bullet). Exercises the four User-class kinds through the same
//! dispatch path the production router uses, against the real `bunker` policy
//! cores and real in-memory `nostr_*` tables.
//!
//! Coverage:
//!   * `create_invite` mints a pending row and returns the composed
//!     `bunker://…?relay=…&secret=…` connect string (secret revealed once —
//!     only its hash rests in the DB);
//!   * `create_invite` without a linked custodial account → `invalid_params`;
//!   * `list` shows pending → active transitions and audit fields;
//!   * `set_label` / `revoke` mutate only the caller's own rows
//!     (caller-scoping: another actor sees nothing and mutates nothing);
//!   * `revoke` is reflected to the signing plane (the next NIP-46 request is
//!     refused — the roster is the enforcement point).
//!
//! Only compiled under `--features nostr`.

#![cfg(feature = "nostr")]

mod common;
use common::dispatch;
use common::encode;

use std::sync::Arc;

use fauna_bridge_nostr::nip46::{Nip46Method, Nip46Request};
use fauna_bridge_nostr::signing::Keypair;
use fauna_nest::nostr;
use fauna_nest::nostr::bunker;
use fauna_nest::nostr::bunker_handlers::register_nostr_bunker_handlers;
use fauna_nest::nostr::db;
use fauna_nest::nostr::key_crypto::encrypt_nostr_privkey;
use fauna_nest::routes::AppState;
use fauna_nest::rpc_router::RpcRouter;
use fauna_protocol::decode_strict as decode;
use fauna_protocol::nostr::{
    CreateBunkerInviteReply, CreateBunkerInviteRequest, ListBunkerAppsReply, ListBunkerAppsRequest,
    RevokeBunkerAppReply, RevokeBunkerAppRequest, SetBunkerAppLabelReply, SetBunkerAppLabelRequest,
};

async fn router_and_state() -> (RpcRouter, Arc<AppState>) {
    let db = Arc::new(fauna_nest::db::CacheDb::open_in_memory().expect("open in-memory db"));
    nostr::init_db(&db).await.expect("init nostr tables");
    let state = Arc::new(AppState::for_test(db));
    let mut b = RpcRouter::builder();
    register_nostr_bunker_handlers(&mut b);
    (b.build(), state)
}

/// Link a custodial account for `actor` and return the user keypair.
async fn link_custodial(state: &AppState, actor: [u8; 32]) -> Keypair {
    let nest_key = state.nest_identity.signing_key.to_bytes();
    let kp = Keypair::generate();
    let encrypted = encrypt_nostr_privkey(&nest_key, &kp.secret_bytes()).unwrap();
    let conn = state.db.conn().await;
    db::link_account(
        &conn,
        &hex::encode(actor),
        &kp.public_key_hex(),
        "generated",
        Some(&encrypted),
        None,
        None,
    )
    .unwrap();
    drop(conn);
    kp
}

#[tokio::test]
async fn create_invite_then_list_label_revoke_round_trip() {
    let (router, state) = router_and_state().await;
    let actor: [u8; 32] = [0x11; 32];
    link_custodial(&state, actor).await;

    // create_invite — connect string composed nest-side.
    let reply_bytes = dispatch(
        &router,
        state.clone(),
        actor,
        "fauna.nostr.bunker.create_invite",
        encode(&CreateBunkerInviteRequest::default()),
    )
    .await
    .expect("create_invite ok");
    let invite: CreateBunkerInviteReply = decode(&reply_bytes).expect("decode invite");
    assert_eq!(invite.signer_pubkey.len(), 64);
    assert!(
        invite
            .connect_string
            .starts_with(&format!("bunker://{}?relay=wss://", invite.signer_pubkey)),
        "{}",
        invite.connect_string
    );
    assert!(invite.connect_string.contains("/nostr&secret="));

    // The one-time secret never rests in plaintext: only a hash is stored.
    let secret = invite
        .connect_string
        .split("secret=")
        .nth(1)
        .expect("secret param")
        .to_string();
    {
        let conn = state.db.conn().await;
        let stored: Vec<u8> = conn
            .query_row(
                "SELECT secret_hash FROM nostr_bunker_apps WHERE id = ?1",
                [invite.connection_id],
                |r| r.get(0),
            )
            .unwrap();
        assert_ne!(
            stored,
            secret.as_bytes(),
            "secret must not rest in plaintext"
        );
    }

    // list — one pending row.
    let reply_bytes = dispatch(
        &router,
        state.clone(),
        actor,
        "fauna.nostr.bunker.list",
        encode(&ListBunkerAppsRequest::default()),
    )
    .await
    .expect("list ok");
    let list: ListBunkerAppsReply = decode(&reply_bytes).expect("decode list");
    assert_eq!(list.apps.len(), 1);
    assert_eq!(list.apps[0].status, "pending");
    assert_eq!(list.apps[0].id, invite.connection_id);
    assert!(list.apps[0].app_pubkey.is_none());

    // Activate through the signing plane (what the relay carve-out calls).
    let app = Keypair::generate();
    {
        let conn = state.db.conn().await;
        let nest_key = state.nest_identity.signing_key.to_bytes();
        let req = Nip46Request {
            id: "c1".into(),
            method: Nip46Method::Connect,
            params: vec![invite.signer_pubkey.clone(), secret],
        };
        let resp = bunker::handle_request(
            &conn,
            &nest_key,
            &invite.signer_pubkey,
            &app.public_key_hex(),
            &req,
            1_000,
        )
        .unwrap();
        assert!(resp.contains("ack"), "{resp}");
    }

    // list — now active with the app pubkey pinned + audit fields.
    let reply_bytes = dispatch(
        &router,
        state.clone(),
        actor,
        "fauna.nostr.bunker.list",
        encode(&ListBunkerAppsRequest::default()),
    )
    .await
    .expect("list ok");
    let list: ListBunkerAppsReply = decode(&reply_bytes).expect("decode list");
    assert_eq!(list.apps[0].status, "active");
    assert_eq!(
        list.apps[0].app_pubkey.as_deref(),
        Some(app.public_key_hex().as_str())
    );
    assert!(list.apps[0].use_count >= 1);

    // set_label.
    let reply_bytes = dispatch(
        &router,
        state.clone(),
        actor,
        "fauna.nostr.bunker.set_label",
        encode(&SetBunkerAppLabelRequest {
            connection_id: invite.connection_id,
            label: "Amethyst on phone".into(),
            extra: Default::default(),
        }),
    )
    .await
    .expect("set_label ok");
    let labeled: SetBunkerAppLabelReply = decode(&reply_bytes).expect("decode set_label");
    assert!(labeled.updated);

    // revoke — row disappears from list AND the signing plane refuses next.
    let reply_bytes = dispatch(
        &router,
        state.clone(),
        actor,
        "fauna.nostr.bunker.revoke",
        encode(&RevokeBunkerAppRequest {
            connection_id: invite.connection_id,
            extra: Default::default(),
        }),
    )
    .await
    .expect("revoke ok");
    let revoked: RevokeBunkerAppReply = decode(&reply_bytes).expect("decode revoke");
    assert!(revoked.revoked);

    let reply_bytes = dispatch(
        &router,
        state.clone(),
        actor,
        "fauna.nostr.bunker.list",
        encode(&ListBunkerAppsRequest::default()),
    )
    .await
    .expect("list ok");
    let list: ListBunkerAppsReply = decode(&reply_bytes).expect("decode list");
    assert!(list.apps.is_empty(), "revoked row must not be listed");

    {
        let conn = state.db.conn().await;
        let nest_key = state.nest_identity.signing_key.to_bytes();
        let req = Nip46Request {
            id: "p1".into(),
            method: Nip46Method::Ping,
            params: vec![],
        };
        let resp = bunker::handle_request(
            &conn,
            &nest_key,
            &invite.signer_pubkey,
            &app.public_key_hex(),
            &req,
            1_001,
        )
        .unwrap();
        assert!(
            resp.contains("unauthorized"),
            "revoke must bind on the signing plane: {resp}"
        );
    }
}

#[tokio::test]
async fn create_invite_requires_custodial_account() {
    let (router, state) = router_and_state().await;
    let actor: [u8; 32] = [0x22; 32];
    // No linked account at all.
    let err = dispatch(
        &router,
        state.clone(),
        actor,
        "fauna.nostr.bunker.create_invite",
        encode(&CreateBunkerInviteRequest::default()),
    )
    .await
    .expect_err("must refuse without an account");
    assert_eq!(err.code, "fauna.nostr.invalid_params");

    // Linked, but no deposited key (remote signing mode) — same refusal.
    {
        let conn = state.db.conn().await;
        db::link_account(
            &conn,
            &hex::encode(actor),
            "pk_remote",
            "remote",
            None,
            None,
            None,
        )
        .unwrap();
    }
    let err = dispatch(
        &router,
        state.clone(),
        actor,
        "fauna.nostr.bunker.create_invite",
        encode(&CreateBunkerInviteRequest::default()),
    )
    .await
    .expect_err("must refuse without a deposited key");
    assert_eq!(err.code, "fauna.nostr.invalid_params");
}

#[tokio::test]
async fn roster_is_caller_scoped() {
    let (router, state) = router_and_state().await;
    let owner: [u8; 32] = [0x33; 32];
    let stranger: [u8; 32] = [0x44; 32];
    link_custodial(&state, owner).await;
    link_custodial(&state, stranger).await;

    let reply_bytes = dispatch(
        &router,
        state.clone(),
        owner,
        "fauna.nostr.bunker.create_invite",
        encode(&CreateBunkerInviteRequest::default()),
    )
    .await
    .expect("create_invite ok");
    let invite: CreateBunkerInviteReply = decode(&reply_bytes).expect("decode invite");

    // The stranger's list is empty.
    let reply_bytes = dispatch(
        &router,
        state.clone(),
        stranger,
        "fauna.nostr.bunker.list",
        encode(&ListBunkerAppsRequest::default()),
    )
    .await
    .expect("list ok");
    let list: ListBunkerAppsReply = decode(&reply_bytes).expect("decode list");
    assert!(list.apps.is_empty(), "another actor's roster must be empty");

    // The stranger cannot revoke or relabel the owner's row.
    let reply_bytes = dispatch(
        &router,
        state.clone(),
        stranger,
        "fauna.nostr.bunker.revoke",
        encode(&RevokeBunkerAppRequest {
            connection_id: invite.connection_id,
            extra: Default::default(),
        }),
    )
    .await
    .expect("revoke dispatch ok");
    let revoked: RevokeBunkerAppReply = decode(&reply_bytes).expect("decode revoke");
    assert!(!revoked.revoked, "cross-actor revoke must be a no-op");

    let reply_bytes = dispatch(
        &router,
        state.clone(),
        stranger,
        "fauna.nostr.bunker.set_label",
        encode(&SetBunkerAppLabelRequest {
            connection_id: invite.connection_id,
            label: "hijack".into(),
            extra: Default::default(),
        }),
    )
    .await
    .expect("set_label dispatch ok");
    let labeled: SetBunkerAppLabelReply = decode(&reply_bytes).expect("decode set_label");
    assert!(!labeled.updated, "cross-actor set_label must be a no-op");

    // The owner still sees the untouched pending row.
    let reply_bytes = dispatch(
        &router,
        state.clone(),
        owner,
        "fauna.nostr.bunker.list",
        encode(&ListBunkerAppsRequest::default()),
    )
    .await
    .expect("list ok");
    let list: ListBunkerAppsReply = decode(&reply_bytes).expect("decode list");
    assert_eq!(list.apps.len(), 1);
    assert_eq!(list.apps[0].label, "");
}
