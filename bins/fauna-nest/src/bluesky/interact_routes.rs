//! Bluesky interaction plumbing.
//!
//! The bluesky-specific `interact/*` HTTP routes were ripped. What remains is
//! `route_unified_interaction` and its ATProto record helpers for the
//! **like/repost** verbs, which the unified `fauna.posts.interact` path
//! (`crate::interact_routes`) calls when a post's `source` is "bluesky".
//!
//! `reply`/`quote` here are the **eligibility door only** (`bridges.md`
//! § Interactions, ratified 2026-09-26): the reply is the replier's own signed
//! post, created through `fauna.posts.create`, and the write-through's create
//! leg (`super::write_through_create_inner`) derives the Bluesky record from
//! its `Reference::Reply`/`Quote` — so this door answers the native arm's ack
//! when the caller may cross-post, and refuses otherwise. The arm that minted
//! the reply at the PDS from the interact `body` (no local post existed) was
//! retired the same day, with no older-client path kept: it fired only for a
//! `content` row with source `bluesky`, which nothing writes yet.

use axum::response::Json;
use serde::Deserialize;
use serde_json::json;

use fauna_bridge_atproto::atrium_api::com::atproto::repo::{create_record, delete_record};
use fauna_bridge_atproto::atrium_api::types::string::{Datetime, Nsid};

use crate::api_error::ApiError;
use crate::bluesky::db_helpers;
use crate::routes::AppState;

#[derive(Deserialize)]
pub struct PostInteraction {
    uri: String,
    cid: String,
}

/// Helper: create an interaction record (like or repost) on Bluesky.
async fn create_interaction_record(
    state: &AppState,
    actor_hex: &str,
    req: &PostInteraction,
    collection: &str,
    interaction_type: &str,
) -> Result<Json<serde_json::Value>, ApiError> {
    let agent = db_helpers::get_agent_for_actor(state, actor_hex).await?;

    let did = agent
        .did()
        .await
        .ok_or_else(|| ApiError::internal("could not determine DID from session"))?;

    // Build the record JSON with $type included (required by ATProto).
    let now = Datetime::now();
    let record_json = json!({
        "$type": collection,
        "subject": {
            "uri": req.uri,
            "cid": req.cid,
        },
        "createdAt": now.as_str(),
    });

    let record_value: fauna_bridge_atproto::atrium_api::types::Unknown =
        serde_json::from_value(record_json)
            .map_err(|e| ApiError::internal(format!("record serialization failed: {e}")))?;

    let nsid: Nsid = collection
        .parse()
        .map_err(|_| ApiError::internal("invalid collection NSID"))?;

    let input = create_record::InputData {
        collection: nsid,
        record: record_value,
        repo: fauna_bridge_atproto::atrium_api::types::string::AtIdentifier::Did(did),
        rkey: None,
        swap_commit: None,
        validate: None,
    };

    let output = agent
        .api
        .com
        .atproto
        .repo
        .create_record(input.into())
        .await
        .map_err(|e| {
            tracing::warn!("{collection} create failed for {actor_hex}: {e}");
            ApiError::bad_gateway(format!("Bluesky API error: {e}"))
        })?;

    let record_uri = output.uri.clone();

    // Store the interaction for later undo.
    let conn = state.db.conn().await;
    if let Err(e) =
        db_helpers::store_interaction(&conn, actor_hex, &req.uri, interaction_type, &record_uri)
    {
        tracing::warn!("failed to store {interaction_type} interaction: {e}");
    }

    Ok(Json(json!({ "record_uri": record_uri })))
}

/// Helper: delete an interaction record (unlike or unrepost) on Bluesky.
async fn delete_interaction_record(
    state: &AppState,
    actor_hex: &str,
    record_uri: &str,
    collection: &str,
) -> Result<Json<serde_json::Value>, ApiError> {
    let agent = db_helpers::get_agent_for_actor(state, actor_hex).await?;

    let did = agent
        .did()
        .await
        .ok_or_else(|| ApiError::internal("could not determine DID from session"))?;

    // Extract rkey from the record URI (last path segment after `/`).
    let rkey_str = record_uri
        .rsplit('/')
        .next()
        .ok_or_else(|| ApiError::bad_request("invalid record_uri: no rkey"))?;

    let rkey: fauna_bridge_atproto::atrium_api::types::string::RecordKey = rkey_str
        .parse()
        .map_err(|_| ApiError::bad_request("invalid rkey in record_uri"))?;

    let nsid: Nsid = collection
        .parse()
        .map_err(|_| ApiError::internal("invalid collection NSID"))?;

    let input = delete_record::InputData {
        collection: nsid,
        repo: fauna_bridge_atproto::atrium_api::types::string::AtIdentifier::Did(did),
        rkey,
        swap_commit: None,
        swap_record: None,
    };

    agent
        .api
        .com
        .atproto
        .repo
        .delete_record(input.into())
        .await
        .map_err(|e| {
            tracing::warn!("{collection} delete failed for {actor_hex}: {e}");
            ApiError::bad_gateway(format!("Bluesky API error: {e}"))
        })?;

    // Remove the stored interaction.
    let conn = state.db.conn().await;
    if let Err(e) = db_helpers::remove_interaction(&conn, actor_hex, record_uri) {
        tracing::warn!("failed to remove interaction record: {e}");
    }

    Ok(Json(json!({ "deleted": true })))
}

/// The remedy a refused reply/quote door names. One string for both
/// refusals, because the fix is the same: the consume-side link on the
/// Bluesky page is what lets this nest write to a Bluesky repo as the user.
const REPLY_NEEDS_LINK: &str = "replying to or quoting a Bluesky post from Fauna needs a linked \
                                Bluesky account — link one from the Bluesky page first";

/// The eligibility door for `reply`/`quote` on a Bluesky post
/// (`bridges.md` § Interactions). `Ok` is the native arm's ack, verbatim
/// (`{action, target_post_id, source}`): the app then composes the signed
/// referencing post and the write-through derives the record. `Err` is the
/// affordance's inline failure, and nothing is composed.
///
/// Two refusals, both `400`: no consume-side link at all, and a link D7
/// withholds (`consume_side_poll_allowed` — an actor whose ATProto identity
/// this nest hosts writes through its own repo, never through an external
/// session). No PDS round trip: the door decides from the account tables
/// alone, so it is pinned headlessly and costs nothing on the tap.
async fn reply_door(
    state: &AppState,
    actor_hex: &str,
    post_id_hex: &str,
    action: &str,
) -> Result<Json<serde_json::Value>, ApiError> {
    let conn = state.db.conn().await;
    let linked = db_helpers::get_linked_account(&conn, actor_hex)
        .map_err(|e| ApiError::internal(format!("db error: {e}")))?
        .is_some();
    let admitted = linked
        && db_helpers::consume_side_poll_allowed(&conn, actor_hex)
            .map_err(|e| ApiError::internal(format!("db error: {e}")))?;
    drop(conn);
    if !admitted {
        return Err(ApiError::bad_request(REPLY_NEEDS_LINK));
    }
    Ok(Json(json!({
        "action": action,
        "target_post_id": post_id_hex,
        "source": "bluesky",
    })))
}

/// Route a unified interaction to Bluesky.
///
/// Called by the unified `fauna.posts.interact` core when the post source is
/// "bluesky". `reply`/`quote` take the eligibility door ([`reply_door`]) and
/// never touch the PDS; every other verb resolves the AT-URI and CID, then
/// dispatches to the matching record create/delete.
pub async fn route_unified_interaction(
    state: &AppState,
    actor_hex: &str,
    post_id_hex: &str,
    action: &str,
) -> Result<Json<serde_json::Value>, ApiError> {
    if matches!(action, "reply" | "quote") {
        return reply_door(state, actor_hex, post_id_hex, action).await;
    }

    // Resolve AT-URI + CID for this post
    let (at_uri, cid) = match db_helpers::resolve_uri_and_cid(state, post_id_hex).await {
        Ok(Some(pair)) => pair,
        Ok(None) => {
            return Err(ApiError::not_found(
                "no Bluesky mapping found for this post; it may not have been cross-posted",
            ));
        }
        Err(e) => {
            tracing::error!("resolve_uri_and_cid error: {e}");
            return Err(ApiError::internal("failed to resolve Bluesky post"));
        }
    };

    match action {
        "like" => {
            let req = PostInteraction { uri: at_uri, cid };
            create_interaction_record(state, actor_hex, &req, "app.bsky.feed.like", "like").await
        }
        "repost" => {
            let req = PostInteraction { uri: at_uri, cid };
            create_interaction_record(state, actor_hex, &req, "app.bsky.feed.repost", "repost")
                .await
        }
        "unlike" | "unrepost" => {
            let (interaction_type, collection) = if action == "unlike" {
                ("like", "app.bsky.feed.like")
            } else {
                ("repost", "app.bsky.feed.repost")
            };
            let conn = state.db.conn().await;
            let record_uri =
                match db_helpers::get_interaction(&conn, actor_hex, &at_uri, interaction_type) {
                    Ok(Some(uri)) => uri,
                    Ok(None) => {
                        return Err(ApiError::not_found(format!(
                            "no prior {interaction_type} record found for this post"
                        )));
                    }
                    Err(e) => {
                        tracing::error!("get_interaction error: {e}");
                        return Err(ApiError::internal("failed to look up prior interaction"));
                    }
                };
            drop(conn);
            delete_interaction_record(state, actor_hex, &record_uri, collection).await
        }
        _ => Err(ApiError::bad_request(format!(
            "action '{action}' is not supported on Bluesky"
        ))),
    }
}

#[cfg(test)]
mod reply_door_tests {
    //! The reply/quote door decides from the account tables alone, so every
    //! branch is pinned without a PDS (`bridges.md` § Interactions).
    use super::*;
    use crate::bluesky::init_db;
    use crate::db::CacheDb;
    use std::sync::Arc;

    async fn test_state() -> Arc<AppState> {
        let db = Arc::new(CacheDb::open_in_memory().unwrap());
        init_db(&db).await.unwrap();
        Arc::new(AppState::for_test(db))
    }

    const ACTOR: [u8; 32] = [0x21u8; 32];
    const TARGET: &str = "ab00ab00ab00ab00ab00ab00ab00ab00ab00ab00ab00ab00ab00ab00ab00ab00";

    fn link(conn: &rusqlite::Connection, actor: [u8; 32]) {
        db_helpers::upsert_linked_account(
            conn,
            &hex::encode(actor),
            "did:plc:example",
            "example.bsky.social",
        )
        .unwrap();
    }

    /// A linked account gets the native arm's ack, and no `bluesky_posts`
    /// mapping is consulted: the target's record is resolved later, by the
    /// write-through, so a door that needed it here would refuse every reply
    /// to a post whose mapping the ingestion has yet to write.
    #[tokio::test]
    async fn a_linked_account_gets_the_native_ack_without_a_mapping_or_a_pds() {
        let state = test_state().await;
        link(&*state.db.conn().await, ACTOR);

        for action in ["reply", "quote"] {
            let Json(ack) = route_unified_interaction(&state, &hex::encode(ACTOR), TARGET, action)
                .await
                .unwrap_or_else(|e| panic!("{action} must ack for a linked account: {e}"));
            assert_eq!(ack["action"], action);
            assert_eq!(ack["target_post_id"], TARGET);
            assert_eq!(ack["source"], "bluesky");
        }
    }

    /// No consume-side link → the affordance's inline failure, naming the
    /// remedy; the app composes nothing (`ui/feed.md` § Interaction bar).
    #[tokio::test]
    async fn an_unlinked_account_is_refused_with_the_remedy() {
        let state = test_state().await;
        let err = route_unified_interaction(&state, &hex::encode(ACTOR), TARGET, "reply")
            .await
            .expect_err("no link must refuse");
        assert_eq!(err.status, axum::http::StatusCode::BAD_REQUEST);
        assert!(err.message.contains("Bluesky page"), "{}", err.message);
    }

    /// D7: an actor whose ATProto identity this nest hosts is refused even
    /// with a stale consume-side row — that identity writes through its own
    /// repo, never through an external session.
    #[tokio::test]
    async fn a_hosted_backing_is_refused_even_when_a_link_row_exists() {
        let state = test_state().await;
        {
            let conn = state.db.conn().await;
            link(&conn, ACTOR);
            conn.execute(
                "INSERT INTO atproto_identities (actor_id, method, status, did, created_at, updated_at)
                 VALUES (?1, 'plc', 'active', 'did:plc:hosted', 0, 0)",
                rusqlite::params![&ACTOR[..]],
            )
            .expect("insert hosted identity");
        }
        let res = route_unified_interaction(&state, &hex::encode(ACTOR), TARGET, "quote").await;
        assert!(
            res.is_err(),
            "a hosted backing must not ack a consume-side reply"
        );
    }
}
