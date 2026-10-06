//! **The HTTP record door** — a remote principal's own `ext.*` records over
//! plain HTTPS (`docs/goal/architecture/third-party-kinds.md` § Kind
//! namespacing → *Two doors onto the same plane*, § The record doors; HTTP
//! residue per `api-layers.md`, because a remote server will not speak
//! DAG-CBOR WS-RPC).
//!
//! One plane, two doors: every request here is admitted exactly as the
//! principal session's upgrade is (`principal_session::admit_presentation` —
//! the DPoP-bound token, its proof, a built Fauna-family scope, the
//! failed-credential throttle), resolved once (`resolve_principal`: a revoked
//! row, an account without authority or external apps switched OFF refuse
//! here), and then dispatched onto the same two principal handlers the
//! session reaches — `fauna.account.state.put` and `fauna.sync.changes.list`
//! (`principal_handlers::dispatch_resolved`: the `ThirdParty` ceiling, the
//! `records` scope, the handler's per-kind reach check and writer check). So
//! there is no second check path and no second storage path, and a row put
//! here is the row a device app lists over the session.
//!
//! The surface:
//!
//! - `PUT /api/v1/records/{kind}/{key}?writer_seq=N` — body = the sealed
//!   entry the principal sealed as its writer under `ext:<kind>`; `{key}` is
//!   the row's 32-byte blinded `item_key` in hex (the nest never holds a
//!   logical key), `writer_seq` the sequence the seal binds.
//! - `DELETE /api/v1/records/{kind}/{key}?writer_seq=N` — the same, the body a
//!   principal-sealed tombstone entry (a deletion is an ordinary sealed entry,
//!   never a bare cleartext row; `fauna.account.state.retire` stays the
//!   owner's).
//! - `GET /api/v1/records/{kind}?cursor=N` — the cursor walk: one page of the
//!   kind's rows past `cursor`, and the cursor to ask for next.
//! - `GET /api/v1/records/{kind}/{key}` — the walk filtered to one item.
//!
//! The nest validates only the floor — the kind string (the handler's
//! `ext:<kind>` parse: a `fauna.*` kind is reachable by no name), the size
//! (the body limit at the route, then the put's own ceiling), and a gen-0 (v1)
//! envelope shape — and stores the opaque bytes. Every admitted request counts
//! against the principal's [`crate::bridge_rate_limit::RECORDS_DOOR_LIMITER_CONFIG`].

use std::sync::Arc;

use axum::body::Bytes as BodyBytes;
use axum::extract::{Path, Query, State};
use axum::http::{HeaderMap, HeaderValue, StatusCode, header::RETRY_AFTER};
use axum::response::{IntoResponse, Response};
use base64::Engine as _;
use bytes::Bytes;
use serde_bytes::ByteBuf;

use fauna_protocol::account_state::{
    AccountStatePutReply, AccountStatePutRequest, ItemClass, KIND_STATE_PUT, OP_STATE_PUT,
    OP_TOMBSTONE,
};
use fauna_protocol::sync::{SyncChangesListReply, SyncChangesListRequest};

use crate::principal_handlers::PrincipalCaller;
use crate::principal_session::rpc_error_response;
use crate::routes::AppState;

/// The read kind both doors list a kind's rows through.
const KIND_CHANGES_LIST: &str = "fauna.sync.changes.list";

/// The walk's query: the cursor a previous page handed back (0 = from the
/// start).
#[derive(serde::Deserialize)]
pub struct WalkQuery {
    #[serde(default)]
    cursor: i64,
}

/// A write's query: the writer sequence the sealed entry binds.
#[derive(serde::Deserialize)]
pub struct WriteQuery {
    writer_seq: i64,
}

/// `GET /api/v1/records/{kind}` — one page of the kind's rows past `cursor`.
/// Answers `200 {"records": [...], "cursor": N}`; the walk is done when a page
/// comes back empty.
pub async fn walk_handler(
    State(state): State<Arc<AppState>>,
    Path(kind): Path<String>,
    Query(query): Query<WalkQuery>,
    headers: HeaderMap,
    crate::registration::OptionalConnectInfo(peer_addr): crate::registration::OptionalConnectInfo,
) -> Response {
    let htu =
        fauna_bridge_atproto::oauth_metadata::records_htu(&state.web_serving_domain(), &kind, None);
    let caller = match admit(&state, &headers, "GET", &htu, peer_addr).await {
        Ok(caller) => caller,
        Err(response) => return response,
    };
    match list_page(&state, caller, &kind, query.cursor).await {
        Ok(page) => (
            StatusCode::OK,
            axum::Json(serde_json::json!({
                "records": page.changes.iter().map(record_json).collect::<Vec<_>>(),
                "cursor": next_cursor(&page, query.cursor),
            })),
        )
            .into_response(),
        Err(response) => response,
    }
}

/// `GET /api/v1/records/{kind}/{key}` — the walk filtered to one item: every
/// row the kind's feed serves at this `item_key`. `404` when there is none.
pub async fn get_handler(
    State(state): State<Arc<AppState>>,
    Path((kind, key)): Path<(String, String)>,
    headers: HeaderMap,
    crate::registration::OptionalConnectInfo(peer_addr): crate::registration::OptionalConnectInfo,
) -> Response {
    let htu = fauna_bridge_atproto::oauth_metadata::records_htu(
        &state.web_serving_domain(),
        &kind,
        Some(&key),
    );
    let caller = match admit(&state, &headers, "GET", &htu, peer_addr).await {
        Ok(caller) => caller,
        Err(response) => return response,
    };
    let Some(item_key) = parse_item_key(&key) else {
        return bad_key();
    };
    let wanted = hex::encode(item_key);
    let mut found = Vec::new();
    let mut cursor = 0;
    loop {
        let page = match list_page(&state, caller.clone(), &kind, cursor).await {
            Ok(page) => page,
            Err(response) => return response,
        };
        found.extend(
            page.changes
                .iter()
                .filter(|c| c.path_hash == wanted)
                .map(record_json),
        );
        let next = next_cursor(&page, cursor);
        // A page that served nothing, or a cursor that did not move, is the
        // end of the feed.
        if page.changes.is_empty() || next <= cursor {
            break;
        }
        cursor = next;
    }
    if found.is_empty() {
        return (
            StatusCode::NOT_FOUND,
            axum::Json(serde_json::json!({ "error": "not_found" })),
        )
            .into_response();
    }
    (
        StatusCode::OK,
        axum::Json(serde_json::json!({ "records": found })),
    )
        .into_response()
}

/// `PUT /api/v1/records/{kind}/{key}?writer_seq=N` — put one sealed row.
/// Answers `200 {"seq": N}`, the nest-log seq it landed at.
pub async fn put_handler(
    state: State<Arc<AppState>>,
    path: Path<(String, String)>,
    query: Query<WriteQuery>,
    headers: HeaderMap,
    peer: crate::registration::OptionalConnectInfo,
    body: BodyBytes,
) -> Response {
    write(state, path, query, headers, peer, body, "PUT", OP_STATE_PUT).await
}

/// `DELETE /api/v1/records/{kind}/{key}?writer_seq=N` — put the principal's
/// sealed tombstone for one item. Answers as the put does.
pub async fn delete_handler(
    state: State<Arc<AppState>>,
    path: Path<(String, String)>,
    query: Query<WriteQuery>,
    headers: HeaderMap,
    peer: crate::registration::OptionalConnectInfo,
    body: BodyBytes,
) -> Response {
    write(
        state,
        path,
        query,
        headers,
        peer,
        body,
        "DELETE",
        OP_TOMBSTONE,
    )
    .await
}

#[allow(clippy::too_many_arguments)] // the two methods' shared extractor set
async fn write(
    State(state): State<Arc<AppState>>,
    Path((kind, key)): Path<(String, String)>,
    Query(query): Query<WriteQuery>,
    headers: HeaderMap,
    crate::registration::OptionalConnectInfo(peer_addr): crate::registration::OptionalConnectInfo,
    body: BodyBytes,
    htm: &str,
    op: &str,
) -> Response {
    let htu = fauna_bridge_atproto::oauth_metadata::records_htu(
        &state.web_serving_domain(),
        &kind,
        Some(&key),
    );
    let caller = match admit(&state, &headers, htm, &htu, peer_addr).await {
        Ok(caller) => caller,
        Err(response) => return response,
    };
    let Some(item_key) = parse_item_key(&key) else {
        return bad_key();
    };
    // The principal writes only as itself: the writer is the key it attested,
    // never one the request names. A principal that attested none sends an
    // empty id, which the handler's writer check refuses.
    let writer_id = caller.writer_ed25519.map(hex::encode).unwrap_or_default();
    let payload = match fauna_protocol::encode_canonical(&AccountStatePutRequest {
        scope: ext_scope(&kind),
        writer_id,
        writer_seq: query.writer_seq,
        item_key: ByteBuf::from(item_key.to_vec()),
        op: op.to_string(),
        entry: ByteBuf::from(body.to_vec()),
        ..Default::default()
    }) {
        Ok(p) => Bytes::from(p.to_vec()),
        Err(_) => return StatusCode::INTERNAL_SERVER_ERROR.into_response(),
    };
    match crate::principal_handlers::dispatch_resolved(state, caller, KIND_STATE_PUT, payload).await
    {
        Ok(reply) => match fauna_protocol::decode_strict::<AccountStatePutReply>(&reply) {
            Ok(reply) => (
                StatusCode::OK,
                axum::Json(serde_json::json!({ "seq": reply.seq })),
            )
                .into_response(),
            Err(_) => StatusCode::INTERNAL_SERVER_ERROR.into_response(),
        },
        Err(err) => rpc_error_response(&err),
    }
}

/// Admit the presentation, count it against the principal's budget, and
/// resolve the caller — once, for the whole request.
async fn admit(
    state: &Arc<AppState>,
    headers: &HeaderMap,
    htm: &str,
    htu: &str,
    peer_addr: Option<std::net::SocketAddr>,
) -> Result<PrincipalCaller, Response> {
    let presented = crate::principal_session::parse_http_presentation(headers);
    let admission = crate::principal_session::admit_presentation(
        state,
        presented.as_ref(),
        htm,
        htu,
        peer_addr.map(|a| a.ip()),
    )
    .await?;
    let binding = &admission.binding;
    if !state.records_door_rate_limit.check(
        &binding.account,
        &binding.account,
        &format!("records:{}", hex::encode(&binding.principal_id)),
    ) {
        return Err(too_many_requests());
    }
    match crate::principal_handlers::resolve_principal(state, binding).await {
        Ok(Some(caller)) => Ok(caller),
        Ok(None) => Err(rpc_error_response(
            &crate::rpc_errors::central_permission_denied(),
        )),
        Err(_) => Err(StatusCode::INTERNAL_SERVER_ERROR.into_response()),
    }
}

/// One page of `kind`'s class-2 feed past `cursor`, through the principal
/// list handler. The cursor rides as the serve-order watermark
/// (`held_through_seq`) as well as `since`: with no frontier named, a device
/// writer — the principal itself — is served from above it, not from 0, which
/// is what makes the walk move (`account-sync-plane.md` § Feeds and cursors →
/// *Compaction is a serve-order watermark*). The principal handler marks no
/// walker, so the walk holds back no retire.
async fn list_page(
    state: &Arc<AppState>,
    caller: PrincipalCaller,
    kind: &str,
    cursor: i64,
) -> Result<SyncChangesListReply, Response> {
    let request = SyncChangesListRequest {
        scope: Some(ext_scope(kind)),
        item_class: Some(ItemClass::StateEntry.as_wire().to_string()),
        since: cursor,
        held_through_seq: Some(cursor),
        ..Default::default()
    };
    let payload = fauna_protocol::encode_canonical(&request)
        .map(|p| Bytes::from(p.to_vec()))
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR.into_response())?;
    let reply = crate::principal_handlers::dispatch_resolved(
        state.clone(),
        caller,
        KIND_CHANGES_LIST,
        payload,
    )
    .await
    .map_err(|err| rpc_error_response(&err))?;
    fauna_protocol::decode_strict(&reply)
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR.into_response())
}

/// The cursor to walk on from: how far the page is complete
/// (`complete_through_seq`), else its last row's seq, else where it started.
fn next_cursor(page: &SyncChangesListReply, cursor: i64) -> i64 {
    page.complete_through_seq
        .or_else(|| page.changes.iter().map(|c| c.seq).max())
        .unwrap_or(cursor)
        .max(cursor)
}

/// One served row as the door renders it: the plane's cleartext coordinates
/// and the sealed entry, byte for byte (unpadded base64url).
fn record_json(change: &fauna_protocol::sync::SyncChange) -> serde_json::Value {
    serde_json::json!({
        "key": change.path_hash,
        "writer": change.origin_writer,
        "writer_seq": change.origin_seq,
        "op": change.change_type,
        "seq": change.seq,
        "entry": change.entry.as_ref().map(|e| {
            base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(e.as_ref())
        }),
    })
}

/// The scope string a kind's rows live under. The handler parses it — a
/// string that is not an `ext.*` kind (a `fauna.*` kind included) refuses
/// there, the one check path both doors share.
fn ext_scope(kind: &str) -> String {
    format!("ext:{kind}")
}

/// `{key}`: exactly 64 hex characters, the 32-byte blinded `item_key`.
fn parse_item_key(key: &str) -> Option<[u8; 32]> {
    hex::decode(key).ok()?.try_into().ok()
}

fn bad_key() -> Response {
    (
        StatusCode::BAD_REQUEST,
        axum::Json(serde_json::json!({
            "error": "invalid_request",
            "detail": "the key is the row's 32-byte blinded item_key, in hex",
        })),
    )
        .into_response()
}

/// The per-principal budget is spent: `429`, retry once the window has moved.
fn too_many_requests() -> Response {
    let window = crate::bridge_rate_limit::RECORDS_DOOR_LIMITER_CONFIG.window;
    let mut response = (
        StatusCode::TOO_MANY_REQUESTS,
        axum::Json(serde_json::json!({ "error": "rate_limited" })),
    )
        .into_response();
    response
        .headers_mut()
        .insert(RETRY_AFTER, HeaderValue::from(window.as_secs()));
    response
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_key_is_exactly_32_bytes_of_hex() {
        assert_eq!(parse_item_key(&"ab".repeat(32)), Some([0xab; 32]));
        assert_eq!(parse_item_key(&"ab".repeat(31)), None);
        assert_eq!(parse_item_key(&"ab".repeat(33)), None);
        assert_eq!(parse_item_key("notes/first"), None);
    }

    #[test]
    fn the_cursor_never_moves_backwards() {
        let mut page = SyncChangesListReply::default();
        assert_eq!(next_cursor(&page, 7), 7, "an empty page stays put");
        page.changes.push(fauna_protocol::sync::SyncChange {
            seq: 9,
            ..Default::default()
        });
        assert_eq!(next_cursor(&page, 7), 9, "else the last row");
        page.complete_through_seq = Some(12);
        assert_eq!(next_cursor(&page, 7), 12, "the watermark wins");
        page.complete_through_seq = Some(3);
        assert_eq!(next_cursor(&page, 7), 7);
    }
}
