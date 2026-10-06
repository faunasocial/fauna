//! Handler for /internal/router-status — reports nest health and capacity for fauna-router.
//!
//! Contract owner: `docs/goal/architecture/nest/worker.md` § Router Status Endpoint.
//!
//! Everything here is a **projection of client-set nest state**. The router is a
//! frontend, not an authority: it holds no registration/capacity policy of its own
//! and reads all of it from this poll, so an admin's choice in their client is the
//! only thing that can change it. A router-side copy of any of these values would
//! be config theatre — the same anti-pattern already ruled out for
//! `fauna.admin.set_serving_port` on a router-fronted nest (`common.md` § Serving
//! port). Fields are additive-only: an older router ignores what it does not know.

use crate::routes::AppState;
use axum::extract::State;
use axum::response::{IntoResponse, Json};
use serde_json::json;
use std::sync::Arc;

pub async fn get_router_status(State(state): State<Arc<AppState>>) -> impl IntoResponse {
    let current_users = state.db.count_all_users().await.unwrap_or(0);
    // The live client-set storage cap (boot-resolved from the
    // `nest_max_storage_bytes` DB row, else the `config.nest.max_storage_bytes`
    // seed) — never `config.nest.max_storage_bytes` directly. `None` ⇒ no cap ⇒
    // the historical 5000-user default.
    let max_users = (*state.max_storage_bytes.read().await)
        .map(|b| b / (200 * 1024 * 1024))
        .unwrap_or(5000) as i64;
    let nest_id = hex::encode(state.nest_identity.public_key_bytes());

    // The live client-set registration posture, as the `(open,
    // invite_required)` pair the router reads — one projection owner
    // (`RegistrationMode::to_wire_booleans`).
    // This is a *report*, not a delegation: enforcement stays here, on
    // `fauna.account.register`. The router mirrors it on its NodeInfo; it must
    // never gate on it, because then two authorities could disagree.
    let (mode, _max_free_users) = *state.registration_mode.read().await;
    let (registration_open, invite_required) = mode.to_wire_booleans();

    // The domain handles are minted under — the client-set primary domain
    // (identity_domain), else the artifact seed; `None` on a domainless box
    // (never the "localhost" placeholder). The router projects this into its
    // NodeInfo `handleDomain`; its own `[registration] handle_domain` TOML key
    // was the last hand-edited copy and is deleted (2026-07-17).
    let handle_domain = state.handle_domain_if_set();

    Json(json!({
        "nest_id": nest_id,
        "healthy": true,
        "max_users": max_users,
        "current_users": current_users,
        "labels": [],
        "version": env!("CARGO_PKG_VERSION"),
        "registration_open": registration_open,
        "invite_required": invite_required,
        "handle_domain": handle_domain,
    }))
}
