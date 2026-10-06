//! File-versions WS-RPC handlers (bearer connection) — the version-history
//! surface backing the `file-version-history` client component
//! (`docs/goal/behavior/file-sync.md` § File Versions, ratified 2026-07-09).
//!
//! Version history is a **projection over the append-only `sync_changes`
//! table** — every recorded change IS a version, so history is retroactive and
//! GC-pinned. Two kinds:
//!
//! - `fauna.files.versions.list` —
//!   `list_file_versions_in_sets(readable_set_ids, path_hash)`, oldest→newest;
//!   `req.folder` narrows to one named readable set (unknown/unreadable name
//!   → empty list, no set-name existence oracle).
//! - `fauna.files.versions.get` — `get_file_version_by_seq(path_hash,
//!   version_num)`; missing OR outside the caller's readable sets →
//!   `fauna.files.not_found` (no existence oracle).
//!
//! Both are **label-audience scoped**: the readable-set enumeration
//! (`folder_authz::enumerate_readable_folders`) is filtered to
//! `FolderReadGrant::is_label_audience()` — owner + roster member, never the
//! Q5 admin-discovery grant. Version history (path existence + manifest +
//! author + timestamps) is content metadata, not the discovery metadata the Q5
//! grant is scoped to, and an admin-reachable reply would be an online
//! existence oracle for a guessed `path_hash` (ruled 2026-07-30;
//! `encryption-at-rest.md` § Carve-outs). On top of the `User | Admin`
//! allowlist check. `version_num` is the
//! recording row's `seq` (file-sync.md § File Versions). The `path_hash` /
//! `manifest_hash` ride as raw 32-byte buffers.

use std::time::Duration;

use fauna_protocol::files::{
    FileVersionInfo, FilesVersionsGetRequest, FilesVersionsListReply, FilesVersionsListRequest,
};
use fauna_protocol::{ByteBuf, RpcError, decode_strict as decode};

use crate::db::FileVersionRow;
use crate::rpc_router::{RpcHandler, RpcKindMeta, RpcRouterBuilder};

/// Error namespace for every `fauna.files.*` code.
const NS: &str = "files";

// ── Helpers (mirroring `stats_handlers`, scoped to `files`) ──────────────────

use crate::rpc_errors::{encode_reply, malformed};

fn internal(err: impl std::fmt::Display) -> RpcError {
    crate::rpc_errors::internal_ns(NS, err)
}

fn not_found() -> RpcError {
    crate::rpc_errors::not_found_ns(NS, "file version not found")
}

fn invalid_request(reason: impl std::fmt::Display) -> RpcError {
    crate::rpc_errors::invalid_request_ns(NS, reason)
}

/// Resolve the connection actor's `CallerClass` and check the kind's allowlist
/// arm — the WS-RPC counterpart of the HTTP `BearerAuth` extractor gate.
use crate::bridge_method_allowlist::require_permission_default as require_permission;

/// A wire `path_hash` (raw bytes) → the `[u8; 32]` the DB methods take; a
/// wrong-length buffer is a malformed request.
fn path_hash_array(path_hash: &ByteBuf) -> Result<[u8; 32], RpcError> {
    crate::rpc_errors::require_bytes32("path_hash", path_hash.as_ref()).map_err(invalid_request)
}

/// DB projection row → wire metadata. `set_name` is resolved from the caller's
/// readable-set list (the row's `folder_id` is nest-internal);
/// `author_handle` is the recorder's nest-resolved handle (empty folds to
/// absent, so the client shows the id fallback rather than a blank).
fn row_to_info(
    r: &FileVersionRow,
    set: Option<&crate::db::FolderRow>,
    author_handle: Option<String>,
) -> FileVersionInfo {
    // The recovery-browse markers (file-versions.md § Retention (3)): present
    // exactly when the row is soft-pruned — the default projection never
    // returns such a row, so an ordinary listing's wire bytes are unchanged.
    let pruned = r.pruned_at.is_some();
    FileVersionInfo {
        path_hash: ByteBuf::from(r.path_hash.clone()),
        version_num: r.version_num,
        manifest_hash: ByteBuf::from(r.manifest_hash.clone()),
        size_bytes: r.size_bytes,
        created_at: r.created_at,
        folder: set.map(|fs| fs.name.clone()),
        folder_hash: set.and_then(|fs| fs.name_hash.clone()).map(ByteBuf::from),
        content_key_version: r.content_key_version.map(|v| v as u64),
        // Nest-stamped attribution (multi-writer Phase 1) — the row's
        // recorder; the set owner on every pre-multi-writer row.
        author_actor_id: hex::encode(&r.actor_id),
        author_handle: author_handle.filter(|h| !h.is_empty()),
        pruned: pruned.then_some(true),
        purge_after: if pruned { r.purge_after } else { None },
        // The rest of the row's writer-signed statement, echoed verbatim
        // (writer-signed change records (2)); the delegated signer's cert
        // rides the list reply's `signer_certs`.
        device_id: r.device_id.clone().map(ByteBuf::from),
        change_type: Some(r.change_type.clone()),
        path_sealed: r.path_sealed.clone().map(ByteBuf::from),
        thumbnail_hash: r.thumbnail_hash.clone(),
        derived_through: r.derived_through,
        is_resolution: r.is_resolution,
        is_retention: r.is_retention,
        signature: r.signature.clone().map(ByteBuf::from),
        signer_key: r.signer_key.clone().map(ByteBuf::from),
        // Reader-stamped, never on the wire: a reader sets it from its own
        // verdict (ruling (8)(c)).
        signed_as_current: false,
        signed_as: None,
        extra: Default::default(),
    }
}

/// Resolve the recorder handles for a row batch — one lookup per distinct
/// author (the common case is a single author for the whole history).
async fn author_handles(
    db: &crate::db::CacheDb,
    rows: &[FileVersionRow],
) -> Result<std::collections::HashMap<Vec<u8>, Option<String>>, RpcError> {
    let mut handles: std::collections::HashMap<Vec<u8>, Option<String>> = Default::default();
    for r in rows {
        if handles.contains_key(&r.actor_id) {
            continue;
        }
        let handle = match <[u8; 32]>::try_from(r.actor_id.as_slice()) {
            Ok(actor) => db.get_handle(&actor).await.map_err(internal)?,
            Err(_) => None,
        };
        handles.insert(r.actor_id.clone(), handle);
    }
    Ok(handles)
}

// ── fauna.files.versions.list ────────────────────────────────────────────────

fn list_handler() -> RpcHandler {
    Box::new(|state, actor_id, payload| {
        Box::pin(async move {
            require_permission(&state, &actor_id, "fauna.files.versions.list").await?;
            let req: FilesVersionsListRequest = decode(&payload).map_err(malformed)?;
            let path_hash = path_hash_array(&req.path_hash)?;

            // The caller's audience sets (owner + roster member — the
            // admin-discovery grant is filtered out; module doc), optionally
            // narrowed to the one named set. An unknown or unreadable name
            // yields the empty scope — the same absent-not-denied shape as a
            // path with no history (no set-name existence oracle). The named
            // set narrows hash-first (S5b): a present `name_hash` alone selects,
            // so the scope survives the plaintext name blanking.
            let name_hash = crate::routes::parse_name_hash(&req.name_hash, |m| invalid_request(m))?;
            let sets = crate::folder_authz::enumerate_readable_folders(&state.db, &actor_id)
                .await
                .map_err(internal)?;
            let scope: Vec<_> = sets
                .iter()
                .filter(|(fs, grant)| {
                    grant.is_label_audience()
                        && match (name_hash, req.folder.as_deref()) {
                            (Some(h), _) => fs.name_hash.as_deref() == Some(&h[..]),
                            (None, Some(name)) => fs.name == name,
                            (None, None) => true,
                        }
                })
                .collect();
            let scope_ids: Vec<i64> = scope.iter().map(|(fs, _)| fs.id).collect();

            let rows = state
                .db
                .list_file_versions_in_sets(
                    &scope_ids,
                    &path_hash,
                    req.include_pruned.unwrap_or(false),
                )
                .await
                .map_err(internal)?;
            let handles = author_handles(&state.db, &rows).await?;
            let versions = rows
                .iter()
                .map(|r| {
                    let set = scope
                        .iter()
                        .map(|(fs, _)| fs)
                        .find(|fs| fs.id == r.folder_id);
                    row_to_info(r, set, handles.get(&r.actor_id).cloned().flatten())
                })
                .collect();
            let signer_certs = crate::sync_handlers::signer_certs_for_signers(
                &state.db,
                rows.iter().filter_map(|r| {
                    Some((
                        <[u8; 32]>::try_from(r.actor_id.as_slice()).ok()?,
                        <[u8; 32]>::try_from(r.signer_key.as_deref()?).ok()?,
                    ))
                }),
            )
            .await
            .map_err(internal)?;
            encode_reply(&FilesVersionsListReply {
                versions,
                signer_certs,
                extra: Default::default(),
            })
        })
    })
}

// ── fauna.files.versions.get ─────────────────────────────────────────────────

fn get_handler() -> RpcHandler {
    Box::new(|state, actor_id, payload| {
        Box::pin(async move {
            require_permission(&state, &actor_id, "fauna.files.versions.get").await?;
            let req: FilesVersionsGetRequest = decode(&payload).map_err(malformed)?;
            let path_hash = path_hash_array(&req.path_hash)?;

            let row = state
                .db
                .get_file_version_by_seq(&path_hash, req.version_num)
                .await
                .map_err(internal)?
                .ok_or_else(not_found)?;

            // Audience gate: a version in a set the caller is not the label
            // audience of (non-owner, non-member — the admin-discovery grant
            // is filtered out; module doc) is indistinguishable from a missing
            // one (not_found, no oracle).
            let sets = crate::folder_authz::enumerate_readable_folders(&state.db, &actor_id)
                .await
                .map_err(internal)?;
            let set = sets
                .iter()
                .filter(|(_, grant)| grant.is_label_audience())
                .map(|(fs, _)| fs)
                .find(|fs| fs.id == row.folder_id)
                .ok_or_else(not_found)?;
            let handles = author_handles(&state.db, std::slice::from_ref(&row)).await?;
            encode_reply(&row_to_info(
                &row,
                Some(set),
                handles.get(&row.actor_id).cloned().flatten(),
            ))
        })
    })
}

// ── fauna.files.versions.undelete ────────────────────────────────────────────

/// Restore a soft-pruned version to the listable population — the Layer-3
/// recovery verb of the retention pipeline (`file-versions.md` § Retention
/// (3); the `fauna.filesync.snapshot.undelete` twin).
///
/// **Owner-scoped**, deliberately narrower than the read surface: the
/// automatic prune ran per the *owner's* resting policy against the *owner's*
/// storage, so undoing it is the owner's affordance — a roster member neither
/// configures the policy nor pays for the retained bytes. A non-owner (member
/// included) gets the same `not_found` a missing version yields — the reads'
/// no-oracle rule, held on the mutation too.
fn undelete_handler() -> RpcHandler {
    Box::new(|state, actor_id, payload| {
        Box::pin(async move {
            require_permission(&state, &actor_id, "fauna.files.versions.undelete").await?;
            let req: fauna_protocol::files::FilesVersionsUndeleteRequest =
                decode(&payload).map_err(malformed)?;
            let path_hash = path_hash_array(&req.path_hash)?;

            // Resolve regardless of pruned state (`get_file_version_by_seq`
            // deliberately serves soft-pruned rows), then owner-gate on the
            // row's set before touching anything.
            let row = state
                .db
                .get_file_version_by_seq(&path_hash, req.version_num)
                .await
                .map_err(internal)?
                .ok_or_else(not_found)?;
            let fs = state
                .db
                .get_folder_by_id(row.folder_id)
                .await
                .map_err(internal)?
                .ok_or_else(not_found)?;
            if fs.actor_id != actor_id.as_slice() {
                return Err(not_found());
            }

            // `false` = the row is not currently soft-pruned (never pruned, or
            // already undeleted) — indistinguishable from missing, because a
            // "this exists but is not pruned" reply would give the recovery
            // browse nothing actionable the list did not already say.
            if !state
                .db
                .undelete_version(req.version_num)
                .await
                .map_err(internal)?
            {
                return Err(not_found());
            }
            encode_reply(&fauna_protocol::files::FilesVersionsUndeleteReply {
                undeleted: true,
                extra: Default::default(),
            })
        })
    })
}

// ── Registration entry point ────────────────────────────────────────────────

/// Register the authenticated file-versions surface on the **bearer** router.
/// Per-kind replay semantics + rationale: see
/// `KindRegistry::register_files_versions_kinds`. All `forbid_replay = false`
/// @5 s — the two reads are pure, and undelete is an idempotent single-row
/// restore (a replay re-asserts "keep this version").
pub fn register_files_handlers(b: &mut RpcRouterBuilder) {
    let read = |handler| RpcKindMeta {
        forbid_replay: false,
        default_deadline: Duration::from_secs(5),
        handler,
    };
    b.add("fauna.files.versions.list", read(list_handler()));
    b.add("fauna.files.versions.get", read(get_handler()));
    b.add("fauna.files.versions.undelete", read(undelete_handler()));
}
