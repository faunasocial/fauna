//! The federated follow's delivery — `fauna.federation.follow.deliver`, the
//! delete leg and the purge (`docs/goal/behavior/federated-follow.md`
//! § Decision 1, § Decision 3 and § Decision 5's delete paragraph).
//!
//! **On the author's nest** every post a local author stores is queued once
//! per distinct home nest of the author's remote subscribers, as the stored
//! `EmbedAsBytes` body verbatim, and the outbox worker sends it in
//! `post.forward`'s request shape; an approval backfills the author's newest
//! posts to a home that held no subscriber yet, and a deletion is queued to
//! the same homes as `fauna.federation.post.delete`.
//!
//! **On the follower's nest** the delivery is admitted only on this nest's own
//! follow record — a local reader's `approved` follow of the post's author
//! homed at the delivering nest — and stored as a marked foreign copy:
//! `follow_delivered = 1`, listed only to the readers who follow the author
//! (`db::feeds::FeedLocality`), never bridged, never re-forwarded, never in the
//! Search corpus. The home's tombstone removes it; so does the purge, once no
//! local reader follows the author.
//!
//! The kind is registered beside the subscribe twin's in
//! [`crate::follow_federation::register_follow_federation_handlers`].

use std::sync::Arc;
use std::time::Duration;

use crate::federation_handlers::{
    FedPostForwardReply, FedPostForwardRequest, ForwardedPost, decode_forwarded_post,
};
use crate::federation_router::RpcHandler;
use crate::routes::AppState;
use crate::rpc_errors::{encode_reply, internal};

/// `fauna.federation.follow.deliver` — an author's home delivers one stored
/// post envelope to a follower's home nest.
pub const DELIVER_KIND: &str = "fauna.federation.follow.deliver";

/// `outbox.entry_type` of a queued [`DELIVER_KIND`]; the payload is the stored
/// `EmbedAsBytes` body and `peer_nest_id` the follower's home.
pub const ENTRY_TYPE_FOLLOW_DELIVER: &str = "follow_deliver";

/// `outbox.entry_type` of a queued deletion to a follower's home; the payload
/// is the author-signed tombstone wire, sent as `fauna.federation.post.delete`.
pub const ENTRY_TYPE_FOLLOW_DELETE: &str = "follow_delete";

/// At most this many pending delivery entries per `(author, follower home)`,
/// the oldest dropped first (§ Decision 1). A follower whose nest was away past
/// it re-follows to backfill.
pub const PENDING_PER_PEER_CAP: i64 = 1_000;

/// The approval backfill: the author's newest this-many servable posts.
pub const BACKFILL_POSTS: i64 = 50;

/// How often the purge looks for delivered envelopes nobody follows any more.
const PURGE_INTERVAL: Duration = Duration::from_secs(3_600);

/// Envelopes the purge removes per pass.
const PURGE_BATCH: i64 = 500;

// ── Author's nest: the producers ───────────────────────────────────────

/// Queue `body` — a post `author` just stored — to every home nest of the
/// author's remote subscribers. Best-effort like every post-commit enqueue:
/// the post has landed, and losing one delivery never fails the create.
pub(crate) async fn enqueue_follow_deliveries(state: &AppState, author: &[u8; 32], body: &[u8]) {
    enqueue_to_homes(state, author, body, ENTRY_TYPE_FOLLOW_DELIVER).await;
}

/// Queue a deletion's signed tombstone to every home nest of the author's
/// remote subscribers (§ Decision 5).
pub(crate) async fn enqueue_follow_deletes(
    state: &AppState,
    author: &[u8; 32],
    signed_tombstone: &[u8],
) {
    enqueue_to_homes(state, author, signed_tombstone, ENTRY_TYPE_FOLLOW_DELETE).await;
}

async fn enqueue_to_homes(state: &AppState, author: &[u8; 32], payload: &[u8], entry_type: &str) {
    let homes = match state.db.subscriber_homes(author).await {
        Ok(homes) => homes,
        Err(e) => {
            tracing::warn!("follow delivery: read the author's subscriber homes: {e:#}");
            return;
        }
    };
    for home in homes {
        if let Err(e) = state
            .db
            .outbox_enqueue_follow_delivery(
                author,
                payload,
                entry_type,
                &home,
                PENDING_PER_PEER_CAP,
            )
            .await
        {
            tracing::warn!("follow delivery: enqueue {entry_type}: {e:#}");
        }
    }
}

/// A remote subscriber homed at `home` was just approved for `tier`: unless
/// another of the author's subscribers is already homed there (its index holds
/// the posts already), queue the author's newest servable posts to it
/// (§ Decision 1's backfill). A tier designated `unlocks_post` adds the
/// designated post whatever its age, deduplicated with the window — the buyer
/// bought that post, and the newest-50 window will not hold an old one, nor
/// another subscriber's earlier backfill to that home. The same entry, the
/// same queue — never a pull.
pub(crate) async fn backfill_new_home(
    state: &AppState,
    author: &[u8; 32],
    home: &[u8; 32],
    tier: &str,
) {
    let mut ids = match state.db.subscribers_homed_at(author, home).await {
        Ok(1) => match state
            .db
            .author_backfill_post_ids(author, BACKFILL_POSTS)
            .await
        {
            Ok(ids) => ids,
            Err(e) => {
                tracing::warn!("follow backfill: list the author's posts: {e:#}");
                Vec::new()
            }
        },
        Ok(_) => Vec::new(),
        Err(e) => {
            tracing::warn!("follow backfill: count the home's subscribers: {e:#}");
            return;
        }
    };
    if let Some(post) = designated_post(state, author, tier).await
        && !ids.contains(&post)
    {
        ids.push(post);
    }
    for id in ids {
        let body =
            match crate::segments::post::read_body_by_post_id(&state.post_segments, &state.db, &id)
                .await
            {
                Ok(Some(body)) => body,
                Ok(None) => continue,
                Err(e) => {
                    tracing::warn!("follow backfill: read a post body: {e:#}");
                    continue;
                }
            };
        if let Err(e) = state
            .db
            .outbox_enqueue_follow_delivery(
                author,
                &body,
                ENTRY_TYPE_FOLLOW_DELIVER,
                home,
                PENDING_PER_PEER_CAP,
            )
            .await
        {
            tracing::warn!("follow backfill: enqueue: {e:#}");
        }
    }
}

/// The post a tier sells (`unlocks_post`), when it is the author's own and
/// servable to a non-author.
async fn designated_post(state: &AppState, author: &[u8; 32], tier: &str) -> Option<[u8; 32]> {
    let designation = match state.db.get_subscription_tier(author, tier).await {
        Ok(t) => t?.unlocks_post?,
        Err(e) => {
            tracing::warn!("follow backfill: read the tier's designation: {e:#}");
            return None;
        }
    };
    let post = fauna_core::hex32::decode(&designation).ok()?;
    match state.db.author_backfill_post_servable(author, &post).await {
        Ok(true) => Some(post),
        Ok(false) => None,
        Err(e) => {
            tracing::warn!("follow backfill: read the designated post: {e:#}");
            None
        }
    }
}

/// A remote subscriber homed at `home` left the roster: once none of the
/// author's subscribers is homed there, the author's pending deliveries to it
/// are dropped (§ Decision 1's discard). The `follow_state` push saying so
/// stays queued.
pub(crate) async fn discard_if_home_left(state: &AppState, author: &[u8; 32], home: &[u8; 32]) {
    match state.db.subscribers_homed_at(author, home).await {
        Ok(0) => {
            if let Err(e) = state
                .db
                .outbox_discard_follow_deliveries(author, home)
                .await
            {
                tracing::warn!("follow delivery: discard for a departed home: {e:#}");
            }
        }
        Ok(_) => {}
        Err(e) => tracing::warn!("follow delivery: count the home's subscribers: {e:#}"),
    }
}

// ── Follower's nest: the receiver ──────────────────────────────────────

/// `fauna.federation.follow.deliver`: the envelope verified by the one relayed
/// post decode the paired forward shares (author envelope + the future bound),
/// a replay of a post this nest saw deleted answered `Ok` with no store, then
/// the follow gate — a local reader's `approved` follow of the post's author
/// homed at the verified origin — then the marked store and, for a room post
/// whose room this nest homes, the room view.
pub(crate) fn deliver_handler() -> RpcHandler {
    Box::new(|state, origin_nest_id, payload| {
        Box::pin(async move {
            let req: FedPostForwardRequest =
                fauna_protocol::decode_strict(&payload).map_err(crate::rpc_errors::malformed)?;
            let ForwardedPost {
                post,
                stored_bytes,
                post_id,
            } = decode_forwarded_post(&req)?;
            if state
                .db
                .post_was_deleted(&post_id)
                .await
                .map_err(internal)?
            {
                return encode_reply(&FedPostForwardReply {});
            }
            let Some(follow_home_url) = state
                .db
                .has_approved_remote_follow_from(&post.author.0, &origin_nest_id)
                .await
                .map_err(internal)?
            else {
                return Err(crate::rpc_errors::forbidden_ns(
                    "federation",
                    "no reader here follows this author at this nest",
                ));
            };
            // The URL this nest stored when its reader followed — its own
            // record, never the delivering peer's word — bound to the verified
            // origin's proven address where it has one.
            let home_url = crate::federation_handlers::resolve_origin_home_url(
                &state,
                &origin_nest_id,
                Some(&follow_home_url),
            )
            .await?
            .unwrap_or(follow_home_url);
            let newly_stored = !state.db.post_exists(&post_id).await.map_err(internal)?;
            let stored = crate::segments::post::store_delivered_post(
                &state.post_segments,
                &state.db,
                &post_id,
                &stored_bytes,
                &home_url,
                &origin_nest_id,
            )
            .await
            .map_err(internal)?;
            // Custody is per home: a second home's delivery adds its custody
            // beside the first and is answered `Ok` like any other
            // (§ Decision 3, *What a reader-named home is trusted for*).
            // ...and a custody add re-attempts the view while the post has none:
            // an earlier delivering home that could not serve the body (it
            // holds the envelope alone) left the room without its view, and
            // this home may serve it (§ Decision 4, the room arm).
            let attempt = match stored {
                crate::db::posts::DeliveredStore::Stored => newly_stored,
                crate::db::posts::DeliveredStore::CustodyAdded => !state
                    .db
                    .room_post_view_exists(&post_id)
                    .await
                    .map_err(internal)?,
                _ => false,
            };
            if attempt {
                crate::room_post_view::index_room_post(
                    &state,
                    &post_id,
                    &stored_bytes,
                    Some(&origin_nest_id),
                )
                .await;
            }
            encode_reply(&FedPostForwardReply {})
        })
    })
}

/// `fauna.federation.post.delete`'s follow arm (§ Decision 5): for a stored
/// row a federated follow delivered, the tombstone is admitted from any home
/// holding custody of it (a `follow_delivery_homes` row) — `Some(true)`
/// admit, `Some(false)` refuse — and the row goes whole, its custody rows with
/// it. `None` for every other row, which keeps the paired forward's gate.
pub(crate) async fn delete_custody(
    state: &AppState,
    origin_nest_id: &[u8; 32],
    post_id: &[u8; 32],
) -> Result<Option<bool>, fauna_protocol::RpcError> {
    // By identity, never by address: custody is keyed on each delivering
    // home's verified channel id, so a nest that merely proves the same URL
    // holds none. The tombstone is the author's own signature, so custody
    // decides only the door (§ Decision 3, rule 3).
    state
        .db
        .delivered_custody_of(post_id, origin_nest_id)
        .await
        .map_err(internal)
}

// ── Follower's nest: the purge ─────────────────────────────────────────

/// § Decision 3's purge, two sweeps: first drop each home's custody of a
/// delivered row once no local reader follows its author at that home
/// (`CacheDb::drop_unfollowed_custody`, which re-points the row's origin
/// columns to its oldest remaining custody), then remove the delivered rows
/// no custody is left of: the projection, the room views derived from it, and
/// the segment record. No deletion witness is written — the author deleted
/// nothing, and a reader who follows again receives them again. Returns how
/// many rows were removed.
pub(crate) async fn purge_unfollowed(state: &AppState) -> usize {
    match state.db.drop_unfollowed_custody(PURGE_BATCH).await {
        Ok(0) => {}
        Ok(n) => tracing::info!("follow purge: dropped {n} unfollowed custodies"),
        Err(e) => tracing::warn!("follow purge: drop unfollowed custody: {e:#}"),
    }
    let ids = match state.db.follow_delivered_orphans(PURGE_BATCH).await {
        Ok(ids) => ids,
        Err(e) => {
            tracing::warn!("follow purge: list orphans: {e:#}");
            return 0;
        }
    };
    let mut removed = 0;
    for id in ids {
        let seg = crate::segments::post::lookup_scope_by_post_id(&state.db, &id).await;
        match state
            .db
            .delete_post_projection_with_witness(&id, None)
            .await
        {
            Ok(_) => removed += 1,
            Err(e) => {
                tracing::warn!("follow purge: remove projection: {e:#}");
                continue;
            }
        }
        if let Err(e) = state.db.purge_room_post_views_for_post(&id).await {
            tracing::warn!("follow purge: room post views: {e:#}");
        }
        if let Ok(Some((scope, seg_id))) = seg {
            let cid = fauna_cbor::Cid::from_digest_dag_cbor(id);
            if let Err(e) =
                crate::segments::post::tombstone_by_cid(&state.db, &scope, seg_id, &cid).await
            {
                tracing::warn!("follow purge: segment record: {e:#}");
            }
        }
    }
    removed
}

/// The purge's loop, beside the outbox worker.
pub async fn run_follow_purge(state: Arc<AppState>) {
    loop {
        let removed = purge_unfollowed(&state).await;
        if removed > 0 {
            tracing::info!("follow purge: removed {removed} delivered posts nobody follows");
        }
        tokio::time::sleep(PURGE_INTERVAL).await;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    use fauna_core::data::{Post, PostBody, Timestamp};
    use fauna_core::encoding::{EmbedAsBytes, canonical_encode, sign_envelope};
    use fauna_core::identity::ActorKeypair;

    use crate::db::posts::{DeliveredStanding, DeliveredStore};
    use crate::db::remote_follows::RemoteFollowState;
    use crate::test_support::fixture_state;

    const HOME: [u8; 32] = [0xB0; 32];
    const HOME_URL: &str = "https://home.example";

    fn signed(author: &ActorKeypair) -> (Vec<u8>, [u8; 32]) {
        let post = Post {
            author: author.actor_id(),
            created_at: Timestamp::now(),
            body: PostBody::Text {
                content: "delivered".into(),
                facets: vec![],
            },
            references: vec![],
            expires_at: None,
            gated: None,
            content_warning: None,
            origin: None,
        };
        let (bytes, env) = sign_envelope(author, &post).unwrap();
        let wire = canonical_encode(&EmbedAsBytes::from_signed(bytes, env)).unwrap();
        let id = *blake3::hash(&wire).as_bytes();
        (wire, id)
    }

    async fn delivered(state: &AppState, author: &ActorKeypair) -> [u8; 32] {
        let (wire, id) = signed(author);
        crate::segments::post::store_delivered_post(
            &state.post_segments,
            &state.db,
            &id,
            &wire,
            HOME_URL,
            &HOME,
        )
        .await
        .unwrap();
        id
    }

    /// § Decision 3's purge: a delivered envelope stays while a local reader
    /// follows its author (pending or approved) and goes once none does — and
    /// the purge writes no deletion witness, so a later follow can restore it.
    #[tokio::test]
    async fn the_purge_removes_only_envelopes_nobody_here_follows() {
        let state = fixture_state();
        let (followed, dropped) = (ActorKeypair::generate(), ActorKeypair::generate());
        let kept = delivered(&state, &followed).await;
        let gone = delivered(&state, &dropped).await;
        state
            .db
            .upsert_remote_follow(
                &[0xF1; 32],
                &followed.actor_id().0,
                fauna_core::subscription::FOLLOWERS_TIER,
                &HOME,
                HOME_URL,
                RemoteFollowState::Pending,
                None,
                1,
            )
            .await
            .unwrap();

        assert_eq!(purge_unfollowed(&state).await, 1);
        assert!(state.db.post_exists(&kept).await.unwrap());
        assert!(!state.db.post_exists(&gone).await.unwrap());
        assert!(
            !state.db.post_was_deleted(&gone).await.unwrap(),
            "the author deleted nothing: a re-delivery is not refused"
        );
        assert_eq!(purge_unfollowed(&state).await, 0, "idempotent");
    }

    /// § Decision 5's delete arm: a delivered row is deleted only on the word
    /// of the nest it was delivered from, by verified identity — a nest that
    /// proves the same address holds no custody — and a row this nest holds
    /// as its own keeps the paired gate.
    #[tokio::test]
    async fn a_delivered_rows_tombstone_is_admitted_only_from_its_home() {
        let state = fixture_state();
        let id = delivered(&state, &ActorKeypair::generate()).await;
        let impostor = [0xA2; 32];
        state
            .db
            .record_nest_address(&impostor, HOME_URL, true)
            .await
            .unwrap();
        assert_eq!(
            delete_custody(&state, &impostor, &id).await.unwrap(),
            Some(false),
            "a proven sighting of the home's address is not the home"
        );
        assert_eq!(
            delete_custody(&state, &HOME, &id).await.unwrap(),
            Some(true)
        );
        assert_eq!(
            delete_custody(&state, &HOME, &[0x77; 32]).await.unwrap(),
            None,
            "a row no follow delivered keeps the paired gate"
        );
    }

    /// The custody-add re-attempt gate reads false for a post no room has a
    /// view of, so a custody add runs the fetch (the true half needs a `rooms`
    /// row and is pinned by the room-view conformance test).
    #[tokio::test]
    async fn a_post_with_no_room_view_reads_as_viewless() {
        let state = fixture_state();
        assert!(!state.db.room_post_view_exists(&[0x5A; 32]).await.unwrap());
    }

    /// Custody is per home (§ Decision 3, *What a reader-named home is
    /// trusted for*): a second home delivering a row the first delivered adds
    /// its custody beside the first — the row's origin columns stay the first
    /// home's, either custodian's tombstone is admitted and a third nest's is
    /// not, and a re-delivery from either is idempotent.
    #[tokio::test]
    async fn a_second_home_delivering_a_row_adds_its_custody_beside_the_first() {
        let state = fixture_state();
        let author = ActorKeypair::generate();
        let (wire, id) = signed(&author);
        let store = async |url: &str, home: [u8; 32]| {
            crate::segments::post::store_delivered_post(
                &state.post_segments,
                &state.db,
                &id,
                &wire,
                url,
                &home,
            )
            .await
            .unwrap()
        };
        assert_eq!(store(HOME_URL, HOME).await, DeliveredStore::Stored);
        let other = [0xA3; 32];
        assert_eq!(
            store("https://other.example", other).await,
            DeliveredStore::CustodyAdded
        );
        assert_eq!(
            state.db.delivered_standing(&id).await.unwrap(),
            Some(DeliveredStanding::Delivered(Some(HOME))),
            "the row's origin columns stay the first home's"
        );
        let homes = |v: Vec<([u8; 32], String)>| {
            let mut h = v.into_iter().map(|(h, _)| h).collect::<Vec<_>>();
            h.sort();
            h
        };
        assert_eq!(
            homes(state.db.delivered_post_homes(&id).await.unwrap()),
            vec![other, HOME]
        );
        assert_eq!(
            delete_custody(&state, &HOME, &id).await.unwrap(),
            Some(true)
        );
        assert_eq!(
            delete_custody(&state, &other, &id).await.unwrap(),
            Some(true)
        );
        assert_eq!(
            delete_custody(&state, &[0xA4; 32], &id).await.unwrap(),
            Some(false),
            "a nest holding no custody is refused"
        );
        assert_eq!(
            store(HOME_URL, HOME).await,
            DeliveredStore::Stored,
            "the first home re-delivering is idempotent"
        );
        assert_eq!(
            store("https://other.example", other).await,
            DeliveredStore::Stored,
            "the second home re-delivering is idempotent"
        );
        assert_eq!(
            homes(state.db.delivered_post_homes(&id).await.unwrap()),
            vec![other, HOME]
        );
        // A custodian the row's columns do not name, re-delivering from a new
        // address, refreshes its own custody URL and never the columns': the
        // row's URL and identity keep naming the same home (§ Decision 3).
        const MOVED_URL: &str = "https://moved.example";
        assert_eq!(store(MOVED_URL, other).await, DeliveredStore::Stored);
        assert_eq!(
            state
                .db
                .get_post_origin_nest_url(&id)
                .await
                .unwrap()
                .as_deref(),
            Some(HOME_URL),
            "a second custodian's refresh never rewrites the first home's URL"
        );
        assert!(
            state
                .db
                .delivered_post_homes(&id)
                .await
                .unwrap()
                .contains(&(other, MOVED_URL.to_string()))
        );
    }

    /// § Decision 3's purge, per home: dropping the custody the row's origin
    /// columns named re-points them to the remaining custody, the row stays
    /// listed to the remaining home's follower, and the last drop purges the
    /// row.
    #[tokio::test]
    async fn a_dropped_custody_re_points_the_rows_origin_columns_and_the_last_drop_purges_the_row()
    {
        let state = fixture_state();
        let author = ActorKeypair::generate();
        let (wire, id) = signed(&author);
        let other = [0xA3; 32];
        const OTHER_URL: &str = "https://other.example";
        let (r_home, r_other) = ([0xF1; 32], [0xF2; 32]);
        let follow = async |reader: [u8; 32], home: [u8; 32], url: &str, st| {
            state
                .db
                .upsert_remote_follow(
                    &reader,
                    &author.actor_id().0,
                    fauna_core::subscription::FOLLOWERS_TIER,
                    &home,
                    url,
                    st,
                    None,
                    1,
                )
                .await
                .unwrap();
        };
        follow(r_other, other, OTHER_URL, RemoteFollowState::Approved).await;
        follow(r_home, HOME, HOME_URL, RemoteFollowState::Approved).await;
        for (url, home) in [(OTHER_URL, other), (HOME_URL, HOME)] {
            crate::segments::post::store_delivered_post(
                &state.post_segments,
                &state.db,
                &id,
                &wire,
                url,
                &home,
            )
            .await
            .unwrap();
        }
        assert_eq!(
            state.db.delivered_standing(&id).await.unwrap(),
            Some(DeliveredStanding::Delivered(Some(other)))
        );
        assert_eq!(purge_unfollowed(&state).await, 0, "both homes followed");

        // The reader at the first home leaves: its custody goes, the origin
        // columns move to the home, and the home's follower still lists it.
        follow(r_other, other, OTHER_URL, RemoteFollowState::Removed).await;
        assert_eq!(purge_unfollowed(&state).await, 0);
        assert!(state.db.post_exists(&id).await.unwrap());
        assert_eq!(
            state.db.delivered_post_homes(&id).await.unwrap(),
            vec![(HOME, HOME_URL.to_string())]
        );
        assert_eq!(
            state.db.delivered_standing(&id).await.unwrap(),
            Some(DeliveredStanding::Delivered(Some(HOME))),
            "the origin columns name the remaining custody"
        );
        assert!(
            !state
                .db
                .delivered_post_hidden_from(&id, Some(r_home))
                .await
                .unwrap()
        );
        assert_eq!(
            delete_custody(&state, &other, &id).await.unwrap(),
            Some(false),
            "a dropped custody opens no tombstone door"
        );

        // The last reader leaves: the last custody goes and the row with it.
        follow(r_home, HOME, HOME_URL, RemoteFollowState::Removed).await;
        assert_eq!(purge_unfollowed(&state).await, 1);
        assert!(!state.db.post_exists(&id).await.unwrap());
        assert!(state.db.delivered_post_homes(&id).await.unwrap().is_empty());
        assert!(
            !state.db.post_was_deleted(&id).await.unwrap(),
            "the author deleted nothing"
        );
    }

    /// § Decision 5: a custodian's tombstone removes the row whole, every
    /// home's custody with it.
    #[tokio::test]
    async fn a_custodians_tombstone_removes_the_row_and_every_custody() {
        let state = fixture_state();
        let (wire, id) = signed(&ActorKeypair::generate());
        for (url, home) in [("https://other.example", [0xA3; 32]), (HOME_URL, HOME)] {
            crate::segments::post::store_delivered_post(
                &state.post_segments,
                &state.db,
                &id,
                &wire,
                url,
                &home,
            )
            .await
            .unwrap();
        }
        assert!(state.db.delete_post_projection(&id).await.unwrap());
        assert!(state.db.delivered_post_homes(&id).await.unwrap().is_empty());
        assert_eq!(delete_custody(&state, &HOME, &id).await.unwrap(), None);
    }

    /// A row this nest already lists nest-wide keeps its standing and its
    /// origin columns under a delivery: it is never relabelled as another
    /// nest's.
    #[tokio::test]
    async fn a_nest_wide_row_keeps_its_standing_and_origin_under_a_delivery() {
        let state = fixture_state();
        let author = ActorKeypair::generate();
        let (wire, id) = signed(&author);
        state.db.put_post_index_only(&id, &wire).await.unwrap();
        let outcome = crate::segments::post::store_delivered_post(
            &state.post_segments,
            &state.db,
            &id,
            &wire,
            HOME_URL,
            &HOME,
        )
        .await
        .unwrap();
        assert_eq!(outcome, DeliveredStore::KeptNestWide);
        assert_eq!(
            state.db.delivered_standing(&id).await.unwrap(),
            Some(DeliveredStanding::NestWide)
        );
        let conn = state.db.conn().await;
        let (url, origin): (Option<String>, Option<Vec<u8>>) = conn
            .query_row(
                "SELECT origin_nest_url, origin_nest_id FROM content WHERE id = ?1",
                [id.as_slice()],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .unwrap();
        assert_eq!((url, origin), (None, None));
    }

    // -- The blob and segment doors (§ Decision 4, (mcliv-r2)) -----------

    fn photo_post(
        author: fauna_core::identity::ActorId,
        photo: fauna_core::data::ContentHash,
    ) -> Post {
        Post {
            author,
            created_at: Timestamp::now(),
            body: PostBody::TextWithMedia {
                content: "look".into(),
                facets: vec![],
                items: vec![fauna_core::data::MediaItem {
                    blob_hash: photo,
                    media_type: "image/png".into(),
                    size_bytes: 11,
                    dimensions: None,
                    thumbnail: None,
                    ..Default::default()
                }],
            },
            references: vec![],
            expires_at: None,
            gated: None,
            content_warning: None,
            origin: None,
        }
    }

    /// A follow-delivered photo post, stored the way `deliver_handler` stores it.
    async fn delivered_photo(
        state: &AppState,
        author: &ActorKeypair,
        photo: fauna_core::data::ContentHash,
    ) -> [u8; 32] {
        let (bytes, env) = sign_envelope(author, &photo_post(author.actor_id(), photo)).unwrap();
        let wire = canonical_encode(&EmbedAsBytes::from_signed(bytes, env)).unwrap();
        let id = *blake3::hash(&wire).as_bytes();
        crate::segments::post::store_delivered_post(
            &state.post_segments,
            &state.db,
            &id,
            &wire,
            HOME_URL,
            &HOME,
        )
        .await
        .unwrap();
        id
    }

    async fn rebuild_withhold(state: &AppState) {
        crate::moderation_withhold::recompute(
            &state.db,
            crate::backup::gc::PostBodySource {
                segments: &state.post_segments,
            },
        )
        .await
        .unwrap();
    }

    async fn door(state: &AppState, photo: &fauna_core::data::ContentHash) -> Option<u16> {
        crate::blob_routes::legal_takedown_gate(&state.db, &photo.digest())
            .await
            .map(|r| r.status().as_u16())
    }

    /// A flagged delivered row's attachment answers nothing at the blob and
    /// segment doors (`legal_takedown_gate` is their shared first step), on
    /// either ground: this nest holds none of those bytes, and a 451 or 403
    /// where an unflagged delivered row's attachment answers 404 would tell an
    /// anonymous asker that a local reader follows the author. A local post
    /// naming the same hash still withholds it, and an unflagged delivered row
    /// does not exempt a local takedown's photo.
    #[tokio::test]
    async fn a_flagged_delivered_rows_attachment_is_not_withheld_at_the_blob_door() {
        let state = fixture_state();
        let author = ActorKeypair::generate();
        let photo = fauna_core::data::ContentHash::of_raw(b"a photo resting at the author's home");
        let id = delivered_photo(&state, &author, photo).await;

        state
            .db
            .set_post_legal_takedown(&id, Some("EU-DSA-2026/12"))
            .await
            .unwrap();
        assert_eq!(
            state
                .db
                .get_post_legal_takedown(&id)
                .await
                .unwrap()
                .as_deref(),
            Some("EU-DSA-2026/12"),
            "the flag must land on the delivered row, or this pins nothing"
        );
        rebuild_withhold(&state).await;
        assert_eq!(door(&state, &photo).await, None, "legal takedown");

        state.db.set_post_legal_takedown(&id, None).await.unwrap();
        state
            .db
            .set_post_admin_block(&id, Some("report-12"))
            .await
            .unwrap();
        rebuild_withhold(&state).await;
        assert_eq!(door(&state, &photo).await, None, "admin block");

        // A LOCAL post naming the same hash under a takedown still withholds
        // it, and the unflagged-then delivered row does not exempt it.
        state.db.set_post_admin_block(&id, None).await.unwrap();
        let local = photo_post(fauna_core::identity::ActorId([0x5A; 32]), photo);
        let local_id: [u8; 32] = fauna_core::encoding::compute_post_id(&local)
            .unwrap()
            .as_bytes()[4..]
            .try_into()
            .unwrap();
        let body = canonical_encode(&local).unwrap();
        crate::segments::post::store_post(&state.post_segments, &state.db, &local_id, &body, None)
            .await
            .unwrap();
        state.db.index_post(&local_id, &local).await.unwrap();
        state
            .db
            .set_post_legal_takedown(&local_id, Some("EU-DSA-2026/13"))
            .await
            .unwrap();
        rebuild_withhold(&state).await;
        assert_eq!(
            door(&state, &photo).await,
            Some(451),
            "a local takedown's photo is withheld whatever a delivered row names"
        );
    }

    /// The home's tombstone for a delivered row deleted while taken down
    /// records no digests: the withhold the deletion carries forward
    /// (`moderation.md` § Legal takedown → *Posts*) is for bytes this nest
    /// holds, and a delivered row's rest at its home.
    #[tokio::test]
    async fn a_taken_down_delivered_rows_delete_carries_no_withhold_forward() {
        let state = fixture_state();
        let author = ActorKeypair::generate();
        let photo = fauna_core::data::ContentHash::of_raw(b"a delivered photo, taken down");
        let id = delivered_photo(&state, &author, photo).await;
        state
            .db
            .set_post_legal_takedown(&id, Some("EU-DSA-2026/14"))
            .await
            .unwrap();

        let tombstone = fauna_core::data::Tombstone {
            author: author.actor_id(),
            post_id: fauna_core::data::PostId::from_digest_dag_cbor(id),
            created_at: Timestamp::now(),
        };
        crate::routes::delete_post_core(
            &state,
            author.actor_id().0,
            &tombstone,
            id,
            crate::routes::RenderSite::Now,
        )
        .await
        .unwrap_or_else(|_| panic!("the home's tombstone deletes the delivered row"));
        assert!(!state.db.post_exists(&id).await.unwrap());
        assert!(
            state
                .db
                .taken_down_deleted_post_blob_digests()
                .await
                .unwrap()
                .is_empty(),
            "a delivered row's attachments were carried into the withhold"
        );
        rebuild_withhold(&state).await;
        assert_eq!(door(&state, &photo).await, None);
    }

    /// A subscriber homed at [`HOME`] on `tier`, as an approved relayed
    /// request leaves the roster row.
    async fn homed_subscriber(state: &AppState, author: &[u8; 32], tier: &str) -> [u8; 32] {
        let sub = ActorKeypair::generate().actor_id().0;
        let id = state
            .db
            .insert_subscribe_request(author, &sub, tier, "subscribe", None)
            .await
            .unwrap();
        state
            .db
            .set_subscribe_request_home(id, &HOME, Some(HOME_URL))
            .await
            .unwrap();
        state
            .db
            .add_subscriber(author, &sub, tier, None)
            .await
            .unwrap();
        state
            .db
            .copy_subscribe_request_home(id, author, &sub, tier)
            .await
            .unwrap();
        sub
    }

    /// § Decision 1's backfill, the bought post (ruled with the redeem twin):
    /// the approval of a per-post tier's request enqueues the designated post
    /// to the buyer's home even when the newest-50 window is skipped — another
    /// subscriber already homed there — and exactly once beside the window.
    #[tokio::test]
    async fn a_per_post_tiers_approval_backfills_the_designated_post_whatever_the_window() {
        let state = fixture_state();
        let author = ActorKeypair::generate();
        let author_id = author.actor_id().0;
        state
            .db
            .create_user(&author_id, "free", "author")
            .await
            .unwrap();
        let (wire, post_id) = signed(&author);
        crate::segments::post::store_post(&state.post_segments, &state.db, &post_id, &wire, None)
            .await
            .unwrap();
        let designation = fauna_core::hex32::encode(&post_id);
        state
            .db
            .create_subscription_tier(
                &author_id,
                "post-sale",
                1,
                None,
                None,
                None,
                false,
                Some(&designation),
                None,
                false,
            )
            .await
            .unwrap();
        state
            .db
            .create_subscription_tier(
                &author_id, "gold", 1, None, None, None, false, None, None, false,
            )
            .await
            .unwrap();
        let deliveries = || async {
            state
                .db
                .outbox_pending_follow(100)
                .await
                .unwrap()
                .into_iter()
                .filter(|e| {
                    e.entry_type == ENTRY_TYPE_FOLLOW_DELIVER
                        && e.peer_nest_id.as_deref() == Some(HOME.as_slice())
                        && e.payload == wire
                })
                .count()
        };

        // The first subscriber homed there: the window carries the post, and
        // the designation adds no second copy.
        homed_subscriber(&state, &author_id, "post-sale").await;
        backfill_new_home(&state, &author_id, &HOME, "post-sale").await;
        assert_eq!(deliveries().await, 1, "deduplicated with the window");

        // A second buyer homed at the same nest: the window is skipped, the
        // bought post still goes.
        homed_subscriber(&state, &author_id, "post-sale").await;
        backfill_new_home(&state, &author_id, &HOME, "post-sale").await;
        assert_eq!(
            deliveries().await,
            2,
            "the bought post goes whatever the window"
        );

        // An undesignated tier's approval with the window skipped sends nothing.
        homed_subscriber(&state, &author_id, "gold").await;
        backfill_new_home(&state, &author_id, &HOME, "gold").await;
        assert_eq!(deliveries().await, 2);
    }
}
