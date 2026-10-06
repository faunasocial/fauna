//! Integration round-trip for `fauna.notifications.{list,mark_read,count}` —
//! a behavior-preserving transport migration of the HTTP routes
//! `GET /api/v1/notifications/{actor}`,
//! `POST /api/v1/notifications/{actor}/read`,
//! `GET /api/v1/notifications/{actor}/count`. The handlers reuse the same
//! `CacheDb` methods the HTTP twins call (`list_notifications` /
//! `mark_notifications_read` / `count_unread_notifications`) — these tests
//! exercise the WS-RPC layer: request decode, the reused `CacheDb` reaching
//! a real in-memory store, reply encoding, the actor scoping keyed on the
//! connection actor (replacing the HTTP path-param + bearer match), the
//! replay metadata, and the allowlist.
//!
//! Notifications are seeded directly via the public
//! `CacheDb::insert_notification` (the production write path — the same
//! method the like/reply/etc. notification-creation sites call), so the
//! `list` / `mark_read` / `count` reads have real rows to operate on.
//!
//! Authority for the wire types: `libs/fauna-protocol/src/notifications.rs`.
//! Slice: tracked internally (§ T1).
//!
//! Tier: tier_3 (real `fauna-nest` `AppState` + real `CacheDb` — no mocks).
//! Matches the posts / feed / conversations conformance harnesses, which
//! are likewise full-stack router dispatch.

mod common;
use common::dispatch;

use std::sync::Arc;

use bytes::Bytes;

use fauna_nest::{
    bridge_method_allowlist::{CallerClass, is_permitted},
    db::CacheDb,
    notifications_handlers,
    routes::AppState,
    rpc_router::RpcRouter,
};
use fauna_protocol::{
    RpcErrorAction, decode_strict as decode, encode_canonical,
    notifications::{
        CODE_NOTIFICATION_RETAINED, NotifClearReply, NotifClearRequest, NotifCountReply,
        NotifCountRequest, NotifDismissReply, NotifDismissRequest, NotifListReply,
        NotifListRequest, NotifMarkReadReply, NotifMarkReadRequest, SECURITY_NOTICE_RETENTION_SECS,
    },
};

async fn router_with_db_only() -> (RpcRouter, Arc<AppState>) {
    let db = Arc::new(CacheDb::open_in_memory().unwrap());
    let state = Arc::new(AppState::for_test(db));
    let mut b = RpcRouter::builder();
    notifications_handlers::register_notifications_handlers(&mut b);
    (b.build(), state)
}

fn list_payload(cursor: Option<i64>, limit: Option<i64>) -> Bytes {
    let req = NotifListRequest {
        cursor,
        limit,
        extra: std::collections::BTreeMap::new(),
    };
    Bytes::from(encode_canonical(&req).unwrap().to_vec())
}

fn mark_read_payload(up_to: Option<i64>) -> Bytes {
    let req = NotifMarkReadRequest {
        up_to,
        extra: std::collections::BTreeMap::new(),
    };
    Bytes::from(encode_canonical(&req).unwrap().to_vec())
}

fn count_payload() -> Bytes {
    let req = NotifCountRequest {
        extra: std::collections::BTreeMap::new(),
    };
    Bytes::from(encode_canonical(&req).unwrap().to_vec())
}

fn dismiss_payload(id: i64) -> Bytes {
    let req = NotifDismissRequest {
        id,
        extra: std::collections::BTreeMap::new(),
    };
    Bytes::from(encode_canonical(&req).unwrap().to_vec())
}

fn clear_payload(up_to: Option<i64>) -> Bytes {
    let req = NotifClearRequest {
        up_to,
        extra: std::collections::BTreeMap::new(),
    };
    Bytes::from(encode_canonical(&req).unwrap().to_vec())
}

async fn summaries(router: &RpcRouter, state: &Arc<AppState>, actor: [u8; 32]) -> Vec<String> {
    let reply: NotifListReply = decode(
        &dispatch(
            router,
            state.clone(),
            actor,
            "fauna.notifications.list",
            list_payload(None, Some(100)),
        )
        .await
        .expect("list ok"),
    )
    .unwrap();
    reply.notifications.into_iter().map(|n| n.summary).collect()
}

/// Seed a `like` notification for `actor` from `sender` on `content` at
/// `created_at` (micros). The production write path — the same method the
/// like/reply/mention notification-creation sites call.
async fn seed_notification(
    state: &Arc<AppState>,
    actor: &[u8; 32],
    sender: &[u8; 32],
    content: &[u8; 32],
    summary: &str,
    created_at: i64,
) {
    state
        .db
        .insert_notification(
            actor,
            &fauna_protocol::notifications::NotifType::Like,
            "fauna",
            Some(sender),
            Some(content),
            None,
            // Keyed like a production like; the tests below assert on the
            // `summary` they pass, which rides beside the body unchanged.
            &fauna_nest::db::notifications::NotificationText::new(
                fauna_protocol::LocalizedText::new("notifications.row_like")
                    .with_arg("sender", "alice"),
                summary,
            ),
            created_at,
        )
        .await
        .expect("insert notification")
        .expect("notification is not a dedup no-op");
}

// ── fauna.notifications.list ───────────────────────────────────

#[tokio::test]
async fn list_returns_seeded_notification() {
    let (router, state) = router_with_db_only().await;
    let actor = [11u8; 32];
    seed_notification(
        &state,
        &actor,
        &[2u8; 32],
        &[3u8; 32],
        "alice liked your post",
        1000,
    )
    .await;

    let reply: NotifListReply = decode(
        &dispatch(
            &router,
            state,
            actor,
            "fauna.notifications.list",
            list_payload(None, None),
        )
        .await
        .expect("list ok"),
    )
    .unwrap();

    assert_eq!(reply.notifications.len(), 1);
    let item = &reply.notifications[0];
    assert_eq!(
        item.notif_type,
        fauna_protocol::notifications::NotifType::Like
    );
    assert_eq!(item.source, "fauna");
    assert_eq!(item.summary, "alice liked your post");
    assert!(!item.is_read);
    assert_eq!(item.created_at, 1000);
    // sender_id / content_id ride as hex of the raw [u8; 32].
    assert_eq!(
        item.sender_id.as_deref(),
        Some(hex::encode([2u8; 32])).as_deref()
    );
    assert_eq!(
        item.content_id.as_deref(),
        Some(hex::encode([3u8; 32])).as_deref()
    );
    // The next-page cursor is the id of the last (only) row.
    assert_eq!(reply.cursor, Some(item.id));
}

#[tokio::test]
async fn list_scopes_to_connection_actor() {
    let (router, state) = router_with_db_only().await;
    let mine = [11u8; 32];
    let other = [99u8; 32];
    seed_notification(&state, &mine, &[2u8; 32], &[3u8; 32], "for me", 1000).await;

    // A different connection actor sees none of `mine`'s notifications —
    // the scoping that the HTTP twin enforced via the path param + bearer
    // match now keys on the connection actor.
    let reply: NotifListReply = decode(
        &dispatch(
            &router,
            state,
            other,
            "fauna.notifications.list",
            list_payload(None, None),
        )
        .await
        .expect("list ok"),
    )
    .unwrap();
    assert!(reply.notifications.is_empty());
    assert_eq!(reply.cursor, None);
}

#[tokio::test]
async fn list_empty_returns_no_cursor() {
    let (router, state) = router_with_db_only().await;
    let actor = [12u8; 32];
    let reply: NotifListReply = decode(
        &dispatch(
            &router,
            state,
            actor,
            "fauna.notifications.list",
            list_payload(None, None),
        )
        .await
        .expect("list ok"),
    )
    .unwrap();
    assert!(reply.notifications.is_empty());
    assert_eq!(reply.cursor, None);
}

#[tokio::test]
async fn list_paginates_with_cursor() {
    let (router, state) = router_with_db_only().await;
    let actor = [13u8; 32];
    for i in 0..5i64 {
        seed_notification(
            &state,
            &actor,
            &[(i as u8) + 10; 32],
            &[(i as u8) + 20; 32],
            &format!("notif {i}"),
            (i + 1) * 1000,
        )
        .await;
    }

    let page1: NotifListReply = decode(
        &dispatch(
            &router,
            state.clone(),
            actor,
            "fauna.notifications.list",
            list_payload(None, Some(3)),
        )
        .await
        .expect("page1 ok"),
    )
    .unwrap();
    assert_eq!(page1.notifications.len(), 3);
    let cursor = page1.cursor.expect("page1 has a cursor");

    let page2: NotifListReply = decode(
        &dispatch(
            &router,
            state,
            actor,
            "fauna.notifications.list",
            list_payload(Some(cursor), Some(3)),
        )
        .await
        .expect("page2 ok"),
    )
    .unwrap();
    assert_eq!(page2.notifications.len(), 2);
}

#[tokio::test]
async fn list_rejects_malformed_payload() {
    let (router, state) = router_with_db_only().await;
    let actor = [14u8; 32];
    let err = dispatch(
        &router,
        state,
        actor,
        "fauna.notifications.list",
        Bytes::from_static(&[0xff, 0xff, 0xff]),
    )
    .await
    .expect_err("malformed payload rejected");
    assert_eq!(err.code, "fauna.protocol.malformed");
}

// ── fauna.notifications.mark_read + count ──────────────────────

#[tokio::test]
async fn mark_read_flips_is_read_and_affects_count() {
    let (router, state) = router_with_db_only().await;
    let actor = [21u8; 32];
    seed_notification(&state, &actor, &[2u8; 32], &[3u8; 32], "notif 1", 1000).await;
    seed_notification(&state, &actor, &[4u8; 32], &[5u8; 32], "notif 2", 2000).await;

    // Two unread to start.
    let count0: NotifCountReply = decode(
        &dispatch(
            &router,
            state.clone(),
            actor,
            "fauna.notifications.count",
            count_payload(),
        )
        .await
        .expect("count ok"),
    )
    .unwrap();
    assert_eq!(count0.count, 2);

    // Mark read up to ts=1500 → only the first row.
    let marked: NotifMarkReadReply = decode(
        &dispatch(
            &router,
            state.clone(),
            actor,
            "fauna.notifications.mark_read",
            mark_read_payload(Some(1500)),
        )
        .await
        .expect("mark_read ok"),
    )
    .unwrap();
    assert_eq!(marked.marked_read, 1);

    // One unread remains.
    let count1: NotifCountReply = decode(
        &dispatch(
            &router,
            state.clone(),
            actor,
            "fauna.notifications.count",
            count_payload(),
        )
        .await
        .expect("count ok"),
    )
    .unwrap();
    assert_eq!(count1.count, 1);

    // The first row now reports is_read = true via list.
    let list: NotifListReply = decode(
        &dispatch(
            &router,
            state,
            actor,
            "fauna.notifications.list",
            list_payload(None, None),
        )
        .await
        .expect("list ok"),
    )
    .unwrap();
    let read_count = list.notifications.iter().filter(|n| n.is_read).count();
    assert_eq!(read_count, 1, "exactly one notification flipped to read");
}

#[tokio::test]
async fn mark_read_defaults_to_now_when_up_to_omitted() {
    let (router, state) = router_with_db_only().await;
    let actor = [22u8; 32];
    // created_at far in the past (micros) — a None up_to defaults to "now",
    // which is later, so this row is marked read.
    seed_notification(&state, &actor, &[2u8; 32], &[3u8; 32], "old notif", 1000).await;

    let marked: NotifMarkReadReply = decode(
        &dispatch(
            &router,
            state.clone(),
            actor,
            "fauna.notifications.mark_read",
            mark_read_payload(None),
        )
        .await
        .expect("mark_read ok"),
    )
    .unwrap();
    assert_eq!(
        marked.marked_read, 1,
        "None up_to marks everything up to now"
    );

    let count: NotifCountReply = decode(
        &dispatch(
            &router,
            state,
            actor,
            "fauna.notifications.count",
            count_payload(),
        )
        .await
        .expect("count ok"),
    )
    .unwrap();
    assert_eq!(count.count, 0);
}

#[tokio::test]
async fn count_returns_zero_for_empty_actor() {
    let (router, state) = router_with_db_only().await;
    let actor = [23u8; 32];
    let reply: NotifCountReply = decode(
        &dispatch(
            &router,
            state,
            actor,
            "fauna.notifications.count",
            count_payload(),
        )
        .await
        .expect("count ok"),
    )
    .unwrap();
    assert_eq!(reply.count, 0);
}

#[tokio::test]
async fn mark_read_rejects_malformed_payload() {
    let (router, state) = router_with_db_only().await;
    let actor = [24u8; 32];
    let err = dispatch(
        &router,
        state,
        actor,
        "fauna.notifications.mark_read",
        Bytes::from_static(&[0xff, 0xff, 0xff]),
    )
    .await
    .expect_err("malformed payload rejected");
    assert_eq!(err.code, "fauna.protocol.malformed");
}

// ── fauna.notifications.dismiss / clear ────────────────────────
//
// `behavior/notifications.md` § Retention, rule 2: nothing on the nest sweeps
// a row by age or count; the user's own dismiss and clear are the deletes.

/// Dismiss deletes exactly the named row of the connection actor — the
/// sibling rows stay, and the reply says a row went.
#[tokio::test]
async fn dismiss_deletes_exactly_the_named_row() {
    let (router, state) = router_with_db_only().await;
    let actor = [31u8; 32];
    seed_notification(&state, &actor, &[2u8; 32], &[3u8; 32], "keep me", 1000).await;
    seed_notification(&state, &actor, &[4u8; 32], &[5u8; 32], "dismiss me", 2000).await;
    let doomed = state
        .db
        .list_notifications(&actor, None, 10)
        .await
        .unwrap()
        .into_iter()
        .find(|r| r.summary == "dismiss me")
        .unwrap()
        .id;

    let reply: NotifDismissReply = decode(
        &dispatch(
            &router,
            state.clone(),
            actor,
            "fauna.notifications.dismiss",
            dismiss_payload(doomed),
        )
        .await
        .expect("dismiss ok"),
    )
    .unwrap();
    assert!(reply.dismissed);
    assert_eq!(summaries(&router, &state, actor).await, vec!["keep me"]);

    // Idempotent: the second dismiss of a gone row deletes nothing and is not
    // an error — a replay or a double-tap converges.
    let reply: NotifDismissReply = decode(
        &dispatch(
            &router,
            state.clone(),
            actor,
            "fauna.notifications.dismiss",
            dismiss_payload(doomed),
        )
        .await
        .expect("dismiss ok"),
    )
    .unwrap();
    assert!(!reply.dismissed);
    assert_eq!(summaries(&router, &state, actor).await, vec!["keep me"]);
}

/// A row id that belongs to another actor is "not found" for the caller —
/// never a delete of someone else's record.
#[tokio::test]
async fn dismiss_scopes_to_connection_actor() {
    let (router, state) = router_with_db_only().await;
    let mine = [32u8; 32];
    let other = [99u8; 32];
    seed_notification(&state, &other, &[2u8; 32], &[3u8; 32], "theirs", 1000).await;
    let theirs = state.db.list_notifications(&other, None, 10).await.unwrap()[0].id;

    let reply: NotifDismissReply = decode(
        &dispatch(
            &router,
            state.clone(),
            mine,
            "fauna.notifications.dismiss",
            dismiss_payload(theirs),
        )
        .await
        .expect("dismiss ok"),
    )
    .unwrap();
    assert!(!reply.dismissed);
    assert_eq!(summaries(&router, &state, other).await, vec!["theirs"]);
}

/// Clear deletes every row of the caller's created at or before `up_to` and
/// nothing after it — so a page that listed, then cleared, cannot delete a
/// row that arrived after it looked — and touches no other actor.
#[tokio::test]
async fn clear_deletes_the_callers_rows_up_to_and_nothing_else() {
    let (router, state) = router_with_db_only().await;
    let actor = [33u8; 32];
    let other = [98u8; 32];
    seed_notification(&state, &actor, &[2u8; 32], &[3u8; 32], "old", 1000).await;
    seed_notification(&state, &actor, &[4u8; 32], &[5u8; 32], "at the cut", 2000).await;
    seed_notification(&state, &actor, &[6u8; 32], &[7u8; 32], "newer", 3000).await;
    seed_notification(&state, &other, &[2u8; 32], &[3u8; 32], "theirs", 1000).await;

    let reply: NotifClearReply = decode(
        &dispatch(
            &router,
            state.clone(),
            actor,
            "fauna.notifications.clear",
            clear_payload(Some(2000)),
        )
        .await
        .expect("clear ok"),
    )
    .unwrap();
    assert_eq!(reply.cleared, 2);
    assert_eq!(summaries(&router, &state, actor).await, vec!["newer"]);
    assert_eq!(summaries(&router, &state, other).await, vec!["theirs"]);
}

/// `up_to` omitted ⇒ everything up to now goes (the `mark_read` default).
#[tokio::test]
async fn clear_defaults_to_now_when_up_to_omitted() {
    let (router, state) = router_with_db_only().await;
    let actor = [34u8; 32];
    let now = fauna_core::data::Timestamp::now_or_zero().as_i64();
    seed_notification(&state, &actor, &[2u8; 32], &[3u8; 32], "a", now - 2_000_000).await;
    seed_notification(&state, &actor, &[4u8; 32], &[5u8; 32], "b", now - 1_000_000).await;

    let reply: NotifClearReply = decode(
        &dispatch(
            &router,
            state.clone(),
            actor,
            "fauna.notifications.clear",
            clear_payload(None),
        )
        .await
        .expect("clear ok"),
    )
    .unwrap();
    assert_eq!(reply.cleared, 2);
    assert!(summaries(&router, &state, actor).await.is_empty());
    let count: NotifCountReply = decode(
        &dispatch(
            &router,
            state.clone(),
            actor,
            "fauna.notifications.count",
            count_payload(),
        )
        .await
        .expect("count ok"),
    )
    .unwrap();
    assert_eq!(count.count, 0);
}

// ── the security-notice window (§ Retention, rule 2's carve-out) ─────

/// Seed a `security.notice` row the way `SecurityNotifier` writes one: no
/// sender, a per-event dedup token as `content_id` (so a second notice rings
/// again rather than deduping against the first).
async fn seed_security_notice(
    state: &Arc<AppState>,
    actor: &[u8; 32],
    dedup: u8,
    summary: &str,
    created_at: i64,
) {
    state
        .db
        .insert_notification(
            actor,
            &fauna_protocol::notifications::NotifType::SecurityNotice,
            "fauna",
            None,
            Some(&[dedup; 8]),
            None,
            &fauna_nest::db::notifications::NotificationText::new(
                fauna_protocol::LocalizedText::new(
                    "notifications.row_security_pending_action_queued",
                ),
                summary,
            ),
            created_at,
        )
        .await
        .expect("insert security notice")
        .expect("security notice is not a dedup no-op");
}

fn now_micros() -> i64 {
    fauna_core::data::Timestamp::now_or_zero().as_i64()
}

/// `days` before `now`, in micros. The pins below spell the ruled 14-day
/// length as literals, never through the constant — a pin derived from the
/// constant moves with it and cannot catch a wrong window.
fn days_ago(now: i64, days: i64) -> i64 {
    now - days * 86_400 * 1_000_000
}

/// 15 days before `now` — one day past the window.
fn past_the_window(now: i64) -> i64 {
    days_ago(now, 15)
}

/// The shared constant is the ruled 14 days (§ Retention).
#[test]
fn security_notice_window_is_fourteen_days() {
    assert_eq!(SECURITY_NOTICE_RETENTION_SECS, 14 * 86_400);
}

/// A 13-day-old security notice is still inside the window: neither grain
/// removes it — the window is not shorter than ruled.
#[tokio::test]
async fn security_notice_thirteen_days_old_is_still_retained() {
    let (router, state) = router_with_db_only().await;
    let actor = [40u8; 32];
    seed_security_notice(
        &state,
        &actor,
        1,
        "recent sign-in",
        days_ago(now_micros(), 13),
    )
    .await;
    let notice = state.db.list_notifications(&actor, None, 10).await.unwrap()[0].id;

    let err = dispatch(
        &router,
        state.clone(),
        actor,
        "fauna.notifications.dismiss",
        dismiss_payload(notice),
    )
    .await
    .expect_err("a 13-day-old security notice is retained");
    assert_eq!(err.code, CODE_NOTIFICATION_RETAINED);

    let reply: NotifClearReply = decode(
        &dispatch(
            &router,
            state.clone(),
            actor,
            "fauna.notifications.clear",
            clear_payload(None),
        )
        .await
        .expect("clear ok"),
    )
    .unwrap();
    assert_eq!(reply.cleared, 0);
    assert_eq!(
        summaries(&router, &state, actor).await,
        vec!["recent sign-in"]
    );
}

/// The session a fresh security notice exposes cannot erase it: a dismiss
/// inside the window is refused with the typed `retained` code — never the
/// `{ dismissed: false }` that means "not yours" — and the row stays listed.
#[tokio::test]
async fn dismiss_refuses_a_security_notice_inside_the_window() {
    let (router, state) = router_with_db_only().await;
    let actor = [36u8; 32];
    seed_security_notice(&state, &actor, 1, "new sign-in", now_micros()).await;
    let notice = state.db.list_notifications(&actor, None, 10).await.unwrap()[0].id;

    let err = dispatch(
        &router,
        state.clone(),
        actor,
        "fauna.notifications.dismiss",
        dismiss_payload(notice),
    )
    .await
    .expect_err("a retained security notice is not dismissable");
    assert_eq!(err.code, CODE_NOTIFICATION_RETAINED);
    assert_eq!(err.action(), RpcErrorAction::Rejected);
    assert_eq!(summaries(&router, &state, actor).await, vec!["new sign-in"]);
}

/// Clear skips a retained security notice: it counts only what went and the
/// notice stays listed, while an ordinary row of the same age goes.
#[tokio::test]
async fn clear_skips_a_security_notice_inside_the_window() {
    let (router, state) = router_with_db_only().await;
    let actor = [37u8; 32];
    let now = now_micros();
    seed_security_notice(&state, &actor, 1, "new sign-in", now - 2_000_000).await;
    seed_notification(
        &state,
        &actor,
        &[2u8; 32],
        &[3u8; 32],
        "a like",
        now - 1_000_000,
    )
    .await;

    let reply: NotifClearReply = decode(
        &dispatch(
            &router,
            state.clone(),
            actor,
            "fauna.notifications.clear",
            clear_payload(None),
        )
        .await
        .expect("clear ok"),
    )
    .unwrap();
    assert_eq!(reply.cleared, 1);
    assert_eq!(summaries(&router, &state, actor).await, vec!["new sign-in"]);
}

/// After the window a security notice is the user's to remove again, by
/// either grain — the carve-out moves the timing, not the affordance.
#[tokio::test]
async fn security_notice_past_the_window_is_deletable_by_both_grains() {
    let (router, state) = router_with_db_only().await;
    let actor = [38u8; 32];
    let old = past_the_window(now_micros());
    seed_security_notice(&state, &actor, 1, "old sign-in", old).await;
    seed_security_notice(&state, &actor, 2, "older sign-in", old - 1).await;
    let first = state
        .db
        .list_notifications(&actor, None, 10)
        .await
        .unwrap()
        .into_iter()
        .find(|r| r.summary == "old sign-in")
        .unwrap()
        .id;

    let reply: NotifDismissReply = decode(
        &dispatch(
            &router,
            state.clone(),
            actor,
            "fauna.notifications.dismiss",
            dismiss_payload(first),
        )
        .await
        .expect("dismiss ok"),
    )
    .unwrap();
    assert!(reply.dismissed);

    let reply: NotifClearReply = decode(
        &dispatch(
            &router,
            state.clone(),
            actor,
            "fauna.notifications.clear",
            clear_payload(None),
        )
        .await
        .expect("clear ok"),
    )
    .unwrap();
    assert_eq!(reply.cleared, 1);
    assert!(summaries(&router, &state, actor).await.is_empty());
}

/// The retained refusal is about the caller's own row: another actor's fresh
/// security notice is still "not found" (`{ dismissed: false }`), so the code
/// never confirms that someone else's notice exists.
#[tokio::test]
async fn dismiss_of_another_actors_security_notice_is_not_found() {
    let (router, state) = router_with_db_only().await;
    let mine = [39u8; 32];
    let other = [97u8; 32];
    seed_security_notice(&state, &other, 1, "their sign-in", now_micros()).await;
    let theirs = state.db.list_notifications(&other, None, 10).await.unwrap()[0].id;

    let reply: NotifDismissReply = decode(
        &dispatch(
            &router,
            state.clone(),
            mine,
            "fauna.notifications.dismiss",
            dismiss_payload(theirs),
        )
        .await
        .expect("dismiss ok"),
    )
    .unwrap();
    assert!(!reply.dismissed);
    assert_eq!(
        summaries(&router, &state, other).await,
        vec!["their sign-in"]
    );
}

#[tokio::test]
async fn dismiss_and_clear_reject_malformed_payload() {
    let (router, state) = router_with_db_only().await;
    let actor = [35u8; 32];
    for kind in ["fauna.notifications.dismiss", "fauna.notifications.clear"] {
        let err = dispatch(
            &router,
            state.clone(),
            actor,
            kind,
            Bytes::from_static(b"\xff\xff not cbor"),
        )
        .await
        .expect_err("malformed payload is refused");
        assert_eq!(err.code, "fauna.protocol.malformed", "{kind}");
    }
}

// ── replay metadata ────────────────────────────────────────────

#[tokio::test]
async fn replay_metadata_matches_spec() {
    let (router, _state) = router_with_db_only().await;
    // All five kinds are replay-safe @5s: list/count are pure reads,
    // mark_read is an idempotent upsert (no score-increment hazard like
    // posts.interact), dismiss/clear are idempotent deletes (a replay
    // deletes nothing new).
    for kind in [
        "fauna.notifications.list",
        "fauna.notifications.mark_read",
        "fauna.notifications.count",
        "fauna.notifications.dismiss",
        "fauna.notifications.clear",
    ] {
        let m = router.kind_meta(kind).expect("kind registered");
        assert!(
            !m.forbid_replay,
            "{kind} is replay-safe (pure read / idempotent upsert)"
        );
        assert_eq!(m.default_deadline, std::time::Duration::from_secs(5));
    }
}

// ── allowlist ──────────────────────────────────────────────────

#[tokio::test]
async fn notifications_kinds_are_user_and_admin_at_allowlist_layer() {
    // `fauna.notifications.*` are User-class kinds; Admin inherits them via the
    // deliberate admin ⊇ user override (`bridge_method_allowlist::is_permitted`
    // head). Only the bridge classes are denied. (This test
    // predated the override and asserted Admin denied — stale; corrected.)
    for kind in [
        "fauna.notifications.list",
        "fauna.notifications.mark_read",
        "fauna.notifications.count",
        "fauna.notifications.dismiss",
        "fauna.notifications.clear",
    ] {
        for class in [CallerClass::User, CallerClass::Admin] {
            assert!(
                is_permitted(class, kind),
                "{kind} should be permitted for {class:?}"
            );
        }
        for class in [CallerClass::BridgeMta, CallerClass::BridgeMda] {
            assert!(
                !is_permitted(class, kind),
                "{kind} should be denied for {class:?}"
            );
        }
    }
}
