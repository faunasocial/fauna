//! Test-only HTTP endpoints driving the **production** DMARC aggregate-report
//! sender (`outbound_dmarc::run_one_emit_pass`) deterministically.
//!
//! Gated on the `test-hooks` Cargo feature alone, like
//! `outbound_tlsrpt_test_hook`. The Go MTA reports each inbound message it
//! verified via `fauna.bridges.report_dmarc_observation` → the production
//! `AppState.email.dmarc_aggregator`. These hooks let an e2e (1) read the
//! aggregator's buckets, (2) fire one daily pass at a scripted `now` against a
//! stub TXT map (so DNS is never touched), and (3) read the persisted
//! `dmarc_outbound_aggregate_reports` rows. Reports ride the real outbound
//! queue, so the MTA delivers them like any other outbound mail.
//!
//! `POST /api/v1/test/dmarc/prune_now` also runs the inbound retention prune at
//! a scripted `now` (`dmarc-reporting.md` § Retention).
//!
//! Consumer: `tests/e2e-unified/tests/test_mail_dmarc_reports.py`. Production
//! never compiles this module.

#![cfg(feature = "test-hooks")]

use std::collections::HashMap;
use std::sync::Arc;

use axum::extract::State;
use axum::response::{IntoResponse, Json};
use axum::routing::{get, post};
use fauna_mail::outbound::dmarc::DmarcRecordFetcher;
use serde::Deserialize;
use serde_json::json;

use crate::api_error::ApiError;
use crate::routes::AppState;

/// Stub TXT resolver: a name absent from the map has no records.
struct StubDmarcRecordFetcher {
    txt: HashMap<String, Vec<String>>,
}

#[async_trait::async_trait]
impl DmarcRecordFetcher for StubDmarcRecordFetcher {
    async fn txt(&self, name: &str) -> anyhow::Result<Vec<String>> {
        Ok(self
            .txt
            .get(&name.to_ascii_lowercase())
            .cloned()
            .unwrap_or_default())
    }
}

#[derive(Deserialize)]
struct EmitDmarcNowBody {
    /// Epoch second to run the pass at (the report window, ids and the
    /// outbound queue's `due_at` derive from it).
    now: i64,
    /// TXT records by owner name (`_dmarc.<domain>`,
    /// `<policy>._report._dmarc.<rua-domain>`), each a list of strings.
    #[serde(default)]
    txt: HashMap<String, Vec<String>>,
}

/// `POST /api/v1/test/outbound/emit_dmarc_now` — run exactly one
/// `run_one_emit_pass` against the production aggregator at the scripted
/// `now`, resolving TXT from the stub map. Replies the parts queued.
async fn handle_emit_dmarc_now(
    State(state): State<Arc<AppState>>,
    Json(body): Json<EmitDmarcNowBody>,
) -> impl IntoResponse {
    let fetcher = StubDmarcRecordFetcher {
        txt: body
            .txt
            .into_iter()
            .map(|(k, v)| (k.to_ascii_lowercase(), v))
            .collect(),
    };
    let queued = crate::outbound_dmarc::run_one_emit_pass(&state, &fetcher, body.now).await;
    Json(json!({ "queued": queued })).into_response()
}

/// `GET /api/v1/test/outbound/dmarc_aggregator` — the buckets the next pass
/// will consume, by From domain.
async fn handle_dmarc_aggregator(State(state): State<Arc<AppState>>) -> impl IntoResponse {
    let snap = match state.email.dmarc_aggregator.lock() {
        Ok(g) => g.dump_snapshot(),
        Err(poisoned) => poisoned.into_inner().dump_snapshot(),
    };
    Json(json!({ "domains": snap })).into_response()
}

/// `GET /api/v1/test/outbound/dmarc_sent` — the persisted outbound report
/// rows, each with its XML gunzipped.
async fn handle_dmarc_sent(State(state): State<Arc<AppState>>) -> impl IntoResponse {
    match state.db.fetch_dmarc_outbound_reports_for_test().await {
        Ok(rows) => {
            let rows: Vec<_> = rows
                .into_iter()
                .map(|r| {
                    let xml = fauna_mail::dmarc_report::gunzip_xml(&r.payload_xml_gz)
                        .map(|x| String::from_utf8_lossy(&x).into_owned())
                        .unwrap_or_default();
                    json!({
                        "report_id": r.report_id,
                        "peer_domain": r.peer_domain,
                        "destination": r.destination,
                        "begin_at": r.begin_at,
                        "end_at": r.end_at,
                        "sent_at": r.sent_at,
                        "xml": xml,
                    })
                })
                .collect();
            Json(json!({ "rows": rows })).into_response()
        }
        Err(e) => ApiError::internal(format!("fetch_dmarc_outbound_reports_for_test: {e}"))
            .into_response(),
    }
}

#[derive(Deserialize)]
struct PruneDmarcNowBody {
    /// Epoch second to run the retention prune at.
    now: i64,
}

/// `POST /api/v1/test/dmarc/prune_now` — run the daily mail sweep's DMARC
/// retention prune (`run_dmarc_aggregate_report_gc`) at the scripted `now`, so
/// an e2e witnesses § Retention without waiting 90 days.
async fn handle_prune_dmarc_now(
    State(state): State<Arc<AppState>>,
    Json(body): Json<PruneDmarcNowBody>,
) -> impl IntoResponse {
    crate::bridge_routing_handlers::run_dmarc_aggregate_report_gc_at(&state, body.now).await;
    Json(json!({ "ok": true })).into_response()
}

/// Mount the production-path DMARC sender test routes.
pub fn routes() -> axum::Router<Arc<AppState>> {
    axum::Router::new()
        .route(
            "/api/v1/test/outbound/emit_dmarc_now",
            post(handle_emit_dmarc_now),
        )
        .route(
            "/api/v1/test/outbound/dmarc_aggregator",
            get(handle_dmarc_aggregator),
        )
        .route("/api/v1/test/outbound/dmarc_sent", get(handle_dmarc_sent))
        .route("/api/v1/test/dmarc/prune_now", post(handle_prune_dmarc_now))
}
