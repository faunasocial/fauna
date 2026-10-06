//! Test-only HTTP endpoints for driving WS-RPC push events, plus the read-only
//! push dispatch counters (`GET /api/v1/test/push/dispatches`).
//!
//! Gated on the `test-hooks` Cargo feature so this module — and the routes
//! it registers — never compile into the production binary. The e2e build
//! (`cargo build -p fauna-nest --features test-hooks`) wires them under
//! `/api/v1/test/push/*`.
//!
//! Consumers: `tests/e2e-unified/tests/test_sp_linux_ws_rpc_push.py` and
//! its sibling per-app variants. Production push emission happens via
//! the real feature handlers in `routes.rs`, `channel_routes.rs`, etc.

#![cfg(feature = "test-hooks")]

use std::sync::Arc;

use axum::extract::State;
use axum::response::{IntoResponse, Json};
use axum::routing::{get, post};
use serde::Deserialize;
use serde_json::json;

use crate::api_error::ApiError;
use crate::routes::AppState;

#[derive(Deserialize)]
pub struct NotifyBody {
    pub actor_id: String,
    #[serde(default)]
    pub summary: Option<String>,
    #[serde(default)]
    pub notif_type: Option<String>,
    /// Optional hex `content_id` — the **distinctness** knob.
    ///
    /// `insert_notification` deduplicates on `(actor_id, notif_type, sender_id,
    /// content_id)` and returns `None` for a duplicate, which this handler surfaces as
    /// a 500. With all three of `notif_type`/`sender_id`/`content_id` fixed, an actor
    /// can only ever receive **one** test notification: the second call is a no-op and
    /// the hook 500s. That bites any suite whose `nest_instance`/`test_user` are
    /// session-scoped (they are) and which fires more than one push at the same actor —
    /// e.g. a client-parametrized test that runs once per client, or two push tests in
    /// one run. Pass a fresh `content_id` per call to make each notification genuinely
    /// distinct. Omit it to keep the legacy single-notification behaviour.
    #[serde(default)]
    pub content_id: Option<String>,
    /// Optional unified `source` — the origin protocol
    /// (`fauna`/`bluesky`/`nostr`/`activitypub`, `notifications.md` § the
    /// `NotifItem` shape). Defaults to `test-hooks`.
    ///
    /// Its one purpose is letting a suite seed a **bridged** row and witness
    /// that the unified list renders it and carries its source — the render
    /// leg of `bridges.md` § Bluesky bridge → *Notifications*. It seeds a row
    /// that looks like the bridge's output; it does **not** witness the bridge
    /// (that is `bluesky::notif_sync`'s and `notif_worker`'s own tests, which
    /// run the real ingest and the real D7 enumeration gate).
    #[serde(default)]
    pub source: Option<String>,
    /// Optional localized body — the catalog key, with `body_args` as its data
    /// (`notifications.md` § Localized body). Lets a suite seed a row whose
    /// `body` and `summary` deliberately differ, which is the only way to
    /// witness *which one an app painted*: a key the app has must win over the
    /// summary, and a key it does not have must lose to it. Omitted → the row
    /// has no body, exercising the summary fallback a key the app lacks also takes.
    #[serde(default)]
    pub body_key: Option<String>,
    #[serde(default)]
    pub body_args: std::collections::BTreeMap<String, String>,
}

/// `POST /api/v1/test/push/notify` — insert a unified notification row
/// for `actor_id` and fire a single `PushEvent::Notification` push at the
/// actor's currently-connected WS-RPC clients. Mirrors the post-knock
/// emission in `routes.rs::store_knock` (without the contact request).
///
/// Returns `{"ok": true, "notification_id": N}` on success.
async fn handle_notify(
    State(state): State<Arc<AppState>>,
    Json(body): Json<NotifyBody>,
) -> impl IntoResponse {
    let actor_id_bytes = match fauna_core::hex32::decode(&body.actor_id) {
        Ok(arr) => arr,
        Err(_) => return ApiError::bad_request("invalid actor_id hex").into_response(),
    };
    let summary = body
        .summary
        .unwrap_or_else(|| "test-hooks push notification".to_string());
    let notif_type = fauna_protocol::notifications::NotifType::from(
        body.notif_type.unwrap_or_else(|| "test".to_string()),
    );
    let text = crate::db::notifications::NotificationText::from_test_hook(
        body.body_key.map(|key| {
            let mut localized = fauna_protocol::LocalizedText::new(key);
            localized.args = body.body_args;
            localized
        }),
        summary,
    );

    let now_us = fauna_core::data::Timestamp::now().as_i64();
    // Decode the optional distinctness knob. Invalid hex is a caller bug, not a
    // silent no-op — surface it rather than dedup-500 later and look like the push
    // path is broken.
    let content_id_bytes = match body.content_id.as_deref() {
        Some(hex_str) => match hex::decode(hex_str) {
            Ok(bytes) => Some(bytes),
            Err(_) => return ApiError::bad_request("invalid content_id hex").into_response(),
        },
        None => None,
    };

    let source = body.source.unwrap_or_else(|| "test-hooks".to_string());

    let notif_id = match state
        .db
        .insert_notification(
            &actor_id_bytes,
            &notif_type,
            &source,
            None,
            content_id_bytes.as_deref(),
            None,
            &text,
            now_us,
        )
        .await
    {
        Ok(Some(id)) => id,
        Ok(None) => {
            return ApiError::internal(
                "notification not inserted (duplicate of an existing \
                 (actor_id, notif_type, sender_id, content_id) row — pass a fresh \
                 `content_id` to fire a distinct notification at the same actor)",
            )
            .into_response();
        }
        Err(e) => return ApiError::internal(format!("insert_notification: {e}")).into_response(),
    };

    state.ws.notify_push(
        &actor_id_bytes,
        fauna_protocol::PushEvent::Notification(fauna_protocol::push_events::NotificationPayload {
            notification_id: notif_id,
            notif_type,
            source,
            sender_id: None,
            // The wire carries the hex form (`ContentIdHex = String`); the DB row holds
            // the decoded bytes. Same value, two representations — mirror the real
            // producers (`routes.rs::store_knock`).
            content_id: body.content_id.clone(),
            summary: text.summary().to_string(),
            body: text.body().cloned(),
            timestamp: fauna_core::data::Timestamp::now_secs() as u64,
            extra: std::collections::BTreeMap::new(),
        }),
    );

    Json(json!({ "ok": true, "notification_id": notif_id })).into_response()
}

#[derive(Deserialize)]
pub struct KnockBody {
    pub actor_id: String,
    /// Optional hex-64 sender actor id — the **distinctness** knob, same role as
    /// `NotifyBody::content_id`. `push_knock` is a plain INSERT (no dedup), so a
    /// repeat is harmless, but a session-scoped, client-parametrized test that fires
    /// once per `--client` wants a fresh sender per call to keep each knock row
    /// distinct and the sender text meaningful. Omit for a fixed placeholder sender.
    #[serde(default)]
    pub sender_id: Option<String>,
    #[serde(default)]
    pub summary: Option<String>,
}

/// `POST /api/v1/test/push/knock` — store a real `knocks` row for `actor_id` and
/// fire a single `PushEvent::Knock` push at the actor's currently-connected WS-RPC
/// clients. The dedicated-knock-seam twin of [`handle_notify`]: it drives the exact
/// production path `routes.rs::store_knock` uses (`db.push_knock` + `ws.notify_push`
/// of the `fauna.knock` broker kind), so the client sees a real stored knock over a
/// real socket — only the *trigger* is a test endpoint instead of a second actor's
/// `fauna.inbox.send`. Consumer: `test_knock_live_refresh.py` (the mounted-contacts
/// live-refresh check, the knock counterpart of `test_push_live_refresh.py`).
///
/// Returns `{"ok": true, "knock_id": N}` on success.
async fn handle_knock(
    State(state): State<Arc<AppState>>,
    Json(body): Json<KnockBody>,
) -> impl IntoResponse {
    let actor_id_bytes = match fauna_core::hex32::decode(&body.actor_id) {
        Ok(arr) => arr,
        Err(_) => return ApiError::bad_request("invalid actor_id hex").into_response(),
    };
    // A fixed, obviously-synthetic placeholder sender when the caller doesn't supply
    // one (0xAB repeated); a caller wanting distinct rows passes a fresh hex-64.
    let sender_id_bytes = match body.sender_id.as_deref() {
        Some(hex_str) => match fauna_core::hex32::decode(hex_str) {
            Ok(arr) => arr,
            Err(_) => return ApiError::bad_request("invalid sender_id hex").into_response(),
        },
        None => [0xABu8; 32],
    };
    let summary = body
        .summary
        .unwrap_or_else(|| "test-hooks knock".to_string());

    // Store a real knock row via the production db method. `sender_node` is a
    // synthetic placeholder (only display/accept read it; the row lists + counts
    // regardless), and a bare (empty) held payload is a valid knock —
    // `store_knock` itself stores `&[]` when the arrival exceeds the held cap.
    let knock_id = match state
        .db
        .push_knock(
            &actor_id_bytes,
            &sender_id_bytes,
            b"https://test-hooks.invalid",
            &summary,
            &[],
        )
        .await
    {
        Ok(id) => id,
        Err(e) => return ApiError::internal(format!("push_knock: {e}")).into_response(),
    };

    state.ws.notify_push(
        &actor_id_bytes,
        fauna_protocol::PushEvent::Knock(fauna_protocol::push_events::KnockPayload {
            sender_id: hex::encode(sender_id_bytes),
            body: Some(crate::routes::knock_body(&sender_id_bytes, &summary)),
            summary,
            ..Default::default()
        }),
    );

    Json(json!({ "ok": true, "knock_id": knock_id })).into_response()
}

/// `GET /api/v1/test/push/dispatches` — snapshot the push dispatch counters:
/// `{"initiated", "settled", "dialled", "delivered"}`, each `null` for a nest
/// with no push service. Why they are the causal anchors for the per-device
/// asserts: [`crate::push::PushService::dispatch_counters`]. Consumer:
/// `tests/e2e-unified/tests/api/test_push_dispatch.py`.
async fn handle_dispatches(State(state): State<Arc<AppState>>) -> impl IntoResponse {
    let counters = state
        .push_service
        .as_ref()
        .map(|push| push.dispatch_counters());
    Json(json!({
        "initiated": counters.map(|c| c.initiated),
        "settled": counters.map(|c| c.settled),
        "dialled": counters.map(|c| c.dialled),
        "delivered": counters.map(|c| c.delivered),
    }))
    .into_response()
}

#[derive(Deserialize)]
pub struct SubscriptionsQuery {
    pub actor_id: String,
}

/// `GET /api/v1/test/push/subscriptions?actor_id=<hex>` — the actor's push
/// rows as `[{device_id, transport}]`: the persisted truth an app's Settings
/// push control is witnessed against (`notifications.md` outcome 9 — off stays
/// off, the row follows whoever is signed in). Endpoints and keys are left
/// out: a witness asks which devices are registered, never where they dial.
/// Consumer: `tests/e2e-unified/tests/test_push_settings.py`.
async fn handle_subscriptions(
    State(state): State<Arc<AppState>>,
    axum::extract::Query(q): axum::extract::Query<SubscriptionsQuery>,
) -> impl IntoResponse {
    let actor_id = match fauna_core::hex32::decode(&q.actor_id) {
        Ok(arr) => arr,
        Err(_) => return ApiError::bad_request("invalid actor_id hex").into_response(),
    };
    match state.db.list_push_subscriptions(&actor_id).await {
        Ok(rows) => Json(json!(
            rows.into_iter()
                .map(|r| json!({"device_id": r.device_id, "transport": r.transport}))
                .collect::<Vec<_>>()
        ))
        .into_response(),
        Err(e) => ApiError::internal(format!("list push subscriptions: {e}")).into_response(),
    }
}

/// Mount the `/api/v1/test/push/*` routes.
pub fn routes() -> axum::Router<Arc<AppState>> {
    axum::Router::new()
        .route("/api/v1/test/push/subscriptions", get(handle_subscriptions))
        .route("/api/v1/test/push/dispatches", get(handle_dispatches))
        .route("/api/v1/test/push/notify", post(handle_notify))
        .route("/api/v1/test/push/knock", post(handle_knock))
}
