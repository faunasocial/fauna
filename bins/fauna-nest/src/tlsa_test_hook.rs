//! Test-only HTTP endpoint scripting the DANE/TLSA lookup seam.
//!
//! Gated on `test-hooks` **alone** (like `mta_sts_test_hook`) so it is present
//! in the standard `cargo build -p fauna-nest --features test-hooks` e2e build
//! that `build_node()` produces. It lets a tier_3 test script the records
//! `fauna.bridges.fetch_tlsa` returns for a test MX host — installing a set of
//! TLSA records (or an empty no-DANE result) into
//! [`crate::routes::AppState::tlsa_override`] without a real DNSSEC-signed
//! zone. `fetch_tlsa_handler` consults the override ahead of the production
//! `tlsa_resolver`, so a test can drive DANE pin-match / pin-mismatch /
//! no-DANE end-to-end against a TLS stub MX whose cert hash it knows.
//!
//! Consumer: the mail-bridge outbound DANE enforcement e2e (T2.1b).
//! Production never compiles this module.

#![cfg(feature = "test-hooks")]

use std::sync::Arc;

use axum::extract::State;
use axum::response::{IntoResponse, Json};
use axum::routing::post;
use serde::Deserialize;
use serde_json::json;

use crate::api_error::ApiError;
use crate::routes::AppState;
use fauna_mail::outbound::dane::TlsaRecord;

#[derive(Deserialize)]
struct TlsaBody {
    /// MX host to script the lookup for (the key the handler reads).
    mx_host: String,
    /// The TLSA records to return. An empty list scripts "no usable DANE
    /// records" (the bridge falls back to MTA-STS / opportunistic).
    #[serde(default)]
    records: Vec<TlsaRecordBody>,
}

#[derive(Deserialize)]
struct TlsaRecordBody {
    /// RFC 6698 usage (0 PKIX-TA / 1 PKIX-EE / 2 DANE-TA / 3 DANE-EE).
    usage: u8,
    /// RFC 6698 selector (0 Full cert / 1 SubjectPublicKeyInfo).
    selector: u8,
    /// RFC 6698 matching (0 Exact / 1 SHA-256 / 2 SHA-512).
    matching: u8,
    /// Certificate-association data as a lowercase hex string (the test
    /// computes e.g. `sha256(stub_cert_der)` and passes it here).
    data_hex: String,
}

/// `POST /api/v1/test/outbound/tlsa` — install a scripted TLSA lookup for
/// `mx_host`. Returns `{"ok": true, "mx_host": <mx_host>}`.
async fn handle_tlsa(
    State(state): State<Arc<AppState>>,
    Json(body): Json<TlsaBody>,
) -> impl IntoResponse {
    let mut records = Vec::with_capacity(body.records.len());
    for r in body.records {
        let data = match hex::decode(&r.data_hex) {
            Ok(d) => d,
            Err(e) => {
                return ApiError::bad_request(format!("invalid data_hex: {e}")).into_response();
            }
        };
        records.push(TlsaRecord {
            usage: r.usage,
            selector: r.selector,
            matching: r.matching,
            data,
        });
    }
    state
        .tlsa_override
        .lock()
        .expect("tlsa_override mutex poisoned")
        .insert(body.mx_host.clone(), records);
    Json(json!({ "ok": true, "mx_host": body.mx_host })).into_response()
}

/// Mount the `/api/v1/test/outbound/tlsa` route.
pub fn routes() -> axum::Router<Arc<AppState>> {
    axum::Router::new().route("/api/v1/test/outbound/tlsa", post(handle_tlsa))
}
