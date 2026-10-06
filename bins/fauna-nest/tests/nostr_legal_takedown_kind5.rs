#![cfg(feature = "nostr")]
//! tier_3: the outbound kind-5 leg of a LEGAL TAKEDOWN — the derived-event
//! lifecycle ("a derived Nostr event must not outlive the post it was derived
//! from") honored on its second trigger (`docs/goal/behavior/moderation.md`
//! § Legal takedown; `docs/goal/ui/nostr.md` § The relay event store, NIP-09).
//!
//! Drives the REAL production flow over router dispatch: author creates a
//! post via `fauna.posts.create` → it is materialized into the relay store →
//! an admin takes it down via `fauna.moderation.legal_takedown` → the store
//! holds a signed kind-5 naming the DERIVED event id, the derived row is
//! gone, the kind-5 reached the crosspost queue, a repeat takedown does not
//! re-publish — and a RESTORE does not re-materialize (the overturn ruling:
//! the permanent TakenDown obligation row excludes the post from every
//! materialization path; re-publication needs a fresh author act). The
//! reconcile arm heals a takedown whose propagation never ran (crash between
//! commit and propagate).

mod common;
use common::dispatch;

use std::sync::Arc;

use bytes::Bytes;

use fauna_bridge_nostr::signing::{Keypair, verify_event};
use fauna_bridge_nostr::types::Filter;
use fauna_nest::db::CacheDb;
use fauna_nest::moderation_handlers;
use fauna_nest::nostr::key_crypto::encrypt_nostr_privkey;
use fauna_nest::nostr::{self, db as nostr_db, store, sync_worker::OutboundEvent};
use fauna_nest::posts_handlers;
use fauna_nest::routes::AppState;
use fauna_nest::rpc_router::RpcRouter;
use fauna_protocol::{
    decode_strict as decode, encode_canonical,
    moderation::{ModerationLegalTakedownReply, ModerationLegalTakedownRequest},
    posts::PostCreateRequest,
};

/// Real `AppState` with the nostr tables and a capturable outbound channel
/// (the default test `NostrState` drops its receiver, which would make the
/// crosspost-enqueue assertion vacuous).
async fn state_with_outbound() -> (
    RpcRouter,
    Arc<AppState>,
    tokio::sync::mpsc::Receiver<OutboundEvent>,
) {
    let db = Arc::new(CacheDb::open_in_memory().expect("open in-memory db"));
    nostr::init_db(&db).await.expect("init nostr tables");
    let mut state = AppState::for_test(db);
    let (sync_tx, sync_rx) = tokio::sync::mpsc::channel(16);
    state.nostr.sync_tx = sync_tx;
    let state = Arc::new(state);
    let mut b = RpcRouter::builder();
    posts_handlers::register_posts_handlers(&mut b);
    moderation_handlers::register_moderation_handlers(&mut b);
    (b.build(), state, sync_rx)
}

/// Deposit the author's nsec (encrypted to this state's nest key) so derived
/// events — and their kind-5 retraction — can be signed at the established
/// position.
async fn link_nostr_account(state: &Arc<AppState>, actor: [u8; 32], nostr_kp: &Keypair) {
    let nest_key = state.nest_identity.signing_key.to_bytes();
    let encrypted = encrypt_nostr_privkey(&nest_key, &nostr_kp.secret_bytes()).unwrap();
    let conn = state.db.conn().await;
    nostr_db::link_account(
        &conn,
        &hex::encode(actor),
        &nostr_kp.public_key_hex(),
        "generated",
        Some(&encrypted),
        None,
        None,
    )
    .unwrap();
}

/// Arrange the materialized-post precondition (e2e carve-out (b): fixture
/// setup, not the mutation under test — which is the TAKEDOWN below). Same
/// shape as `nostr_post_delete_kind5.rs`.
async fn create_and_materialize(
    router: &RpcRouter,
    state: &Arc<AppState>,
    author_kp: &fauna_core::identity::ActorKeypair,
    nostr_kp: &Keypair,
    marker: &str,
) -> (String, String) {
    let actor = author_kp.actor_id().0;
    let body = {
        use fauna_core::data::{Post, PostBody, Timestamp};
        use fauna_core::encoding::sign_and_pack;
        let post = Post {
            author: author_kp.actor_id(),
            created_at: Timestamp(1_000_000_000_000),
            body: PostBody::Text {
                content: marker.into(),
                facets: vec![],
            },
            references: vec![],
            expires_at: None,
            gated: None,
            content_warning: None,
            origin: None,
        };
        sign_and_pack(author_kp, &post).unwrap()
    };
    let post_id_bytes = *blake3::hash(&body).as_bytes();
    let post_id = hex::encode(post_id_bytes);

    dispatch(
        router,
        state.clone(),
        actor,
        "fauna.posts.create",
        Bytes::from(
            encode_canonical(&PostCreateRequest {
                body: serde_bytes::ByteBuf::from(body),
                extra: Default::default(),
            })
            .unwrap()
            .to_vec(),
        ),
    )
    .await
    .expect("create ok");

    let n = store::materialize_account(
        &state.db,
        &state.post_segments,
        &hex::encode(actor),
        nostr_kp,
    )
    .await
    .expect("materialize ok");
    assert_eq!(n, 1, "the fresh post materialized");
    let conn = state.db.conn().await;
    let ids = nostr_db::list_event_ids_by_fauna_id(&conn, &post_id).expect("map read");
    assert_eq!(ids.len(), 1, "one derived event mapped");
    (post_id, ids[0].clone())
}

fn stored_kind5s(conn: &rusqlite::Connection) -> Vec<fauna_bridge_nostr::types::Event> {
    store::query_events(
        conn,
        &[Filter {
            kinds: Some(vec![5]),
            ..Default::default()
        }],
        100,
    )
    .unwrap()
}

fn takedown_payload(post_id_hex: &str, reference: &str, restore: bool) -> Bytes {
    Bytes::from(
        encode_canonical(&ModerationLegalTakedownRequest {
            content_id: post_id_hex.to_string(),
            content_type: "post".into(),
            legal_reference: reference.to_string(),
            restore,
            extra: Default::default(),
        })
        .unwrap()
        .to_vec(),
    )
}

#[tokio::test]
async fn takedown_publishes_kind5_and_restore_does_not_rematerialize() {
    let (router, state, mut sync_rx) = state_with_outbound().await;
    let author_kp = fauna_core::identity::ActorKeypair::generate();
    let actor = author_kp.actor_id().0;
    let nostr_kp = Keypair::generate();
    let admin = [0x2Au8; 32];
    state.db.add_admin_actor(&admin).await.unwrap();

    link_nostr_account(&state, actor, &nostr_kp).await;
    let (post_id, derived_id) = create_and_materialize(
        &router,
        &state,
        &author_kp,
        &nostr_kp,
        "takedown kind5 post",
    )
    .await;

    // The takedown over the wire — the production trigger.
    let reply = dispatch(
        &router,
        state.clone(),
        admin,
        "fauna.moderation.legal_takedown",
        takedown_payload(&post_id, "EU-DSA-2026/777", false),
    )
    .await
    .expect("takedown ok");
    let reply: ModerationLegalTakedownReply = decode(&reply).unwrap();
    assert_eq!(reply.status, "taken_down");

    let conn = state.db.conn().await;

    // A signed kind-5 is stored, and its `e` tag names the DERIVED event id.
    let kind5s = stored_kind5s(&conn);
    assert_eq!(kind5s.len(), 1, "exactly one deletion event stored");
    let kind5 = &kind5s[0];
    assert!(verify_event(kind5), "the kind-5 carries a valid signature");
    assert_eq!(kind5.pubkey, nostr_kp.public_key_hex());
    let e_values: Vec<&str> = kind5
        .tags
        .iter()
        .filter(|t| t.name() == Some("e"))
        .filter_map(|t| t.value())
        .collect();
    assert_eq!(e_values, vec![derived_id.as_str()]);
    // No reason, no legal reference in the event: the kind-5 asserts only
    // that the derived event's source is gone, never why.
    assert!(kind5.content.is_empty(), "kind-5 content stays empty");

    // The derived event is gone from the store; the map rows are dropped.
    let remaining = store::query_events(
        &conn,
        &[Filter {
            ids: Some(vec![derived_id.clone()]),
            ..Default::default()
        }],
        10,
    )
    .unwrap();
    assert!(
        remaining.is_empty(),
        "the derived event must not outlive the takedown"
    );
    assert!(
        nostr_db::list_event_ids_by_fauna_id(&conn, &post_id)
            .unwrap()
            .is_empty(),
        "map rows dropped after propagation"
    );
    drop(conn);

    // The kind-5 reached the external-relay crosspost queue.
    let outbound = sync_rx.try_recv().expect("a crossposted event");
    assert_eq!(outbound.event.kind, 5);
    assert_eq!(outbound.event.id, kind5.id);

    // A repeat takedown is idempotent on the Nostr side: no second kind-5.
    dispatch(
        &router,
        state.clone(),
        admin,
        "fauna.moderation.legal_takedown",
        takedown_payload(&post_id, "EU-DSA-2026/777", false),
    )
    .await
    .expect("repeat takedown ok");
    let conn = state.db.conn().await;
    assert_eq!(stored_kind5s(&conn).len(), 1, "no re-publish on retry");
    drop(conn);
    assert!(sync_rx.try_recv().is_err(), "no second crosspost");

    // RESTORE — the flag clears and the post re-serves on Fauna, but the
    // overturn does NOT re-materialize: the permanent TakenDown obligation
    // row excludes the post from the shared materializer input query
    // (`unmaterialized_posts_sql`), so every path — the immediate toggle
    // path, the periodic sweep, a re-expose — skips it by construction.
    let reply = dispatch(
        &router,
        state.clone(),
        admin,
        "fauna.moderation.legal_takedown",
        takedown_payload(&post_id, "overturned on appeal", true),
    )
    .await
    .expect("restore ok");
    let reply: ModerationLegalTakedownReply = decode(&reply).unwrap();
    assert_eq!(reply.status, "restored");

    let n = store::materialize_account(
        &state.db,
        &state.post_segments,
        &hex::encode(actor),
        &nostr_kp,
    )
    .await
    .expect("materialize sweep ok");
    assert_eq!(n, 0, "an overturned takedown must not auto-re-materialize");
    let conn = state.db.conn().await;
    assert!(
        nostr_db::list_event_ids_by_fauna_id(&conn, &post_id)
            .unwrap()
            .is_empty(),
        "no new derived event after restore"
    );
}

#[tokio::test]
async fn reconcile_retracts_a_takedown_whose_propagation_never_ran() {
    let (router, state, mut sync_rx) = state_with_outbound().await;
    let author_kp = fauna_core::identity::ActorKeypair::generate();
    let actor = author_kp.actor_id().0;
    let nostr_kp = Keypair::generate();

    link_nostr_account(&state, actor, &nostr_kp).await;
    let (post_id, derived_id) = create_and_materialize(
        &router,
        &state,
        &author_kp,
        &nostr_kp,
        "reconcile heal post",
    )
    .await;

    // The takedown transaction lands but its propagation never runs — a crash
    // between commit and propagate.
    let mut digest = [0u8; 32];
    hex::decode_to_slice(&post_id, &mut digest).unwrap();
    state
        .db
        .post_legal_takedown_txn(
            &digest,
            &post_id,
            Some("EU-DSA-2026/888"),
            &actor,
            &[0x2Bu8; 32],
            "admin=test reference=EU-DSA-2026/888",
            fauna_core::data::Timestamp::now_or_zero().as_i64(),
        )
        .await
        .expect("takedown txn");

    // The derived event still stands — exactly the state the reconcile heals.
    {
        let conn = state.db.conn().await;
        assert_eq!(
            nostr_db::list_event_ids_by_fauna_id(&conn, &post_id)
                .unwrap()
                .len(),
            1,
            "precondition: propagation has not run"
        );
    }

    let n = nostr::retract_taken_down_posts(&state)
        .await
        .expect("reconcile ok");
    assert_eq!(n, 1, "one post retracted");

    let conn = state.db.conn().await;
    let kind5s = stored_kind5s(&conn);
    assert_eq!(kind5s.len(), 1, "the reconcile signed the kind-5");
    let e_values: Vec<&str> = kind5s[0]
        .tags
        .iter()
        .filter(|t| t.name() == Some("e"))
        .filter_map(|t| t.value())
        .collect();
    assert_eq!(e_values, vec![derived_id.as_str()]);
    assert!(
        nostr_db::list_event_ids_by_fauna_id(&conn, &post_id)
            .unwrap()
            .is_empty(),
        "map rows dropped"
    );
    drop(conn);
    assert!(sync_rx.try_recv().is_ok(), "crossposted");

    // A second reconcile pass finds nothing — idempotent by map-row absence.
    let n = nostr::retract_taken_down_posts(&state)
        .await
        .expect("second reconcile ok");
    assert_eq!(n, 0, "nothing left to retract");
    let conn = state.db.conn().await;
    assert_eq!(stored_kind5s(&conn).len(), 1, "no duplicate kind-5");
}
