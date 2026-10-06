//! ActivityPub interaction routing for the unified interact endpoint.
//!
//! Maps unified actions to AP activity types:
//! - like/react/upvote  -> Like activity
//! - repost             -> Announce activity
//! - reply              -> Create(Note) with inReplyTo
//! - quote              -> Create(Note) carrying the FEP-e232 link + the `quote`/`quoteUri`/`_misskey_quote` spellings
//!
//! `reply`/`quote` here are the eligibility + target-info door only: the reply is the
//! replier's own signed post and `push.rs` derives its `Create` — `activitypub.md`
//! § Reply and quote. The door enqueues nothing.

use axum::response::Json;
use serde_json::json;

use crate::api_error::ApiError;
use crate::routes::AppState;

/// Route a unified interaction to ActivityPub.
///
/// Resolves the AP URL from `ap_post_map`, constructs the activity JSON,
/// and enqueues it for delivery to the remote actor's inbox.
pub async fn route_unified_interaction(
    state: &AppState,
    actor_hex: &str,
    post_id_hex: &str,
    action: &str,
) -> Result<Json<serde_json::Value>, ApiError> {
    let conn = state.db.conn().await;

    // Look up the AP URL (+ owning remote actor, when the inbound ingest
    // recorded it) for this post.
    let (ap_url, remote_actor_uri) = super::db_helpers::get_ap_target_for_post(&conn, post_id_hex)
        .map_err(|e| {
            tracing::error!("ap post map lookup error: {e}");
            ApiError::internal("failed to look up ActivityPub URL")
        })?
        .ok_or_else(|| ApiError::not_found("no ActivityPub mapping found for this post"))?;

    // Look up the user's AP account
    let account = super::db_helpers::get_account(&conn, actor_hex)
        .map_err(|e| {
            tracing::error!("ap account lookup error: {e}");
            ApiError::internal("failed to look up ActivityPub account")
        })?
        .ok_or_else(|| {
            ApiError::bad_request(
                "no linked ActivityPub account; enable the ActivityPub bridge from the Bridges page first",
            )
        })?;

    let actor_url = &account.actor_url;

    // Resolve the delivery inbox once: the object owner's cached real inbox
    // when the inbound ingest recorded the actor URI, else the
    // URL-shape heuristic (an uncached actor, or a local row).
    let inbox_url =
        super::db_helpers::resolve_delivery_inbox(&conn, remote_actor_uri.as_deref(), &ap_url);

    match action {
        "like" => {
            let activity = json!({
                "@context": "https://www.w3.org/ns/activitystreams",
                "type": "Like",
                "actor": actor_url,
                "object": ap_url,
            });
            let activity_json = serde_json::to_string(&activity)
                .map_err(|e| ApiError::internal(format!("JSON serialization error: {e}")))?;

            super::db_helpers::enqueue_delivery(&conn, &activity_json, &inbox_url).map_err(
                |e| {
                    tracing::error!("enqueue_delivery error: {e}");
                    ApiError::internal("failed to enqueue AP delivery")
                },
            )?;

            tracing::info!(
                actor = actor_hex,
                ap_url,
                inbox = inbox_url,
                "ap: enqueued Like activity"
            );

            Ok(Json(json!({
                "ok": true,
                "protocol": "activitypub",
                "activity_type": "Like",
                "target": ap_url,
            })))
        }
        "repost" => {
            let activity = json!({
                "@context": "https://www.w3.org/ns/activitystreams",
                "type": "Announce",
                "actor": actor_url,
                "object": ap_url,
            });
            let activity_json = serde_json::to_string(&activity)
                .map_err(|e| ApiError::internal(format!("JSON serialization error: {e}")))?;

            super::db_helpers::enqueue_delivery(&conn, &activity_json, &inbox_url).map_err(
                |e| {
                    tracing::error!("enqueue_delivery error: {e}");
                    ApiError::internal("failed to enqueue AP delivery")
                },
            )?;

            tracing::info!(
                actor = actor_hex,
                ap_url,
                inbox = inbox_url,
                "ap: enqueued Announce activity"
            );

            Ok(Json(json!({
                "ok": true,
                "protocol": "activitypub",
                "activity_type": "Announce",
                "target": ap_url,
            })))
        }
        // The eligibility door: the caller must be able to federate the post
        // it is about to compose; the native arm's ack verbatim (the
        // dispatcher adds the target's counters — it is a local row).
        "reply" | "quote" => {
            if !account.enabled {
                return Err(ApiError::bad_request(
                    "ActivityPub federation is switched off; enable it from the Bridges page first",
                ));
            }
            Ok(Json(json!({
                "action": action,
                "target_post_id": post_id_hex,
                "source": "activitypub",
            })))
        }
        _ => Err(ApiError::bad_request(format!(
            "action '{action}' is not supported on ActivityPub"
        ))),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::CacheDb;
    use std::sync::Arc;

    const TARGET: &str = "abababababababababababababababababababababababababababababababab";

    /// A nest with one ingested fediverse note mapped at `TARGET`, and — when
    /// `enabled` is `Some` — the caller's AP account in that state.
    async fn door_state(actor_hex: &str, enabled: Option<bool>) -> AppState {
        let db = Arc::new(CacheDb::open_in_memory().expect("in-memory CacheDb"));
        crate::activitypub::init_db(&db).await.expect("init_db");
        let state = AppState::for_test(db);
        let conn = state.db.conn().await;
        super::super::db_helpers::insert_post_map(
            &conn,
            TARGET,
            "https://masto.example/users/bob/statuses/9",
            actor_hex,
            Some("https://masto.example/users/bob"),
        )
        .unwrap();
        if let Some(enabled) = enabled {
            super::super::db_helpers::create_account(
                &conn,
                actor_hex,
                "alice",
                "https://nest.example/ap/users/alice",
                b"sealed",
                "pem",
            )
            .unwrap();
            super::super::db_helpers::update_settings(
                &conn,
                actor_hex,
                &super::super::db_helpers::ApSettings {
                    enabled: Some(enabled),
                    ..Default::default()
                },
            )
            .unwrap();
        }
        drop(conn);
        state
    }

    /// `activitypub.md` § Reply and quote → *The door*: the native arm's ack
    /// verbatim, and nothing enqueued — the reply is the caller's own post.
    #[tokio::test]
    async fn reply_and_quote_answer_the_native_ack_and_enqueue_nothing() {
        let actor_hex = hex::encode([0x21u8; 32]);
        let state = door_state(&actor_hex, Some(true)).await;
        for action in ["reply", "quote"] {
            let Json(ack) = route_unified_interaction(&state, &actor_hex, TARGET, action)
                .await
                .unwrap_or_else(|e| panic!("{action} must ack: {}", e.message));
            assert_eq!(
                ack,
                json!({"action": action, "target_post_id": TARGET, "source": "activitypub"})
            );
        }
        let conn = state.db.conn().await;
        let queued: i64 = conn
            .query_row("SELECT COUNT(*) FROM ap_delivery_queue", [], |r| r.get(0))
            .unwrap();
        assert_eq!(queued, 0);
    }

    /// No linked account, or one whose federation is switched off, cannot
    /// federate a reply: `400` naming the Bridges-page remedy.
    #[tokio::test]
    async fn reply_is_refused_without_a_linked_and_enabled_account() {
        let actor_hex = hex::encode([0x22u8; 32]);
        for enabled in [None, Some(false)] {
            let state = door_state(&actor_hex, enabled).await;
            let err = route_unified_interaction(&state, &actor_hex, TARGET, "reply")
                .await
                .expect_err("must refuse");
            assert_eq!(
                err.status,
                axum::http::StatusCode::BAD_REQUEST,
                "{enabled:?}"
            );
            assert!(err.message.contains("Bridges page"), "{}", err.message);
        }
    }
}
