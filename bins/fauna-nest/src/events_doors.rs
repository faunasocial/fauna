//! The third-party **events doors** (`transport.md` § Push events →
//! *Third-party event doors*): "something changed" for a principal, without
//! polling its feeds blind.
//!
//! One vocabulary, two doors, one filter:
//!
//! - **The frame** is the account plane's own scope-tagged nudge,
//!   `fauna.sync.changed`, reduced to the scope that moved
//!   ([`SyncChangedPayload::scope_nudge`]) — never the folder, never content.
//!   No `PushEvent` variant is added.
//! - **The filter** is [`fauna_scope::event_reaches`]: the session holds
//!   `fauna:events:subscribe` and the scope is one its `records` arm lets it
//!   list. It is applied to the live reach ([`resolve_principal`]: the row's
//!   granted scopes ∩ the token's, the account's authority and its
//!   external-apps switch), read at the moment of delivery.
//! - **WS-RPC push** — [`on_scope_changed`], called wherever a scope-tagged
//!   nudge fires for the account's own scopes, pushes the frame to every
//!   principal session the filter admits.
//! - **The poll** — `fauna.events.poll`, the arm's ceiling kind: which
//!   reachable scopes moved past a cursor. The cursor is the nest-log `seq`,
//!   the coordinate `fauna.sync.changes.list`'s `since` walks, so it is
//!   durable across restarts and a dropped push is caught by the next poll —
//!   the plane's nudge-plus-walk rule.
//! - **HTTP** — `GET /api/v1/events?cursor=&wait=`, the poll as a long-poll for
//!   a remote server that cannot hold a WS-RPC session: answered at once when
//!   anything moved, else held until a change wakes it or `wait` runs out.
//! - **The webhook** — [`crate::events_webhook`]: a remote server whose
//!   document declares `events_uri` is POSTed a signed, payload-free
//!   notification and comes to poll; the third door, same filter, same
//!   cursor.

use std::sync::Arc;
use std::time::Duration;

use axum::extract::{Query, State};
use axum::http::{HeaderMap, StatusCode};
use axum::response::{IntoResponse, Response};
use bytes::Bytes;

use fauna_bridge_atproto::fauna_scope;
use fauna_protocol::push_events::{
    EventsPollReply, EventsPollRequest, KIND_EVENTS_POLL, SyncChangedPayload,
};
use fauna_protocol::{PushEvent, RpcError, decode_strict as decode};

use crate::principal_handlers::{PrincipalCaller, PrincipalHandler, resolve_principal};
use crate::routes::AppState;
use crate::rpc_errors::{encode_reply, internal, malformed};
use crate::rpc_router::{RpcHandler, RpcKindMeta, RpcRouterBuilder};

/// The most scopes one poll names. A capped page loses nothing: the cursor
/// stops at the last head returned, and every scope left out lies above it.
const POLL_PAGE_SCOPES: u32 = 256;

/// The longest the HTTP door holds a poll open — under the common 30 s idle
/// timeout of the proxies a remote server's request crosses.
pub const LONG_POLL_MAX_WAIT: Duration = Duration::from_secs(25);

/// A scope-tagged change landed on `account`'s scope `scope`: push the
/// principal frame to every session whose live reach admits it, wake the
/// account's HTTP long-polls, and notify its webhooks off this path.
/// Best-effort, like every nudge — the poll's cursor is the correctness
/// backstop.
pub(crate) async fn on_scope_changed(state: &AppState, account: &[u8; 32], scope: &str) {
    for conn in state.ws.account_principal_sessions(account) {
        let Some(binding) = conn.principal.as_ref() else {
            continue;
        };
        // The token's scopes bound the live reach from above: a session
        // whose token never subscribed costs no row read.
        if !fauna_scope::event_reaches(&binding.token_scopes, scope) {
            continue;
        }
        match resolve_principal(state, binding).await {
            Ok(Some(caller)) if fauna_scope::event_reaches(&caller.scopes, scope) => {
                crate::ws::WsState::push_to_connection(
                    &conn,
                    &PushEvent::SyncChanged(SyncChangedPayload::scope_nudge(scope)),
                );
            }
            Ok(_) => {}
            Err(e) => tracing::warn!(
                error = %e,
                "events door: principal resolve failed; skipping its frame"
            ),
        }
    }
    state.ws.wake_event_waiters(account);
    crate::events_webhook::notify(state, *account, scope.to_string());
}

/// Answer one poll for a resolved caller.
async fn poll(
    state: &AppState,
    caller: &PrincipalCaller,
    req: &EventsPollRequest,
) -> Result<EventsPollReply, RpcError> {
    let cursor = req.cursor.unwrap_or(0).max(0);
    let heads = state
        .db
        .ext_scope_heads_since(&caller.account, cursor, POLL_PAGE_SCOPES)
        .await
        .map_err(internal)?;
    // The page's coverage includes the scopes the filter drops: the cursor
    // only says how far the nest log has been looked at.
    let covered = heads.last().map_or(cursor, |(_, head)| *head);
    let frames = heads
        .iter()
        .filter(|(scope, _)| fauna_scope::event_reaches(&caller.scopes, scope))
        .map(|(scope, _)| SyncChangedPayload::scope_nudge(scope))
        .collect();
    Ok(EventsPollReply {
        frames,
        cursor: covered,
        extra: Default::default(),
    })
}

/// `fauna.events.poll` for a third-party principal — the WS-RPC face of the
/// poll, answered at once.
pub(crate) fn principal_events_poll_handler() -> PrincipalHandler {
    Box::new(|state, caller, payload| {
        Box::pin(async move {
            let req: EventsPollRequest = decode(&payload).map_err(malformed)?;
            encode_reply(&poll(&state, &caller, &req).await?)
        })
    })
}

/// The actor-side half of `fauna.events.poll`: the kind's wire contract
/// lives in the actor router (one kind, one wire contract), but NO actor
/// class holds it — the account's own apps hear the unfiltered nudges on
/// their own sockets — so the central gate refuses it, always (the
/// `fauna.folders.deposit` precedent).
fn events_poll_actor_handler() -> RpcHandler {
    Box::new(|state, actor_id, _payload| {
        Box::pin(async move {
            crate::bridge_method_allowlist::require_permission_default(
                &state,
                &actor_id,
                KIND_EVENTS_POLL,
            )
            .await?;
            // Unreachable while the matrix arm admits no actor class.
            Err(crate::rpc_errors::central_permission_denied())
        })
    })
}

/// Register the poll kind on the actor router — a read, answered at once.
pub fn register_events_handlers(b: &mut RpcRouterBuilder) {
    b.add(
        KIND_EVENTS_POLL,
        RpcKindMeta {
            forbid_replay: false,
            default_deadline: Duration::from_secs(5),
            handler: events_poll_actor_handler(),
        },
    );
}

/// The HTTP door's query.
#[derive(serde::Deserialize)]
pub struct EventsQuery {
    /// The previous answer's `cursor`; absent = from the start.
    #[serde(default)]
    cursor: Option<i64>,
    /// How long to hold the poll open when nothing has moved, in seconds;
    /// absent = [`LONG_POLL_MAX_WAIT`], `0` = answer at once. Capped there.
    #[serde(default)]
    wait: Option<u64>,
}

/// `GET /api/v1/events` — a remote principal's long-poll. The token rides
/// `Authorization: DPoP <token>` with its `DPoP` proof (RFC 9449; `htm` GET,
/// `htu` this door). Answers `200 {"frames": [{"scope": …}], "cursor": N}`
/// as soon as a reachable scope has moved past `cursor`, or with no frames
/// when the wait runs out; the next poll sends `cursor` back.
pub async fn events_http_handler(
    State(state): State<Arc<AppState>>,
    Query(query): Query<EventsQuery>,
    headers: HeaderMap,
    crate::registration::OptionalConnectInfo(peer_addr): crate::registration::OptionalConnectInfo,
) -> Response {
    let htu = fauna_bridge_atproto::oauth_metadata::events_htu(&state.web_serving_domain());
    let presented = crate::principal_session::parse_http_presentation(&headers);
    let admission = match crate::principal_session::admit_presentation(
        &state,
        presented.as_ref(),
        "GET",
        &htu,
        peer_addr.map(|a| a.ip()),
    )
    .await
    {
        Ok(admission) => admission,
        Err(response) => return response,
    };
    let wait = query
        .wait
        .map_or(LONG_POLL_MAX_WAIT, Duration::from_secs)
        .min(LONG_POLL_MAX_WAIT);
    let deadline = tokio::time::Instant::now() + wait;
    let waiter = state.ws.event_waiter(&admission.binding.account);
    let mut cursor = query.cursor.unwrap_or(0).max(0);
    loop {
        // Enabled before the read: a change landing between the read and the
        // wait still wakes this poll.
        let notified = waiter.notified();
        tokio::pin!(notified);
        notified.as_mut().enable();

        let reply = match dispatch_poll(&state, &admission.binding, cursor).await {
            Ok(reply) => reply,
            Err(err) => return crate::principal_session::rpc_error_response(&err),
        };
        let advanced = reply.cursor > cursor;
        cursor = reply.cursor;
        if !reply.frames.is_empty() {
            return render(&reply);
        }
        // A page the filter emptied still moved the cursor: read on at once.
        if advanced {
            continue;
        }
        if tokio::time::timeout_at(deadline, notified).await.is_err() {
            return render(&reply);
        }
    }
}

/// One poll through the principal gate — the ceiling, the arm, the live row.
async fn dispatch_poll(
    state: &Arc<AppState>,
    binding: &crate::principal_handlers::PrincipalBinding,
    cursor: i64,
) -> Result<EventsPollReply, RpcError> {
    let payload = fauna_protocol::encode_canonical(&EventsPollRequest {
        cursor: Some(cursor),
        extra: Default::default(),
    })
    .map_err(internal)?;
    let reply = crate::principal_handlers::dispatch_principal(
        Arc::clone(state),
        binding,
        KIND_EVENTS_POLL,
        Bytes::from(payload.to_vec()),
    )
    .await?;
    decode(&reply).map_err(internal)
}

/// The HTTP door's JSON: each frame as the one thing it carries, its scope.
fn render(reply: &EventsPollReply) -> Response {
    let frames: Vec<_> = reply
        .frames
        .iter()
        .filter_map(|f| f.scope.as_deref())
        .map(|scope| serde_json::json!({ "scope": scope }))
        .collect();
    (
        StatusCode::OK,
        axum::Json(serde_json::json!({ "frames": frames, "cursor": reply.cursor })),
    )
        .into_response()
}

#[cfg(test)]
mod tests;
