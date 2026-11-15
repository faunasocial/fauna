//! The Nostr advertisement of a sold post — `fauna.nostr.advertise.{set,clear}`
//! and its lifecycle (`docs/goal/behavior/monetization.md` § The Nostr
//! advertisement of a sold post).
//!
//! Driven through the **real handlers** on a real `AppState` + `CacheDb`: the
//! act's typed refusals (§ *The act*), the publication it produces (a kind-1
//! note carrying the tier's asking price, mapped `outbound`, served by the
//! nest's relay and handed to the sync worker for the account's publish relays
//! — § *Reach*), and the lifecycle bullet's cases: a price edit retracts and
//! re-publishes with the new tag, a cleared price withdraws and clears the
//! flag, `clear` withdraws and drops the map row, `expose_content` off spares
//! it, and the `r` tag follows web publication and the site origin
//! (§ *Lifecycle* (1), (2), (3), (5), (6)).
//!
//! The gate surface (`zaps.advertise` on `set`, `clear` ungated) is pinned in
//! `conformance_feature_gate_surfaces.rs`, beside every other surface.
//!
//! Tier: tier_3 (real `fauna-nest` `AppState` + real `CacheDb` — no mocks).

#![cfg(all(feature = "nostr", feature = "zaps"))]

mod common;

use std::sync::Arc;

use bytes::Bytes;

use fauna_bridge_nostr::types::Event;
use fauna_nest::nostr::{db, store};
use fauna_nest::routes::AppState;
use fauna_nest::rpc_router::RpcRouter;
use fauna_nest::state::NostrState;
use fauna_protocol::nostr::{
    AdvertiseClearReply, AdvertiseClearRequest, AdvertiseSetReply, AdvertiseSetRequest,
};
use fauna_protocol::subscriptions::{
    TierAskingPrice, TierClearFieldRequest, TierClearableField, TierUpdateRequest,
};
use fauna_protocol::{RpcError, decode_strict as decode, encode_canonical};

use common::zap::link_payee_with_nsec;

// ── harness ──────────────────────────────────────────────────────

const RELAYS: &str = r#"["wss://relay.example"]"#;

struct Harness {
    router: RpcRouter,
    state: Arc<AppState>,
    /// The sync worker's outbound queue — what the act pushes to the publish
    /// relays.
    outbound: tokio::sync::mpsc::Receiver<fauna_nest::nostr::sync_worker::OutboundEvent>,
}

async fn harness() -> Harness {
    let db = Arc::new(fauna_nest::db::CacheDb::open_in_memory().unwrap());
    fauna_nest::nostr::init_db(&db).await.unwrap();
    let (tx, outbound) = tokio::sync::mpsc::channel(64);
    let state = Arc::new(AppState {
        nostr: NostrState {
            sync_tx: tx,
            ..NostrState::default()
        },
        ..AppState::for_test(db)
    });
    let mut b = RpcRouter::builder();
    fauna_nest::subscription_handlers::register_subscription_handlers(&mut b);
    fauna_nest::nostr::advertise::register_nostr_advertise_handlers(&mut b);
    fauna_nest::web_handlers::register_web_handlers(&mut b);
    Harness {
        router: b.build(),
        state,
        outbound,
    }
}

async fn call<T: serde::Serialize>(
    h: &Harness,
    kind: &str,
    actor: [u8; 32],
    req: T,
) -> Result<Bytes, RpcError> {
    let bytes = encode_canonical(&req).unwrap();
    common::call_raw(
        &h.router,
        h.state.clone(),
        kind,
        actor,
        Bytes::from(bytes.to_vec()),
    )
    .await
}

async fn set(h: &Harness, actor: [u8; 32], post: &str) -> Result<AdvertiseSetReply, RpcError> {
    call(
        h,
        "fauna.nostr.advertise.set",
        actor,
        AdvertiseSetRequest {
            post_id: post.into(),
            extra: Default::default(),
        },
    )
    .await
    .map(|b| decode(&b).unwrap())
}

async fn clear(h: &Harness, actor: [u8; 32], post: &str) -> Result<AdvertiseClearReply, RpcError> {
    call(
        h,
        "fauna.nostr.advertise.clear",
        actor,
        AdvertiseClearRequest {
            post_id: post.into(),
            extra: Default::default(),
        },
    )
    .await
    .map(|b| decode(&b).unwrap())
}

async fn user(h: &Harness, seed: u8) -> [u8; 32] {
    let actor = [seed; 32];
    h.state
        .db
        .create_user(&actor, "free", &format!("user{seed}"))
        .await
        .unwrap();
    actor
}

/// A post written through the production `CacheDb::put_post` path (which
/// projects `gated_tier` into `content_meta`), gated to `gate` when given.
async fn put_post(h: &Harness, author: [u8; 32], gate: Option<&str>) -> String {
    use fauna_core::data::{ContentHash, Post, PostBody, Timestamp};
    use fauna_core::identity::ActorId;
    use fauna_core::subscription::types::{GatedInfo, KeyAccess};
    static NONCE: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
    let n = NONCE.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    let post = Post {
        author: ActorId(author),
        created_at: Timestamp(1_700_000_000_000_000 + n),
        body: PostBody::Text {
            content: format!("public teaser {n}"),
            facets: vec![],
        },
        references: vec![],
        expires_at: None,
        gated: gate.map(|tier| GatedInfo {
            encrypted_ref: ContentHash::from_digest_raw([9u8; 32]),
            key_access: KeyAccess::Broadcast {
                key_blob_ref: ContentHash::from_digest_raw([8u8; 32]),
            },
            tier: tier.to_string(),
            tier_rank: 2,
            seal_id: ContentHash::from_digest_raw([7u8; 32]),
            attachment_refs: vec![],
        }),
        content_warning: None,
        origin: None,
    };
    let cid = fauna_core::encoding::compute_post_id(&post).unwrap();
    let digest: [u8; 32] = cid.as_bytes()[4..].try_into().unwrap();
    let bytes = fauna_core::encoding::canonical_encode(&post).unwrap();
    h.state.db.put_post(&digest, &bytes, None).await.unwrap();
    hex::encode(digest)
}

/// The degenerate single-post tier that sells `post`, optionally priced.
async fn sell(h: &Harness, author: [u8; 32], tier: &str, post: &str, msats: Option<u64>) {
    h.state
        .db
        .create_subscription_tier(
            &author,
            tier,
            2,
            None,
            None,
            None,
            false,
            Some(post),
            msats.map(TierAskingPrice::msats).as_ref(),
            false,
        )
        .await
        .unwrap();
}

/// A real sold, priced post of `author`'s.
async fn sold_post(h: &Harness, author: [u8; 32], tier: &str, msats: u64) -> String {
    let post = put_post(h, author, Some(tier)).await;
    sell(h, author, tier, &post, Some(msats)).await;
    post
}

async fn mapped_events(h: &Harness, post: &str) -> Vec<String> {
    let conn = h.state.db.conn().await;
    db::list_event_ids_by_fauna_id(&conn, post).unwrap()
}

async fn served(h: &Harness, id: &str) -> Option<Event> {
    let conn = h.state.db.conn().await;
    store::query_events(
        &conn,
        &[fauna_bridge_nostr::types::Filter {
            ids: Some(vec![id.to_string()]),
            ..Default::default()
        }],
        10,
    )
    .unwrap()
    .into_iter()
    .next()
}

fn price_tag(event: &Event) -> Option<(String, String)> {
    event
        .tags
        .iter()
        .find(|t| t.name() == Some("price"))
        .map(|t| (t.0[1].clone(), t.0[2].clone()))
}

async fn flag(h: &Harness, post: &str) -> Option<i64> {
    let mut id = [0u8; 32];
    hex::decode_to_slice(post, &mut id).unwrap();
    h.state
        .db
        .conn()
        .await
        .query_row(
            "SELECT nostr_advertised_at FROM content_meta WHERE content_id = ?1",
            [id.as_slice()],
            |r| r.get(0),
        )
        .unwrap()
}

/// The one advertisement event the act pushed, drained from the outbound queue
/// (anything else queued — a kind 5 — is returned too, in order).
fn drain(h: &mut Harness) -> Vec<fauna_nest::nostr::sync_worker::OutboundEvent> {
    let mut out = vec![];
    while let Ok(e) = h.outbound.try_recv() {
        out.push(e);
    }
    out
}

// ── the act ──────────────────────────────────────────────────────

/// The whole door: a sold, priced post advertised through `set` is served by
/// the nest's relay as a kind-1 note carrying `["price", value, unit]`, mapped
/// `outbound` (the row a zap receipt resolves through), and pushed to the
/// account's publish relays — with `expose_content` off, which the act neither
/// needs nor consults. A replay keeps the first act's time and publishes
/// nothing new.
#[tokio::test]
async fn set_publishes_the_priced_teaser_and_maps_it() {
    let mut h = harness().await;
    let author = user(&h, 0x11).await;
    let kp = link_payee_with_nsec(&h.state, author, Some(RELAYS)).await;
    let post = sold_post(&h, author, "unlock-a", 21_000).await;

    let reply = set(&h, author, &post)
        .await
        .expect("a sold, priced post advertises");
    assert!(reply.advertised_at > 0);
    assert_eq!(flag(&h, &post).await, Some(reply.advertised_at as i64));

    let ids = mapped_events(&h, &post).await;
    assert_eq!(ids.len(), 1, "one advertisement, mapped outbound");
    let event = served(&h, &ids[0])
        .await
        .expect("the nest's relay serves it");
    assert_eq!(event.kind, 1);
    assert_eq!(event.pubkey, kp.public_key_hex());
    assert_eq!(price_tag(&event), Some(("21000".into(), "msat".into())));

    let pushed = drain(&mut h);
    assert_eq!(pushed.len(), 1, "pushed to the publish relays");
    assert_eq!(pushed[0].event.id, event.id);
    assert_eq!(
        pushed[0].relay_urls,
        vec!["wss://relay.example".to_string()]
    );

    let again = set(&h, author, &post).await.unwrap();
    assert_eq!(
        again.advertised_at, reply.advertised_at,
        "the first act's time is kept"
    );
    assert_eq!(mapped_events(&h, &post).await, ids, "nothing re-signed");
    assert!(drain(&mut h).is_empty(), "nothing re-pushed");
}

/// Every refusal is typed and fail-closed, and none leaves a flag behind.
#[tokio::test]
async fn set_refuses_what_it_cannot_honestly_advertise() {
    let h = harness().await;
    let author = user(&h, 0x21).await;
    let stranger = user(&h, 0x22).await;
    link_payee_with_nsec(&h.state, author, Some(RELAYS)).await;
    link_payee_with_nsec(&h.state, stranger, Some(RELAYS)).await;

    // Another author's sold post reads as a missing one.
    let theirs = sold_post(&h, stranger, "their-unlock", 1_000).await;
    let e = set(&h, author, &theirs).await.unwrap_err();
    assert_eq!(e.code, "fauna.nostr.not_found");
    let e = set(&h, author, "not-hex").await.unwrap_err();
    assert_eq!(e.code, "fauna.nostr.not_found");

    // An ordinary public post: nothing sells it.
    let public = put_post(&h, author, None).await;
    let e = set(&h, author, &public).await.unwrap_err();
    assert_eq!(e.code, RpcError::CODE_NOSTR_NOT_SOLD);

    // A post gated to a tier that does not designate it is subscription
    // content, not a sale (both directions of the binding).
    let gated = put_post(&h, author, Some("supporters")).await;
    sell(&h, author, "supporters", &public, Some(1_000)).await;
    let e = set(&h, author, &gated).await.unwrap_err();
    assert_eq!(e.code, RpcError::CODE_NOSTR_NOT_SOLD);

    // Sold, but at no machine price: a priceless advertisement could only
    // ever yield tips.
    let priceless = put_post(&h, author, Some("unpriced")).await;
    sell(&h, author, "unpriced", &priceless, None).await;
    let e = set(&h, author, &priceless).await.unwrap_err();
    assert_eq!(e.code, RpcError::CODE_NOSTR_NO_ASKING_PRICE);

    for post in [&theirs, &public, &gated, &priceless] {
        assert_eq!(flag(&h, post).await, None, "no refusal leaves a flag");
    }
}

/// The bridging gate's own refusals: no linked key, or a key the nest cannot
/// sign with.
#[tokio::test]
async fn set_refuses_an_account_with_no_deposited_key() {
    let h = harness().await;
    let unlinked = user(&h, 0x31).await;
    let post = sold_post(&h, unlinked, "u1", 1_000).await;
    let e = set(&h, unlinked, &post).await.unwrap_err();
    assert_eq!(e.code, RpcError::CODE_NOSTR_NOT_LINKED);

    let remote = user(&h, 0x32).await;
    common::zap::link_payee(&h.state, remote).await; // no encrypted key
    let post = sold_post(&h, remote, "u2", 1_000).await;
    let e = set(&h, remote, &post).await.unwrap_err();
    assert_eq!(e.code, RpcError::CODE_NOSTR_NO_CUSTODIAL_KEY);
    assert_eq!(flag(&h, &post).await, None);
}

// ── the lifecycle ────────────────────────────────────────────────

/// (1) A price edit retracts the published note (a kind 5 the nest's relay
/// applies and the publish relays receive) and the post is re-signed with the
/// new tag, still advertised. An edit that leaves the price alone retracts
/// nothing.
#[tokio::test]
async fn a_price_edit_retracts_and_republishes_with_the_new_price() {
    let mut h = harness().await;
    let author = user(&h, 0x41).await;
    link_payee_with_nsec(&h.state, author, Some(RELAYS)).await;
    let post = sold_post(&h, author, "unlock-p", 21_000).await;
    set(&h, author, &post).await.unwrap();
    let old = mapped_events(&h, &post).await;
    drain(&mut h);

    // Not a price edit: nothing moves.
    call(
        &h,
        "fauna.subscriptions.tiers.update",
        author,
        TierUpdateRequest {
            name: "unlock-p".into(),
            description: Some("now with a description".into()),
            ..Default::default()
        },
    )
    .await
    .unwrap();
    assert_eq!(mapped_events(&h, &post).await, old);
    assert!(drain(&mut h).is_empty());

    call(
        &h,
        "fauna.subscriptions.tiers.update",
        author,
        TierUpdateRequest {
            name: "unlock-p".into(),
            asking_price: Some(TierAskingPrice::msats(30_000)),
            ..Default::default()
        },
    )
    .await
    .unwrap();

    assert!(
        served(&h, &old[0]).await.is_none(),
        "the old offer is withdrawn"
    );
    let new = mapped_events(&h, &post).await;
    assert_eq!(new.len(), 1);
    assert_ne!(new, old);
    let event = served(&h, &new[0]).await.expect("re-published");
    assert_eq!(price_tag(&event), Some(("30000".into(), "msat".into())));
    assert!(flag(&h, &post).await.is_some(), "still advertised");

    let pushed = drain(&mut h);
    let kinds: Vec<u64> = pushed.iter().map(|o| o.event.kind).collect();
    assert_eq!(kinds, vec![5, 1], "the retraction, then the new offer");
    assert!(
        pushed[0]
            .event
            .tags
            .iter()
            .any(|t| t.value() == Some(old[0].as_str()))
    );
}

/// (2) Clearing the asking price withdraws the advertisement and clears the
/// flag — an advertisement without a price cannot stand, and nothing
/// re-publishes it.
#[tokio::test]
async fn clearing_the_price_withdraws_and_clears_the_flag() {
    let h = harness().await;
    let author = user(&h, 0x51).await;
    link_payee_with_nsec(&h.state, author, Some(RELAYS)).await;
    let post = sold_post(&h, author, "unlock-c", 21_000).await;
    set(&h, author, &post).await.unwrap();
    let old = mapped_events(&h, &post).await;

    call(
        &h,
        "fauna.subscriptions.tiers.clear_field",
        author,
        TierClearFieldRequest {
            name: "unlock-c".into(),
            field: TierClearableField::AskingPrice,
            extra: Default::default(),
        },
    )
    .await
    .unwrap();

    assert_eq!(flag(&h, &post).await, None);
    assert!(served(&h, &old[0]).await.is_none());
    assert!(mapped_events(&h, &post).await.is_empty());
    fauna_nest::nostr::advertise::kick_account(&h.state, &hex::encode(author)).await;
    assert!(
        mapped_events(&h, &post).await.is_empty(),
        "a cleared flag is never re-materialized"
    );
}

/// (3) `clear` withdraws the note and drops its map row, so a later zap on a
/// cached copy names no subject (a tip); it is idempotent, and another
/// author's post reads as missing.
#[tokio::test]
async fn clear_withdraws_and_drops_the_map_row() {
    let mut h = harness().await;
    let author = user(&h, 0x61).await;
    let stranger = user(&h, 0x62).await;
    link_payee_with_nsec(&h.state, author, Some(RELAYS)).await;
    let post = sold_post(&h, author, "unlock-x", 21_000).await;
    set(&h, author, &post).await.unwrap();
    let old = mapped_events(&h, &post).await;
    drain(&mut h);

    assert_eq!(
        clear(&h, stranger, &post).await.unwrap_err().code,
        "fauna.nostr.not_found"
    );
    assert!(clear(&h, author, &post).await.unwrap().cleared);
    assert_eq!(flag(&h, &post).await, None);
    assert!(served(&h, &old[0]).await.is_none());
    assert!(mapped_events(&h, &post).await.is_empty());
    let pushed = drain(&mut h);
    assert_eq!(pushed.len(), 1);
    assert_eq!(
        pushed[0].event.kind, 5,
        "the publish relays are asked to delete"
    );

    assert!(
        !clear(&h, author, &post).await.unwrap().cleared,
        "idempotent"
    );
}

/// (5) `expose_content` off — the account toggle's withdrawal — spares the
/// advertisement and its map row; the public class goes.
#[tokio::test]
async fn expose_off_spares_the_advertisement() {
    let h = harness().await;
    let author = user(&h, 0x71).await;
    let kp = link_payee_with_nsec(&h.state, author, Some(RELAYS)).await;
    let post = sold_post(&h, author, "unlock-e", 21_000).await;
    set(&h, author, &post).await.unwrap();
    let ad = mapped_events(&h, &post).await;

    let conn = h.state.db.conn().await;
    store::delete_derived_for_pubkey(&conn, &kp.public_key_hex()).unwrap();
    drop(conn);
    assert_eq!(mapped_events(&h, &post).await, ad);
    assert!(served(&h, &ad[0]).await.is_some());
}

fn r_tag(event: &Event) -> Option<String> {
    event
        .tags
        .iter()
        .find(|t| t.name() == Some("r"))
        .and_then(|t| t.value())
        .map(str::to_string)
}

/// The one advertisement the post has on the nest's relay right now.
async fn current_ad(h: &Harness, post: &str) -> Event {
    let ids = mapped_events(h, post).await;
    assert_eq!(ids.len(), 1, "one live advertisement");
    served(h, &ids[0]).await.expect("served")
}

async fn web_publish(h: &Harness, author: [u8; 32], post: &str, slug: Option<&str>) {
    call(
        h,
        "fauna.web.publish.set",
        author,
        fauna_protocol::web::WebPublishSetRequest {
            post_id: fauna_protocol::ByteBuf::from(hex::decode(post).unwrap()),
            slug: slug.map(str::to_string),
            extra: Default::default(),
        },
    )
    .await
    .unwrap();
}

/// (6) The `r` tag follows the post's public URL (§ *The event*, § *Lifecycle*
/// (6)): publishing the advertised post to the web retracts the note and
/// re-signs it carrying the page URL the apps' *Copy web link* copies; a
/// change of the author's site origin no write path announces — here a custom
/// domain's cert going active — is caught by the reconcile the sync worker runs
/// on its tick; unpublishing retracts again and the new note carries no `r`.
#[tokio::test]
async fn the_r_tag_follows_web_publication_and_the_site_origin() {
    let mut h = harness().await;
    let author = user(&h, 0x81).await;
    h.state.db.set_handle(&author, "alice").await.unwrap();
    h.state.db.set_subdomain_enabled(&author).await.unwrap();
    h.state
        .identity_domain
        .store(Some(Arc::new("example.org".to_string())));
    link_payee_with_nsec(&h.state, author, Some(RELAYS)).await;
    let post = sold_post(&h, author, "unlock-w", 21_000).await;
    set(&h, author, &post).await.unwrap();
    let first = current_ad(&h, &post).await;
    assert_eq!(r_tag(&first), None, "not web-published: no r tag");
    drain(&mut h);

    web_publish(&h, author, &post, Some("the-teaser")).await;
    assert!(
        served(&h, &first.id).await.is_none(),
        "the URL-less note is withdrawn"
    );
    let published = current_ad(&h, &post).await;
    assert_eq!(
        r_tag(&published).as_deref(),
        Some("https://alice.example.org/post/the-teaser.html")
    );
    assert_eq!(price_tag(&published), Some(("21000".into(), "msat".into())));
    let kinds: Vec<u64> = drain(&mut h).iter().map(|o| o.event.kind).collect();
    assert_eq!(
        kinds,
        vec![5, 1],
        "the retraction, then the note with its URL"
    );

    // Steady state: the reconcile finds nothing to move.
    assert_eq!(
        fauna_nest::nostr::advertise::reconcile_advertised_urls(&h.state, None)
            .await
            .unwrap(),
        0
    );

    // A custom domain's cert goes active — the cert lifecycle's own write.
    h.state
        .db
        .insert_web_domain(&author, "blog.example.com", "tok")
        .await
        .unwrap();
    h.state
        .db
        .update_web_domain_status("blog.example.com", "active")
        .await
        .unwrap();
    assert_eq!(
        fauna_nest::nostr::advertise::reconcile_advertised_urls(&h.state, None)
            .await
            .unwrap(),
        1
    );
    let moved = current_ad(&h, &post).await;
    assert_eq!(
        r_tag(&moved).as_deref(),
        Some("https://blog.example.com/post/the-teaser.html"),
        "an active custom domain wins, as on Copy web link"
    );
    drain(&mut h);

    call(
        &h,
        "fauna.web.publish.unset",
        author,
        fauna_protocol::web::WebPublishUnsetRequest {
            post_id: fauna_protocol::ByteBuf::from(hex::decode(&post).unwrap()),
            extra: Default::default(),
        },
    )
    .await
    .unwrap();
    assert!(served(&h, &moved.id).await.is_none());
    let unpublished = current_ad(&h, &post).await;
    assert_eq!(r_tag(&unpublished), None, "unpublished: the r tag is gone");
    assert!(flag(&h, &post).await.is_some(), "still advertised");
}

/// The feed read carries the flag (`FeedPostRow::nostr_advertised_at` → the
/// wire `FeedPostItem.nostr_advertised_at`), so an app picks the arm of the
/// own-post verb pair after a reload: `None` before the act, the act's time
/// after it, `None` again once withdrawn (`monetization.md` § The Nostr
/// advertisement of a sold post → *What the author sees and agrees to*).
#[tokio::test]
async fn the_feed_read_carries_the_advertised_flag() {
    let h = harness().await;
    let author = user(&h, 0x71).await;
    link_payee_with_nsec(&h.state, author, Some(RELAYS)).await;
    let post = sold_post(&h, author, "unlock-feed", 21_000).await;

    let read = |h: &Harness| {
        let db = h.state.db.clone();
        async move {
            db.query_feed_for_authors(
                &[],
                fauna_core::scoring::FilterCombination::All,
                &[],
                &[author.to_vec()],
                None,
                100,
                fauna_nest::db::feeds::FeedAudience::OnBox,
                fauna_nest::db::feeds::FeedLocality::NestWide,
            )
            .await
            .unwrap()
        }
    };
    let row_flag = |rows: &[fauna_nest::db::FeedPostRow]| {
        rows.iter()
            .find(|r| hex::encode(&r.post_id) == post)
            .expect("the author's sold post is in their own feed read")
            .nostr_advertised_at
    };

    assert_eq!(row_flag(&read(&h).await), None, "not advertised yet");
    let at = set(&h, author, &post).await.unwrap().advertised_at;
    assert_eq!(row_flag(&read(&h).await), Some(at));
    clear(&h, author, &post).await.unwrap();
    assert_eq!(row_flag(&read(&h).await), None, "withdrawn");
}
