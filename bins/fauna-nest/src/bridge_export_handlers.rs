//! WS-RPC handlers for the mailbox-export surface
//! (`docs/goal/behavior/mail-export.md`).
//!
//! Twelve **User-class, caller-scoped** kinds, the read-out twin of
//! [`crate::bridge_import_handlers`]. The user's own Fauna app drives the
//! whole pipeline: it pulls its mail down as ciphertext, converts and seals
//! each chunk locally, pushes the sealed frames back up, and finally downloads
//! the assembled blob over `GET /api/v1/export/<session-id>`.
//!
//! # What the nest is, and what it deliberately is not
//!
//! **A chunk relay** (§ Export pipeline, ratified unconditional 2026-07-23).
//! Every message rests sealed to the recipient's MSEK-derived key and the nest
//! holds no read key at all — Phase 3 dropped the last plaintext-custody path
//! — so format conversion runs at the one position where plaintext and the
//! unwrap capability coexist: the user's client. Nothing in this module
//! decodes a message, opens a chunk, or touches the per-session key; the
//! blob-append leg concatenates opaque bytes in the order the store's cursor
//! admits them. A nest-side conversion arm is the **recorded rejected
//! alternative** (§ The rejected alternative), not an optimization anyone may
//! reach for later.
//!
//! # Why every kind is caller-scoped
//!
//! § Cross-actor isolation: "a user exports their own mailboxes only", and the
//! admin cannot trigger an export of another user's mail. That is enforced
//! three times over, deliberately: the allowlist admits only `User`
//! (`bridge_method_allowlist.rs`), no request type carries a target actor at
//! all, and every storage call takes the authenticated actor and filters by it
//! (`db::mail_export`). The first two are what make the third unreachable by
//! construction rather than by discipline.

use std::sync::Arc;
use std::time::Duration;

use fauna_protocol::bridge_routing::{
    BridgeExportCompletePush, BridgeExportErrorPush, BridgeExportProgressPush,
    DiscardExportBlobReply, EXPORT_STREAM_SUPERSEDED, ExportCiphertextMessage, ExportMailboxEntry,
    ExportSessionActionReply, ExportSessionActionRequest, ExportSessionInfo,
    FailExportSessionRequest, FetchExportChunkCiphertextReply, FetchExportChunkCiphertextRequest,
    ListExportSessionsReply, ListExportSessionsRequest, ListOwnMailboxesReply,
    ListOwnMailboxesRequest, MAX_EXPORT_FETCH_BYTES, MAX_EXPORT_FETCH_MESSAGES,
    RestartExportSessionRequest, StartExportSessionReply, StartExportSessionRequest,
    UploadExportChunkReply, UploadExportChunkRequest,
};
use fauna_protocol::decode_strict as decode;

use crate::bridge_imap_handlers::{
    emit_bootstrap_create_records, require_local_mail_serving, split_flags,
};
use crate::bridge_routing_handlers::{encode_reply, internal, malformed, not_found, require_class};
use crate::db::mail_export::{
    AppendExportChunkOutcome, CreateExportSessionOutcome, EXPORT_STATE_COMPLETED,
    EXPORT_STATE_RUNNING, ExportSessionRow, ExportState, export_counter_to_sql,
};
use crate::db::mail_policy::ExportCeilings;
use crate::email_handlers::invalid_params;
use crate::mail_export_blobs::{append_export_blob, export_blob_path_for, unlink_export_blob};
use crate::rpc_router::{RpcKindMeta, RpcRouterBuilder};

/// The actor-authenticated download path (§ Download flow; the route itself is
/// `export_routes::handle_export_session_blob`, inventoried in
/// `api-layers.md` § HTTP residue).
///
/// **Derived, never stored.** § Session row model lists a `download_url`
/// column, but the URL is a pure function of the session id, so a stored copy
/// would be a second truth that a route rename desynchronizes silently. The
/// wire reply carries it; the table does not.
pub fn export_download_url(session_id: &str) -> String {
    format!("/api/v1/export/{session_id}")
}

/// The three formats § Format choices ratifies. A fourth would mean a fourth
/// serializer in `libs/fauna-mail/src/export/`, which § Compile-time decisions
/// names explicitly as a code change and not a knob — so the list is closed
/// here too rather than accepting whatever string a client sends and letting
/// it fail at conversion time, on the client, after a full download.
/// Kept in step with `fauna_mail::export::ExportFormat::wire_name` by that
/// crate's `the_three_wire_format_tokens_are_the_nests_closed_list`, not by a
/// shared const: importing the enum would mean the nest enabling `mail-export`
/// and linking the serializers into its own binary, which is the nest-side
/// conversion shape § Export pipeline records as rejected.
const EXPORT_FORMATS: [&str; 3] = ["mbox", "maildir", "eml-zip"];

fn row_to_info(row: ExportSessionRow) -> ExportSessionInfo {
    // § Architectural rules, "no partial-blob download": the download handle
    // appears only once the session is `completed`, which is the only state in
    // which the blob carries its terminator frame (§ Blob shape on disk). A
    // client that has the id but not the handle therefore cannot ask for a
    // half-archive at all — and the route enforces the same condition itself,
    // because a reply is a convenience and never the gate.
    let download_url = if row.state == EXPORT_STATE_COMPLETED {
        export_download_url(&row.session_id)
    } else {
        String::new()
    };
    ExportSessionInfo {
        session_id: row.session_id,
        format: row.format,
        state: row.state,
        started_at: row.started_at,
        last_progress_at: row.last_progress_at,
        total_count: row.total_count,
        exported_count: row.exported_count,
        skipped_count: row.skipped_count,
        errored_count: row.errored_count,
        last_processed_message_id: row.last_processed_message_id,
        error_reason: row.error_reason,
        blob_bytes: row.blob_bytes,
        expires_at: row.expires_at,
        scope_descriptor: (!row.scope_descriptor.is_empty())
            .then(|| serde_bytes::ByteBuf::from(row.scope_descriptor)),
        // The wrapped key rides the session view because § Key material's whole
        // point is that ANY of the user's clients can open the download, not
        // just the one that started the session. It is wrapped under the actor
        // key and opaque to the nest, so serving it to the owning actor hands
        // out nothing the actor does not already hold the root for.
        wrapped_session_key: row
            .blob_decryption_key_wrapped_for_actor
            .map(serde_bytes::ByteBuf::from),
        download_url,
        next_chunk_idx: row.next_chunk_idx,
        stream_generation: row.stream_generation,
        ..Default::default()
    }
}

/// § Quota composition's ceilings, read from nest state every call — the two
/// the admin chooses and the per-user footprint derived from them.
///
/// Nest state and not a constant because they are **admin choices**
/// (`principles.md` § One configuration surface: a value a human would ever
/// want to choose lives in the app UI + nest state, never a file, env var or
/// flag). The admin write RPC and the `admin-mail` element are gated on
/// ui.yaml ratification (`mail-policy-config.md` § Export), so today every
/// deployment reads the catalog defaults — but it reads them *from the row*,
/// which is what makes wiring the UI a write path rather than a re-plumb.
async fn export_ceilings(state: &Arc<crate::routes::AppState>) -> Result<ExportCeilings, RpcError> {
    Ok(state
        .db
        .get_export_policy()
        .await
        .map_err(internal)?
        .effective())
}

type RpcError = fauna_protocol::RpcError;

/// Refuse a client-supplied counter the store cannot hold exactly — it would
/// otherwise reach [`export_counter_to_sql`]'s floor as an opaque internal
/// error rather than the malformed request it is.
fn require_counter_in_range(name: &str, value: u64) -> Result<(), RpcError> {
    export_counter_to_sql(name, value)
        .map(|_| ())
        .map_err(|_| malformed(format!("{name} {value} is out of range")))
}

/// § Quota composition's per-user footprint: the caller's live exports already
/// hold (or with this chunk would hold) more than `blob_bytes × concurrent`.
/// The remedy differs from `session_blob_oversize`'s — narrowing this export
/// does not help when the disk is held by finished ones — so it is a code of
/// its own, and the message names the remedy.
fn export_footprint_exceeded(held_bytes: u64, ceiling: u64) -> RpcError {
    let mut e = RpcError::new(
        "fauna.bridges.export_footprint_exceeded",
        "error.bridges.export_footprint_exceeded",
    );
    e.details = Some(Box::new(fauna_protocol::Value::String(format!(
        "your exports already hold {held_bytes} of the {ceiling} bytes this nest keeps per user; \
         download and discard a finished export first"
    ))));
    e
}

/// § Quota composition: the caller already holds `max_concurrent` sessions in
/// `running`/`paused`. Typed rather than free-text so the wizard can render
/// "finish or cancel one of your running exports" instead of a generic failure.
fn export_concurrency_cap_reached(cap: u32) -> RpcError {
    let mut e = RpcError::new(
        "fauna.bridges.export_concurrency_cap_reached",
        "error.bridges.export_concurrency_cap_reached",
    );
    e.details = Some(Box::new(fauna_protocol::Value::String(format!(
        "you already have {cap} exports running or paused; finish, cancel or discard one first"
    ))));
    e
}

/// § Quota composition's per-session disk ceiling, surfaced under the name the
/// doc gives it. The user's remedy is to narrow the scope or split the export,
/// so the message says so rather than reporting a number they cannot act on.
fn session_blob_oversize(ceiling: u64) -> RpcError {
    let mut e = RpcError::new(
        "fauna.bridges.session_blob_oversize",
        "error.bridges.session_blob_oversize",
    );
    e.details = Some(Box::new(fauna_protocol::Value::String(format!(
        "this export would exceed the {ceiling}-byte per-session ceiling; \
         narrow the scope or split it into several exports"
    ))));
    e
}

/// The frame index the caller sent is not the one this session expects next
/// (§ Blob shape on disk). Carries the expected index, because the recovery is
/// to resume from it — not to restart an export that may already hold gigabytes.
fn export_chunk_out_of_order(expected: u64) -> RpcError {
    let mut e = RpcError::new(
        "fauna.bridges.export_chunk_out_of_order",
        "error.bridges.export_chunk_out_of_order",
    );
    e.details = Some(Box::new(fauna_protocol::Value::String(format!(
        "the next chunk this session accepts is {expected}"
    ))));
    e
}

/// The caller drives a stream generation that is no longer the session's
/// (`mail-export.md` § Resume): another of the user's devices restarted the
/// export. Typed, because the client's correct reaction is unlike any other
/// refusal's — drop the run and touch nothing, above all not the failure path
/// whose cancel would dispose of the stream that replaced its own.
fn export_stream_superseded(current: u64) -> RpcError {
    let mut e = RpcError::new(
        EXPORT_STREAM_SUPERSEDED,
        "error.bridges.export_stream_superseded",
    );
    e.details = Some(Box::new(fauna_protocol::Value::String(format!(
        "this export was restarted from another app session (stream generation {current})"
    ))));
    e
}

fn notify_export_progress(
    state: &crate::routes::AppState,
    actor: &[u8; 32],
    row: &ExportSessionRow,
) {
    state.ws.notify_push(
        actor,
        fauna_protocol::PushEvent::BridgeExportProgress(BridgeExportProgressPush {
            session_id: row.session_id.clone(),
            exported_count: row.exported_count,
            skipped_count: row.skipped_count,
            errored_count: row.errored_count,
            extra: Default::default(),
        }),
    );
}

/// Run a conditional transition, mapping the store's `None` (no rows moved) to
/// the precise typed error — `not_found` when the session is not the caller's,
/// `invalid_params` when it exists but the transition does not apply.
///
/// The convergence arm mirrors the import twin's: all four transition kinds are
/// `forbid_replay: false`, which asserts the handler is naturally idempotent,
/// so a call landing after the session already reached the target state must
/// answer with the row rather than a wrong-state error for something that
/// already got what it asked for.
///
/// `expected_generation` is § Resume's only-while-I-am-still-the-driver
/// condition, and a mismatch is decided **before** the convergence arm: a
/// superseded driver whose `resume` finds the session already `running` has not
/// "got what it asked for" — the stream running is somebody else's.
async fn transition_or_typed_err(
    state: &Arc<crate::routes::AppState>,
    actor: &[u8; 32],
    session_id: &str,
    new_state: ExportState,
    allowed_from: &[ExportState],
    error_reason: Option<&str>,
    expected_generation: Option<u64>,
) -> Result<ExportSessionRow, RpcError> {
    if let Some(row) = state
        .db
        .transition_export_session(
            actor,
            session_id,
            new_state,
            allowed_from,
            error_reason,
            expected_generation,
        )
        .await
        .map_err(internal)?
    {
        return Ok(row);
    }
    match state
        .db
        .get_export_session(actor, session_id)
        .await
        .map_err(internal)?
    {
        None => Err(not_found(format!(
            "export session '{session_id}' not found"
        ))),
        Some(row) if expected_generation.is_some_and(|g| g != row.stream_generation) => {
            Err(export_stream_superseded(row.stream_generation))
        }
        Some(row) if row.state == new_state.as_str() => Ok(row),
        Some(row) => Err(invalid_params(&format!(
            "export session '{session_id}' is {}; cannot move to {}",
            row.state,
            new_state.as_str()
        ))),
    }
}

// ── list_own_mailboxes ────────────────────────────────────────────

fn list_own_mailboxes_handler() -> crate::rpc_router::RpcHandler {
    Box::new(|state, actor_id, payload| {
        Box::pin(async move {
            require_class(&state, &actor_id, "fauna.bridges.list_own_mailboxes").await?;
            let _req: ListOwnMailboxesRequest = decode(&payload).map_err(malformed)?;
            // The caller IS the target — there is no second actor to resolve.
            // That asymmetry with the MDA's `list_mailboxes` is the whole
            // reason this is a separate kind (`mail-export.md` § Wire shapes).
            require_local_mail_serving(&state, &actor_id).await?;
            let newly_seeded = state
                .db
                .ensure_bridge_imap_mailboxes(&actor_id)
                .await
                .map_err(internal)?;
            emit_bootstrap_create_records(&state, &actor_id, newly_seeded).await?;
            let rows = state
                .db
                .list_bridge_imap_mailbox_state(&actor_id)
                .await
                .map_err(internal)?;
            let mut mailboxes = Vec::with_capacity(rows.len());
            for row in rows {
                let (exists, _unseen) = state
                    .db
                    .count_bridge_imap_mailbox(&actor_id, &row.name)
                    .await
                    .map_err(internal)?;
                mailboxes.push(ExportMailboxEntry {
                    name: row.name,
                    exists,
                    uid_validity: row.uid_validity,
                    ..Default::default()
                });
            }
            // Ascending by the name's raw bytes — the order § Container shape's
            // determinism contract emits entries in. Sorting here means the
            // wizard's checklist and the archive agree without every client
            // re-deriving the same total order.
            mailboxes.sort_by(|a, b| a.name.as_bytes().cmp(b.name.as_bytes()));
            encode_reply(&ListOwnMailboxesReply {
                mailboxes,
                ..Default::default()
            })
        })
    })
}

// ── start_export_session ──────────────────────────────────────────

fn start_export_session_handler() -> crate::rpc_router::RpcHandler {
    Box::new(|state, actor_id, payload| {
        Box::pin(async move {
            require_class(&state, &actor_id, "fauna.bridges.start_export_session").await?;
            let req: StartExportSessionRequest = decode(&payload).map_err(malformed)?;
            if !EXPORT_FORMATS.contains(&req.format.as_str()) {
                return Err(malformed(format!(
                    "format must be one of {}",
                    EXPORT_FORMATS.join(" / ")
                )));
            }
            // A session with no wrapped key produces a blob NO client can ever
            // open — § Key material makes the stored wrapped blob the only path
            // from the actor key to the archive. Refusing at the door is the
            // difference between a typed error now and a 10 GiB artifact that
            // resting for 30 days serves nobody.
            let wrapped = match req.wrapped_session_key.as_ref() {
                Some(k) if !k.is_empty() => k,
                _ => {
                    return Err(malformed(
                        "wrapped_session_key is required: a session without one produces a blob \
                         no client can open",
                    ));
                }
            };
            require_counter_in_range("total_count", req.total_count)?;
            require_local_mail_serving(&state, &actor_id).await?;
            let ceilings = export_ceilings(&state).await?;
            let session_id = uuid::Uuid::new_v4().to_string();
            // § Reclaim rule 1 — row before file: the row carrying `blob_path`
            // is committed here, and the file is created only by the first
            // accepted `upload_export_chunk`. The orphan reclaim lists the
            // directory before it reads the rows, which is race-free only
            // under exactly this ordering.
            let blob_path = export_blob_path_for(&session_id);
            match state
                .db
                .create_export_session(
                    &actor_id,
                    &session_id,
                    &req.format,
                    &req.scope_descriptor,
                    &blob_path,
                    Some(&wrapped[..]),
                    &ceilings,
                )
                .await
                .map_err(internal)?
            {
                CreateExportSessionOutcome::Created => {
                    if req.total_count > 0 {
                        state
                            .db
                            .record_export_progress(
                                &actor_id,
                                &session_id,
                                0,
                                0,
                                0,
                                None,
                                Some(req.total_count),
                                None,
                            )
                            .await
                            .map_err(internal)?;
                    }
                    encode_reply(&StartExportSessionReply {
                        session_id,
                        ..Default::default()
                    })
                }
                CreateExportSessionOutcome::ConcurrencyCapReached => {
                    Err(export_concurrency_cap_reached(ceilings.concurrent))
                }
                CreateExportSessionOutcome::FootprintCapReached {
                    held_bytes,
                    ceiling,
                } => Err(export_footprint_exceeded(held_bytes, ceiling)),
            }
        })
    })
}

// ── list_export_sessions ──────────────────────────────────────────

fn list_export_sessions_handler() -> crate::rpc_router::RpcHandler {
    Box::new(|state, actor_id, payload| {
        Box::pin(async move {
            require_class(&state, &actor_id, "fauna.bridges.list_export_sessions").await?;
            let _req: ListExportSessionsRequest = decode(&payload).map_err(malformed)?;
            let rows = state
                .db
                .list_export_sessions(&actor_id)
                .await
                .map_err(internal)?;
            encode_reply(&ListExportSessionsReply {
                sessions: rows.into_iter().map(row_to_info).collect(),
                ..Default::default()
            })
        })
    })
}

// ── fetch_export_chunk_ciphertext — the pipeline's down-leg ───────

fn fetch_export_chunk_ciphertext_handler() -> crate::rpc_router::RpcHandler {
    Box::new(|state, actor_id, payload| {
        Box::pin(async move {
            require_class(
                &state,
                &actor_id,
                "fauna.bridges.fetch_export_chunk_ciphertext",
            )
            .await?;
            let req: FetchExportChunkCiphertextRequest = decode(&payload).map_err(malformed)?;
            let row = state
                .db
                .get_export_session(&actor_id, &req.session_id)
                .await
                .map_err(internal)?
                .ok_or_else(|| {
                    not_found(format!("export session '{}' not found", req.session_id))
                })?;
            if row.state != EXPORT_STATE_RUNNING {
                return Err(invalid_params(&format!(
                    "export session '{}' is {}; resume it before fetching",
                    req.session_id, row.state
                )));
            }
            require_local_mail_serving(&state, &actor_id).await?;

            let limit = match req.max_messages {
                0 => MAX_EXPORT_FETCH_MESSAGES,
                n => n.min(MAX_EXPORT_FETCH_MESSAGES),
            };
            let byte_budget = match req.max_bytes {
                0 => MAX_EXPORT_FETCH_BYTES,
                n => n.min(MAX_EXPORT_FETCH_BYTES),
            };
            // One row past the limit, so "the mailbox ended" is a fact about
            // the query rather than an inference from a short page — a UID gap
            // would otherwise read as the end of the mailbox and silently
            // truncate the archive.
            let metas = state
                .db
                .query_bridge_imap_messages(
                    &actor_id,
                    &req.mailbox,
                    None,
                    (req.after_uid > 0).then_some(req.after_uid),
                    None,
                    Some(limit + 1),
                )
                .await
                .map_err(internal)?;
            let mut mailbox_done = metas.len() as u32 <= limit;
            let mut next_after_uid = req.after_uid;
            let mut used: u64 = 0;
            let mut messages: Vec<ExportCiphertextMessage> = Vec::new();

            for meta in metas.into_iter().take(limit as usize) {
                // The sealed record, exactly as it rests. `read_sealed_body_
                // with_floor` resolves a continuation head into its parts, so
                // this leg is identical whether the message rested inline or
                // as parts — the export must not care.
                let Some((body, _hint, floor)) =
                    crate::segments::mail::read_sealed_body_with_floor(
                        &state.mail_segments,
                        &state.db,
                        &actor_id,
                        &meta.message_id,
                    )
                    .await
                    .map_err(internal)?
                else {
                    // The placement row outlived its record. Skipping advances
                    // the cursor, so the walk cannot wedge on it; the client
                    // counts it as skipped when it sees the UID gap close.
                    next_after_uid = meta.uid;
                    continue;
                };
                // Byte budget: always admit the first message, so a single
                // record larger than the whole budget still crosses (it rides
                // the byte plane) instead of wedging the walk forever.
                if !messages.is_empty() && used.saturating_add(body.len() as u64) > byte_budget {
                    mailbox_done = false;
                    break;
                }
                used = used.saturating_add(body.len() as u64);
                let reply_msg =
                    if fauna_mail::body_ref::mail_body_needs_reference(body.len() as u64, 0) {
                        // Over the RPC frame budget: stage the ciphertext on
                        // the bulk-byte plane and hand back a reference the
                        // client GETs. Same escape `fetch_message_ciphertext`
                        // uses — and the only reason a mailbox holding a large
                        // attachment can be exported at all.
                        let body_ref =
                            crate::mail_body_plane::stage_sealed_body(&state, &body).await?;
                        ExportCiphertextMessage {
                            mailbox: req.mailbox.clone(),
                            uid: meta.uid,
                            message_id: meta.message_id.to_vec(),
                            flags: split_flags(&meta.flags),
                            internal_date: floor.timestamp,
                            // ms → unix seconds, the unknown `0` passing
                            // through — `inbox.fetch`'s projection exactly.
                            stored_at: meta.stored_at.max(0) / 1000,
                            sealed_body: Vec::new(),
                            ciphertext_size: floor.ciphertext_size,
                            body_ref: Some(body_ref),
                            ..Default::default()
                        }
                    } else {
                        ExportCiphertextMessage {
                            mailbox: req.mailbox.clone(),
                            uid: meta.uid,
                            message_id: meta.message_id.to_vec(),
                            flags: split_flags(&meta.flags),
                            internal_date: floor.timestamp,
                            stored_at: meta.stored_at.max(0) / 1000,
                            sealed_body: body,
                            ciphertext_size: floor.ciphertext_size,
                            body_ref: None,
                            ..Default::default()
                        }
                    };
                next_after_uid = meta.uid;
                messages.push(reply_msg);
            }

            encode_reply(&FetchExportChunkCiphertextReply {
                messages,
                next_after_uid,
                mailbox_done,
                ..Default::default()
            })
        })
    })
}

// ── upload_export_chunk — the pipeline's up-leg ───────────────────

fn upload_export_chunk_handler() -> crate::rpc_router::RpcHandler {
    Box::new(|state, actor_id, payload| {
        Box::pin(async move {
            require_class(&state, &actor_id, "fauna.bridges.upload_export_chunk").await?;
            let req: UploadExportChunkRequest = decode(&payload).map_err(malformed)?;
            if req.sealed_chunk.is_empty() {
                return Err(malformed("sealed_chunk must not be empty"));
            }
            // Before the reservation, so a refused counter leaves no frame on
            // disk whose batch was never folded.
            require_counter_in_range("exported_delta", req.exported_delta)?;
            require_counter_in_range("skipped_delta", req.skipped_delta)?;
            require_counter_in_range("errored_delta", req.errored_delta)?;
            if let Some(total) = req.revised_total_count {
                require_counter_in_range("revised_total_count", total)?;
            }
            let ceilings = export_ceilings(&state).await?;

            // The reservation comes first and decides everything: the session
            // must be `running`, the frame must be the expected one, and the
            // ceiling is checked against a total that only this mutex advances.
            // Only then do bytes reach the disk — a refused chunk must leave
            // nothing behind, or a client that narrows its scope and retries is
            // locked out by its own rejected upload.
            let blob_path = match state
                .db
                .append_export_blob_bytes(
                    &actor_id,
                    &req.session_id,
                    req.chunk_idx,
                    req.sealed_chunk.len() as u64,
                    &ceilings,
                    req.stream_generation,
                )
                .await
                .map_err(internal)?
            {
                AppendExportChunkOutcome::NoSuchSession => {
                    // Ambiguous between "not yours / gone" and "not running",
                    // because the reservation's SELECT filters both. Re-read to
                    // say which, so a paused session reads as resumable rather
                    // than lost.
                    return Err(
                        match state
                            .db
                            .get_export_session(&actor_id, &req.session_id)
                            .await
                            .map_err(internal)?
                        {
                            None => {
                                not_found(format!("export session '{}' not found", req.session_id))
                            }
                            Some(row) => invalid_params(&format!(
                                "export session '{}' is {}; resume it before uploading",
                                req.session_id, row.state
                            )),
                        },
                    );
                }
                AppendExportChunkOutcome::ChunkOutOfOrder { expected } => {
                    return Err(export_chunk_out_of_order(expected));
                }
                AppendExportChunkOutcome::BlobOversize { ceiling, .. } => {
                    return Err(session_blob_oversize(ceiling));
                }
                AppendExportChunkOutcome::FootprintExceeded {
                    held_bytes,
                    ceiling,
                } => {
                    return Err(export_footprint_exceeded(held_bytes, ceiling));
                }
                AppendExportChunkOutcome::StreamSuperseded { current } => {
                    return Err(export_stream_superseded(current));
                }
                // ⚠ The path comes from the reservation, read under the same
                // lock as the generation check — never from a second look at
                // the row. A restart may land between here and the append, and
                // it repoints the row at the NEW generation's file: a re-read
                // would put this frame at the head of somebody else's stream.
                // Appended to the path it reserved under, a frame that lost
                // that race lands in (or fails to find) the OLD file, which no
                // row names any more (§ Resume, § Reclaim rule 3).
                AppendExportChunkOutcome::Appended { blob_path, .. } => blob_path,
            };

            let data_dir = crate::mail_enable::data_dir_from_db_path(&state.config.nest.db_path)
                .ok_or_else(|| {
                    internal(anyhow::anyhow!(
                        "nest data dir cannot be derived from db_path"
                    ))
                })?;
            append_export_blob(&data_dir, &blob_path, req.chunk_idx, &req.sealed_chunk)
                .await
                .map_err(internal)?;

            // Progress folds only after the append succeeded, and only into a
            // session still in flight (`record_export_progress` refuses a
            // terminal one in its own UPDATE, so no counter bump re-arms a
            // finished export's 30-day window). It is conditioned on the
            // generation the frame was reserved under, because a restart
            // landing since then has zeroed these counters for the new stream.
            let Some(row) = state
                .db
                .record_export_progress(
                    &actor_id,
                    &req.session_id,
                    req.exported_delta,
                    req.skipped_delta,
                    req.errored_delta,
                    (!req.last_processed_message_id.is_empty())
                        .then_some(req.last_processed_message_id.as_str()),
                    req.revised_total_count,
                    Some(req.stream_generation),
                )
                .await
                .map_err(internal)?
            else {
                return Err(
                    match state
                        .db
                        .get_export_session(&actor_id, &req.session_id)
                        .await
                        .map_err(internal)?
                    {
                        Some(row) if row.stream_generation != req.stream_generation => {
                            export_stream_superseded(row.stream_generation)
                        }
                        // A cancel or finalize landed between the append and
                        // the fold: the session is no longer in flight.
                        Some(row) => invalid_params(&format!(
                            "export session '{}' is {}; this chunk's progress was not counted",
                            req.session_id, row.state
                        )),
                        None => not_found(format!("export session '{}' not found", req.session_id)),
                    },
                );
            };
            notify_export_progress(&state, &actor_id, &row);
            encode_reply(&UploadExportChunkReply {
                blob_bytes: row.blob_bytes,
                next_chunk_idx: row.next_chunk_idx,
                exported_count: row.exported_count,
                skipped_count: row.skipped_count,
                errored_count: row.errored_count,
                ..Default::default()
            })
        })
    })
}

// ── pause / resume / cancel / finalize / fail ─────────────────────

fn session_transition_handler(
    kind: &'static str,
    new_state: ExportState,
    allowed_from: &'static [ExportState],
) -> crate::rpc_router::RpcHandler {
    Box::new(move |state, actor_id, payload| {
        Box::pin(async move {
            require_class(&state, &actor_id, kind).await?;
            let req: ExportSessionActionRequest = decode(&payload).map_err(malformed)?;
            let row = transition_or_typed_err(
                &state,
                &actor_id,
                &req.session_id,
                new_state,
                allowed_from,
                None,
                req.stream_generation,
            )
            .await?;

            if new_state == ExportState::Cancelled {
                // § Wire shapes: cancel is "abort + unlink blob". The row stays
                // (the wizard still lists a cancelled session) but its bytes go
                // immediately — a half-written whole-mailbox snapshot has no
                // reader, and holding it for 30 days would be the obligation
                // § Expiry exists to refuse. The row keeps naming the path,
                // which is harmless: the unlink is idempotent and the orphan
                // reclaim only ever removes files no row names.
                if let Some(data_dir) =
                    crate::mail_enable::data_dir_from_db_path(&state.config.nest.db_path)
                {
                    unlink_export_blob(&data_dir, &row.blob_path)
                        .await
                        .map_err(internal)?;
                }
            }
            if new_state == ExportState::Completed {
                state.ws.notify_push(
                    &actor_id,
                    fauna_protocol::PushEvent::BridgeExportComplete(BridgeExportCompletePush {
                        session_id: row.session_id.clone(),
                        download_url: export_download_url(&row.session_id),
                        blob_bytes: row.blob_bytes,
                        extra: Default::default(),
                    }),
                );
            }
            encode_reply(&ExportSessionActionReply {
                session: row_to_info(row),
                ..Default::default()
            })
        })
    })
}

// ── restart_export_session — the cold resume ──────────────────────

fn restart_export_session_handler() -> crate::rpc_router::RpcHandler {
    Box::new(|state, actor_id, payload| {
        Box::pin(async move {
            require_class(&state, &actor_id, "fauna.bridges.restart_export_session").await?;
            let req: RestartExportSessionRequest = decode(&payload).map_err(malformed)?;
            // The same door `start_export_session` keeps: a generation with no
            // wrapped key is a blob no client can ever open — and here the
            // restart would also have thrown away the key that opened the old
            // one.
            let wrapped = match req.wrapped_session_key.as_ref() {
                Some(k) if !k.is_empty() => k,
                _ => {
                    return Err(malformed(
                        "wrapped_session_key is required: each stream generation is sealed \
                         under a key of its own",
                    ));
                }
            };
            require_counter_in_range("total_count", req.total_count)?;
            // ⚠ No "is the driver really gone?" check, deliberately (§ Resume):
            // the nest cannot see one — the frames carry no device identity and
            // a last-progress age is a wall-clock guess — so the user's Resume
            // is taken at its word, and the generation bumped here is what
            // shuts a driver that turns out to be alive out of the new stream.
            let Some((row, previous_blob_path)) = state
                .db
                .restart_export_session(
                    &actor_id,
                    &req.session_id,
                    wrapped,
                    (req.total_count > 0).then_some(req.total_count),
                )
                .await
                .map_err(internal)?
            else {
                return Err(
                    match state
                        .db
                        .get_export_session(&actor_id, &req.session_id)
                        .await
                        .map_err(internal)?
                    {
                        None => not_found(format!("export session '{}' not found", req.session_id)),
                        Some(row) => invalid_params(&format!(
                            "export session '{}' is {}; only a running or paused export can be \
                             restarted",
                            req.session_id, row.state
                        )),
                    },
                );
            };
            // Row first, file after (§ Reclaim, the restart sentence): the old
            // generation's file is named by no row from the UPDATE above, so a
            // failure here costs disk until the orphan reclaim's next pass and
            // nothing else — it must not fail a restart that has already
            // happened.
            if let Some(data_dir) =
                crate::mail_enable::data_dir_from_db_path(&state.config.nest.db_path)
                && let Err(e) = unlink_export_blob(&data_dir, &previous_blob_path).await
            {
                tracing::warn!(
                    blob_path = previous_blob_path,
                    error = %e,
                    "export restart: the previous stream generation's blob would not unlink; \
                     the orphan reclaim will collect it"
                );
            }
            notify_export_progress(&state, &actor_id, &row);
            encode_reply(&ExportSessionActionReply {
                session: row_to_info(row),
                ..Default::default()
            })
        })
    })
}

// ── discard_export_blob ───────────────────────────────────────────

fn discard_export_blob_handler() -> crate::rpc_router::RpcHandler {
    Box::new(|state, actor_id, payload| {
        Box::pin(async move {
            require_class(&state, &actor_id, "fauna.bridges.discard_export_blob").await?;
            let req: ExportSessionActionRequest = decode(&payload).map_err(malformed)?;
            let Some(row) = state
                .db
                .get_export_session(&actor_id, &req.session_id)
                .await
                .map_err(internal)?
            else {
                // The same answer a second discard gives — idempotent from the
                // client's side, and the wizard's "Discard now" button should
                // not fail because the user pressed it twice.
                return encode_reply(&DiscardExportBlobReply {
                    existed: false,
                    ..Default::default()
                });
            };
            // ⚠ § Reclaim rule 2 — FILE BEFORE ROW. An unlink that fails leaves
            // the row standing, so the discard is retryable; the reverse order
            // deletes the only record of where a whole-mailbox snapshot is.
            if let Some(data_dir) =
                crate::mail_enable::data_dir_from_db_path(&state.config.nest.db_path)
            {
                unlink_export_blob(&data_dir, &row.blob_path)
                    .await
                    .map_err(internal)?;
            }
            // The delete names the file it follows, so it lands only while the
            // row still names what was just unlinked.
            if state
                .db
                .delete_export_session(&actor_id, &req.session_id, &row.blob_path)
                .await
                .map_err(internal)?
            {
                return encode_reply(&DiscardExportBlobReply {
                    existed: true,
                    ..Default::default()
                });
            }
            match state
                .db
                .get_export_session(&actor_id, &req.session_id)
                .await
                .map_err(internal)?
            {
                // A concurrent discard got there first — the answer a second
                // press gives.
                None => encode_reply(&DiscardExportBlobReply {
                    existed: false,
                    ..Default::default()
                }),
                // A cold resume (§ Resume) repointed the row at a new stream's
                // file between the read and the delete. That file was not
                // unlinked, so the row stays; the one this call removed was the
                // generation the restart had already disowned.
                Some(_) => Err(invalid_params(&format!(
                    "export session '{}' was restarted while it was being discarded; \
                     discard it again",
                    req.session_id
                ))),
            }
        })
    })
}

/// Record a session-fatal condition, unlink the partial blob, and push
/// `BridgeExportError` — the body of `fauna.bridges.fail_export_session`
/// (`mail-export.md` § Resume), kept as one function so the transition, the
/// unlink and the push can never drift apart.
///
/// The client owns the conversion, so the errors an export can suffer are the
/// client's to classify: the nest records the reason it is handed and judges
/// none of it.
///
/// `expected_generation` is § Resume's only-while-I-am-still-the-driver
/// condition. A failure is always *a driver's*, and one that is this device's
/// alone — a dropped fetch — may land after another device restarted the
/// export, so it must dispose of nothing: the drive loop passes
/// `Some(generation)` and hears [`EXPORT_STREAM_SUPERSEDED`] when it is no
/// longer the driver.
///
/// **The blob order is `cancel_export_session`'s, not § Reclaim rule 2's.**
/// Rule 2 (file before row) governs the paths that *delete the row*; here the
/// row survives and keeps naming the path, which is harmless — the unlink is
/// idempotent and the orphan sweep only ever removes files no row names.
/// Unlinking first would instead be the one order that can lose the record
/// this kind exists to keep: the bytes gone under a row still reading
/// `running`, had the transition then failed.
pub async fn fail_export_session(
    state: &Arc<crate::routes::AppState>,
    actor: &[u8; 32],
    session_id: &str,
    reason: &str,
    expected_generation: Option<u64>,
) -> Result<ExportSessionRow, RpcError> {
    let row = transition_or_typed_err(
        state,
        actor,
        session_id,
        ExportState::Errored,
        &[ExportState::Running, ExportState::Paused],
        Some(reason),
        expected_generation,
    )
    .await?;
    // The partial blob has no terminator frame and can never be opened, so it
    // goes the way a cancel's does rather than resting out the whole § Expiry
    // window on the user's footprint. What a failed session keeps, and a
    // cancelled one does not, is the record of why.
    if let Some(data_dir) = crate::mail_enable::data_dir_from_db_path(&state.config.nest.db_path) {
        unlink_export_blob(&data_dir, &row.blob_path)
            .await
            .map_err(internal)?;
    }
    state.ws.notify_push(
        actor,
        fauna_protocol::PushEvent::BridgeExportError(BridgeExportErrorPush {
            session_id: row.session_id.clone(),
            reason: reason.to_string(),
            extra: Default::default(),
        }),
    );
    Ok(row)
}

fn fail_export_session_handler() -> crate::rpc_router::RpcHandler {
    Box::new(|state, actor_id, payload| {
        Box::pin(async move {
            require_class(&state, &actor_id, "fauna.bridges.fail_export_session").await?;
            let req: FailExportSessionRequest = decode(&payload).map_err(malformed)?;
            // The import twin's refusal (`fail_import_session`): a reason that
            // says nothing leaves the `cancelled`-shaped row this kind exists
            // to improve on, and the client always has one to give.
            if req.reason.is_empty() {
                return Err(malformed("reason must not be empty"));
            }
            let row = fail_export_session(
                &state,
                &actor_id,
                &req.session_id,
                &req.reason,
                req.stream_generation,
            )
            .await?;
            encode_reply(&ExportSessionActionReply {
                session: row_to_info(row),
                ..Default::default()
            })
        })
    })
}

pub fn register_bridge_export_handlers(b: &mut RpcRouterBuilder) {
    let fetch = Duration::from_secs(5);
    // 60 s: a down-leg page carries up to 16 MiB of sealed records, and an
    // up-leg frame is the same budget (§ Export pipeline's 16 MiB chunks).
    let routing = Duration::from_secs(60);

    b.add(
        "fauna.bridges.list_own_mailboxes",
        RpcKindMeta {
            forbid_replay: false,
            default_deadline: fetch,
            handler: list_own_mailboxes_handler(),
        },
    );
    // `forbid_replay: true` — the session id is minted server-side and, unlike
    // the import twin, no per-source lock stops a replay: it opens a SECOND
    // session, burning one of § Quota composition's three concurrency slots and
    // leaving a 30-day orphan row the caller cannot name. The honest
    // `RpcDisconnected { was_in_flight }` sends the client to
    // `list_export_sessions`, which is the documented resume path anyway.
    b.add(
        "fauna.bridges.start_export_session",
        RpcKindMeta {
            forbid_replay: true,
            default_deadline: fetch,
            handler: start_export_session_handler(),
        },
    );
    b.add(
        "fauna.bridges.list_export_sessions",
        RpcKindMeta {
            forbid_replay: false,
            default_deadline: fetch,
            handler: list_export_sessions_handler(),
        },
    );
    // A read plus an idempotent stage of derived bytes on the byte plane
    // (content-addressed, so a repeat lands the same chunks).
    b.add(
        "fauna.bridges.fetch_export_chunk_ciphertext",
        RpcKindMeta {
            forbid_replay: false,
            default_deadline: routing,
            handler: fetch_export_chunk_ciphertext_handler(),
        },
    );
    // `forbid_replay: true` — an accumulator (the counters) AND an append (the
    // frame), so a replay would double-count and double-append. The chunk-index
    // guard turns the second attempt into a typed out-of-order refusal carrying
    // the expected index: recoverable, but an answer to reach deliberately.
    b.add(
        "fauna.bridges.upload_export_chunk",
        RpcKindMeta {
            forbid_replay: true,
            default_deadline: routing,
            handler: upload_export_chunk_handler(),
        },
    );
    // The five transitions are at-most-once by construction: each is one keyed
    // `UPDATE … WHERE state IN (allowed_from)`, and no target state is in its
    // own `allowed_from`, so a replay landing after the first application
    // matches zero rows and converges on the already-reached state.
    b.add(
        "fauna.bridges.pause_export_session",
        RpcKindMeta {
            forbid_replay: false,
            default_deadline: fetch,
            handler: session_transition_handler(
                "fauna.bridges.pause_export_session",
                ExportState::Paused,
                &[ExportState::Running],
            ),
        },
    );
    b.add(
        "fauna.bridges.resume_export_session",
        RpcKindMeta {
            forbid_replay: false,
            default_deadline: fetch,
            handler: session_transition_handler(
                "fauna.bridges.resume_export_session",
                ExportState::Running,
                &[ExportState::Paused],
            ),
        },
    );
    // `forbid_replay: true` — every application opens a NEW stream generation,
    // so a replay would supersede the stream the first one just opened, whose
    // generation the caller never learned (its reply is what was lost).
    b.add(
        "fauna.bridges.restart_export_session",
        RpcKindMeta {
            forbid_replay: true,
            default_deadline: fetch,
            handler: restart_export_session_handler(),
        },
    );
    b.add(
        "fauna.bridges.cancel_export_session",
        RpcKindMeta {
            forbid_replay: false,
            default_deadline: fetch,
            handler: session_transition_handler(
                "fauna.bridges.cancel_export_session",
                ExportState::Cancelled,
                &[ExportState::Running, ExportState::Paused],
            ),
        },
    );
    b.add(
        "fauna.bridges.finalize_export_session",
        RpcKindMeta {
            forbid_replay: false,
            default_deadline: fetch,
            handler: session_transition_handler(
                "fauna.bridges.finalize_export_session",
                ExportState::Completed,
                &[ExportState::Running],
            ),
        },
    );
    // Same at-most-once shape as the four transitions above, plus an unlink
    // that is itself idempotent — so a replay landing on the already-`errored`
    // session converges on it and removes a file that is already gone.
    b.add(
        "fauna.bridges.fail_export_session",
        RpcKindMeta {
            forbid_replay: false,
            default_deadline: fetch,
            handler: fail_export_session_handler(),
        },
    );
    b.add(
        "fauna.bridges.discard_export_blob",
        RpcKindMeta {
            forbid_replay: false,
            default_deadline: fetch,
            handler: discard_export_blob_handler(),
        },
    );
}

#[cfg(test)]
mod tests {
    use super::*;
    use bytes::Bytes;
    use fauna_protocol::encode_canonical;

    use crate::db::CacheDb;
    use crate::db::mail_export::{EXPORT_STATE_CANCELLED, EXPORT_STATE_ERRORED};
    use crate::routes::AppState;

    const USER: [u8; 32] = [7u8; 32];
    const OTHER: [u8; 32] = [8u8; 32];

    /// An `AppState` whose `db_path` lives in a tempdir, so the data dir the
    /// blob writer derives (`db_path`'s parent) is one the test owns.
    ///
    /// The bare `AppState::for_test` has an empty `db_path` and therefore no
    /// data dir at all — which is exactly the state in which `upload_export_
    /// chunk` has nowhere to write. Every test here touches the blob file, so
    /// every test needs the real thing rather than a mock.
    async fn fixture() -> (Arc<AppState>, tempfile::TempDir) {
        let db = Arc::new(CacheDb::open_in_memory().unwrap());
        let dir = tempfile::tempdir().unwrap();
        let db_path = dir.path().join("nest.db").to_string_lossy().into_owned();
        let base = AppState::for_test(db);
        let config = Arc::new(crate::config::NestConfig {
            nest: crate::config::NestSection {
                db_path,
                ..base.config.nest.clone()
            },
            ..(*base.config).clone()
        });
        let state = Arc::new(AppState { config, ..base });
        // The dispatch gate classes an actor `User` only when a `users` row
        // exists; `require_local_mail_serving` additionally needs the actor to
        // be a local mail-serving one. Fixture setup, not the action under test.
        state.db.create_user(&USER, "free", "test").await.unwrap();
        state.db.create_user(&OTHER, "free", "other").await.unwrap();
        crate::test_support::seed_recipient_seal_key(
            &state.db,
            &USER,
            &crate::test_support::FIXTURE_MSEK,
        )
        .await;
        crate::test_support::seed_recipient_seal_key(
            &state.db,
            &OTHER,
            &crate::test_support::FIXTURE_MSEK,
        )
        .await;
        (state, dir)
    }

    fn payload<T: serde::Serialize>(req: &T) -> Bytes {
        Bytes::from(encode_canonical(req).unwrap().to_vec())
    }

    async fn start(state: &Arc<AppState>, actor: [u8; 32]) -> String {
        let req = StartExportSessionRequest {
            format: "mbox".into(),
            scope_descriptor: b"scope-cbor".to_vec(),
            wrapped_session_key: Some(serde_bytes::ByteBuf::from(b"wrapped".to_vec())),
            total_count: 3,
            ..Default::default()
        };
        let bytes = start_export_session_handler()(state.clone(), actor, payload(&req))
            .await
            .expect("start ok");
        let reply: StartExportSessionReply = fauna_cbor::decode_strict(&bytes).unwrap();
        reply.session_id
    }

    async fn upload(
        state: &Arc<AppState>,
        actor: [u8; 32],
        sid: &str,
        idx: u64,
        body: &[u8],
    ) -> Result<UploadExportChunkReply, RpcError> {
        let req = UploadExportChunkRequest {
            session_id: sid.into(),
            chunk_idx: idx,
            sealed_chunk: body.to_vec(),
            exported_delta: 1,
            skipped_delta: 0,
            errored_delta: 0,
            last_processed_message_id: format!("m{idx}"),
            ..Default::default()
        };
        let bytes = upload_export_chunk_handler()(state.clone(), actor, payload(&req)).await?;
        Ok(fauna_cbor::decode_strict(&bytes).unwrap())
    }

    /// [`upload`] as the driver of stream generation `generation`.
    async fn upload_as(
        state: &Arc<AppState>,
        sid: &str,
        generation: u64,
        idx: u64,
        body: &[u8],
    ) -> Result<UploadExportChunkReply, RpcError> {
        let req = UploadExportChunkRequest {
            session_id: sid.into(),
            chunk_idx: idx,
            sealed_chunk: body.to_vec(),
            exported_delta: 1,
            stream_generation: generation,
            ..Default::default()
        };
        let bytes = upload_export_chunk_handler()(state.clone(), USER, payload(&req)).await?;
        Ok(fauna_cbor::decode_strict(&bytes).unwrap())
    }

    async fn restart(
        state: &Arc<AppState>,
        actor: [u8; 32],
        sid: &str,
        key: Option<&[u8]>,
    ) -> Result<ExportSessionInfo, RpcError> {
        let req = RestartExportSessionRequest {
            session_id: sid.into(),
            wrapped_session_key: key.map(|k| serde_bytes::ByteBuf::from(k.to_vec())),
            total_count: 0,
            ..Default::default()
        };
        let bytes = restart_export_session_handler()(state.clone(), actor, payload(&req)).await?;
        let reply: ExportSessionActionReply = fauna_cbor::decode_strict(&bytes).unwrap();
        Ok(reply.session)
    }

    /// A driver's own cancel: conditioned on the generation it drives.
    async fn cancel_as_driver(
        state: &Arc<AppState>,
        sid: &str,
        generation: u64,
    ) -> Result<ExportSessionInfo, RpcError> {
        let req = ExportSessionActionRequest {
            session_id: sid.into(),
            stream_generation: Some(generation),
            ..Default::default()
        };
        let bytes = session_transition_handler(
            "fauna.bridges.cancel_export_session",
            ExportState::Cancelled,
            &[ExportState::Running, ExportState::Paused],
        )(state.clone(), USER, payload(&req))
        .await?;
        let reply: ExportSessionActionReply = fauna_cbor::decode_strict(&bytes).unwrap();
        Ok(reply.session)
    }

    /// A driver's own failure report: conditioned on the generation it drives.
    async fn fail(
        state: &Arc<AppState>,
        actor: [u8; 32],
        sid: &str,
        reason: &str,
        as_driver_of: Option<u64>,
    ) -> Result<ExportSessionInfo, RpcError> {
        let req = FailExportSessionRequest {
            session_id: sid.into(),
            reason: reason.into(),
            stream_generation: as_driver_of,
            ..Default::default()
        };
        let bytes = fail_export_session_handler()(state.clone(), actor, payload(&req)).await?;
        let reply: ExportSessionActionReply = fauna_cbor::decode_strict(&bytes).unwrap();
        Ok(reply.session)
    }

    async fn action(
        state: &Arc<AppState>,
        actor: [u8; 32],
        kind: &'static str,
        new_state: ExportState,
        allowed: &'static [ExportState],
        sid: &str,
    ) -> Result<ExportSessionInfo, RpcError> {
        let req = ExportSessionActionRequest {
            session_id: sid.into(),
            ..Default::default()
        };
        let bytes = session_transition_handler(kind, new_state, allowed)(
            state.clone(),
            actor,
            payload(&req),
        )
        .await?;
        let reply: ExportSessionActionReply = fauna_cbor::decode_strict(&bytes).unwrap();
        Ok(reply.session)
    }

    async fn finalize(
        state: &Arc<AppState>,
        actor: [u8; 32],
        sid: &str,
    ) -> Result<ExportSessionInfo, RpcError> {
        action(
            state,
            actor,
            "fauna.bridges.finalize_export_session",
            ExportState::Completed,
            &[ExportState::Running],
            sid,
        )
        .await
    }

    fn blob_file(dir: &tempfile::TempDir, sid: &str) -> std::path::PathBuf {
        dir.path().join(export_blob_path_for(sid))
    }

    // ── the pipeline, end to end through the handlers ─────────────

    #[tokio::test]
    async fn chunks_concatenate_in_order_and_finalize_opens_the_download() {
        let (state, dir) = fixture().await;
        let sid = start(&state, USER).await;

        // No file until the first chunk lands: § Reclaim rule 1 is row before
        // file, and the reclaim's directory-listing-then-rows order is race-free
        // only if a session that has uploaded nothing has nothing on disk.
        assert!(!blob_file(&dir, &sid).exists());

        let r0 = upload(&state, USER, &sid, 0, b"frame-0").await.unwrap();
        assert_eq!(
            (r0.blob_bytes, r0.next_chunk_idx, r0.exported_count),
            (7, 1, 1)
        );
        let r1 = upload(&state, USER, &sid, 1, b"frame-1").await.unwrap();
        assert_eq!(
            (r1.blob_bytes, r1.next_chunk_idx, r1.exported_count),
            (14, 2, 2)
        );
        assert_eq!(
            std::fs::read(blob_file(&dir, &sid)).unwrap(),
            b"frame-0frame-1"
        );

        // § Architectural rules, no partial-blob download: a running session
        // hands out no download handle.
        let listed = list_one(&state, USER, &sid).await;
        assert_eq!(listed.download_url, "");
        assert_eq!(listed.next_chunk_idx, 2);

        let done = finalize(&state, USER, &sid).await.unwrap();
        assert_eq!(done.state, EXPORT_STATE_COMPLETED);
        assert_eq!(done.download_url, format!("/api/v1/export/{sid}"));
        assert_eq!(done.blob_bytes, 14);
        // § Key material: the wrapped key rides the session view so ANY of the
        // user's clients can open the download.
        assert_eq!(
            done.wrapped_session_key.as_ref().map(|b| &b[..]),
            Some(&b"wrapped"[..])
        );
    }

    async fn list_one(state: &Arc<AppState>, actor: [u8; 32], sid: &str) -> ExportSessionInfo {
        let bytes = list_export_sessions_handler()(
            state.clone(),
            actor,
            payload(&ListExportSessionsRequest {}),
        )
        .await
        .expect("list ok");
        let reply: ListExportSessionsReply = fauna_cbor::decode_strict(&bytes).unwrap();
        reply
            .sessions
            .into_iter()
            .find(|s| s.session_id == sid)
            .expect("the session is in the caller's list")
    }

    /// The nest appends opaque bytes and parses no frame, so it can neither
    /// reorder an upload nor notice afterwards that one was reordered. Without
    /// the cursor guard this would be a blob that fails AEAD at download time,
    /// hours later, naming nothing.
    #[tokio::test]
    async fn an_out_of_order_chunk_is_refused_with_the_index_to_resume_from() {
        let (state, dir) = fixture().await;
        let sid = start(&state, USER).await;
        upload(&state, USER, &sid, 0, b"frame-0").await.unwrap();

        for wrong in [0u64, 2, 99] {
            let err = upload(&state, USER, &sid, wrong, b"XXXX")
                .await
                .unwrap_err();
            assert_eq!(err.code, "fauna.bridges.export_chunk_out_of_order");
            assert!(
                format!("{:?}", err.details).contains(" 1"),
                "the refusal must carry the expected index, got {:?}",
                err.details
            );
        }
        // Nothing the guard refused reached the disk.
        assert_eq!(std::fs::read(blob_file(&dir, &sid)).unwrap(), b"frame-0");
    }

    /// § Quota composition's per-session ceiling. The refusal must not advance
    /// the running total either — a client that narrows its scope and retries
    /// would otherwise be locked out by its own rejected chunk.
    #[tokio::test]
    async fn the_ceiling_refuses_without_writing_a_byte_or_advancing_the_total() {
        let (state, dir) = fixture().await;
        state
            .db
            .put_export_policy(crate::db::mail_policy::ExportPolicyOverrides {
                max_blob_bytes: Some(8),
                max_concurrent_per_user: None,
            })
            .await
            .unwrap();
        let sid = start(&state, USER).await;
        upload(&state, USER, &sid, 0, b"12345").await.unwrap();

        let err = upload(&state, USER, &sid, 1, b"123456").await.unwrap_err();
        assert_eq!(err.code, "fauna.bridges.session_blob_oversize");
        assert_eq!(std::fs::read(blob_file(&dir, &sid)).unwrap(), b"12345");
        assert_eq!(list_one(&state, USER, &sid).await.blob_bytes, 5);

        // The narrowed retry still fits and is accepted at the SAME index.
        let ok = upload(&state, USER, &sid, 1, b"12").await.unwrap();
        assert_eq!(ok.blob_bytes, 7);
    }

    /// § Quota composition's concurrency cap, read from nest state.
    #[tokio::test]
    async fn the_concurrency_cap_is_read_from_nest_state_not_a_constant() {
        let (state, _dir) = fixture().await;
        state
            .db
            .put_export_policy(crate::db::mail_policy::ExportPolicyOverrides {
                max_blob_bytes: None,
                max_concurrent_per_user: Some(1),
            })
            .await
            .unwrap();
        let _first = start(&state, USER).await;
        let req = StartExportSessionRequest {
            format: "mbox".into(),
            scope_descriptor: b"scope".to_vec(),
            wrapped_session_key: Some(serde_bytes::ByteBuf::from(b"w".to_vec())),
            total_count: 0,
            ..Default::default()
        };
        let err = start_export_session_handler()(state.clone(), USER, payload(&req))
            .await
            .unwrap_err();
        assert_eq!(err.code, "fauna.bridges.export_concurrency_cap_reached");
    }

    /// A session with no wrapped key produces a blob NO client can open
    /// (§ Key material). Refusing at the door is the difference between a typed
    /// error now and a 10 GiB artifact that serves nobody for 30 days.
    #[tokio::test]
    async fn a_session_without_a_wrapped_key_or_a_known_format_is_refused() {
        let (state, _dir) = fixture().await;
        for (format, key) in [
            ("mbox", None),
            ("mbox", Some(serde_bytes::ByteBuf::new())),
            ("pst", Some(serde_bytes::ByteBuf::from(b"w".to_vec()))),
        ] {
            let req = StartExportSessionRequest {
                format: format.into(),
                scope_descriptor: b"s".to_vec(),
                wrapped_session_key: key,
                total_count: 0,
                ..Default::default()
            };
            assert!(
                start_export_session_handler()(state.clone(), USER, payload(&req))
                    .await
                    .is_err(),
                "format={format} must be refused"
            );
        }
    }

    // ── § Cross-actor isolation ───────────────────────────────────

    /// "A user exports their own mailboxes only." Every session kind must be
    /// blind to another actor's session — and blind in the not-found direction,
    /// never a wrong-state error that would confirm the session exists.
    #[tokio::test]
    async fn a_foreign_actor_can_neither_read_drive_nor_discard_the_session() {
        let (state, dir) = fixture().await;
        let sid = start(&state, USER).await;
        upload(&state, USER, &sid, 0, b"frame-0").await.unwrap();

        assert!(list_one_opt(&state, OTHER, &sid).await.is_none());
        for (kind, new_state, allowed) in [
            (
                "fauna.bridges.pause_export_session",
                ExportState::Paused,
                &[ExportState::Running][..],
            ),
            (
                "fauna.bridges.cancel_export_session",
                ExportState::Cancelled,
                &[ExportState::Running, ExportState::Paused][..],
            ),
            (
                "fauna.bridges.finalize_export_session",
                ExportState::Completed,
                &[ExportState::Running][..],
            ),
        ] {
            assert!(
                action(&state, OTHER, kind, new_state, allowed, &sid)
                    .await
                    .is_err(),
                "{kind} must refuse a foreign caller"
            );
        }
        assert!(upload(&state, OTHER, &sid, 1, b"x").await.is_err());

        let req = ExportSessionActionRequest {
            session_id: sid.clone(),
            ..Default::default()
        };
        let bytes = discard_export_blob_handler()(state.clone(), OTHER, payload(&req))
            .await
            .unwrap();
        let reply: DiscardExportBlobReply = fauna_cbor::decode_strict(&bytes).unwrap();
        assert!(!reply.existed, "a foreign discard must be a no-op");

        // None of it damaged the owner's session or its bytes.
        assert_eq!(std::fs::read(blob_file(&dir, &sid)).unwrap(), b"frame-0");
        let mine = list_one(&state, USER, &sid).await;
        assert_eq!(mine.state, EXPORT_STATE_RUNNING);
        assert_eq!(mine.blob_bytes, 7);
    }

    async fn list_one_opt(
        state: &Arc<AppState>,
        actor: [u8; 32],
        sid: &str,
    ) -> Option<ExportSessionInfo> {
        let bytes = list_export_sessions_handler()(
            state.clone(),
            actor,
            payload(&ListExportSessionsRequest {}),
        )
        .await
        .expect("list ok");
        let reply: ListExportSessionsReply = fauna_cbor::decode_strict(&bytes).unwrap();
        reply.sessions.into_iter().find(|s| s.session_id == sid)
    }

    // ── § Reclaim: the file follows the row ───────────────────────

    /// § Wire shapes: cancel is "abort + unlink blob". A half-written
    /// whole-mailbox snapshot has no reader, and holding it for 30 days is the
    /// obligation § Expiry exists to refuse.
    // ── § Resume — the cold resume ─────────────────────────────────────

    #[tokio::test]
    async fn a_restart_gives_the_new_stream_an_empty_file_of_its_own() {
        let (state, dir) = fixture().await;
        let sid = start(&state, USER).await;
        upload(&state, USER, &sid, 0, b"old-0").await.unwrap();
        upload(&state, USER, &sid, 1, b"old-1").await.unwrap();
        let old_file = blob_file(&dir, &sid);
        assert_eq!(std::fs::read(&old_file).unwrap(), b"old-0old-1");

        let info = restart(&state, USER, &sid, Some(b"fresh-key"))
            .await
            .unwrap();
        assert_eq!(info.state, "running");
        assert_eq!(info.stream_generation, 1);
        assert_eq!(
            (info.next_chunk_idx, info.blob_bytes, info.exported_count),
            (0, 0, 0)
        );
        assert_eq!(
            info.wrapped_session_key.as_deref().map(|k| &k[..]),
            Some(&b"fresh-key"[..]),
            "the session view must hand the NEW key to whoever downloads"
        );
        assert!(
            !old_file.exists(),
            "the abandoned generation's blob is unlinked"
        );

        upload_as(&state, &sid, 1, 0, b"new-0").await.unwrap();
        upload_as(&state, &sid, 1, 1, b"new-1").await.unwrap();
        let new_file = dir
            .path()
            .join(crate::mail_export_blobs::export_blob_path_for_generation(
                &sid, 1,
            ));
        assert_eq!(
            std::fs::read(&new_file).unwrap(),
            b"new-0new-1",
            "the restarted blob holds the new stream and not one stale byte"
        );
        assert!(!old_file.exists());

        // The download serves the row's CURRENT path once finalized.
        let done = finalize(&state, USER, &sid).await.unwrap();
        assert_eq!(done.state, "completed");
        assert_eq!(done.blob_bytes, 10);
    }

    #[tokio::test]
    async fn a_running_session_restarts_because_nobody_was_left_to_pause_it() {
        // The main case: the app was closed mid-export, so the row still reads
        // `running`. A `paused`-only restart would be unreachable exactly here.
        let (state, _dir) = fixture().await;
        let sid = start(&state, USER).await;
        assert_eq!(list_one(&state, USER, &sid).await.state, "running");
        assert_eq!(
            restart(&state, USER, &sid, Some(b"k"))
                .await
                .unwrap()
                .stream_generation,
            1
        );
        // …and a paused one restarts straight to `running`.
        action(
            &state,
            USER,
            "fauna.bridges.pause_export_session",
            ExportState::Paused,
            &[ExportState::Running],
            &sid,
        )
        .await
        .unwrap();
        let info = restart(&state, USER, &sid, Some(b"k2")).await.unwrap();
        assert_eq!(
            (info.state.as_str(), info.stream_generation),
            ("running", 2)
        );
    }

    #[tokio::test]
    async fn the_superseded_driver_is_shut_out_and_cannot_cancel_its_successor() {
        let (state, _dir) = fixture().await;
        let sid = start(&state, USER).await;
        upload(&state, USER, &sid, 0, b"old-0").await.unwrap();
        restart(&state, USER, &sid, Some(b"k")).await.unwrap();

        // Whatever index the old driver sends — its own next one, or the one
        // the new stream happens to expect — the answer is "not the driver".
        for idx in [0, 1] {
            let err = upload_as(&state, &sid, 0, idx, b"stale").await.unwrap_err();
            assert_eq!(err.code, EXPORT_STREAM_SUPERSEDED, "idx {idx}: {err:?}");
        }
        // Its failure path's cancel is conditioned on ITS generation…
        let err = cancel_as_driver(&state, &sid, 0).await.unwrap_err();
        assert_eq!(err.code, EXPORT_STREAM_SUPERSEDED);
        let row = list_one(&state, USER, &sid).await;
        assert_eq!((row.state.as_str(), row.next_chunk_idx), ("running", 0));
        // …while the new driver's own is honoured, and so is the user's
        // unconditional Cancel from any device.
        upload_as(&state, &sid, 1, 0, b"new-0").await.unwrap();
        assert_eq!(
            cancel_as_driver(&state, &sid, 1).await.unwrap().state,
            "cancelled"
        );
    }

    #[tokio::test]
    async fn a_superseded_resume_is_not_mistaken_for_one_that_already_happened() {
        // Device A paused and parked its stream; device B restarted. A's warm
        // Resume finds the session `running` — the convergence arm's "already
        // got what it asked for" — but the stream running is B's.
        let (state, _dir) = fixture().await;
        let sid = start(&state, USER).await;
        restart(&state, USER, &sid, Some(b"k")).await.unwrap();
        let req = ExportSessionActionRequest {
            session_id: sid.clone(),
            stream_generation: Some(0),
            ..Default::default()
        };
        let err = session_transition_handler(
            "fauna.bridges.resume_export_session",
            ExportState::Running,
            &[ExportState::Paused],
        )(state.clone(), USER, payload(&req))
        .await
        .unwrap_err();
        assert_eq!(err.code, EXPORT_STREAM_SUPERSEDED);
    }

    #[tokio::test]
    async fn a_restart_needs_a_key_an_in_flight_session_and_its_owner() {
        let (state, _dir) = fixture().await;
        let sid = start(&state, USER).await;
        assert!(restart(&state, USER, &sid, None).await.is_err());
        assert!(restart(&state, USER, &sid, Some(b"")).await.is_err());
        assert!(restart(&state, OTHER, &sid, Some(b"k")).await.is_err());
        assert_eq!(list_one(&state, USER, &sid).await.stream_generation, 0);

        action(
            &state,
            USER,
            "fauna.bridges.cancel_export_session",
            ExportState::Cancelled,
            &[ExportState::Running, ExportState::Paused],
            &sid,
        )
        .await
        .unwrap();
        assert!(
            restart(&state, USER, &sid, Some(b"k")).await.is_err(),
            "a cancelled session released its slot; it does not come back"
        );
    }

    #[tokio::test]
    async fn cancel_unlinks_the_blob_and_keeps_the_row() {
        let (state, dir) = fixture().await;
        let sid = start(&state, USER).await;
        upload(&state, USER, &sid, 0, b"frame-0").await.unwrap();
        assert!(blob_file(&dir, &sid).exists());

        let row = action(
            &state,
            USER,
            "fauna.bridges.cancel_export_session",
            ExportState::Cancelled,
            &[ExportState::Running, ExportState::Paused],
            &sid,
        )
        .await
        .unwrap();
        assert_eq!(row.state, EXPORT_STATE_CANCELLED);
        assert!(!blob_file(&dir, &sid).exists());
        assert!(list_one_opt(&state, USER, &sid).await.is_some());
    }

    /// `mail-export.md` § Resume — the kind's whole point. The disposal is the
    /// cancel's (the partial blob has no terminator and can never be opened),
    /// and what the cancel could not do is what this adds: the row survives
    /// reading `errored` and carrying WHY, where a second client can read it.
    #[tokio::test]
    async fn fail_records_the_reason_unlinks_the_blob_and_keeps_the_row() {
        let (state, dir) = fixture().await;
        let sid = start(&state, USER).await;
        upload(&state, USER, &sid, 0, b"frame-0").await.unwrap();
        assert!(blob_file(&dir, &sid).exists());

        let row = fail(&state, USER, &sid, "INBOX uid 2 will not open", Some(0))
            .await
            .unwrap();
        assert_eq!(row.state, EXPORT_STATE_ERRORED);
        assert_eq!(row.error_reason, "INBOX uid 2 will not open");
        assert!(
            !blob_file(&dir, &sid).exists(),
            "an archive without its terminator frame is disposed of, not kept"
        );

        // The listing is where the record has to land: this is the device that
        // saw the failure, and the point is that the OTHERS learn of it.
        let listed = list_one(&state, USER, &sid).await;
        assert_eq!(listed.state, EXPORT_STATE_ERRORED);
        assert_eq!(listed.error_reason, "INBOX uid 2 will not open");
    }

    /// An empty reason leaves exactly the record-free row this kind exists to
    /// improve on, and the client always has a reason to give — so it is
    /// refused, as the import twin refuses it, before the session moves.
    #[tokio::test]
    async fn a_failure_with_no_reason_is_refused_and_moves_nothing() {
        let (state, dir) = fixture().await;
        let sid = start(&state, USER).await;
        upload(&state, USER, &sid, 0, b"frame-0").await.unwrap();

        assert!(fail(&state, USER, &sid, "", Some(0)).await.is_err());
        assert_eq!(list_one(&state, USER, &sid).await.state, "running");
        assert!(blob_file(&dir, &sid).exists(), "and nothing was unlinked");
    }

    /// A failure that is one device's alone — a dropped fetch — landing after
    /// another device restarted the export. § Resume's only-while-I-am-still-
    /// the-driver condition holds for this kind exactly as for the cancel: the
    /// healthy stream is neither disposed of nor painted `errored`.
    #[tokio::test]
    async fn a_superseded_driver_cannot_fail_the_stream_that_replaced_its_own() {
        let (state, dir) = fixture().await;
        let sid = start(&state, USER).await;
        upload(&state, USER, &sid, 0, b"old-0").await.unwrap();
        restart(&state, USER, &sid, Some(b"k")).await.unwrap();
        upload_as(&state, &sid, 1, 0, b"new-0").await.unwrap();

        let err = fail(&state, USER, &sid, "a dropped fetch", Some(0))
            .await
            .unwrap_err();
        assert_eq!(err.code, EXPORT_STREAM_SUPERSEDED);
        let row = list_one(&state, USER, &sid).await;
        assert_eq!((row.state.as_str(), row.stream_generation), ("running", 1));
        assert!(row.error_reason.is_empty());
        let new_file = dir
            .path()
            .join(crate::mail_export_blobs::export_blob_path_for_generation(
                &sid, 1,
            ));
        assert_eq!(
            std::fs::read(&new_file).unwrap(),
            b"new-0",
            "the running stream's blob is untouched"
        );
    }

    /// Only the owner may fail a session, and a foreign caller learns nothing
    /// about one that is not theirs (§ Cross-actor isolation).
    #[tokio::test]
    async fn a_foreign_actor_cannot_fail_the_owners_session() {
        let (state, dir) = fixture().await;
        let sid = start(&state, USER).await;
        upload(&state, USER, &sid, 0, b"frame-0").await.unwrap();

        assert!(
            fail(&state, OTHER, &sid, "not mine to fail", None)
                .await
                .is_err()
        );
        assert_eq!(list_one(&state, USER, &sid).await.state, "running");
        assert!(blob_file(&dir, &sid).exists());
    }

    /// § Reclaim rule 2 — file before row. The row is gone only once the file
    /// is, and a second discard answers the same thing rather than failing.
    #[tokio::test]
    async fn discard_unlinks_then_deletes_and_is_idempotent() {
        let (state, dir) = fixture().await;
        let sid = start(&state, USER).await;
        upload(&state, USER, &sid, 0, b"frame-0").await.unwrap();

        let req = ExportSessionActionRequest {
            session_id: sid.clone(),
            ..Default::default()
        };
        let bytes = discard_export_blob_handler()(state.clone(), USER, payload(&req))
            .await
            .unwrap();
        let reply: DiscardExportBlobReply = fauna_cbor::decode_strict(&bytes).unwrap();
        assert!(reply.existed);
        assert!(!blob_file(&dir, &sid).exists());
        assert!(list_one_opt(&state, USER, &sid).await.is_none());

        let bytes = discard_export_blob_handler()(state.clone(), USER, payload(&req))
            .await
            .unwrap();
        let reply: DiscardExportBlobReply = fauna_cbor::decode_strict(&bytes).unwrap();
        assert!(!reply.existed);
    }

    /// § Reclaim rule 2's other half: an unlink that fails leaves the row
    /// standing, so the discard can be retried rather than orphaning a
    /// whole-mailbox snapshot nothing can find.
    #[tokio::test]
    async fn a_discard_whose_unlink_fails_keeps_the_row_for_the_retry() {
        let (state, dir) = fixture().await;
        let sid = start(&state, USER).await;
        upload(&state, USER, &sid, 0, b"frame-0").await.unwrap();
        // `remove_file` on a directory fails on every platform, as root too.
        std::fs::remove_file(blob_file(&dir, &sid)).unwrap();
        std::fs::create_dir(blob_file(&dir, &sid)).unwrap();

        let req = ExportSessionActionRequest {
            session_id: sid.clone(),
            ..Default::default()
        };
        assert!(
            discard_export_blob_handler()(state.clone(), USER, payload(&req))
                .await
                .is_err(),
            "a blob that will not unlink must fail the discard"
        );
        assert!(
            list_one_opt(&state, USER, &sid).await.is_some(),
            "the row is the only record of where the blob is"
        );

        std::fs::remove_dir(blob_file(&dir, &sid)).unwrap();
        let bytes = discard_export_blob_handler()(state.clone(), USER, payload(&req))
            .await
            .unwrap();
        let reply: DiscardExportBlobReply = fauna_cbor::decode_strict(&bytes).unwrap();
        assert!(reply.existed, "the retry completes");
        assert!(list_one_opt(&state, USER, &sid).await.is_none());
    }

    /// A discard that raced a cold resume unlinked the OLD generation's file;
    /// the row now names the new one, which nobody unlinked, so it must stay.
    #[tokio::test]
    async fn a_discard_racing_a_restart_leaves_the_new_stream_its_row() {
        let (state, dir) = fixture().await;
        let sid = start(&state, USER).await;
        upload(&state, USER, &sid, 0, b"frame-0").await.unwrap();
        // What the discard read before the restart landed.
        let unlinked = state
            .db
            .get_export_session(&USER, &sid)
            .await
            .unwrap()
            .unwrap()
            .blob_path;
        restart(&state, USER, &sid, Some(&b"k2"[..])).await.unwrap();
        upload_as(&state, &sid, 1, 0, b"new-0").await.unwrap();

        assert!(
            !state
                .db
                .delete_export_session(&USER, &sid, &unlinked)
                .await
                .unwrap()
        );
        let row = list_one(&state, USER, &sid).await;
        assert_eq!(row.stream_generation, 1);
        assert!(
            dir.path()
                .join("exports")
                .join(format!("{sid}.1.zip.zst.sealed"))
                .exists(),
            "the new stream's file is still named, so it is still there"
        );
    }

    /// § Quota composition's per-user footprint, end to end: finished exports
    /// hold it after their concurrency slots are free, the refusal comes at
    /// Start when there is no room and at the chunk that would cross it
    /// otherwise, and Discard — the remedy the message names — frees it.
    #[tokio::test]
    async fn the_footprint_holds_finished_exports_until_they_are_discarded() {
        let (state, dir) = fixture().await;
        // 10 bytes per session × 3 sessions = 30 bytes per user.
        state
            .db
            .put_export_policy(crate::db::mail_policy::ExportPolicyOverrides {
                max_blob_bytes: Some(10),
                max_concurrent_per_user: Some(3),
            })
            .await
            .unwrap();
        let first = start(&state, USER).await;
        upload(&state, USER, &first, 0, b"0123456789")
            .await
            .unwrap();
        finalize(&state, USER, &first).await.unwrap();
        let second = start(&state, USER).await;
        upload(&state, USER, &second, 0, b"0123456789")
            .await
            .unwrap();
        finalize(&state, USER, &second).await.unwrap();
        let third = start(&state, USER).await;
        upload(&state, USER, &third, 0, b"01234567").await.unwrap();

        // 28 held, one slot in use. A 3-byte frame fits the fourth session's
        // own 10 but not the user's 30.
        let fourth = start(&state, USER).await;
        let err = upload(&state, USER, &fourth, 0, b"abc").await.unwrap_err();
        assert_eq!(err.code, "fauna.bridges.export_footprint_exceeded");
        assert!(
            !blob_file(&dir, &fourth).exists(),
            "the refused chunk must not reach the disk"
        );
        assert_eq!(list_one(&state, USER, &fourth).await.next_chunk_idx, 0);
        upload(&state, USER, &fourth, 0, b"ab").await.unwrap();

        // 30 held, two slots in use: the next Start is refused over the disk,
        // not the slots.
        let req = StartExportSessionRequest {
            format: "mbox".into(),
            scope_descriptor: b"scope".to_vec(),
            wrapped_session_key: Some(serde_bytes::ByteBuf::from(b"w".to_vec())),
            total_count: 0,
            ..Default::default()
        };
        let err = start_export_session_handler()(state.clone(), USER, payload(&req))
            .await
            .unwrap_err();
        assert_eq!(err.code, "fauna.bridges.export_footprint_exceeded");
        // Another user is not rationed by this one's exports.
        start(&state, OTHER).await;

        // Discard — the remedy the refusal names — frees the footprint.
        let discard = ExportSessionActionRequest {
            session_id: first.clone(),
            ..Default::default()
        };
        discard_export_blob_handler()(state.clone(), USER, payload(&discard))
            .await
            .unwrap();
        start(&state, USER).await;
    }

    /// Hardening: the counters are the client's numbers in `INTEGER` columns.
    /// A value with no exact `i64` is a malformed request — refused before the
    /// reservation, so no frame lands whose batch was never counted.
    #[tokio::test]
    async fn an_out_of_range_counter_is_refused_before_anything_is_written() {
        let (state, dir) = fixture().await;
        let too_big = i64::MAX as u64 + 1;
        let sid = start(&state, USER).await;
        for req in [
            UploadExportChunkRequest {
                exported_delta: too_big,
                ..Default::default()
            },
            UploadExportChunkRequest {
                skipped_delta: too_big,
                ..Default::default()
            },
            UploadExportChunkRequest {
                errored_delta: too_big,
                ..Default::default()
            },
            UploadExportChunkRequest {
                revised_total_count: Some(too_big),
                ..Default::default()
            },
        ] {
            let req = UploadExportChunkRequest {
                session_id: sid.clone(),
                sealed_chunk: b"frame-0".to_vec(),
                ..req
            };
            let err = upload_export_chunk_handler()(state.clone(), USER, payload(&req))
                .await
                .unwrap_err();
            assert_eq!(err.code, malformed("").code);
        }
        assert!(!blob_file(&dir, &sid).exists());
        let row = list_one(&state, USER, &sid).await;
        assert_eq!((row.next_chunk_idx, row.exported_count), (0, 0));

        let start_req = StartExportSessionRequest {
            format: "mbox".into(),
            scope_descriptor: b"scope".to_vec(),
            wrapped_session_key: Some(serde_bytes::ByteBuf::from(b"w".to_vec())),
            total_count: too_big,
            ..Default::default()
        };
        let err = start_export_session_handler()(state.clone(), USER, payload(&start_req))
            .await
            .unwrap_err();
        assert_eq!(err.code, malformed("").code);
        let restart_req = RestartExportSessionRequest {
            session_id: sid.clone(),
            wrapped_session_key: Some(serde_bytes::ByteBuf::from(b"k".to_vec())),
            total_count: too_big,
            ..Default::default()
        };
        let err = restart_export_session_handler()(state.clone(), USER, payload(&restart_req))
            .await
            .unwrap_err();
        assert_eq!(err.code, malformed("").code);
        assert_eq!(list_one(&state, USER, &sid).await.stream_generation, 0);
    }

    /// A paused session accepts no chunks, and the refusal says *paused* rather
    /// than *not found* — the difference between "resume it" and "it is gone".
    #[tokio::test]
    async fn a_paused_session_refuses_chunks_and_says_why() {
        let (state, _dir) = fixture().await;
        let sid = start(&state, USER).await;
        action(
            &state,
            USER,
            "fauna.bridges.pause_export_session",
            ExportState::Paused,
            &[ExportState::Running],
            &sid,
        )
        .await
        .unwrap();

        let err = upload(&state, USER, &sid, 0, b"x").await.unwrap_err();
        assert!(
            format!("{:?}", err.details).contains("paused"),
            "the refusal must name the state, got {:?}",
            err.details
        );

        action(
            &state,
            USER,
            "fauna.bridges.resume_export_session",
            ExportState::Running,
            &[ExportState::Paused],
            &sid,
        )
        .await
        .unwrap();
        assert!(upload(&state, USER, &sid, 0, b"x").await.is_ok());
    }

    /// A terminal session's 30-day window must not be re-armable. Guarded twice:
    /// the fold runs only behind the reservation's `state = 'running'` check,
    /// and refuses a session no longer in flight in its own UPDATE.
    #[tokio::test]
    async fn a_completed_session_takes_no_further_chunk_or_counter() {
        let (state, _dir) = fixture().await;
        let sid = start(&state, USER).await;
        upload(&state, USER, &sid, 0, b"frame-0").await.unwrap();
        let done = finalize(&state, USER, &sid).await.unwrap();

        assert!(upload(&state, USER, &sid, 1, b"more").await.is_err());
        let after = list_one(&state, USER, &sid).await;
        assert_eq!(after.exported_count, done.exported_count);
        assert_eq!(after.blob_bytes, done.blob_bytes);
        assert_eq!(after.expires_at, done.expires_at);
    }

    // ── list_own_mailboxes ────────────────────────────────────────

    /// Caller-scoped by construction — the request carries no actor at all —
    /// and ordered by the name's raw bytes, the total order § Container shape's
    /// determinism contract emits archive entries in.
    #[tokio::test]
    async fn own_mailboxes_are_the_callers_own_and_byte_ordered() {
        let (state, _dir) = fixture().await;
        let bytes =
            list_own_mailboxes_handler()(state.clone(), USER, payload(&ListOwnMailboxesRequest {}))
                .await
                .expect("list own mailboxes ok");
        let reply: ListOwnMailboxesReply = fauna_cbor::decode_strict(&bytes).unwrap();
        assert!(
            reply.mailboxes.iter().any(|m| m.name == "INBOX"),
            "a mail-serving actor always has INBOX, got {:?}",
            reply.mailboxes
        );
        let names: Vec<&str> = reply.mailboxes.iter().map(|m| m.name.as_str()).collect();
        let mut sorted = names.clone();
        sorted.sort_by(|a, b| a.as_bytes().cmp(b.as_bytes()));
        assert_eq!(names, sorted, "mailboxes must come back byte-ordered");
    }

    // ── fetch_export_chunk_ciphertext ─────────────────────────────

    /// The down-leg refuses a session that is not running, and reports
    /// `mailbox_done` as a fact about the query rather than an inference from a
    /// short page — a client that guessed from emptiness would truncate the
    /// archive at the first UID gap.
    #[tokio::test]
    async fn the_down_leg_needs_a_running_session_and_reports_the_end_of_a_mailbox() {
        let (state, _dir) = fixture().await;
        let sid = start(&state, USER).await;
        let req = FetchExportChunkCiphertextRequest {
            session_id: sid.clone(),
            mailbox: "INBOX".into(),
            after_uid: 0,
            max_messages: 0,
            max_bytes: 0,
            ..Default::default()
        };
        let bytes = fetch_export_chunk_ciphertext_handler()(state.clone(), USER, payload(&req))
            .await
            .expect("fetch ok");
        let reply: FetchExportChunkCiphertextReply = fauna_cbor::decode_strict(&bytes).unwrap();
        assert!(reply.messages.is_empty());
        assert!(reply.mailbox_done, "an empty mailbox is a finished one");

        action(
            &state,
            USER,
            "fauna.bridges.pause_export_session",
            ExportState::Paused,
            &[ExportState::Running],
            &sid,
        )
        .await
        .unwrap();
        assert!(
            fetch_export_chunk_ciphertext_handler()(state.clone(), USER, payload(&req))
                .await
                .is_err(),
            "a paused session must not stream ciphertext"
        );

        // And a foreign caller is told not-found, never the session's state.
        let bytes = fetch_export_chunk_ciphertext_handler()(state.clone(), OTHER, payload(&req))
            .await
            .unwrap_err();
        assert!(format!("{:?}", bytes.details).contains("not found"));
    }
}
