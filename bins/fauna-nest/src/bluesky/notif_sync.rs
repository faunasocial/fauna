//! Bluesky notification poller — fetches ATProto notifications and inserts
//! them into the unified notifications table.
//!
//! Target state: `bridges.md` § Bluesky bridge → *Notifications*. The
//! translated notification lands in the unified table with `source =
//! "bluesky"`, and the apps read it through the unified `fauna.notifications.*`
//! surface — they need nothing Bluesky-specific.
//!
//! The file is split so the ingest half is testable without a PDS (the nest
//! test harness has no fake ATProto server — the same reason
//! `WriteThroughCreateOutcome` exists): [`poll_bluesky_notifications`] owns the
//! one network step, and [`ingest_notifications`] owns every decision made
//! about what it returned.

use std::sync::Arc;

use fauna_bridge_atproto::atrium_api::app::bsky::notification::list_notifications;
use fauna_bridge_atproto::translate::translate_notification;
use fauna_bridge_atproto::types::BlueskyNotification;

use crate::bluesky::db_helpers;
use crate::db::CacheDb;
use crate::db::notifications::NotificationText;
use crate::routes::AppState;
use fauna_protocol::LocalizedText;
use fauna_protocol::notifications::NotifType;

/// Parse an ISO 8601 / RFC 3339 timestamp string to microseconds since epoch.
/// Falls back to current time on parse failure.
fn parse_timestamp_micros(s: &str) -> i64 {
    let now_micros = || fauna_core::data::Timestamp::now().as_i64();

    // Parse via atrium's Datetime which wraps chrono::DateTime<FixedOffset>
    match s.parse::<fauna_bridge_atproto::atrium_api::types::string::Datetime>() {
        Ok(dt) => {
            let secs = dt.as_ref().timestamp();
            let subsec_nanos = dt.as_ref().timestamp_subsec_nanos() as i64;
            secs * 1_000_000 + subsec_nanos / 1_000
        }
        Err(_) => now_micros(),
    }
}

/// Map a Bluesky `reason` to the unified notification type. A reason Fauna has
/// no closer type for is the known catch-all [`NotifType::Interaction`] (wire
/// `other`), never a carried unknown: the poller is a producer, and a producer
/// only mints types this build names.
fn notif_type_of(reason: &str) -> NotifType {
    match reason {
        "like" => NotifType::Like,
        "reply" => NotifType::Reply,
        "repost" => NotifType::Repost,
        "quote" => NotifType::Quote,
        "mention" => NotifType::Mention,
        "follow" => NotifType::Follow,
        _ => NotifType::Interaction,
    }
}

/// What one bridged notification says: the catalog key for its `notif_type`,
/// with the sender's display name as data. The name rides as an arg because it
/// exists nowhere else on the row — a bridged sender is a DID, not a 32-byte
/// key, so `sender_id` is `None` (`behavior/notifications.md` § Localized body).
fn text_of(notif_type: &NotifType, sender_display: &str) -> NotificationText {
    let key = match notif_type {
        NotifType::Like => "notifications.row_like",
        NotifType::Reply => "notifications.row_reply",
        NotifType::Repost => "notifications.row_repost",
        NotifType::Quote => "notifications.row_quote",
        NotifType::Mention => "notifications.row_mention",
        NotifType::Follow => "notifications.row_follow",
        _ => "notifications.row_interaction",
    };
    NotificationText::localized(LocalizedText::new(key).with_arg("sender", sender_display))
}

/// [`poll_bluesky_notifications`] refused to run for an actor D7 says the
/// consume-side poller must never run for (`atproto-pds-full.md` § D7).
#[derive(Debug, thiserror::Error)]
#[error("consume-side poll refused for actor {actor_hex} (D7)")]
pub(crate) struct ConsumeSidePollRefused {
    pub actor_hex: String,
}

/// A row [`ingest_notifications`] actually inserted — what the caller needs in
/// order to push it. Returned rather than pushed in place so the ingest half
/// stays free of [`AppState`] and testable against a bare [`CacheDb`].
pub(crate) struct InsertedNotification {
    pub id: i64,
    pub notif_type: NotifType,
    pub text: NotificationText,
}

/// Insert every not-yet-seen notification in `notifs` into the unified table
/// for `actor_id`, preserving the order given. Returns the rows inserted.
///
/// **Dedup.** [`CacheDb::insert_notification`] dedups on
/// `(actor_id, notif_type, sender_id, content_id)`. A bridged notification has
/// no 32-byte fauna sender key and no fauna content id, so without a token of
/// its own every Bluesky like for one actor collapses into a single row that
/// never rings again. The notification's own AT-URI is that token — the
/// `feed_request_dedup_key` pattern `crate::security_notify` uses for the same
/// reason, where a second `NewTokenIssued` must ring rather than be swallowed
/// as a duplicate of the first.
pub(crate) async fn ingest_notifications(
    db: &CacheDb,
    actor_id: &[u8],
    notifs: &[BlueskyNotification],
) -> anyhow::Result<Vec<InsertedNotification>> {
    let mut inserted = Vec::new();

    for translated in notifs {
        let notif_type = notif_type_of(&translated.reason);

        // Use the handle as the summary sender identifier
        let sender_display = translated
            .author_display_name
            .as_deref()
            .unwrap_or(&translated.author_handle);
        let text = text_of(&notif_type, sender_display);

        let created_at = parse_timestamp_micros(&translated.indexed_at);

        if let Ok(Some(notif_id)) = db
            .insert_notification(
                actor_id,
                &notif_type,
                "bluesky",
                None, // sender_id is a DID string, not a 32-byte key
                Some(translated.uri.as_bytes()),
                translated.subject_uri.as_deref(),
                &text,
                created_at,
            )
            .await
        {
            inserted.push(InsertedNotification {
                id: notif_id,
                notif_type,
                text,
            });
        }
    }

    Ok(inserted)
}

/// Poll Bluesky notifications for a single actor and insert new ones into
/// the unified notifications table. Returns the number of new notifications inserted.
pub async fn poll_bluesky_notifications(
    state: &Arc<AppState>,
    actor_hex: &str,
) -> anyhow::Result<u64> {
    // D7, re-checked where the consume-side path starts rather than trusted
    // from the caller. `get_agent_for_actor` restores the OAuth session by DID
    // alone and never sees the backing, so for a caller that did not come
    // through `list_consume_side_linked_actors` this is the only check — and it
    // closes the window a pass over many accounts holds open between the
    // enumeration and this poll, in which a hosted identity can go active.
    {
        let conn = state.db.conn().await;
        if !db_helpers::consume_side_poll_allowed(&conn, actor_hex)? {
            return Err(ConsumeSidePollRefused {
                actor_hex: actor_hex.to_string(),
            }
            .into());
        }
    }

    let agent = db_helpers::get_agent_for_actor(state, actor_hex)
        .await
        .map_err(|_| anyhow::anyhow!("no bluesky agent for actor {actor_hex}"))?;

    let actor_id = hex::decode(actor_hex)?;

    let params = list_notifications::ParametersData {
        cursor: None,
        limit: Some(50u8.try_into().unwrap()),
        priority: None,
        reasons: None,
        seen_at: None,
    };

    let output = agent
        .api
        .app
        .bsky
        .notification
        .list_notifications(params.into())
        .await
        .map_err(|e| anyhow::anyhow!("Bluesky listNotifications error: {e}"))?;

    let translated: Vec<BlueskyNotification> = output
        .notifications
        .iter()
        .map(translate_notification)
        .collect();

    let inserted = ingest_notifications(&state.db, &actor_id, &translated).await?;

    // Push via WebSocket
    if let Ok(actor_arr) = <[u8; 32]>::try_from(actor_id.as_slice()) {
        for row in &inserted {
            state.ws.notify_push(
                &actor_arr,
                fauna_protocol::PushEvent::Notification(
                    fauna_protocol::push_events::NotificationPayload {
                        notification_id: row.id,
                        notif_type: row.notif_type.clone(),
                        source: "bluesky".into(),
                        sender_id: None,
                        content_id: None,
                        summary: row.text.summary().to_string(),
                        body: row.text.body().cloned(),
                        timestamp: fauna_core::data::Timestamp::now_secs() as u64,
                        extra: std::collections::BTreeMap::new(),
                    },
                ),
            );
        }
    }

    Ok(inserted.len() as u64)
}

#[cfg(test)]
mod tests {
    /// Every `notif_type` the poller can mint — the catch-all included — names a
    /// catalog sentence and fills it, and the row's English fallback is that
    /// sentence, not a second copy of it.
    #[test]
    fn every_bridged_notif_type_is_a_complete_catalog_sentence() {
        for reason in [
            "like",
            "reply",
            "repost",
            "quote",
            "mention",
            "follow",
            "starterpack",
        ] {
            let text = super::text_of(&super::notif_type_of(reason), "alice");
            let body = text.body().expect("a bridged row is always keyed");
            crate::db::notifications::assert_body_is_catalog_complete(body);
            assert_eq!(body.args.get("sender").map(String::as_str), Some("alice"));
            assert!(text.summary().starts_with("alice "), "{:?}", text.summary());
        }
    }

    use super::*;

    /// One notification as `translate_notification` would hand it over. `uri`
    /// is the notification's own record URI — the field that distinguishes two
    /// likes by the same person from one another.
    fn notif(reason: &str, uri: &str, subject: &str) -> BlueskyNotification {
        BlueskyNotification {
            uri: uri.to_string(),
            reason: reason.to_string(),
            author_did: "did:plc:alice".to_string(),
            author_handle: "alice.bsky.social".to_string(),
            author_display_name: None,
            author_avatar: None,
            subject_uri: Some(subject.to_string()),
            record_text: None,
            indexed_at: "2026-09-20T10:00:00.000Z".to_string(),
            is_read: false,
        }
    }

    /// The thing the feature turns on: two *distinct* Bluesky likes must both
    /// reach the unified list. `insert_notification` dedups on
    /// (actor, type, sender, content), so a bridged row carrying no token of
    /// its own makes the second like — and every like after it, forever — a
    /// silent duplicate of the first.
    #[tokio::test]
    async fn two_distinct_notifications_both_land() {
        let db = CacheDb::open_in_memory().unwrap();
        let actor = [7u8; 32];

        let notifs = vec![
            notif(
                "like",
                "at://did:plc:alice/app.bsky.feed.like/1",
                "at://post/a",
            ),
            notif(
                "like",
                "at://did:plc:alice/app.bsky.feed.like/2",
                "at://post/b",
            ),
        ];

        let inserted = ingest_notifications(&db, &actor, &notifs).await.unwrap();
        assert_eq!(
            inserted.len(),
            2,
            "two distinct likes must both land — one row means the dedup key \
             swallowed the second"
        );

        let rows = db.list_notifications(&actor, None, 10).await.unwrap();
        assert_eq!(rows.len(), 2);
        assert!(rows.iter().all(|r| r.source == "bluesky"));
    }

    /// The other half: re-polling the same window inserts nothing. Bluesky's
    /// `listNotifications` returns a sliding window, so every tick re-reads
    /// notifications the previous one already ingested.
    #[tokio::test]
    async fn a_re_poll_of_the_same_window_inserts_nothing() {
        let db = CacheDb::open_in_memory().unwrap();
        let actor = [7u8; 32];

        let notifs = vec![
            notif(
                "like",
                "at://did:plc:alice/app.bsky.feed.like/1",
                "at://post/a",
            ),
            notif(
                "follow",
                "at://did:plc:alice/app.bsky.graph.follow/1",
                "at://alice",
            ),
        ];

        assert_eq!(
            ingest_notifications(&db, &actor, &notifs)
                .await
                .unwrap()
                .len(),
            2
        );
        assert_eq!(
            ingest_notifications(&db, &actor, &notifs)
                .await
                .unwrap()
                .len(),
            0,
            "a second poll of the same window must add nothing"
        );
        assert_eq!(
            db.list_notifications(&actor, None, 10).await.unwrap().len(),
            2
        );
    }
}
