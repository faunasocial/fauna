#![cfg(feature = "nostr")]
//! tier_3: the outbound kind-5 leg of self-service post deletion — a derived
//! Nostr event must not outlive the Fauna post it was derived from
//! (`docs/goal/ui/feed.md` § State & data shape → *Post deletion*;
//! `docs/goal/ui/nostr.md` § The relay event store, NIP-09).
//!
//! Drives the REAL production flow end-to-end over router dispatch (no
//! shortcuts on the mutation under test): author creates a signed post via
//! `fauna.posts.create` → the post is materialized into the relay store at
//! the established signing position (`store::materialize_account`) → the
//! author deletes it via `fauna.posts.delete` (signed `Tombstone`) → the
//! store holds a signed kind-5 whose `e` tag names the DERIVED event id
//! (looked up through `nostr_event_map` — the predecessor translate helper
//! computed a never-lookup-able target from the post CID, which is exactly
//! the bug class this pins against), the derived row is gone, the kind-5
//! reached the external-relay crosspost queue, and a delete retry does not
//! re-publish. The key-less arm (nsec unlinked between materialize and
//! delete) still removes the derived row — local consistency never depends
//! on key availability.

mod common;
use common::dispatch;

use std::sync::Arc;

use bytes::Bytes;

use fauna_bridge_nostr::signing::{Keypair, verify_event};
use fauna_bridge_nostr::types::Filter;
use fauna_nest::db::CacheDb;
use fauna_nest::nostr::key_crypto::encrypt_nostr_privkey;
use fauna_nest::nostr::{self, db as nostr_db, store, sync_worker::OutboundEvent};
use fauna_nest::posts_handlers;
use fauna_nest::routes::AppState;
use fauna_nest::rpc_router::RpcRouter;
use fauna_protocol::{
    decode_strict as decode, encode_canonical,
    posts::{PostCreateRequest, PostDeleteReply, PostDeleteRequest},
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
    (b.build(), state, sync_rx)
}

fn signed_tombstone(kp: &fauna_core::identity::ActorKeypair, post_id_hex: &str) -> Bytes {
    use fauna_core::data::{PostId, Timestamp, Tombstone};
    use fauna_core::encoding::sign_and_pack;
    let mut digest = [0u8; 32];
    hex::decode_to_slice(post_id_hex, &mut digest).unwrap();
    let tombstone = Tombstone {
        author: kp.actor_id(),
        post_id: PostId::from_digest_dag_cbor(digest),
        created_at: Timestamp::now(),
    };
    let req = PostDeleteRequest {
        body: serde_bytes::ByteBuf::from(sign_and_pack(kp, &tombstone).unwrap()),
        extra: std::collections::BTreeMap::new(),
    };
    Bytes::from(encode_canonical(&req).unwrap().to_vec())
}

/// Arrange the materialized-post precondition (e2e carve-out (b): fixture
/// setup, not the mutation under test — which is the DELETE below). Creates a
/// public post through the real `fauna.posts.create` wire path (body → the
/// `__post` segment, `content.payload` left empty — the post-cutover shape),
/// then `materialize_account` reads it back **segment-first**
/// (`segments::post::load_post_body`, the fix for the adjacent gap `nostr.md`
/// § The relay event store recorded) and signs + stores its derived Nostr
/// event. Returns the post id (hex) and the derived Nostr event id.
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

#[tokio::test]
async fn delete_publishes_kind5_naming_the_derived_event() {
    let (router, state, mut sync_rx) = state_with_outbound().await;
    let author_kp = fauna_core::identity::ActorKeypair::generate();
    let actor = author_kp.actor_id().0;
    let nostr_kp = Keypair::generate();

    // Deposit the author's nsec (encrypted to this state's nest key) so the
    // kind-5 can be signed at the established position.
    {
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

    let (post_id, derived_id) =
        create_and_materialize(&router, &state, &author_kp, &nostr_kp, "kind5 e2e post").await;

    // Delete the post over the wire — the production trigger.
    let reply = dispatch(
        &router,
        state.clone(),
        actor,
        "fauna.posts.delete",
        signed_tombstone(&author_kp, &post_id),
    )
    .await
    .expect("delete ok");
    let deleted: PostDeleteReply = decode(&reply).unwrap();
    assert!(deleted.deleted);

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
        "the derived event must not outlive the post"
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
    assert!(!outbound.relay_urls.is_empty());

    // A delete retry is idempotent: no second kind-5, no second crosspost.
    let reply = dispatch(
        &router,
        state.clone(),
        actor,
        "fauna.posts.delete",
        signed_tombstone(&author_kp, &post_id),
    )
    .await
    .expect("repeat delete ok");
    let deleted: PostDeleteReply = decode(&reply).unwrap();
    assert!(!deleted.deleted, "already gone");
    let conn = state.db.conn().await;
    assert_eq!(stored_kind5s(&conn).len(), 1, "no re-publish on retry");
    drop(conn);
    assert!(sync_rx.try_recv().is_err(), "no second crosspost");
}

#[tokio::test]
async fn keyless_delete_still_removes_the_derived_event() {
    let (router, state, mut sync_rx) = state_with_outbound().await;
    let author_kp = fauna_core::identity::ActorKeypair::generate();
    let actor = author_kp.actor_id().0;
    let nostr_kp = Keypair::generate();

    {
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

    let (post_id, derived_id) =
        create_and_materialize(&router, &state, &author_kp, &nostr_kp, "keyless arm post").await;

    // The nsec disappears between materialize and delete (account unlink).
    {
        let conn = state.db.conn().await;
        conn.execute(
            "UPDATE nostr_accounts SET encrypted_privkey = NULL WHERE actor_id = ?1",
            [hex::encode(actor)],
        )
        .unwrap();
    }

    let reply = dispatch(
        &router,
        state.clone(),
        actor,
        "fauna.posts.delete",
        signed_tombstone(&author_kp, &post_id),
    )
    .await
    .expect("delete ok");
    let deleted: PostDeleteReply = decode(&reply).unwrap();
    assert!(deleted.deleted);

    let conn = state.db.conn().await;
    // The derived row is gone even without a signable kind-5 …
    let remaining = store::query_events(
        &conn,
        &[Filter {
            ids: Some(vec![derived_id]),
            ..Default::default()
        }],
        10,
    )
    .unwrap();
    assert!(remaining.is_empty(), "derived row removed key-less");
    // … no kind-5 was stored, and nothing was crossposted.
    assert!(stored_kind5s(&conn).is_empty(), "nothing signable to store");
    drop(conn);
    assert!(sync_rx.try_recv().is_err(), "nothing crossposted");
}

/// **Account deletion** retracts over nostr too — and it must do so BEFORE the
/// purge sweep destroys the key that signs the retraction.
///
/// This is the ordering pin, and it is the whole reason `finalize_user_deletion`
/// takes `&Arc<AppState>`. `nostr_accounts` is `Policy::Purge`, so it holds the
/// author's encrypted nsec — the only key that can sign the kind-5 — right up
/// until `purge_orphaned_actor_rows` drops it. Retract first and the fediverse
/// learns the post is gone; purge first and the retraction is silently
/// downgraded to `store.rs`'s key-less arm forever, leaving every relay copy
/// live and now permanently unretractable.
///
/// The final assertion is what makes this a *sequencing* pin rather than a
/// retraction pin: the account row is confirmed **purged** at the end, so a
/// green run proves both steps ran AND that they ran in this order. Swapping
/// them reds the kind-5 assertions while leaving the purge assertion green.
#[tokio::test]
async fn account_deletion_retracts_over_nostr_before_the_purge_destroys_the_key() {
    let (router, state, mut sync_rx) = state_with_outbound().await;
    let author_kp = fauna_core::identity::ActorKeypair::generate();
    let actor = author_kp.actor_id().0;
    let nostr_kp = Keypair::generate();
    let actor_hex = hex::encode(actor);

    {
        let nest_key = state.nest_identity.signing_key.to_bytes();
        let encrypted = encrypt_nostr_privkey(&nest_key, &nostr_kp.secret_bytes()).unwrap();
        let conn = state.db.conn().await;
        nostr_db::link_account(
            &conn,
            &actor_hex,
            &nostr_kp.public_key_hex(),
            "generated",
            Some(&encrypted),
            None,
            None,
        )
        .unwrap();
    }

    let (post_id, derived_id) =
        create_and_materialize(&router, &state, &author_kp, &nostr_kp, "account delete").await;

    // The production trigger: the executor's account-deletion finalize.
    fauna_nest::pending_actions::finalize_user_deletion(&state, &actor)
        .await
        .expect("account deletion finalizes");

    // The local post is gone. Asserted BEFORE taking the connection guard —
    // `CacheDb::conn()` is a `MutexGuard` over the single connection, so any
    // `db.*` helper called while it is held self-deadlocks.
    assert!(
        state
            .db
            .list_posts_by_author(&actor)
            .await
            .unwrap()
            .is_empty(),
        "the deleted actor keeps no live posts"
    );

    let conn = state.db.conn().await;

    // A *signed* kind-5 exists — only possible while the nsec was still there.
    let kind5s = stored_kind5s(&conn);
    assert_eq!(
        kind5s.len(),
        1,
        "account deletion must publish a kind-5 retraction for the actor's post"
    );
    let kind5 = &kind5s[0];
    assert!(verify_event(kind5), "the kind-5 carries a valid signature");
    assert_eq!(kind5.pubkey, nostr_kp.public_key_hex());
    let e_values: Vec<&str> = kind5
        .tags
        .iter()
        .filter(|t| t.name() == Some("e"))
        .filter_map(|t| t.value())
        .collect();
    assert_eq!(
        e_values,
        vec![derived_id.as_str()],
        "the retraction names the derived event"
    );

    // The derived event and its map rows are gone.
    assert!(
        store::query_events(
            &conn,
            &[Filter {
                ids: Some(vec![derived_id]),
                ..Default::default()
            }],
            10,
        )
        .unwrap()
        .is_empty(),
        "the derived event must not outlive the deleted account"
    );
    assert!(
        nostr_db::list_event_ids_by_fauna_id(&conn, &post_id)
            .unwrap()
            .is_empty(),
        "map rows dropped after propagation"
    );

    // The purge DID run afterwards — this is what makes the assertions
    // above a proof of ORDER, not merely of retraction.
    assert!(
        nostr_db::get_account(&conn, &actor_hex).unwrap().is_none(),
        "the purge sweep must still have dropped the nostr account row"
    );
    drop(conn);

    // The retraction reached the external-relay crosspost queue.
    let outbound = sync_rx.try_recv().expect("a crossposted retraction");
    assert_eq!(outbound.event.kind, 5);
    assert_eq!(outbound.event.id, kind5.id);
}
