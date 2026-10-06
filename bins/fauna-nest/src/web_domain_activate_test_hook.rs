//! Test-only HTTP endpoint that finishes a custom web domain's DNS
//! verification the way the lifecycle task's own pass does.
//!
//! Gated on `test-hooks` **alone** (the `web_blank_site_test_hook` shape), so
//! it rides the standard `--features test-hooks` e2e build and never compiles
//! into production.
//!
//! Why it exists: the only writer of an `active` `web_domains` row is
//! [`crate::web_content::domain::verify_pending_domains_once`], which advances
//! a `pending` row only when the shared **public-recursive** `DnsVerifier`
//! finds the row's token at `_fauna-verify.<domain>` — a real lookup against
//! the public DNS, which a tier_3 nest on a loopback port has no honest way to
//! satisfy (`web-content-hosting.md` § Registration and DNS verification step
//! 3). So the DNS leg stays Rust-level, where `MockResolver` can answer it
//! (`web_content::domain::tests::verify_pending_domains_once_advances_on_matching_txt`),
//! and this hook stands in for it by calling the SAME two writers that pass
//! calls, in the same order — `update_web_domain_status(…, "verified")` then
//! `(…, "active")` — followed by the SAME routing pass the lifecycle task runs
//! next, [`crate::web_content::domain::reconcile_custom_domain_routing_once`]
//! (step 5). It never inserts or hand-edits a row: the row under it is the one
//! `fauna.web.domain.set` wrote, and every state it passes through is one
//! production writes.
//!
//! What a test that uses it therefore witnesses is the whole of the routing
//! half — a `pending` domain does not route, an `active` one serves its
//! owner's site through the live `HostResolver` with no restart, and
//! `fauna.web.domain.delete` stops it — and none of the DNS or ACME halves.
//!
//! Consumer: `tests/e2e-unified/tests/api/test_web_host_routing.py`.

#![cfg(feature = "test-hooks")]

use std::sync::Arc;

use axum::extract::State;
use axum::response::{IntoResponse, Json};
use axum::routing::post;
use serde::Deserialize;
use serde_json::json;

use crate::api_error::ApiError;
use crate::routes::AppState;

#[derive(Deserialize)]
struct ActivateDomain {
    /// The registered custom domain, exactly as `fauna.web.domain.set`
    /// normalized and stored it.
    domain: String,
}

/// `POST /api/v1/test/web/activate-domain` — advance a registered `pending`
/// domain to `active` and reconcile the live routing map. Returns
/// `{"ok": true, "status": "active"}`.
async fn handle_activate_domain(
    State(state): State<Arc<AppState>>,
    Json(req): Json<ActivateDomain>,
) -> impl IntoResponse {
    if req.domain.is_empty() {
        return ApiError::bad_request("domain must not be empty").into_response();
    }
    // The row must already exist: this hook finishes a registration, it never
    // invents one. A missing row is the caller's bug and says so.
    match state.db.get_web_domain_by_domain(&req.domain).await {
        Ok(Some(_)) => {}
        Ok(None) => {
            return ApiError::bad_request(format!(
                "no web_domains row for {} — register it with fauna.web.domain.set first",
                req.domain
            ))
            .into_response();
        }
        Err(e) => return ApiError::internal(format!("activate domain: {e:#}")).into_response(),
    }

    // The production pass's own two writes, in its own order.
    for status in ["verified", "active"] {
        if let Err(e) = state.db.update_web_domain_status(&req.domain, status).await {
            return ApiError::internal(format!("activate domain: set {status}: {e:#}"))
                .into_response();
        }
    }

    // …then the routing reconcile the same pass runs next. Without a live
    // resolver the nest is not serving web content at all, so an `active` row
    // would route nowhere and a test asserting the serve would fail with a
    // puzzle instead of a reason.
    let Some(resolver) = &state.host_resolver else {
        return ApiError::internal(
            "activate domain: web hosting is not active on this nest (no host resolver)",
        )
        .into_response();
    };
    match crate::web_content::domain::reconcile_custom_domain_routing_once(&state.db, resolver)
        .await
    {
        Ok(()) => Json(json!({ "ok": true, "status": "active" })).into_response(),
        Err(e) => {
            ApiError::internal(format!("activate domain: routing reconcile: {e:#}")).into_response()
        }
    }
}

/// Mount the `/api/v1/test/web/activate-domain` route.
pub fn routes() -> axum::Router<Arc<AppState>> {
    axum::Router::new().route(
        "/api/v1/test/web/activate-domain",
        post(handle_activate_domain),
    )
}
