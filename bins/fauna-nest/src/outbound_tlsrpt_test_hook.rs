//! Test-only HTTP endpoints driving the **production** TLSRPT outbound
//! reporter (`outbound_tlsrpt::run_one_emit_pass`) deterministically.
//!
//! Gated on the `test-hooks` Cargo feature **alone** (like `mta_sts_test_hook`
//! / `tlsa_test_hook`) so these are present in the plain
//! `cargo build -p fauna-nest --features test-hooks` e2e build the Go MTA
//! bridge's tier_3 fixture uses.
//!
//! The Go MTA reports per-attempt TLS outcomes via
//! `fauna.bridges.report_tls_attempt` (T2.4 G1) → the handler records into the
//! production `AppState.email.tlsrpt_aggregator`. These hooks let an e2e then
//! (1) dump the in-memory aggregator buckets, (2) fire one daily-emit pass at a
//! scripted `now` with a stub TLSRPT-policy fetcher (so DNS isn't touched), and
//! (3) read the persisted `tlsrpt_outbound_reports` rows. The poster is the
//! always-200 `NullTlsrptHttpPoster`, so `https:` rua persist a row without an
//! HTTP server; `mailto:` rua ride the real outbound queue.
//!
//! Consumer: the mail-bridge MTA TLSRPT e2e (T2.4 E1). Production never
//! compiles this module.

#![cfg(feature = "test-hooks")]

use std::collections::HashMap;
use std::sync::Arc;

use axum::extract::State;
use axum::response::{IntoResponse, Json};
use axum::routing::{get, post};
use fauna_mail::outbound::tlsrpt::{NullTlsrptHttpPoster, TlsrptPolicyFetcher};
use serde::Deserialize;
use serde_json::json;

use crate::api_error::ApiError;
use crate::routes::AppState;

/// Stub `_smtp._tls.<domain>` policy fetcher: returns the scripted `rua=`
/// URIs for a known recipient domain (a domain absent from the map → `None`,
/// i.e. "publishes no TLSRPT policy" → no dispatch). Replaces
/// `LiveTlsrptPolicyFetcher` under test so the emit pass touches no DNS.
struct StubTlsrptPolicyFetcher {
    rua: HashMap<String, Vec<String>>,
}

#[async_trait::async_trait]
impl TlsrptPolicyFetcher for StubTlsrptPolicyFetcher {
    async fn lookup(&self, domain: &str) -> anyhow::Result<Option<Vec<String>>> {
        Ok(self.rua.get(domain).cloned())
    }
}

#[derive(Deserialize)]
struct RecordTlsAttemptBody {
    recipient_domain: String,
    #[serde(default)]
    mx_host: String,
    /// RFC 8460 §4.3 result-type token, or absent/null for a successful TLS
    /// session (recorded as a success in the aggregate).
    #[serde(default)]
    result_type: Option<String>,
}

/// `POST /api/v1/test/outbound/record_tls_attempt` — seed one per-attempt
/// outcome into the production aggregator the SAME way the WS-RPC
/// `report_tls_attempt` handler does (`policy_for_attempt` → `record`), but
/// over HTTP. Lets the e2e drive aggregation deterministically without
/// standing up a full outbound TLS delivery (the Go classification + the
/// report_tls_attempt wire are covered by the Go unit tests). Buckets under
/// `no-policy-found` (the `not_published`, no-DANE case) so a domain's
/// differing result-types land in one bucket.
async fn handle_record_tls_attempt(
    State(state): State<Arc<AppState>>,
    Json(body): Json<RecordTlsAttemptBody>,
) -> impl IntoResponse {
    use fauna_mail::outbound::mta_sts::MtaStsLookup;
    use fauna_mail::outbound::tlsrpt::{
        AttemptOutcome, OutboundTlsrptRecorder, policy_for_attempt,
    };
    let policy = policy_for_attempt(
        &body.recipient_domain,
        &body.mx_host,
        &[],
        &MtaStsLookup::NotPublished,
    );
    state.email.tlsrpt_aggregator.record(AttemptOutcome {
        recipient_domain: body.recipient_domain,
        policy,
        failure_type: body.result_type,
    });
    Json(json!({ "ok": true })).into_response()
}

#[derive(Deserialize)]
struct EmitTlsrptNowBody {
    /// Absolute epoch second to run the emit pass at (the report date +
    /// `report_id` + outbound-queue `due_at` derive from it).
    now: i64,
    /// Per recipient-domain `rua=` URIs the stub fetcher returns. A domain the
    /// aggregator recorded but that is absent here → `None` → skipped.
    #[serde(default)]
    rua: HashMap<String, Vec<String>>,
}

/// `POST /api/v1/test/outbound/emit_tlsrpt_now` — run exactly one
/// `run_one_emit_pass` against the production aggregator with a stub policy
/// fetcher + the always-200 null poster, at the scripted `now`.
async fn handle_emit_tlsrpt_now(
    State(state): State<Arc<AppState>>,
    Json(body): Json<EmitTlsrptNowBody>,
) -> impl IntoResponse {
    let fetcher = StubTlsrptPolicyFetcher { rua: body.rua };
    let poster = NullTlsrptHttpPoster;
    crate::outbound_tlsrpt::run_one_emit_pass(&state, &fetcher, &poster, body.now).await;
    Json(json!({ "ok": true })).into_response()
}

/// `GET /api/v1/test/outbound/tlsrpt_aggregator` — snapshot the in-memory
/// production aggregator buckets (what the next emit pass will consume).
async fn handle_tlsrpt_aggregator(State(state): State<Arc<AppState>>) -> impl IntoResponse {
    let snap = match state.email.tlsrpt_aggregator.lock() {
        Ok(g) => g.dump_snapshot(),
        Err(poisoned) => poisoned.into_inner().dump_snapshot(),
    };
    Json(json!({ "domains": snap })).into_response()
}

/// `GET /api/v1/test/outbound/tlsrpt_persisted` — the persisted
/// `tlsrpt_outbound_reports` rows (one per shipped transport; retained 7 days).
async fn handle_tlsrpt_persisted(State(state): State<Arc<AppState>>) -> impl IntoResponse {
    match state.db.fetch_tlsrpt_reports_for_test().await {
        Ok(rows) => Json(json!({ "rows": rows })).into_response(),
        Err(e) => ApiError::internal(format!("fetch_tlsrpt_reports_for_test: {e}")).into_response(),
    }
}

/// Mount the production-path TLSRPT test routes.
pub fn routes() -> axum::Router<Arc<AppState>> {
    axum::Router::new()
        .route(
            "/api/v1/test/outbound/record_tls_attempt",
            post(handle_record_tls_attempt),
        )
        .route(
            "/api/v1/test/outbound/emit_tlsrpt_now",
            post(handle_emit_tlsrpt_now),
        )
        .route(
            "/api/v1/test/outbound/tlsrpt_aggregator",
            get(handle_tlsrpt_aggregator),
        )
        .route(
            "/api/v1/test/outbound/tlsrpt_persisted",
            get(handle_tlsrpt_persisted),
        )
}
