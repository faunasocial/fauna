//! Integration round-trip for the connection-management cluster —
//! `fauna.knocks.{list,accept,block,dismiss}`, `fauna.contacts.{list,confirm}`,
//! `fauna.inbox.mode.{get,set}`. A behavior-preserving transport migration of
//! the HTTP routes `GET /api/v1/knocks/{a}`,
//! `POST /api/v1/knocks/{a}/{accept,block,dismiss}`,
//! `GET /api/v1/contacts/{a}`, `POST /api/v1/contacts/{a}/confirm`,
//! `GET|PUT /api/v1/inbox-mode/{a}`. The handlers reuse the same `CacheDb`
//! methods the HTTP twins call, plus the `pub(crate)` cores `knock_routes.rs`
//! extracts for the multi-DB-call writes — these tests
//! exercise the WS-RPC layer: request decode, the reused logic reaching a real
//! `CacheDb`, reply encoding, the connection-actor scoping (the HTTP twin's
//! path-param-vs-bearer match is implicit here), the invalid-mode gate, and
//! the allowlist.
//!
//! Authority for the wire types: `libs/fauna-protocol/src/contacts.rs`.
//! Slice tracked internally (§ T2).
//!
//! Tier: tier_3 (real `fauna-nest` `AppState` + real in-memory `CacheDb` —
//! no mocks). Matches `conformance_posts.rs` / `conformance_notifications.rs`.

mod common;
use common::dispatch;
use common::encode;

use std::collections::BTreeMap;
use std::sync::Arc;

use bytes::Bytes;

use fauna_nest::{
    bridge_method_allowlist::{CallerClass, is_permitted},
    contacts_handlers,
    db::CacheDb,
    routes::AppState,
    rpc_router::RpcRouter,
};
use fauna_protocol::{
    contacts::{
        ContactConfirmReply, ContactListReply, ContactListRequest, ContactStatusReply,
        InboxModeGetReply, InboxModeGetRequest, InboxModeSetReply, InboxModeSetRequest,
        KnockActionReply, KnockActionRequest, KnockListReply, KnockListRequest,
    },
    decode_strict as decode,
};

async fn router_with_db() -> (RpcRouter, Arc<AppState>) {
    let db = Arc::new(CacheDb::open_in_memory().unwrap());
    let state = Arc::new(AppState::for_test(db));
    let mut b = RpcRouter::builder();
    contacts_handlers::register_contacts_handlers(&mut b);
    (b.build(), state)
}

fn empty_knock_list() -> Bytes {
    encode(&KnockListRequest {
        extra: BTreeMap::new(),
    })
}

fn peer_action(peer_hex: &str) -> Bytes {
    encode(&KnockActionRequest {
        peer_id: peer_hex.into(),
        extra: BTreeMap::new(),
    })
}

/// Seed a knock from `peer` together with its notification row — the
/// doorbell `store_knock` writes beside every knock (`behavior/notifications.md`
/// § Retention, rule 3 binds the two lifecycles).
async fn seed_knock_with_doorbell(state: &Arc<AppState>, actor: &[u8; 32], peer: &[u8; 32]) {
    state
        .db
        .push_knock(actor, peer, b"n", "knock", &[])
        .await
        .unwrap();
    state
        .db
        .insert_notification(
            actor,
            &fauna_protocol::notifications::NotifType::Knock,
            "fauna",
            Some(peer),
            None,
            None,
            &fauna_nest::db::notifications::NotificationText::new(
                fauna_protocol::LocalizedText::new("notifications.row_knock")
                    .with_arg("sender", "aa")
                    .with_arg("message", "knock"),
                "aa wants to connect: knock",
            ),
            1000,
        )
        .await
        .unwrap()
        .expect("doorbell inserted");
}

async fn doorbell_count(state: &Arc<AppState>, actor: &[u8; 32]) -> usize {
    state
        .db
        .list_notifications(actor, None, 100)
        .await
        .unwrap()
        .into_iter()
        .filter(|n| n.notif_type.as_wire() == "knock")
        .count()
}

// ── fauna.knocks.list ──────────────────────────────────────────

#[tokio::test]
async fn knocks_list_returns_seeded_knock() {
    let (router, state) = router_with_db().await;
    let actor = [11u8; 32];
    let peer = [22u8; 32];
    state
        .db
        .push_knock(&actor, &peer, b"node.example", "wants to chat", &[])
        .await
        .unwrap();

    let reply: KnockListReply = decode(
        &dispatch(
            &router,
            state,
            actor,
            "fauna.knocks.list",
            empty_knock_list(),
        )
        .await
        .expect("list ok"),
    )
    .unwrap();
    assert_eq!(reply.knocks.len(), 1);
    assert_eq!(reply.knocks[0].sender, hex::encode(peer));
    assert_eq!(reply.knocks[0].sender_node, "node.example");
    assert_eq!(reply.knocks[0].summary, "wants to chat");
}

#[tokio::test]
async fn knocks_list_scoped_to_connection_actor() {
    let (router, state) = router_with_db().await;
    let actor_a = [11u8; 32];
    let actor_b = [99u8; 32];
    let peer = [22u8; 32];
    state
        .db
        .push_knock(&actor_a, &peer, b"n", "for a", &[])
        .await
        .unwrap();
    state
        .db
        .push_knock(&actor_b, &peer, b"n", "for b", &[])
        .await
        .unwrap();

    // The connection actor (a) sees only its own knock — the HTTP twin's
    // path-param-vs-bearer match is implicit on the WS-RPC plane.
    let reply: KnockListReply = decode(
        &dispatch(
            &router,
            state,
            actor_a,
            "fauna.knocks.list",
            empty_knock_list(),
        )
        .await
        .expect("list ok"),
    )
    .unwrap();
    assert_eq!(reply.knocks.len(), 1);
    assert_eq!(reply.knocks[0].summary, "for a");
}

// ── fauna.knocks.accept / block / dismiss ──────────────────────

#[tokio::test]
async fn knocks_accept_creates_contact_and_clears_knock() {
    let (router, state) = router_with_db().await;
    let actor = [11u8; 32];
    let peer = [22u8; 32];
    state
        .db
        .push_knock(&actor, &peer, b"n", "knock", &[])
        .await
        .unwrap();

    let _: KnockActionReply = decode(
        &dispatch(
            &router,
            state.clone(),
            actor,
            "fauna.knocks.accept",
            peer_action(&hex::encode(peer)),
        )
        .await
        .expect("accept ok"),
    )
    .unwrap();

    assert!(
        state.db.poll_knocks(&actor).await.unwrap().is_empty(),
        "knock cleared"
    );
    assert_eq!(
        state
            .db
            .get_contact_status(&actor, &peer)
            .await
            .unwrap()
            .as_deref(),
        Some("accepted"),
    );
}

// ── the knock doorbell goes with its knock ─────────────────────
//
// `behavior/notifications.md` § Retention, rule 3: dismiss and block delete
// the sender's `knock` notification row with the knock; accept keeps it (the
// knock row goes on accept, so the doorbell is the message's only home and
// the record of an accepted request). A doorbell from a different sender is
// never touched.

#[tokio::test]
async fn knocks_dismiss_deletes_the_senders_doorbell_and_no_other() {
    let (router, state) = router_with_db().await;
    let actor = [11u8; 32];
    let peer = [22u8; 32];
    let bystander = [23u8; 32];
    seed_knock_with_doorbell(&state, &actor, &peer).await;
    seed_knock_with_doorbell(&state, &actor, &bystander).await;
    assert_eq!(doorbell_count(&state, &actor).await, 2);

    let _: KnockActionReply = decode(
        &dispatch(
            &router,
            state.clone(),
            actor,
            "fauna.knocks.dismiss",
            peer_action(&hex::encode(peer)),
        )
        .await
        .expect("dismiss ok"),
    )
    .unwrap();

    let left = state
        .db
        .list_notifications(&actor, None, 100)
        .await
        .unwrap();
    assert_eq!(left.len(), 1, "the dismissed knock's doorbell went with it");
    assert_eq!(left[0].sender_id.as_deref(), Some(bystander.as_slice()));
}

#[tokio::test]
async fn knocks_block_deletes_the_senders_doorbell() {
    let (router, state) = router_with_db().await;
    let actor = [11u8; 32];
    let peer = [22u8; 32];
    seed_knock_with_doorbell(&state, &actor, &peer).await;

    let _: KnockActionReply = decode(
        &dispatch(
            &router,
            state.clone(),
            actor,
            "fauna.knocks.block",
            peer_action(&hex::encode(peer)),
        )
        .await
        .expect("block ok"),
    )
    .unwrap();

    assert_eq!(doorbell_count(&state, &actor).await, 0);
}

#[tokio::test]
async fn knocks_accept_keeps_the_senders_doorbell() {
    let (router, state) = router_with_db().await;
    let actor = [11u8; 32];
    let peer = [22u8; 32];
    seed_knock_with_doorbell(&state, &actor, &peer).await;

    let _: KnockActionReply = decode(
        &dispatch(
            &router,
            state.clone(),
            actor,
            "fauna.knocks.accept",
            peer_action(&hex::encode(peer)),
        )
        .await
        .expect("accept ok"),
    )
    .unwrap();

    assert!(state.db.poll_knocks(&actor).await.unwrap().is_empty());
    assert_eq!(
        doorbell_count(&state, &actor).await,
        1,
        "an accepted request's doorbell is the user's record"
    );
}

#[tokio::test]
async fn knocks_block_marks_blocked_and_clears_knock() {
    let (router, state) = router_with_db().await;
    let actor = [11u8; 32];
    let peer = [22u8; 32];
    state
        .db
        .push_knock(&actor, &peer, b"n", "knock", &[])
        .await
        .unwrap();

    let _: KnockActionReply = decode(
        &dispatch(
            &router,
            state.clone(),
            actor,
            "fauna.knocks.block",
            peer_action(&hex::encode(peer)),
        )
        .await
        .expect("block ok"),
    )
    .unwrap();

    assert!(state.db.poll_knocks(&actor).await.unwrap().is_empty());
    assert_eq!(
        state
            .db
            .get_contact_status(&actor, &peer)
            .await
            .unwrap()
            .as_deref(),
        Some("blocked"),
    );
}

#[tokio::test]
async fn knocks_block_then_unblock_clears_edge() {
    let (router, state) = router_with_db().await;
    let actor = [11u8; 32];
    let peer = [22u8; 32];
    // Block the peer first — the only state from which unblock acts.
    state.db.block_contact(&actor, &peer).await.unwrap();
    assert_eq!(
        state
            .db
            .get_contact_status(&actor, &peer)
            .await
            .unwrap()
            .as_deref(),
        Some("blocked"),
    );

    let _: KnockActionReply = decode(
        &dispatch(
            &router,
            state.clone(),
            actor,
            "fauna.knocks.unblock",
            peer_action(&hex::encode(peer)),
        )
        .await
        .expect("unblock ok"),
    )
    .unwrap();

    // Option (a) clear-the-edge (contacts.md § Where logic lives → Unblock):
    // the relationship returns to no-edge.
    assert_eq!(
        state.db.get_contact_status(&actor, &peer).await.unwrap(),
        None,
        "unblock clears the blocked edge entirely",
    );
}

#[tokio::test]
async fn knocks_unblock_is_noop_on_non_blocked_edge() {
    let (router, state) = router_with_db().await;
    let actor = [11u8; 32];
    let peer = [22u8; 32];
    // A live `accepted` contact — unblock must NOT clear it. The guard
    // (`DELETE ... AND status = 'blocked'`) is what makes a stale client
    // snapshot's "Unblock" click safe against a real relationship.
    state.db.accept_contact(&actor, &peer).await.unwrap();

    let _: KnockActionReply = decode(
        &dispatch(
            &router,
            state.clone(),
            actor,
            "fauna.knocks.unblock",
            peer_action(&hex::encode(peer)),
        )
        .await
        .expect("unblock ok (no-op)"),
    )
    .unwrap();

    assert_eq!(
        state
            .db
            .get_contact_status(&actor, &peer)
            .await
            .unwrap()
            .as_deref(),
        Some("accepted"),
        "unblock is a no-op on a non-blocked edge",
    );
}

#[tokio::test]
async fn knocks_dismiss_clears_knock_and_contact() {
    let (router, state) = router_with_db().await;
    let actor = [11u8; 32];
    let peer = [22u8; 32];
    // Seed an existing contact + a fresh knock; dismiss deletes both so the
    // peer can knock again.
    state.db.accept_contact(&actor, &peer).await.unwrap();
    state
        .db
        .push_knock(&actor, &peer, b"n", "knock", &[])
        .await
        .unwrap();

    let _: KnockActionReply = decode(
        &dispatch(
            &router,
            state.clone(),
            actor,
            "fauna.knocks.dismiss",
            peer_action(&hex::encode(peer)),
        )
        .await
        .expect("dismiss ok"),
    )
    .unwrap();

    assert!(state.db.poll_knocks(&actor).await.unwrap().is_empty());
    assert_eq!(
        state.db.get_contact_status(&actor, &peer).await.unwrap(),
        None
    );
}

#[tokio::test]
async fn knocks_accept_rejects_malformed_peer_id() {
    let (router, state) = router_with_db().await;
    let err = dispatch(
        &router,
        state,
        [11u8; 32],
        "fauna.knocks.accept",
        peer_action("not-hex"),
    )
    .await
    .expect_err("malformed peer_id rejected");
    assert_eq!(err.code, "fauna.knocks.invalid_params");
}

// ── fauna.contacts.list / confirm ──────────────────────────────

#[tokio::test]
async fn contacts_list_returns_accepted_contact() {
    let (router, state) = router_with_db().await;
    let actor = [11u8; 32];
    let peer = [22u8; 32];
    state.db.accept_contact(&actor, &peer).await.unwrap();

    let reply: ContactListReply = decode(
        &dispatch(
            &router,
            state,
            actor,
            "fauna.contacts.list",
            encode(&ContactListRequest {
                extra: BTreeMap::new(),
            }),
        )
        .await
        .expect("list ok"),
    )
    .unwrap();
    assert_eq!(reply.contacts.len(), 1);
    assert_eq!(reply.contacts[0].peer_id, hex::encode(peer));
    assert_eq!(reply.contacts[0].status, "accepted");
    assert!(
        reply.contacts[0].accepted_at.is_some(),
        "accepted contact has accepted_at"
    );
    // `peer` is not a local `users` row → federated default: no handle/domain.
    assert_eq!(reply.contacts[0].handle, None);
    assert_eq!(reply.contacts[0].domain, None);
}

#[tokio::test]
async fn contacts_list_enriches_local_peer_handle_and_domain() {
    // A local peer (a registered user with a handle) is enriched with its
    // handle + this nest's handle domain, joined nest-side from the `users`
    // table; a federated peer (no `users` row) gets neither. Authority:
    // `docs/goal/ui/contacts.md` § State & data shape + § Encryption at rest.
    let db = Arc::new(CacheDb::open_in_memory().unwrap());
    let mut st = AppState::for_test(db.clone());
    st.auth.registration.handle_domain = Some("fauna.example".into());
    let state = Arc::new(st);
    let mut b = RpcRouter::builder();
    contacts_handlers::register_contacts_handlers(&mut b);
    let router = b.build();

    let actor = [11u8; 32];
    let local_peer = [22u8; 32];
    let federated_peer = [33u8; 32];

    // local_peer is a registered user with handle "alice"; federated_peer is
    // not a local user on this nest.
    state
        .db
        .create_user_with_handle(&local_peer, "free", "alice", None)
        .await
        .unwrap();
    state.db.accept_contact(&actor, &local_peer).await.unwrap();
    state
        .db
        .accept_contact(&actor, &federated_peer)
        .await
        .unwrap();

    let reply: ContactListReply = decode(
        &dispatch(
            &router,
            state,
            actor,
            "fauna.contacts.list",
            encode(&ContactListRequest {
                extra: BTreeMap::new(),
            }),
        )
        .await
        .expect("list ok"),
    )
    .unwrap();

    let local = reply
        .contacts
        .iter()
        .find(|c| c.peer_id == hex::encode(local_peer))
        .expect("local peer present");
    assert_eq!(
        local.handle.as_deref(),
        Some("alice"),
        "local peer's handle is joined from the users table"
    );
    assert_eq!(
        local.domain.as_deref(),
        Some("fauna.example"),
        "local peer's domain is this nest's handle domain"
    );

    let federated = reply
        .contacts
        .iter()
        .find(|c| c.peer_id == hex::encode(federated_peer))
        .expect("federated peer present");
    assert_eq!(
        federated.handle, None,
        "federated peer has no cached handle on this nest"
    );
    assert_eq!(
        federated.domain, None,
        "federated peer has no domain on this nest"
    );
}

#[tokio::test]
async fn contacts_confirm_promotes_accepted_to_confirmed() {
    let (router, state) = router_with_db().await;
    let actor = [11u8; 32];
    let peer = [22u8; 32];
    state.db.accept_contact(&actor, &peer).await.unwrap();

    let _: ContactConfirmReply = decode(
        &dispatch(
            &router,
            state.clone(),
            actor,
            "fauna.contacts.confirm",
            peer_action(&hex::encode(peer)),
        )
        .await
        .expect("confirm ok"),
    )
    .unwrap();
    assert_eq!(
        state
            .db
            .get_contact_status(&actor, &peer)
            .await
            .unwrap()
            .as_deref(),
        Some("confirmed"),
    );
}

// ── fauna.contacts.status ──────────────────────────────────────
// The single-actor status lookup the recipient folder contact gate reads
// (`folders.md` § Sharing) — the connection actor's relationship to one peer.

#[tokio::test]
async fn contacts_status_returns_the_stored_status() {
    let (router, state) = router_with_db().await;
    let actor = [11u8; 32];
    let peer = [22u8; 32];
    // `accept_contact` stamps the edge `accepted` — an Auto disposition sharer.
    state.db.accept_contact(&actor, &peer).await.unwrap();

    let reply: ContactStatusReply = decode(
        &dispatch(
            &router,
            state,
            actor,
            "fauna.contacts.status",
            peer_action(&hex::encode(peer)),
        )
        .await
        .expect("status ok"),
    )
    .unwrap();
    assert_eq!(reply.status.as_deref(), Some("accepted"));
}

#[tokio::test]
async fn contacts_status_is_none_for_a_stranger() {
    let (router, state) = router_with_db().await;
    let actor = [11u8; 32];
    let peer = [22u8; 32];
    // No contact edge → `None` (the gate treats a stranger as a knock).
    let reply: ContactStatusReply = decode(
        &dispatch(
            &router,
            state,
            actor,
            "fauna.contacts.status",
            peer_action(&hex::encode(peer)),
        )
        .await
        .expect("status ok"),
    )
    .unwrap();
    assert!(reply.status.is_none());
}

#[tokio::test]
async fn contacts_status_scoped_to_connection_actor() {
    let (router, state) = router_with_db().await;
    let actor_a = [11u8; 32];
    let actor_b = [99u8; 32];
    let peer = [22u8; 32];
    // Only actor_a has an edge to the peer; actor_b (the connection actor) sees
    // its OWN relationship — none — never actor_a's (the HTTP twin's bearer scope).
    state.db.accept_contact(&actor_a, &peer).await.unwrap();

    let reply: ContactStatusReply = decode(
        &dispatch(
            &router,
            state,
            actor_b,
            "fauna.contacts.status",
            peer_action(&hex::encode(peer)),
        )
        .await
        .expect("status ok"),
    )
    .unwrap();
    assert!(
        reply.status.is_none(),
        "the status is scoped to the connection actor, not any actor with an edge"
    );
}

// ── fauna.inbox.mode.get / set ─────────────────────────────────

#[tokio::test]
async fn inbox_mode_get_defaults_to_allow_knock() {
    let (router, state) = router_with_db().await;
    let reply: InboxModeGetReply = decode(
        &dispatch(
            &router,
            state,
            [11u8; 32],
            "fauna.inbox.mode.get",
            encode(&InboxModeGetRequest {
                extra: BTreeMap::new(),
            }),
        )
        .await
        .expect("get ok"),
    )
    .unwrap();
    assert_eq!(reply.mode, "allow_knock");
}

#[tokio::test]
async fn inbox_mode_set_then_get_round_trips() {
    let (router, state) = router_with_db().await;
    let actor = [11u8; 32];

    let _: InboxModeSetReply = decode(
        &dispatch(
            &router,
            state.clone(),
            actor,
            "fauna.inbox.mode.set",
            encode(&InboxModeSetRequest {
                mode: "contacts_only".into(),
                extra: BTreeMap::new(),
            }),
        )
        .await
        .expect("set ok"),
    )
    .unwrap();

    let reply: InboxModeGetReply = decode(
        &dispatch(
            &router,
            state,
            actor,
            "fauna.inbox.mode.get",
            encode(&InboxModeGetRequest {
                extra: BTreeMap::new(),
            }),
        )
        .await
        .expect("get ok"),
    )
    .unwrap();
    assert_eq!(reply.mode, "contacts_only");
}

#[tokio::test]
async fn inbox_mode_set_rejects_invalid_mode() {
    let (router, state) = router_with_db().await;
    let err = dispatch(
        &router,
        state,
        [11u8; 32],
        "fauna.inbox.mode.set",
        encode(&InboxModeSetRequest {
            mode: "bogus".into(),
            extra: BTreeMap::new(),
        }),
    )
    .await
    .expect_err("invalid mode rejected");
    assert_eq!(err.code, "fauna.inbox.invalid_params");
}

// ── replay metadata + allowlist ────────────────────────────────

const CONTACTS_KINDS: [&str; 10] = [
    "fauna.knocks.list",
    "fauna.knocks.accept",
    "fauna.knocks.block",
    "fauna.knocks.unblock",
    "fauna.knocks.dismiss",
    "fauna.contacts.list",
    "fauna.contacts.status",
    "fauna.contacts.confirm",
    "fauna.inbox.mode.get",
    "fauna.inbox.mode.set",
];

#[tokio::test]
async fn replay_metadata_matches_spec() {
    let (router, _state) = router_with_db().await;
    for kind in CONTACTS_KINDS {
        let meta = router.kind_meta(kind).expect("kind registered");
        assert!(
            !meta.forbid_replay,
            "{kind} is replay-safe (read / idempotent write)"
        );
        assert_eq!(meta.default_deadline, std::time::Duration::from_secs(5));
    }
}

#[tokio::test]
async fn contacts_cluster_kinds_are_user_only_at_allowlist_layer() {
    for kind in CONTACTS_KINDS {
        assert!(
            is_permitted(CallerClass::User, kind),
            "{kind} should be permitted for User"
        );
        // `Admin ⊇ User` — an admin is a user who additionally holds the admin
        // role, so it inherits every user-facing kind (documented invariant in
        // `bridge_method_allowlist`). Only the bridge actors have no role on
        // this end-user surface and stay denied.
        assert!(
            is_permitted(CallerClass::Admin, kind),
            "{kind} should be permitted for Admin (Admin ⊇ User)"
        );
        for class in [CallerClass::BridgeMta, CallerClass::BridgeMda] {
            assert!(
                !is_permitted(class, kind),
                "{kind} should be denied for {class:?}"
            );
        }
    }
}
