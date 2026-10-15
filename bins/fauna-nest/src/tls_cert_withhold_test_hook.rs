//! Test-only HTTP endpoints making `fauna.bridges.fetch_tls_cert_blob` answer
//! "no blob" for a named `(bridge_role, bridge_id)`, so a test can boot a
//! REAL mail bridge into the cert-less state `mail-bridge-lifecycle.md`
//! § Implementation status today promises it heals from: its first TLS refresh
//! fails soft, the encrypted ports serve no certificate, and the bridge's
//! refresh loop (or a `"tls"` `config_changed` push) picks a cert up once one
//! is there — with no restart.
//!
//! # Why a hook
//!
//! No honest path reaches that state on a test nest. The nest always has its
//! self-signed floor cert on disk (`self_signed_cert::ensure_floor_present`),
//! the fetch seals that on-disk cert on read, and the bridge attests its
//! x25519 before its first refresh — so an approved bridge's first fetch always
//! succeeds. In production the cert-less boot is a transient first-fetch
//! failure; this seam is that failure, cut at the one reply the bridge reads
//! ([`crate::bridge_blob_handlers`], the `fetch_tls_cert_blob` handler) and
//! nowhere else: the on-disk cert, the stored blobs and every other bridge's
//! fetch are untouched.
//!
//! Gated on `test-hooks` alone (like `capability_withhold_test_hook`), so it is
//! present in the standard `cargo build -p fauna-nest --features test-hooks`
//! e2e build.
//!
//! Consumer: `tests/e2e-unified/tests/test_mail_bridge_mta.py`
//! (`test_mail_started_before_its_certificate_offers_encryption_without_a_restart`),
//! armed before the bridge spawns by the `certless_mta_nest` fixture.
//! Production never compiles this module.

#![cfg(feature = "test-hooks")]

use std::collections::HashSet;
use std::sync::Arc;

use axum::extract::{Path, State};
use axum::response::{IntoResponse, Json};
use axum::routing::post;
use serde_json::json;

use crate::routes::AppState;

/// Whether `(bridge_role, bridge_id)`'s TLS fetch is withheld — the one check
/// the `fetch_tls_cert_blob` handler applies.
pub fn is_withheld(
    withheld: &HashSet<(String, String)>,
    bridge_role: &str,
    bridge_id: &str,
) -> bool {
    withheld.contains(&(bridge_role.to_owned(), bridge_id.to_owned()))
}

/// `POST /api/v1/test/tls-cert-withhold/{role}/{bridge_id}` — answer that
/// bridge's `fetch_tls_cert_blob` with no blob until cleared. Returns
/// `{"ok": true, "role": <role>, "bridge_id": <bridge_id>}`.
async fn withhold(
    State(state): State<Arc<AppState>>,
    Path((role, bridge_id)): Path<(String, String)>,
) -> impl IntoResponse {
    state
        .tls_cert_fetch_withheld
        .lock()
        .expect("tls_cert_fetch_withheld mutex poisoned")
        .insert((role.clone(), bridge_id.clone()));
    Json(json!({ "ok": true, "role": role, "bridge_id": bridge_id })).into_response()
}

/// `POST /api/v1/test/tls-cert-withhold/{role}/{bridge_id}/clear` — serve that
/// bridge's fetch again. Returns `{"ok": true}`.
async fn clear(
    State(state): State<Arc<AppState>>,
    Path((role, bridge_id)): Path<(String, String)>,
) -> impl IntoResponse {
    state
        .tls_cert_fetch_withheld
        .lock()
        .expect("tls_cert_fetch_withheld mutex poisoned")
        .remove(&(role, bridge_id));
    Json(json!({ "ok": true })).into_response()
}

/// Mount the `/api/v1/test/tls-cert-withhold/*` routes.
pub fn routes() -> axum::Router<Arc<AppState>> {
    axum::Router::new()
        .route(
            "/api/v1/test/tls-cert-withhold/{role}/{bridge_id}",
            post(withhold),
        )
        .route(
            "/api/v1/test/tls-cert-withhold/{role}/{bridge_id}/clear",
            post(clear),
        )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_the_named_role_and_bridge_are_withheld() {
        let withheld = HashSet::from([("mta".to_owned(), "a-mta".to_owned())]);
        assert!(is_withheld(&withheld, "mta", "a-mta"));
        assert!(
            !is_withheld(&withheld, "mda", "a-mta"),
            "the role is part of the key"
        );
        assert!(
            !is_withheld(&withheld, "mta", "b-mta"),
            "a sibling bridge is served"
        );
        assert!(!is_withheld(&HashSet::new(), "mta", "a-mta"));
    }
}
