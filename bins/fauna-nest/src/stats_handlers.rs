//! Stats WS-RPC handler (bearer connection) — part of the WS-RPC-everywhere
//! migration (tracked internally). A behavior-preserving transport
//! migration of the (now-deleted) bearer-authed HTTP route `GET /api/v1/stats`.
//!
//! One kind:
//!
//! - `fauna.stats.get` — reuses `backup::stats::{compute_global_stats,
//!   compute_folder_stats}` (no shared core — one compute call plus reply
//!   shaping, like `register_account_user_handlers`). The request's
//!   `folder: Option<String>` replaces the twin's `?folder=` query param:
//!   `None` → global, `Some(name)` → that folder (missing → `not_found`,
//!   the twin's 404). The twin's `dedup_ratio: f64` rides as
//!   `dedup_ratio_micro: i64` (floats are forbidden on the dag-cbor wire; ×1e6,
//!   the feed-`score` precedent).
//!
//! **Authorization (ST-1, 2026-06-27 — was a cross-user IDOR the B17 transport
//! flip inherited from the twin's `_bearer: BearerAuth`):** the connection
//! `actor_id` *does* gate data selection.
//! - `Some(name)` (folder branch) is **owner-scoped** — `compute_folder_stats`
//!   resolves via `get_folder_for_actor`, so a caller only ever reads *their
//!   own* set's aggregate stats (a non-owner gets `not_found`). Any `User` may
//!   call it; the scope is the gate.
//! - `None` (global branch) returns **nest-wide** totals (other users' aggregate
//!   existence + usage), so it is **`Admin`-only** — a deployment metric, the
//!   same posture as the dedicated `fauna.admin.stats` kind (with which it is now
//!   redundant; no client calls the global branch — `repoStats` always passes a
//!   `folder`). A non-admin `User` gets `permission_denied`.
//!
//! The kind-level allowlist stays `User | Admin` (a `User` legitimately reads
//! the folder branch); the global-branch `Admin` requirement is enforced
//! in-handler, one granularity finer than the allowlist.

use std::time::Duration;

use fauna_protocol::stats::{
    BlobTypeCounts, DEDUP_RATIO_MICRO_SCALE, StatsGetReply, StatsGetRequest,
};
use fauna_protocol::{RpcError, decode_strict as decode};

use crate::bridge_method_allowlist::CallerClass;
use crate::routes::AppState;
use crate::rpc_router::{RpcHandler, RpcKindMeta, RpcRouterBuilder};

/// Error namespace for every `fauna.stats.*` code.
const NS: &str = "stats";

// ── Helpers (mirroring `pending_action_handlers`, scoped to `stats`) ─────────

use crate::rpc_errors::{encode_reply, malformed};

fn internal(err: impl std::fmt::Display) -> RpcError {
    crate::rpc_errors::internal_ns(NS, err)
}

fn invalid_request(reason: impl std::fmt::Display) -> RpcError {
    crate::rpc_errors::invalid_request_ns(NS, reason)
}

fn not_found() -> RpcError {
    crate::rpc_errors::not_found_ns(NS, "folder not found")
}

fn permission_denied(reason: impl std::fmt::Display) -> RpcError {
    crate::rpc_errors::permission_denied_ns(NS, reason)
}

/// Resolve the connection actor's `CallerClass` and check the kind's allowlist
/// arm — the WS-RPC counterpart of the HTTP `BearerAuth` extractor gate. Returns
/// the resolved class so the handler can apply finer per-branch gates (the
/// global branch is `Admin`-only — ST-1).
async fn require_permission(
    state: &AppState,
    actor_id: &[u8; 32],
    kind: &str,
) -> Result<CallerClass, RpcError> {
    crate::bridge_method_allowlist::require_permission(&state.db, actor_id, kind, internal).await
}

/// `dedup_ratio: f64` → `dedup_ratio_micro: i64` (×1e6, rounded). Floats are
/// forbidden on the dag-cbor wire.
fn dedup_micro(ratio: f64) -> i64 {
    (ratio * DEDUP_RATIO_MICRO_SCALE).round() as i64
}

// ── fauna.stats.get (≡ GET /api/v1/stats[?folder=…]) ──────────────────────

fn get_handler() -> RpcHandler {
    Box::new(|state, actor_id, payload| {
        Box::pin(async move {
            let class = require_permission(&state, &actor_id, "fauna.stats.get").await?;
            let req: StatsGetRequest = decode(&payload).map_err(malformed)?;

            // A set is named by its hash alone once the app's funnel takes the
            // plaintext off (`fauna_protocol::folders::addressed`), so either
            // carrier selects the per-folder arm — never the nest-wide one.
            let name_hash = crate::routes::parse_name_hash(&req.name_hash, |m| invalid_request(m))?;
            let folder = match (req.folder, name_hash) {
                (Some(name), _) => Some(name),
                (None, Some(_)) => Some(String::new()),
                (None, None) => None,
            };
            match folder {
                Some(name) => {
                    // Per-folder (the twin's `?folder=` branch), owner-scoped
                    // (ST-1): not yours / missing → `not_found` (a 404 that also
                    // hides whether the set exists for another user).
                    let s = crate::backup::stats::compute_folder_stats(
                        &state.db,
                        &name,
                        name_hash.as_ref(),
                        &actor_id,
                    )
                    .await
                    .map_err(internal)?
                    .ok_or_else(not_found)?;
                    encode_reply(&StatsGetReply::Folder {
                        folder: s.folder,
                        snapshot_count: s.snapshot_count,
                        latest_snapshot: s.latest_snapshot,
                        total_files: s.total_files,
                        raw_size_bytes: s.raw_size_bytes,
                        stored_size_bytes: s.stored_size_bytes,
                        dedup_ratio_micro: dedup_micro(s.dedup_ratio),
                        storage_backend: s.storage_backend,
                    })
                }
                None => {
                    // Nest-wide totals leak other users' aggregate existence +
                    // usage — a deployment metric, so `Admin`-only (ST-1). Same
                    // posture as the dedicated `fauna.admin.stats` kind. No client
                    // calls this branch; `repoStats` always passes a `folder`.
                    if class != CallerClass::Admin {
                        return Err(permission_denied(
                            "nest-wide stats are admin-only; pass a folder for your own set",
                        ));
                    }
                    let s = crate::backup::stats::compute_global_stats(&state.db)
                        .await
                        .map_err(internal)?;
                    encode_reply(&StatsGetReply::Global {
                        total_size_bytes: s.total_size_bytes,
                        total_blobs: s.total_blobs,
                        total_snapshots: s.total_snapshots,
                        total_folders: s.total_folders,
                        dedup_ratio_micro: dedup_micro(s.dedup_ratio),
                        blob_types: BlobTypeCounts {
                            chunk: s.blob_types.chunk,
                            manifest: s.blob_types.manifest,
                            extra: Default::default(),
                        },
                    })
                }
            }
        })
    })
}

// ── Registration entry point ────────────────────────────────────────────────

/// Register the authenticated stats surface on the **bearer** router. Per-kind
/// replay semantics + rationale: see `KindRegistry::register_stats_kinds`.
/// `fauna.stats.get` is `forbid_replay = false` @5 s — a pure read.
pub fn register_stats_handlers(b: &mut RpcRouterBuilder) {
    b.add(
        "fauna.stats.get",
        RpcKindMeta {
            forbid_replay: false,
            default_deadline: Duration::from_secs(5),
            handler: get_handler(),
        },
    );
}
