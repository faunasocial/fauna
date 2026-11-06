//! Test-only endpoint that makes this nest **lose an account's generation
//! escrow wraps**, both copies: the holder's table and the fleet plane's
//! replicated row. That is the holder-side half of a dead generation, which no
//! app gesture can produce and no e2e journey can wait for.
//!
//! Gated on `test-hooks` **alone**, so it rides the standard
//! `--features test-hooks` e2e build and is compiled out of every release
//! artifact (`e2e-conventions.md` point 15).
//!
//! # Why the nest drops the wraps rather than the app
//!
//! A generation is dead when no device keys it and no holder wraps it
//! (`docs/goal/architecture/fleet-scope-reclamation.md` § *Fleet-scope
//! reclamation*, clause (3)(j)). A succession no longer reaches that state —
//! the ceremony keeps the holder's copy — so the three roads left are a device
//! lost inside its mint-to-deposit window, the user's own shred at every
//! holder, and a holder that no longer has the wrap: a nest restored from a
//! store older than the deposit. The first is a one-pass race a run cannot aim
//! at, and the second keys the generation on the shredding device. The third is
//! the nest's own state, so the nest is what stages it. Everything after it —
//! the sign-in that finds no wrap, the dead read, the user's let-go — runs the
//! production path unchanged.
//!
//! # Both copies, because a restore older than the deposit loses both
//!
//! The depositing device writes the plane's `fauna.state.escrow-wrap` row
//! beside the receipt row, after the holder acked the deposit
//! (`account-replica-posture.md` § *Escrow custody*, ruling (1);
//! `fauna_account_plane::generation_mint::MintedGeneration::wrap_entry`). So a
//! store from before the deposit holds neither the holder's wrap nor the plane
//! row, and the stand-in drops both. Dropping the holder's table alone is not
//! that state: a seed-holding device opens the plane copy before it asks the
//! holder (*Escrow custody*, ruling (2)), as the first run of the let-go
//! journey measured — the restored device logged `generation escrow recovery:
//! keys recovered recovered=1`.
//!
//! The plane row is sealed and sits at a blinded item key the nest cannot
//! derive, so the caller names it. The journey reads the coordinates from the
//! account's own device before it signs out (the e2e reader
//! `escrow_wrap_coordinates`), and this hook retires every live fleet-scope row
//! at each named item key with the retire door's own `superseded_at` write,
//! ungated and unbelted.
//!
//! `POST /api/v1/test/generation_escrow/{actor}/drop` with the body
//! `{"plane_item_keys": ["<64-hex>", …]}` deletes every wrap the holder serves
//! `actor` (its whole registration chain, as the door's own get and delete read
//! it) and retires the named plane rows. It returns
//! `{"ok": true, "generations": <n>, "wraps": <rows deleted>, "plane_rows":
//! <rows retired>}`. Keyed per actor, so no other test's account on the shared
//! nest is touched.
//!
//! Consumer: the recovery-kit let-go journey
//! (`tests/e2e-unified/tests/test_recovery_kit_let_go.py`).

#![cfg(feature = "test-hooks")]

use std::collections::BTreeSet;
use std::sync::Arc;

use axum::extract::{Path, State};
use axum::response::{IntoResponse, Json};
use axum::routing::post;
use serde::Deserialize;
use serde_json::json;

use crate::api_error::ApiError;
use crate::routes::AppState;

/// The request body: the blinded item keys of the plane's escrow-wrap rows.
#[derive(Deserialize)]
struct DropBody {
    #[serde(default)]
    plane_item_keys: Vec<String>,
}

/// `POST /api/v1/test/generation_escrow/{actor}/drop` (module docs).
async fn handle_drop(
    State(state): State<Arc<AppState>>,
    Path(actor): Path<String>,
    Json(body): Json<DropBody>,
) -> impl IntoResponse {
    let Ok(actor) = fauna_core::hex32::decode(&actor) else {
        return ApiError::bad_request("actor must be 64 hex characters").into_response();
    };
    let mut item_keys = Vec::with_capacity(body.plane_item_keys.len());
    for hex in &body.plane_item_keys {
        let Ok(key) = fauna_core::hex32::decode(hex) else {
            return ApiError::bad_request("each plane item key must be 64 hex characters")
                .into_response();
        };
        item_keys.push(key);
    }
    let rows = match state.db.get_generation_escrow_wraps(&actor, None).await {
        Ok(rows) => rows,
        Err(e) => {
            return ApiError::internal(format!("generation_escrow list: {e:#}")).into_response();
        }
    };
    let generations: BTreeSet<[u8; 32]> = rows.iter().map(|r| r.generation_id).collect();
    let mut wraps = 0i64;
    for g in &generations {
        match state.db.delete_generation_escrow_wraps(&actor, g).await {
            Ok(n) => wraps += n,
            Err(e) => {
                return ApiError::internal(format!("generation_escrow delete: {e:#}"))
                    .into_response();
            }
        }
    }
    let mut plane_rows = 0usize;
    for key in &item_keys {
        match state.db.test_retire_fleet_rows_at(&actor, key).await {
            Ok(n) => plane_rows += n,
            Err(e) => {
                return ApiError::internal(format!("escrow-wrap plane retire: {e:#}"))
                    .into_response();
            }
        }
    }
    Json(json!({
        "ok": true,
        "generations": generations.len(),
        "wraps": wraps,
        "plane_rows": plane_rows,
    }))
    .into_response()
}

/// Mount the `/api/v1/test/generation_escrow/*` route.
pub fn routes() -> axum::Router<Arc<AppState>> {
    axum::Router::new().route(
        "/api/v1/test/generation_escrow/{actor}/drop",
        post(handle_drop),
    )
}
