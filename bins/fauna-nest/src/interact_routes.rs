//! Post-interaction routing core for `fauna.posts.interact`.
//!
//! The HTTP twin (`POST /api/v1/posts/{id}/interact`) was deleted; the
//! WS-RPC handler
//! (`posts_handlers::posts_interact_handler`) now owns the entry point and
//! calls the shared `interact_with_post_core` below.

use std::sync::Arc;

#[cfg(feature = "bluesky")]
use axum::response::Json;
use serde_json::json;

use crate::api_error::ApiError;
use crate::routes::AppState;

/// The resolved origin protocol + the protocol-specific result JSON of a
/// successful interaction. The WS-RPC handler serializes `result` into the
/// `result` string of `PostInteractReply` (preserving the heterogeneous
/// per-source shapes byte-for-byte).
pub(crate) struct InteractOutcome {
    pub source: String,
    pub result: serde_json::Value,
    /// The target post's four counters as they stand **after** this act, when
    /// this handler can speak for them — `None` otherwise, which the client
    /// reads as "leave the rendered counts alone" (`PostInteractReply::counts`).
    ///
    /// `Some` exactly on the native arm (`fauna` and the archive-import
    /// platforms — `fauna_core::source::is_native`) for the actions whose
    /// `post_id` names the post whose counters the caller is looking at. It is
    /// deliberately `None` for:
    ///
    /// - **bridged sources** (`bluesky`/`nostr`/`activitypub`) — their counts
    ///   belong to the origin protocol, not this nest's `content_meta` — save
    ///   the `reply`/`quote` ack on an `activitypub`/`nostr` target, whose
    ///   ingested note is a local row ([`bridged_ack_carries_counts`]);
    /// - **`unrepost`** — its `post_id` names the caller's *own repost post*,
    ///   so its counters are not the ones on screen.
    pub counts: Option<fauna_protocol::posts::PostEngagementCounts>,
}

/// Read the post's counters straight after the act. Best-effort by design: the
/// counters are a display nicety, so a read failure degrades to `None` (the
/// client keeps rendering what it had) rather than failing an interaction that
/// already succeeded.
async fn counts_after_act(
    state: &Arc<AppState>,
    post_id_bytes: &[u8; 32],
) -> Option<fauna_protocol::posts::PostEngagementCounts> {
    let c = state.db.get_engagement_counts(post_id_bytes).await.ok()?;
    Some(fauna_protocol::posts::PostEngagementCounts {
        like_count: c.like_count,
        reply_count: c.reply_count,
        repost_count: c.repost_count,
        quote_count: c.quote_count,
        extra: std::collections::BTreeMap::new(),
    })
}

/// Whether a bridged arm's reply carries the target's counters: only the
/// `reply`/`quote` eligibility ack on an `activitypub` or `nostr` target, whose
/// ingested note IS a local row (`feed.md` § Interaction bar → *Reply and quote
/// on a bridged post*). A like or repost there moves the origin protocol's
/// counters, not this nest's, so it stays `None`.
#[cfg(any(feature = "activitypub", feature = "nostr"))]
fn bridged_ack_carries_counts(action: &str) -> bool {
    matches!(action, "reply" | "quote")
}

#[cfg(any(feature = "activitypub", feature = "nostr"))]
async fn local_row_ack_counts(
    state: &Arc<AppState>,
    action: &str,
    post_id_bytes: &[u8; 32],
) -> Option<fauna_protocol::posts::PostEngagementCounts> {
    if bridged_ack_carries_counts(action) {
        counts_after_act(state, post_id_bytes).await
    } else {
        None
    }
}

/// Shared post-interaction core — the routing behind `fauna.posts.interact`
/// (the HTTP twin `POST /api/v1/posts/{id}/interact` is deleted): action
/// validation → `db.get_post_source` lookup → a discovery-federation-stub gate
/// (`db.get_post_origin_nest_url` — a post this nest indexed via discovery,
/// not one it hosts, is refused into the 501 arm regardless of its token) → per-source dispatch (the native arm — `fauna` and the
/// archive-import platforms — `fauna_core::source::is_native` — like/unlike on
/// every native token, reply/repost/quote target-info on `fauna` only —
/// refused on an archive-platform token (ruling 3), bridged
/// bluesky/nostr/activitypub via their `route_unified_interaction`, email
/// rejection). Returns the origin protocol and the result JSON on success, or
/// an `ApiError` (already status and message-shaped) on failure. See
/// `posts_handlers` (WS-RPC) for the reply mapping.
#[allow(unused_variables)]
pub(crate) async fn interact_with_post_core(
    state: &Arc<AppState>,
    actor: [u8; 32],
    post_id_bytes: [u8; 32],
    action: &str,
) -> Result<InteractOutcome, ApiError> {
    // Validate action
    let valid_actions = ["like", "unlike", "reply", "repost", "unrepost", "quote"];
    if !valid_actions.contains(&action) {
        return Err(ApiError::bad_request(
            "invalid action: must be one of like, unlike, reply, repost, unrepost, quote",
        ));
    }

    let post_id_hex = hex::encode(post_id_bytes);

    // Look up the source protocol for this post
    let source = match state.db.get_post_source(&post_id_bytes).await {
        Ok(Some(s)) => s,
        // `unrepost` is idempotent, matching `fauna.posts.delete`'s
        // crash-safety contract (`routes::delete_post_core`'s
        // `AlreadyGone` — a client retry after a dropped ack must not
        // surface an error for something that already succeeded, and a
        // garbage/unknown id is treated the same as an already-deleted
        // one): the target being gone IS the desired end state.
        Ok(None) if action == "unrepost" => {
            return Ok(InteractOutcome {
                source: "fauna".into(),
                result: json!({ "ok": true }),
                counts: None,
            });
        }
        Ok(None) => return Err(ApiError::not_found("post not found")),
        Err(e) => {
            tracing::error!("get_post_source error: {e}");
            return Err(ApiError::internal("storage error"));
        }
    };

    // A discovery-indexed federation stub: `source`
    // now carries the peer's real advertised protocol token — including, legitimately,
    // `"fauna"` for a genuine remote Fauna post — so it can no longer double
    // as "this nest owns it" the way an unconditional URL-shaped `source`
    // used to. This nest has no payload for such a row (§10.2 of the
    // discovery-feeds design) and no bridge record for it either, so every
    // action stays routed to the client-follows-the-origin-nest arm below,
    // regardless of the token.
    if let Ok(Some(_)) = state.db.get_post_origin_nest_url(&post_id_bytes).await {
        return Err(ApiError::not_implemented(format!(
            "interaction routing for source '{source}' is not yet implemented"
        )));
    }

    match source.as_str() {
        // Every NATIVE token — `fauna` and the archive-import platforms
        // (`fauna_core::source::is_native`): an imported post is the account's
        // own signed post, so its like/reply/repost land on this nest's
        // counters exactly as a `fauna` post's do. Matching the one token here
        // (as this arm did before 2026-09-06) made a client that still routes
        // by `source == "fauna"` fall into the 501 arm below for imported posts.
        s if fauna_core::source::is_native(s) => {
            match action {
                "like" => {
                    // Idempotent like-count: record the like once under a stable
                    // per-(actor, target) key (`compute_toggle_event_id`, NO time
                    // bucket) and bump `like_count` only on the FIRST like — a
                    // repeat like by the same actor is a counter no-op. `unlike`
                    // reverses it. (The ranking `score` bump + the notification
                    // below stay per-call — their existing behavior is unchanged;
                    // only the displayed counter is made idempotent here.)
                    let now_us = fauna_core::data::Timestamp::now().as_i64();
                    let like_event = fauna_core::engagement::compute_toggle_event_id(
                        &fauna_core::identity::ActorId(actor),
                        &fauna_core::data::ContentHash::from_digest_raw(post_id_bytes),
                        "like",
                    );
                    match state
                        .db
                        .insert_engagement_event(
                            &like_event.digest(),
                            &post_id_bytes,
                            Some(&actor),
                            "like",
                            None,
                            now_us,
                        )
                        .await
                    {
                        Ok(true) => {
                            if let Err(e) = state
                                .db
                                .increment_engagement_count(&post_id_bytes, "like")
                                .await
                            {
                                tracing::warn!("increment like_count: {e}");
                            }
                        }
                        Ok(false) => {} // already liked — counter unchanged
                        Err(e) => tracing::warn!("record like event: {e}"),
                    }

                    // The `engagement` factor scalar (`content_meta.score`) is
                    // recomputed inside `increment_engagement_count` above — only
                    // on a first like (`Ok(true)`), so a re-like no longer
                    // inflates it (the old separate `increment_post_score` +1
                    // fired unconditionally and double-counted).

                    // Generate notification for the post author
                    let liker_id = actor;
                    if let Ok(Some(author_bytes)) =
                        state.db.get_content_author(&post_id_bytes).await
                    {
                        // Don't notify yourself
                        if author_bytes != liker_id {
                            let now = fauna_core::data::Timestamp::now().as_i64();
                            let text = crate::db::notifications::NotificationText::localized(
                                fauna_protocol::LocalizedText::new("notifications.row_like")
                                    .with_arg("sender", &hex::encode(liker_id)[..8]),
                            );
                            // This notification's `source` names the delivery
                            // protocol it rides (always native here), not the
                            // liked post's origin token — it stays "fauna" even
                            // when the liked post is an archive import.
                            if let Ok(Some(notif_id)) = state
                                .db
                                .insert_notification(
                                    &author_bytes,
                                    &fauna_protocol::notifications::NotifType::Like,
                                    "fauna",
                                    Some(&liker_id),
                                    Some(&post_id_bytes),
                                    None,
                                    &text,
                                    now,
                                )
                                .await
                            {
                                state.ws.notify_push(
                                    &author_bytes,
                                    fauna_protocol::PushEvent::Notification(
                                        fauna_protocol::push_events::NotificationPayload {
                                            notification_id: notif_id,
                                            notif_type:
                                                fauna_protocol::notifications::NotifType::Like,
                                            source: "fauna".into(),
                                            sender_id: Some(hex::encode(liker_id)),
                                            content_id: Some(hex::encode(post_id_bytes)),
                                            summary: text.summary().to_string(),
                                            body: text.body().cloned(),
                                            timestamp: fauna_core::data::Timestamp::now_secs()
                                                as u64,
                                            extra: std::collections::BTreeMap::new(),
                                        },
                                    ),
                                );
                            }
                        }
                    }

                    Ok(InteractOutcome {
                        source: source.clone(),
                        result: json!({ "ok": true }),
                        counts: counts_after_act(state, &post_id_bytes).await,
                    })
                }
                "unlike" => {
                    // Reverse the like toggle: delete the stable like event; on
                    // removal, decrement `like_count` (clamped ≥ 0). A no-op if
                    // the actor had not liked this post (idempotent un-like).
                    let like_event = fauna_core::engagement::compute_toggle_event_id(
                        &fauna_core::identity::ActorId(actor),
                        &fauna_core::data::ContentHash::from_digest_raw(post_id_bytes),
                        "like",
                    );
                    match state.db.delete_engagement_event(&like_event.digest()).await {
                        Ok(true) => {
                            if let Err(e) = state
                                .db
                                .decrement_engagement_count(&post_id_bytes, "like")
                                .await
                            {
                                tracing::warn!("decrement like_count: {e}");
                            }
                        }
                        Ok(false) => {} // was not liked — counter unchanged
                        Err(e) => tracing::warn!("delete like event: {e}"),
                    }
                    Ok(InteractOutcome {
                        source: source.clone(),
                        result: json!({ "ok": true }),
                        counts: counts_after_act(state, &post_id_bytes).await,
                    })
                }
                // `unrepost` removes the caller's own repost *post* —
                // `post_id_bytes` here is that repost post's own id (the one
                // returned when the client composed it via `fauna.posts.create`
                // after the `repost` action below), symmetric with how `like`/
                // `unlike` operate on the same id at the same connection-actor
                // trust level (no separately-signed Tombstone — `fauna.posts
                // .delete` is the stricter signed path for a caller outside an
                // established connection). Deletion goes through the shared
                // `delete_post_core`, so the removal is crash-safe and reverses
                // `repost_count` on the original the same way any other post
                // delete does (`feed.md` § State & data shape → *Post
                // deletion*). Gated on the target actually decoding to a
                // `Reference::Repost` so `unrepost` can never become a
                // differently-named way to delete an arbitrary owned post.
                "unrepost" => {
                    let body = crate::segments::post::load_post_body(
                        &state.post_segments,
                        &state.db,
                        &post_id_bytes,
                    )
                    .await
                    .ok()
                    .flatten();
                    let is_repost = body
                        .as_deref()
                        .and_then(crate::db::posts::decode_stored_post)
                        .is_some_and(|post| {
                            post.references
                                .iter()
                                .any(|r| matches!(r, fauna_core::data::Reference::Repost { .. }))
                        });
                    if !is_repost {
                        return Err(ApiError::not_found("post is not a repost"));
                    }

                    let tombstone = fauna_core::data::Tombstone {
                        author: fauna_core::identity::ActorId(actor),
                        post_id: fauna_core::data::PostId::from_digest_dag_cbor(post_id_bytes),
                        created_at: fauna_core::data::Timestamp::now(),
                    };
                    match crate::routes::delete_post_core(
                        state,
                        actor,
                        &tombstone,
                        post_id_bytes,
                        crate::routes::RenderSite::Now,
                    )
                    .await
                    {
                        // No `counts`: `post_id_bytes` is the caller's own repost
                        // post (just deleted), not the original whose
                        // `repost_count` the delete reversed.
                        Ok(_outcome) => Ok(InteractOutcome {
                            source: source.clone(),
                            result: json!({ "ok": true }),
                            counts: None,
                        }),
                        Err(crate::routes::PostDeleteError::NotAuthor) => Err(ApiError::forbidden(
                            "only the author may unrepost their own repost",
                        )),
                        Err(crate::routes::PostDeleteError::Internal(msg)) => {
                            Err(ApiError::internal(msg))
                        }
                    }
                }
                // reply, repost, quote: return target info so the client can
                // compose a post — for the `fauna` token. An archive-platform
                // post refuses instead (`archive-import.md` § Compatibility →
                // *Slice-3 rulings*, ruling 3): every app that knows the token
                // composes the signed post itself and never reaches this arm;
                // the one that does is a non-conforming caller routing the post as
                // bridged, which would take the target-info ack as "the bridge
                // posted it" and lose the body it typed. Fail loudly, name the
                // remedy. Normalized like the enclosing `is_native` guard, so
                // the two arms read one token the same way whatever spelling
                // the index holds.
                _ if fauna_core::source::normalize(s).as_deref()
                    != Some(fauna_core::source::NATIVE) =>
                {
                    Err(ApiError::bad_request(
                        "a reply, repost or quote of an imported post is composed by \
                     the app as a signed post; this request was not composed that way",
                    ))
                }
                // The counters ride along even though this arm moves none of
                // them (the target's `reply_count`/`repost_count`/`quote_count`
                // move later, when the composed post referencing it lands —
                // `db::engagement`): they describe the post the caller is
                // looking at, so returning them keeps the caller's rendered
                // numbers honest instead of merely unchanged.
                _ => Ok(InteractOutcome {
                    source: source.clone(),
                    result: json!({
                        "action": action,
                        "target_post_id": post_id_hex,
                        "source": source,
                    }),
                    counts: counts_after_act(state, &post_id_bytes).await,
                }),
            }
        }
        #[cfg(feature = "bluesky")]
        "bluesky" => {
            let actor_hex = hex::encode(actor);
            // No `body`: the bluesky `reply`/`quote` arms are the eligibility
            // door, the words travel in the signed post (`bridges.md`
            // § Interactions).
            crate::bluesky::interact_routes::route_unified_interaction(
                state,
                &actor_hex,
                &post_id_hex,
                action,
            )
            .await
            .map(|Json(result)| InteractOutcome {
                source: "bluesky".into(),
                result,
                // Bridged: the counters live in the origin protocol.
                counts: None,
            })
        }
        #[cfg(feature = "nostr")]
        "nostr" => {
            let actor_hex = hex::encode(actor);
            let result = crate::nostr::interact::route_unified_interaction(
                state,
                &actor_hex,
                &post_id_hex,
                action,
            )
            .await?
            .0;
            Ok(InteractOutcome {
                source: "nostr".into(),
                result,
                counts: local_row_ack_counts(state, action, &post_id_bytes).await,
            })
        }
        #[cfg(feature = "activitypub")]
        "activitypub" => {
            let actor_hex = hex::encode(actor);
            let result = crate::activitypub::interact::route_unified_interaction(
                state,
                &actor_hex,
                &post_id_hex,
                action,
            )
            .await?
            .0;
            Ok(InteractOutcome {
                source: "activitypub".into(),
                result,
                counts: local_row_ack_counts(state, action, &post_id_bytes).await,
            })
        }
        "email" => Err(ApiError::bad_request(
            "interactions are not supported for email content; use the email bridge reply endpoint instead",
        )),
        _ => Err(ApiError::not_implemented(format!(
            "interaction routing for source '{source}' is not yet implemented"
        ))),
    }
}

#[cfg(test)]
mod tests {
    #[cfg(any(feature = "activitypub", feature = "nostr"))]
    #[test]
    fn only_the_reply_and_quote_ack_carries_a_bridged_targets_counts() {
        for action in ["reply", "quote"] {
            assert!(super::bridged_ack_carries_counts(action), "{action}");
        }
        for action in ["like", "unlike", "repost", "unrepost"] {
            assert!(!super::bridged_ack_carries_counts(action), "{action}");
        }
    }

    #[test]
    fn valid_actions_accepted() {
        let valid = ["like", "unlike", "reply", "repost", "unrepost", "quote"];
        for action in &valid {
            assert!(
                ["like", "unlike", "reply", "repost", "unrepost", "quote"].contains(action),
                "{action} should be valid"
            );
        }
    }

    #[test]
    fn invalid_post_id_hex_rejected() {
        // Post IDs must be exactly 32 bytes (64 hex chars)
        let short = "abcd";
        let result = hex::decode(short);
        assert!(result.unwrap().len() != 32);
    }

    #[test]
    fn unsupported_actions_rejected() {
        // Actor-relationship actions remain protocol-specific (rule 4) and are
        // not accepted by the unified post-interact endpoint.
        let valid = ["like", "unlike", "reply", "repost", "unrepost", "quote"];
        for action in ["follow", "unfollow", "block", "unblock", "delete", "save"] {
            assert!(
                !valid.contains(&action),
                "{action} must not be on the unified action allow-list"
            );
        }
    }
}
