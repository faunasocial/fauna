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

/// A remote subscriber homed at `home` was just approved: unless another of
/// the author's subscribers is already homed there (its index holds the posts
/// already), queue the author's newest servable posts to it (§ Decision 1's
/// backfill). The same entry, the same queue — never a pull.
pub(crate) async fn backfill_new_home(state: &AppState, author: &[u8; 32], home: &[u8; 32]) {
    match state.db.subscribers_homed_at(author, home).await {
        Ok(1) => {}
        Ok(_) => return,
        Err(e) => {
            tracing::warn!("follow backfill: count the home's subscribers: {e:#}");
            return;
        }
    }
    let ids = match state
        .db
        .author_backfill_post_ids(author, BACKFILL_POSTS)
        .await
    {
        Ok(ids) => ids,
        Err(e) => {
            tracing::warn!("follow backfill: list the author's posts: {e:#}");
            return;
        }
    };
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
            crate::segments::post::store_delivered_post(
                &state.post_segments,
                &state.db,
                &post_id,
                &stored_bytes,
                &home_url,
                &origin_nest_id,
            )
            .await
            .map_err(internal)?;
            if newly_stored {
                crate::room_post_view::index_room_post(&state, &post_id, &stored_bytes).await;
            }
            encode_reply(&FedPostForwardReply {})
        })
    })
}

/// `fauna.federation.post.delete`'s follow arm (§ Decision 5): for a stored
/// row a federated follow delivered, the tombstone is admitted only from the
/// nest that delivered it — `Some(true)` admit, `Some(false)` refuse. `None`
/// for every other row, which keeps the paired forward's gate.
pub(crate) async fn delete_custody(
    state: &AppState,
    origin_nest_id: &[u8; 32],
    post_id: &[u8; 32],
) -> Result<Option<bool>, fauna_protocol::RpcError> {
    let Some(home_url) = state
        .db
        .delivered_post_home(post_id)
        .await
        .map_err(internal)?
    else {
        return Ok(None);
    };
    // Proven addresses only: an unproven sighting is any peer's declaration,
    // and would let a nest claim another's address to delete its posts. The
    // follow's own dial to the home proved this one.
    let proven = state
        .db
        .proven_foreign_nest_urls(origin_nest_id)
        .await
        .map_err(internal)?;
    let home_url = home_url.trim_end_matches('/');
    Ok(Some(
        proven.iter().any(|u| u.trim_end_matches('/') == home_url),
    ))
}

// ── Follower's nest: the purge ─────────────────────────────────────────

/// Remove the delivered envelopes no local reader follows the author of any
/// more (§ Decision 3's purge): the projection, the room views derived from
/// it, and the segment record. No deletion witness is written — the author
/// deleted nothing, and a reader who follows again receives them again.
/// Returns how many were removed.
pub(crate) async fn purge_unfollowed(state: &AppState) -> usize {
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

    use crate::db::remote_follows::RemoteFollowState;
    use crate::test_support::fixture_state;

    const HOME: [u8; 32] = [0xB0; 32];
    const HOME_URL: &str = "https://home.example";

    async fn delivered(state: &AppState, author: &ActorKeypair) -> [u8; 32] {
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
    /// of the nest proven at the home it came from — an unproven sighting of
    /// that address is any peer's claim — and a row this nest holds as its own
    /// keeps the paired gate.
    #[tokio::test]
    async fn a_delivered_rows_tombstone_is_admitted_only_from_its_home() {
        let state = fixture_state();
        let id = delivered(&state, &ActorKeypair::generate()).await;
        let (home, impostor) = ([0xA1; 32], [0xA2; 32]);
        state
            .db
            .record_nest_address(&impostor, HOME_URL, false)
            .await
            .unwrap();
        assert_eq!(
            delete_custody(&state, &impostor, &id).await.unwrap(),
            Some(false)
        );
        state
            .db
            .record_nest_address(&home, HOME_URL, true)
            .await
            .unwrap();
        assert_eq!(
            delete_custody(&state, &home, &id).await.unwrap(),
            Some(true)
        );
        assert_eq!(
            delete_custody(&state, &home, &[0x77; 32]).await.unwrap(),
            None,
            "a row no follow delivered keeps the paired gate"
        );
    }
}
