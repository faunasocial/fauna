//! tier_3 round-trip for the **CardDAV** encrypted address-book path — the
//! contacts twin of `conformance_caldav_client.rs`. A User-class Fauna app
//! (or the MDA) provisions an address book, PUTs a sealed vCard, queries it
//! back, syncs incrementally, and deletes it — driving the real nest
//! `fauna.bridges.{provision_addressbook,put_card_ciphertext,list_addressbooks,
//! query_cards,sync_addressbook_since,delete_card,delete_addressbook}` handlers against a real
//! in-memory `CacheDb`. It proves nest + the `bridge_carddav_*` store agree on
//! the wire contract and the ciphertext-only storage model (nest stores +
//! returns opaque sealed bodies verbatim; it never decrypts).
//!
//! The WS transport seam is replaced by a direct router dispatch (the same seam
//! `conformance_caldav_client.rs` uses): encode the request, invoke the
//! registered handler with a fixed connection actor, decode the reply. The
//! connection actor is a plain actor (no bridge enrollment, not admin) →
//! `CallerClass::User`, which the CardDAV data-plane arms admit (caller-scoped
//! to its own `actor_id`).
//!
//! Tier: tier_3 (real `fauna-nest` `AppState` + real `CacheDb` — no mocks; the
//! only stand-in is the in-process dispatch for the WebSocket).

mod common;

use std::sync::Arc;

use common::sealed;

use bytes::Bytes;
use serde::Serialize;
use serde::de::DeserializeOwned;

use fauna_nest::{
    bridge_carddav_handlers::register_bridge_carddav_handlers, db::CacheDb, routes::AppState,
    rpc_router::RpcRouter,
};
use fauna_protocol::bridge_routing::{
    CardEntry, DeleteAddressbookReply, DeleteAddressbookRequest, DeleteCardReply,
    DeleteCardRequest, ListAddressbooksReply, ListAddressbooksRequest, ProvisionAddressbookReply,
    ProvisionAddressbookRequest, PutCardCiphertextReply, PutCardCiphertextRequest, QueryCardsReply,
    QueryCardsRequest, SyncAddressbookSinceReply, SyncAddressbookSinceRequest,
};
use fauna_protocol::{decode_strict, encode_canonical};

/// A regular user actor — not a bridge service user, not admin — so it resolves
/// to `CallerClass::User`.
const USER_ACTOR: [u8; 32] = [20u8; 32];
const OTHER_ACTOR: [u8; 32] = [21u8; 32];
/// A stable client-assigned address-book id.
const ADDRESSBOOK_ID: [u8; 32] = [1u8; 32];

/// Build a nest + a router with only the CardDAV handlers registered.
fn nest() -> (Arc<AppState>, RpcRouter) {
    let db = Arc::new(CacheDb::open_in_memory().unwrap());
    let state = Arc::new(AppState::for_test(db));
    let mut b = RpcRouter::builder();
    register_bridge_carddav_handlers(&mut b);
    (state, b.build())
}

/// Encode → dispatch the registered handler keyed on `kind` with `actor` as the
/// connection actor → decode the reply. The exact wire contract the live WS path
/// drives, minus the socket.
async fn call<Req, Reply>(
    router: &RpcRouter,
    state: &Arc<AppState>,
    actor: [u8; 32],
    kind: &'static str,
    req: Req,
) -> Reply
where
    Req: Serialize,
    Reply: DeserializeOwned,
{
    common::seed_dispatch_actor(&state.db, &actor).await;
    let bytes = Bytes::from(encode_canonical(&req).expect("encode request").to_vec());
    let meta = router.kind_meta(kind).expect("kind registered");
    let reply = (meta.handler)(state.clone(), actor, bytes)
        .await
        .unwrap_or_else(|e| panic!("handler {kind} failed: {e:?}"));
    decode_strict(&reply).expect("decode reply")
}

/// Provision `ADDRESSBOOK_ID` for `USER_ACTOR` with sealed (opaque) metadata.
async fn provision(router: &RpcRouter, state: &Arc<AppState>) -> ProvisionAddressbookReply {
    call(
        router,
        state,
        USER_ACTOR,
        "fauna.bridges.provision_addressbook",
        ProvisionAddressbookRequest {
            actor_id: USER_ACTOR.to_vec(),
            addressbook_id: ADDRESSBOOK_ID.to_vec(),
            encrypted_metadata: b"sealed-addressbook-metadata".to_vec(),
            update_metadata: false,
        },
    )
    .await
}

/// PUT a card by `uid_hash` with an opaque sealed body. `body` MUST already be a
/// genuine seal (S6.12) — pass `&sealed(b"...")`; the index hint is sealed here.
async fn put_card(
    router: &RpcRouter,
    state: &Arc<AppState>,
    uid_hash: [u8; 32],
    body: &[u8],
    if_match: Option<String>,
) -> PutCardCiphertextReply {
    call(
        router,
        state,
        USER_ACTOR,
        "fauna.bridges.put_card_ciphertext",
        PutCardCiphertextRequest {
            actor_id: USER_ACTOR.to_vec(),
            addressbook_id: ADDRESSBOOK_ID.to_vec(),
            uid_hash: uid_hash.to_vec(),
            encrypted_body: body.to_vec(),
            encrypted_index_hint: sealed(b"sealed-index-hint"),
            timestamp: 1_700_000_000,
            ciphertext_size: body.len() as u32,
            if_match,
            encrypted_fauna_ext: None,
        },
    )
    .await
}

async fn query_all(router: &RpcRouter, state: &Arc<AppState>) -> QueryCardsReply {
    call(
        router,
        state,
        USER_ACTOR,
        "fauna.bridges.query_cards",
        QueryCardsRequest {
            actor_id: USER_ACTOR.to_vec(),
            addressbook_id: ADDRESSBOOK_ID.to_vec(),
            since_modseq: None,
            after_card_id: None,
            limit: 0,
        },
    )
    .await
}

/// Full write → read round-trip: provision → PUT a sealed vCard → list shows one
/// address book → query returns the *exact* opaque body verbatim (nest never
/// decrypts).
#[tokio::test]
async fn provision_put_query_roundtrip() {
    let (state, router) = nest();

    assert_eq!(
        provision(&router, &state).await,
        ProvisionAddressbookReply::Created
    );

    let body = sealed(b"BEGIN:VCARD\r\nVERSION:4.0\r\nFN:Grandma\r\nEND:VCARD\r\n(sealed)");
    let uid = [9u8; 32];
    let put = put_card(&router, &state, uid, &body, None).await;
    let (card_id, etag) = match put {
        PutCardCiphertextReply::Created { card_id, etag, .. } => (card_id, etag),
        other => panic!("expected Created, got {other:?}"),
    };
    assert_eq!(card_id.len(), 32, "card_id is a 32-byte blob");

    // list_addressbooks shows the one provisioned book with a card_count of 1.
    let list: ListAddressbooksReply = call(
        &router,
        &state,
        USER_ACTOR,
        "fauna.bridges.list_addressbooks",
        ListAddressbooksRequest {
            actor_id: USER_ACTOR.to_vec(),
        },
    )
    .await;
    assert_eq!(list.addressbooks.len(), 1);
    assert_eq!(list.addressbooks[0].addressbook_id, ADDRESSBOOK_ID.to_vec());
    assert_eq!(list.addressbooks[0].card_count, 1);

    // query_cards returns the exact opaque body + matching etag (nest is
    // ciphertext-only — it stored and returned the bytes verbatim).
    match query_all(&router, &state).await {
        QueryCardsReply::Ok { cards, .. } => {
            assert_eq!(cards.len(), 1);
            let c: &CardEntry = &cards[0];
            assert_eq!(c.encrypted_body, body.to_vec());
            assert_eq!(c.uid_hash, uid.to_vec());
            assert_eq!(c.card_id, card_id);
            assert_eq!(c.etag, etag);
        }
        other => panic!("expected Ok, got {other:?}"),
    }
}

/// A second PUT on the same `uid_hash` replaces the card (Updated, modseq bumps),
/// and the query reflects the new body — one logical card, not two.
#[tokio::test]
async fn re_put_same_uid_updates_in_place() {
    let (state, router) = nest();
    provision(&router, &state).await;
    let uid = [9u8; 32];

    // Two DIFFERENT sealed bodies for the same uid → Updated-with-bump; the
    // second body wins, so bind it to assert byte-identity of the served card.
    let first = put_card(&router, &state, uid, &sealed(b"sealed-v1"), None).await;
    let modseq_1 = match first {
        PutCardCiphertextReply::Created { modseq, .. } => modseq,
        other => panic!("expected Created, got {other:?}"),
    };

    let v2 = sealed(b"sealed-v2-longer");
    let second = put_card(&router, &state, uid, &v2, None).await;
    match second {
        PutCardCiphertextReply::Updated { modseq, .. } => {
            assert!(modseq > modseq_1, "modseq must bump on update");
        }
        other => panic!("expected Updated, got {other:?}"),
    }

    match query_all(&router, &state).await {
        QueryCardsReply::Ok { cards, .. } => {
            assert_eq!(cards.len(), 1, "one uid_hash ⇒ one logical card");
            assert_eq!(cards[0].encrypted_body, v2);
        }
        other => panic!("expected Ok, got {other:?}"),
    }
}

/// RFC 6578 sync-collection: a full sync from `"0"` returns the card; after a
/// delete, an incremental sync from the prior token returns the tombstone.
#[tokio::test]
async fn sync_collection_reports_changes_then_tombstone() {
    let (state, router) = nest();
    provision(&router, &state).await;
    let uid = [9u8; 32];
    put_card(&router, &state, uid, &sealed(b"sealed-card"), None).await;

    // Full sync (token "0") returns the changed card, no tombstones.
    let full: SyncAddressbookSinceReply = call(
        &router,
        &state,
        USER_ACTOR,
        "fauna.bridges.sync_addressbook_since",
        SyncAddressbookSinceRequest {
            actor_id: USER_ACTOR.to_vec(),
            addressbook_id: ADDRESSBOOK_ID.to_vec(),
            sync_token: "0".to_string(),
            limit: 0,
            mua_id: None,
        },
    )
    .await;
    let token = match full {
        SyncAddressbookSinceReply::Ok {
            changed,
            expunged,
            new_sync_token,
            ..
        } => {
            assert_eq!(changed.len(), 1);
            assert!(expunged.is_empty());
            new_sync_token
        }
        other => panic!("expected Ok, got {other:?}"),
    };

    // Delete the card, then sync from the prior token → the deletion surfaces as
    // a tombstone (RFC 6578 VANISHED), and no live change.
    let del: DeleteCardReply = call(
        &router,
        &state,
        USER_ACTOR,
        "fauna.bridges.delete_card",
        DeleteCardRequest {
            actor_id: USER_ACTOR.to_vec(),
            addressbook_id: ADDRESSBOOK_ID.to_vec(),
            uid_hash: uid.to_vec(),
            if_match: None,
        },
    )
    .await;
    assert!(
        matches!(del, DeleteCardReply::Deleted { .. }),
        "got {del:?}"
    );

    let delta: SyncAddressbookSinceReply = call(
        &router,
        &state,
        USER_ACTOR,
        "fauna.bridges.sync_addressbook_since",
        SyncAddressbookSinceRequest {
            actor_id: USER_ACTOR.to_vec(),
            addressbook_id: ADDRESSBOOK_ID.to_vec(),
            sync_token: token,
            limit: 0,
            mua_id: None,
        },
    )
    .await;
    match delta {
        SyncAddressbookSinceReply::Ok {
            changed, expunged, ..
        } => {
            assert!(changed.is_empty(), "no live changes after delete");
            assert_eq!(expunged.len(), 1, "the delete surfaces as one tombstone");
            assert_eq!(expunged[0].uid_hash, uid.to_vec());
        }
        other => panic!("expected Ok, got {other:?}"),
    }
}

/// A conditional delete with a stale `if_match` fails `PreconditionFailed`; the
/// card survives. With the correct etag it deletes.
#[tokio::test]
async fn conditional_delete_respects_if_match() {
    let (state, router) = nest();
    provision(&router, &state).await;
    let uid = [9u8; 32];
    let etag = match put_card(&router, &state, uid, &sealed(b"sealed-card"), None).await {
        PutCardCiphertextReply::Created { etag, .. } => etag,
        other => panic!("expected Created, got {other:?}"),
    };

    // Stale etag → PreconditionFailed, card survives.
    let stale: DeleteCardReply = call(
        &router,
        &state,
        USER_ACTOR,
        "fauna.bridges.delete_card",
        DeleteCardRequest {
            actor_id: USER_ACTOR.to_vec(),
            addressbook_id: ADDRESSBOOK_ID.to_vec(),
            uid_hash: uid.to_vec(),
            if_match: Some("\"deadbeefdeadbeef\"".to_string()),
        },
    )
    .await;
    assert!(
        matches!(stale, DeleteCardReply::PreconditionFailed { .. }),
        "got {stale:?}"
    );
    match query_all(&router, &state).await {
        QueryCardsReply::Ok { cards, .. } => {
            assert_eq!(cards.len(), 1, "card survived stale delete")
        }
        other => panic!("expected Ok, got {other:?}"),
    }

    // Correct etag → Deleted.
    let ok: DeleteCardReply = call(
        &router,
        &state,
        USER_ACTOR,
        "fauna.bridges.delete_card",
        DeleteCardRequest {
            actor_id: USER_ACTOR.to_vec(),
            addressbook_id: ADDRESSBOOK_ID.to_vec(),
            uid_hash: uid.to_vec(),
            if_match: Some(etag),
        },
    )
    .await;
    assert!(matches!(ok, DeleteCardReply::Deleted { .. }), "got {ok:?}");
}

/// Querying an address book that was never provisioned returns
/// `AddressbookNotFound` — and a different actor's PROPFIND of *its own* (empty)
/// namespace sees no cards (cross-actor isolation: nest scopes on `actor_id`).
#[tokio::test]
async fn missing_addressbook_and_actor_isolation() {
    let (state, router) = nest();

    // Never provisioned → AddressbookNotFound.
    match query_all(&router, &state).await {
        QueryCardsReply::AddressbookNotFound => {}
        other => panic!("expected AddressbookNotFound, got {other:?}"),
    }

    // USER_ACTOR provisions + writes a card.
    provision(&router, &state).await;
    put_card(&router, &state, [9u8; 32], &sealed(b"sealed-secret"), None).await;

    // OTHER_ACTOR lists its OWN namespace: it never provisioned anything, so it
    // sees zero address books (it cannot see USER_ACTOR's).
    let other_list: ListAddressbooksReply = call(
        &router,
        &state,
        OTHER_ACTOR,
        "fauna.bridges.list_addressbooks",
        ListAddressbooksRequest {
            actor_id: OTHER_ACTOR.to_vec(),
        },
    )
    .await;
    assert!(
        other_list.addressbooks.is_empty(),
        "a different actor sees none of USER_ACTOR's address books"
    );
}

/// A CardDAV collection DELETE (`delete_addressbook`) cascade-deletes the whole
/// book + all its cards through the real router→handler→store path: the reply
/// carries the cascade count, the book vanishes from `list_addressbooks`, a query
/// returns `AddressbookNotFound`, and a second delete is idempotent `NotFound`.
#[tokio::test]
async fn delete_addressbook_cascades_and_is_idempotent() {
    let (state, router) = nest();
    provision(&router, &state).await;
    put_card(&router, &state, [9u8; 32], &sealed(b"sealed-card-a"), None).await;
    put_card(&router, &state, [10u8; 32], &sealed(b"sealed-card-b"), None).await;

    let del: DeleteAddressbookReply = call(
        &router,
        &state,
        USER_ACTOR,
        "fauna.bridges.delete_addressbook",
        DeleteAddressbookRequest {
            actor_id: USER_ACTOR.to_vec(),
            addressbook_id: ADDRESSBOOK_ID.to_vec(),
        },
    )
    .await;
    assert_eq!(
        del,
        DeleteAddressbookReply::Deleted { cards_deleted: 2 },
        "whole book + both cards cascade-deleted"
    );

    // The book is gone from the actor's list.
    let list: ListAddressbooksReply = call(
        &router,
        &state,
        USER_ACTOR,
        "fauna.bridges.list_addressbooks",
        ListAddressbooksRequest {
            actor_id: USER_ACTOR.to_vec(),
        },
    )
    .await;
    assert!(list.addressbooks.is_empty(), "book removed from list");

    // A query against the removed collection reports it absent (the WebDAV 404
    // signal — no cards left behind).
    match query_all(&router, &state).await {
        QueryCardsReply::AddressbookNotFound => {}
        other => panic!("expected AddressbookNotFound after book delete, got {other:?}"),
    }

    // Re-delete is idempotent.
    let again: DeleteAddressbookReply = call(
        &router,
        &state,
        USER_ACTOR,
        "fauna.bridges.delete_addressbook",
        DeleteAddressbookRequest {
            actor_id: USER_ACTOR.to_vec(),
            addressbook_id: ADDRESSBOOK_ID.to_vec(),
        },
    )
    .await;
    assert_eq!(again, DeleteAddressbookReply::NotFound);
}
