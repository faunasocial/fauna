//! Test-only HTTP endpoints for the S6 tier_3 DAV at-rest proofs on a real
//! binary: the calendar/card content-conformance walk.
//!
//! Gated on the `test-hooks` Cargo feature — never compiles into the
//! production binary (Phase-3 sealed-both-modes design, tracked internally).
//!
//! **Both backfill harnesses retired 2026-08-17 with the record-identity
//! cutover** (`message-segment-store.md` § Record identity per kind), which
//! tombstones every pre-cutover record at boot and takes the S4 module with
//! it: the MAIL half (`inject_raw_mail` + `mail_record_sealed`) exercised a
//! raw-mail reseal that now has neither corpus nor code path, and the
//! `uncutover_dav_row` hook manufactured the pre-cutover DAV row the same
//! deletion guarantees no longer exists.
//!
//! What remains is the half that was never about the transition:
//! `dav_content_conformance` walks the live records and asserts they are
//! sealed. After the S6.6 cutover a DAV body lives in its segment with the
//! SQLite column left empty, so an at-rest assertion phrased over the column
//! is vacuous — this walk reads the segment bytes instead, keyed by the
//! record's content cid.
//!
//! The placement-manifest version pair (`downgrade_placement_manifest_to_v1`,
//! `placement_manifest_version`) retired 2026-09-24 with the v1 boot heal it
//! witnessed (the compat-remnant sweep): no v1 manifest exists to heal.
//!
//! `age_dav_tombstones` moves an actor's calendar/card deletion tombstones into
//! the past, so a real binary can be shown a sync token past the retention
//! window without waiting the window out.
//!
//! Consumers: `tests/e2e-unified/tests/test_dav_content_at_rest_e2e.py`, and the
//! stale-token witnesses in `test_caldav_nest_outcomes.py` /
//! `test_carddav_nest_outcomes.py`.

#![cfg(feature = "test-hooks")]

use std::sync::Arc;

use axum::extract::State;
use axum::response::{IntoResponse, Json};
use serde::Deserialize;
use serde_json::json;

use crate::api_error::ApiError;
use crate::routes::AppState;

#[derive(Deserialize)]
pub struct DavConformanceQuery {
    pub actor_id: String,
    /// `"calendar"` or `"card"`.
    pub kind: String,
}

/// `GET /api/v1/test/content/dav_content_conformance?actor_id=<hex>&kind=<calendar|card>`
/// — the S6.12 conformance walk (S6.11's bar): walk every live record in the
/// actor's `__calendar`/`__card` segment store and assert **both** halves (body
/// and index hint) decode as a sealed envelope, via the same
/// `SealedRecordBytes::verify` the PUT handlers mint at the wire edge.
///
/// This is what makes the at-rest assertion non-vacuous: the SQLite rows carry
/// no body at all, so asserting over them would prove nothing. The bytes that
/// matter live only in the segment, and this is the only walk that reads them.
///
/// Returns `{"total": N, "sealed": N, "unsealed": ["<cid hex>", ...]}` —
/// conformant iff `total == sealed` and `unsealed` is empty.
async fn handle_dav_content_conformance(
    State(state): State<Arc<AppState>>,
    axum::extract::Query(q): axum::extract::Query<DavConformanceQuery>,
) -> impl IntoResponse {
    let actor_id = match fauna_core::hex32::decode(&q.actor_id) {
        Ok(a) => a,
        Err(_) => return ApiError::bad_request("invalid actor_id hex").into_response(),
    };
    let kind = match q.kind.as_str() {
        "calendar" => crate::segments::cal::KIND,
        "card" => crate::segments::card::KIND,
        other => {
            return ApiError::bad_request(format!("unknown kind {other:?} (want calendar|card)"))
                .into_response();
        }
    };

    // The live mirror rows ARE the record set, and after the record-identity
    // cutover `record_cid` IS the key the per-kind reader takes — no digest-tail
    // slicing, no PK detour: the stored 36-byte Cid goes straight to
    // `read_record`.
    let cids: Vec<Vec<u8>> = {
        let conn = state.db.conn().await;
        let mut stmt = match conn.prepare(
            "SELECT record_cid FROM segment_records \
             WHERE scope_id = ?1 AND kind = ?2 AND tombstoned = 0",
        ) {
            Ok(s) => s,
            Err(e) => return ApiError::internal(format!("prepare walk: {e:?}")).into_response(),
        };
        let rows = stmt.query_map(rusqlite::params![&actor_id[..], kind], |r| {
            r.get::<_, Vec<u8>>(0)
        });
        match rows.and_then(|rs| rs.collect::<Result<Vec<_>, _>>()) {
            Ok(v) => v,
            Err(e) => return ApiError::internal(format!("walk mirror: {e:?}")).into_response(),
        }
    };

    let mut total = 0usize;
    let mut sealed = 0usize;
    let mut unsealed: Vec<String> = Vec::new();
    for cid_bytes in cids {
        let record_cid = match <[u8; 36]>::try_from(cid_bytes.as_slice())
            .map_err(|_| format!("record_cid is not 36 bytes: {}", hex::encode(&cid_bytes)))
            .and_then(|arr| {
                fauna_cbor::Cid::from_bytes(arr).map_err(|e| {
                    format!("record_cid {} is not a Cid: {e}", hex::encode(&cid_bytes))
                })
            }) {
            Ok(c) => c,
            Err(msg) => return ApiError::internal(msg).into_response(),
        };

        let halves = match kind {
            "calendar" => crate::segments::cal::read_record(
                &state.cal_segments,
                &state.db,
                &actor_id,
                &record_cid,
            )
            .await
            .map(|o| o.map(|(env, _)| (env.encrypted_body, env.encrypted_index_hint))),
            _ => crate::segments::card::read_record(
                &state.card_segments,
                &state.db,
                &actor_id,
                &record_cid,
            )
            .await
            .map(|o| o.map(|(env, _)| (env.encrypted_body, env.encrypted_index_hint))),
        };
        let Ok(Some((body, hint))) = halves else {
            // A mirror row pointing at a record the segment cannot yield is a
            // divergence the walk must surface, not skip.
            unsealed.push(hex::encode(&cid_bytes));
            total += 1;
            continue;
        };

        total += 1;
        let body_ok = fauna_mls::wrapped_blob::SealedRecordBytes::verify(body).is_ok();
        let hint_ok = fauna_mls::wrapped_blob::SealedRecordBytes::verify(hint).is_ok();
        if body_ok && hint_ok {
            sealed += 1;
        } else {
            unsealed.push(hex::encode(&cid_bytes));
        }
    }

    Json(json!({ "total": total, "sealed": sealed, "unsealed": unsealed })).into_response()
}

#[derive(Deserialize)]
pub struct AgeDavTombstonesRequest {
    pub actor_id: String,
    /// `"calendar"` or `"card"`.
    pub kind: String,
    /// How far back to move every one of the actor's tombstones.
    pub days: i64,
}

/// `POST /api/v1/test/content/age_dav_tombstones` — move every one of an
/// actor's calendar or card deletion tombstones `days` into the past, so a real
/// binary can be shown a sync token that predates the tombstone-retention window
/// (`caldav-server.md` § Stale sync-token handling; `carddav-server.md` §
/// Address-book collection model — the 7-day floor makes a real wait
/// impossible in a test). Only `expunged_at` moves: the modseq ordering a sync
/// token is compared against is untouched, so the handler's own retention
/// check decides, exactly as it would on an old box.
///
/// Returns `{"aged": <rows moved>}`.
async fn handle_age_dav_tombstones(
    State(state): State<Arc<AppState>>,
    Json(req): Json<AgeDavTombstonesRequest>,
) -> impl IntoResponse {
    let actor_id = match fauna_core::hex32::decode(&req.actor_id) {
        Ok(a) => a,
        Err(_) => return ApiError::bad_request("invalid actor_id hex").into_response(),
    };
    let table = match req.kind.as_str() {
        "calendar" => "bridge_caldav_expunged",
        "card" => "bridge_carddav_expunged",
        other => {
            return ApiError::bad_request(format!("unknown kind {other:?} (want calendar|card)"))
                .into_response();
        }
    };
    if req.days <= 0 {
        return ApiError::bad_request("days must be positive").into_response();
    }
    let conn = state.db.conn().await;
    match conn.execute(
        &format!("UPDATE {table} SET expunged_at = expunged_at - ?2 WHERE actor_id = ?1"),
        rusqlite::params![actor_id.as_slice(), req.days * 86_400],
    ) {
        Ok(aged) => Json(json!({ "aged": aged })).into_response(),
        Err(e) => ApiError::internal(format!("age tombstones: {e}")).into_response(),
    }
}

/// Mount the `/api/v1/test/content/*` routes.
pub fn routes() -> axum::Router<Arc<AppState>> {
    axum::Router::new()
        .route(
            "/api/v1/test/content/age_dav_tombstones",
            axum::routing::post(handle_age_dav_tombstones),
        )
        .route(
            "/api/v1/test/content/dav_content_conformance",
            axum::routing::get(handle_dav_content_conformance),
        )
}
